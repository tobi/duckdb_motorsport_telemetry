//! SQL surface for the global unit registry.
//!
//! Exposes three things:
//!
//! - `telemetry_units()` — the registry as a table, so the vocabulary is
//!   discoverable rather than buried in Rust.
//! - `telemetry_convert(value, from, to)` — dimension-checked conversion that
//!   errors on nonsense instead of returning a wrong number.
//! - `telemetry_can_convert(from, to)` — a boolean probe for the same.

use duckdb::core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId};
use duckdb::ffi::duckdb_string_t;
use duckdb::types::DuckString;
use duckdb::vscalar::{ScalarFunctionSignature, VScalar};
use duckdb::vtab::arrow::WritableVector;
use duckdb::vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab};
use motorsport_telemetry_core::units::{self, Dimension, UNITS};
use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{ty, FlatVectorExt, VECTOR_SIZE};
use duckdb::core::Inserter;

// ── telemetry_units(): the registry as a table ──────────────────────

/// One row per (unit, spelling) so both canonical names and file-local aliases
/// are searchable. `is_canonical` distinguishes them.
pub struct UnitsBind {
    rows: Vec<UnitRow>,
}

struct UnitRow {
    spelling: &'static str,
    canonical: &'static str,
    is_canonical: bool,
    dimension: Dimension,
    factor: f64,
    offset: f64,
}

pub struct UnitsInit {
    next: AtomicUsize,
}

pub struct UnitsVTab;

impl VTab for UnitsVTab {
    type BindData = UnitsBind;
    type InitData = UnitsInit;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        for (name, logical) in [
            ("unit", LogicalTypeId::Varchar),
            ("canonical_unit", LogicalTypeId::Varchar),
            ("is_canonical", LogicalTypeId::Boolean),
            ("dimension", LogicalTypeId::Varchar),
            ("base_unit", LogicalTypeId::Varchar),
            ("to_base_factor", LogicalTypeId::Double),
            ("to_base_offset", LogicalTypeId::Double),
            ("is_convertible", LogicalTypeId::Boolean),
        ] {
            bind.add_result_column(name, ty(logical));
        }

        let mut rows = Vec::new();
        for def in UNITS {
            rows.push(UnitRow {
                spelling: def.canonical,
                canonical: def.canonical,
                is_canonical: true,
                dimension: def.dimension,
                factor: def.factor,
                offset: def.offset,
            });
            for alias in def.aliases {
                rows.push(UnitRow {
                    spelling: alias,
                    canonical: def.canonical,
                    is_canonical: false,
                    dimension: def.dimension,
                    factor: def.factor,
                    offset: def.offset,
                });
            }
        }
        bind.set_cardinality(rows.len() as u64, true);
        Ok(UnitsBind { rows })
    }

    fn init(_init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        Ok(UnitsInit {
            next: AtomicUsize::new(0),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let bind = func.get_bind_data();
        let state = func.get_init_data();
        let start = state.next.fetch_add(VECTOR_SIZE as usize, Ordering::SeqCst);
        if start >= bind.rows.len() {
            output.set_len(0);
            return Ok(());
        }
        let end = (start + VECTOR_SIZE as usize).min(bind.rows.len());
        for (row, entry) in bind.rows[start..end].iter().enumerate() {
            output.flat_vector(0).insert(row, entry.spelling);
            output.flat_vector(1).insert(row, entry.canonical);
            output.flat_vector(2).typed_slice::<bool>()[row] = entry.is_canonical;
            output.flat_vector(3).insert(row, entry.dimension.name());
            output
                .flat_vector(4)
                .insert(row, entry.dimension.base_unit());
            output.flat_vector(5).typed_slice::<f64>()[row] = entry.factor;
            output.flat_vector(6).typed_slice::<f64>()[row] = entry.offset;
            output.flat_vector(7).typed_slice::<bool>()[row] = entry.dimension.is_convertible();
        }
        output.set_len(end - start);
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![])
    }
}

// ── telemetry_convert(value, from, to) ──────────────────────────────

/// Dimension-checked unit conversion.
///
/// Errors rather than returning a plausible-looking wrong number: converting
/// m/s to bar, or a gear position to a percentage, is a bug in the query and
/// should surface as one.
pub struct ConvertScalar;

impl VScalar for ConvertScalar {
    type State = ();

    fn invoke(
        _state: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn Error>> {
        let rows = input.len();
        let values = input.flat_vector(0);
        let values = unsafe { values.as_slice_with_len::<f64>(rows) };
        let from = input.flat_vector(1);
        let from = unsafe { from.as_slice_with_len::<duckdb_string_t>(rows) };
        let to = input.flat_vector(2);
        let to = unsafe { to.as_slice_with_len::<duckdb_string_t>(rows) };

        let mut result = output.flat_vector();
        let out = unsafe { result.as_mut_slice_with_len::<f64>(rows) };
        for row in 0..rows {
            let from_unit = DuckString::new(&mut { from[row] }).as_str().to_string();
            let to_unit = DuckString::new(&mut { to[row] }).as_str().to_string();
            // Propagate the registry's error verbatim: a dimension mismatch is
            // a query bug and must not be silently coerced into a number.
            out[row] = units::convert(values[row], &from_unit, &to_unit)?;
        }
        Ok(())
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                ty(LogicalTypeId::Double),
                ty(LogicalTypeId::Varchar),
                ty(LogicalTypeId::Varchar),
            ],
            ty(LogicalTypeId::Double),
        )]
    }
}

/// True when a conversion is possible, for filtering without raising.
pub struct CanConvertScalar;

impl VScalar for CanConvertScalar {
    type State = ();

    fn invoke(
        _state: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn Error>> {
        let rows = input.len();
        let from = input.flat_vector(0);
        let from = unsafe { from.as_slice_with_len::<duckdb_string_t>(rows) };
        let to = input.flat_vector(1);
        let to = unsafe { to.as_slice_with_len::<duckdb_string_t>(rows) };

        let mut result = output.flat_vector();
        let out = unsafe { result.as_mut_slice_with_len::<bool>(rows) };
        for row in 0..rows {
            let from_unit = DuckString::new(&mut { from[row] }).as_str().to_string();
            let to_unit = DuckString::new(&mut { to[row] }).as_str().to_string();
            out[row] = units::can_convert(&from_unit, &to_unit);
        }
        Ok(())
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![ty(LogicalTypeId::Varchar), ty(LogicalTypeId::Varchar)],
            ty(LogicalTypeId::Boolean),
        )]
    }
}
