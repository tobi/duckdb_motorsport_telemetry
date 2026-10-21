//! SQL surface for the global unit registry.
//!
//! Exposes three things:
//!
//! - `telemetry_units()` — the registry as a table, so the vocabulary is
//!   discoverable rather than buried in Rust.
//! - `telemetry_convert(value, from, to)` — explicit dimension-checked conversion.
//! - `telemetry_convert_column(column, to)` — reads the source unit tag emitted
//!   by the wide telemetry readers and refuses ordinary scalar expressions.
//! - `telemetry_can_convert(from, to)` — a boolean probe for the same.

use super::table::{
    BindInfo, ChunkWriter as DataChunkHandle, InitInfo, SqlType as LogicalTypeHandle,
    SqlType as LogicalTypeId, TableFunctionInfo, TelemetryTable,
};
use super::{ty, VECTOR_SIZE};
use duckdb_neo::{
    connection::Context,
    data_chunk::VectorCollection,
    extension::Extension,
    logical_type::LogicalType,
    scalar::{ScalarCallbacks, ScalarFunctionBuilder},
    signature::{FunctionDocumentationBuilder, FunctionParameter, SignatureBuilder},
    types::Any,
    vector::{Unknown, Vector},
    DuckDBType,
};
use motorsport_telemetry_core::units::{self, Dimension, UNITS};
use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};

const UNIT_ALIAS_PREFIX: &str = "telemetry_unit:";

/// A DOUBLE carrying a canonical telemetry unit as its DuckDB logical alias.
/// The physical representation stays numeric, so ordinary SQL remains fast.
pub fn tagged_double(unit: &str) -> LogicalTypeHandle {
    let logical = ty(LogicalTypeId::Double);
    match units::normalize(unit) {
        Some(canonical) => {
            LogicalTypeHandle::Alias(Box::new(logical), format!("{UNIT_ALIAS_PREFIX}{canonical}"))
        }
        None => logical,
    }
}

fn tagged_unit(logical: &LogicalType) -> duckdb_neo::Result<Option<String>> {
    Ok(logical
        .name()?
        .strip_prefix(UNIT_ALIAS_PREFIX)
        .map(str::to_owned))
}

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

pub struct UnitsScan;

impl TelemetryTable for UnitsScan {
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
            bind.add_result_column(name, ty(logical))?;
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
        bind.set_cardinality(rows.len() as u64, true)?;
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
            output.set_len(0)?;
            return Ok(());
        }
        let end = (start + VECTOR_SIZE as usize).min(bind.rows.len());
        for (row, entry) in bind.rows[start..end].iter().enumerate() {
            output.flat_vector(0)?.insert(row, entry.spelling)?;
            output.flat_vector(1)?.insert(row, entry.canonical)?;
            output.flat_vector(2)?.typed_slice::<bool>()?[row] = entry.is_canonical;
            output.flat_vector(3)?.insert(row, entry.dimension.name())?;
            output
                .flat_vector(4)?
                .insert(row, entry.dimension.base_unit())?;
            output.flat_vector(5)?.typed_slice::<f64>()?[row] = entry.factor;
            output.flat_vector(6)?.typed_slice::<f64>()?[row] = entry.offset;
            output.flat_vector(7)?.typed_slice::<bool>()?[row] = entry.dimension.is_convertible();
        }
        output.set_len(end - start)?;
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![])
    }
}

// Scalar execution uses typed v2 views, which account for constant/dictionary
// selection and SQL NULLs. No scalar assumes a flat physical input buffer.
pub struct ConvertScalar;
pub struct ConvertColumnScalar;
pub struct CanConvertScalar;

impl ScalarCallbacks for ConvertScalar {
    type BindData = ();
    type InitData = ();
    fn exec(
        &self,
        _bind: Option<&()>,
        _init: Option<&mut ()>,
        _context: &Context,
        input: &mut VectorCollection,
        output: Vector<'_, Unknown>,
    ) -> duckdb_neo::Result<()> {
        let values = input.get_vector_at::<f64>(0)?;
        let from = input.get_vector_at::<String>(1)?;
        let to = input.get_vector_at::<String>(2)?;
        let mut output = output.cast::<f64>()?;
        output.set_size(input.row_count()?)?;
        for row in 0..input.row_count()? {
            let result = match (values.get(row)?, from.get(row)?, to.get(row)?) {
                (Some(value), Some(from), Some(to)) => Some(
                    units::convert(*value, from, to)
                        .map_err(|e| duckdb_neo::Error::invalid_input(e.to_string()))?,
                ),
                _ => None,
            };
            output.write(row, result)?;
        }
        Ok(())
    }
}
impl ScalarCallbacks for ConvertColumnScalar {
    type BindData = ();
    type InitData = ();
    fn exec(
        &self,
        _bind: Option<&()>,
        _init: Option<&mut ()>,
        _context: &Context,
        input: &mut VectorCollection,
        output: Vector<'_, Unknown>,
    ) -> duckdb_neo::Result<()> {
        let values = input.get_vector_at::<f64>(0)?;
        let from = tagged_unit(values.logical_type())?.ok_or_else(|| duckdb_neo::Error::invalid_input(
            "telemetry_convert_column requires a unit-tagged column from read_telemetry, read_cosworth, or read_motec; for a scalar or expression use telemetry_convert(value, from_unit, to_unit)"))?;
        let to = input.get_vector_at::<String>(1)?;
        let mut output = output.cast::<f64>()?;
        output.set_size(input.row_count()?)?;
        for row in 0..input.row_count()? {
            let result = match (values.get(row)?, to.get(row)?) {
                (Some(value), Some(to)) => Some(
                    units::convert(*value, &from, to)
                        .map_err(|e| duckdb_neo::Error::invalid_input(e.to_string()))?,
                ),
                _ => None,
            };
            output.write(row, result)?;
        }
        Ok(())
    }
}
impl ScalarCallbacks for CanConvertScalar {
    type BindData = ();
    type InitData = ();
    fn exec(
        &self,
        _bind: Option<&()>,
        _init: Option<&mut ()>,
        _context: &Context,
        input: &mut VectorCollection,
        output: Vector<'_, Unknown>,
    ) -> duckdb_neo::Result<()> {
        let from = input.get_vector_at::<String>(0)?;
        let to = input.get_vector_at::<String>(1)?;
        let mut output = output.cast::<bool>()?;
        output.set_size(input.row_count()?)?;
        for row in 0..input.row_count()? {
            let result = match (from.get(row)?, to.get(row)?) {
                (Some(from), Some(to)) => Some(units::can_convert(from, to)),
                _ => None,
            };
            output.write(row, result)?;
        }
        Ok(())
    }
}

pub fn register(extension: &Extension) -> duckdb_neo::Result<()> {
    let docs = |description: &str, example: &str| {
        FunctionDocumentationBuilder::new()
            .description(description)
            .add_category("telemetry")
            .add_example("", example)
    };
    ScalarFunctionBuilder::new(
        "telemetry_convert",
        SignatureBuilder::new()
            .param(FunctionParameter::new::<f64>("value"))
            .param(FunctionParameter::new::<String>("from_unit"))
            .param(FunctionParameter::new::<String>("to_unit"))
            .return_type(f64::logical_type(extension)?)
            .documentation(docs(
                "Convert compatible telemetry units; incompatible dimensions raise an error.",
                "telemetry_convert(10.0, 'm/s', 'km/h')",
            )),
        ConvertScalar,
    )
    .register(extension)?;
    ScalarFunctionBuilder::new(
        "telemetry_convert_column",
        SignatureBuilder::new()
            .param(FunctionParameter::new::<Any>("column"))
            .param(FunctionParameter::new::<String>("to_unit"))
            .return_type(f64::logical_type(extension)?)
            .documentation(docs(
                "Convert a numeric column carrying a telemetry unit alias.",
                "telemetry_convert_column(speed, 'km/h')",
            )),
        ConvertColumnScalar,
    )
    .register(extension)?;
    ScalarFunctionBuilder::new(
        "telemetry_can_convert",
        SignatureBuilder::new()
            .param(FunctionParameter::new::<String>("from_unit"))
            .param(FunctionParameter::new::<String>("to_unit"))
            .return_type(bool::logical_type(extension)?)
            .documentation(docs(
                "Whether two telemetry units have compatible dimensions.",
                "telemetry_can_convert('m/s', 'km/h')",
            )),
        CanConvertScalar,
    )
    .register(extension)?;
    Ok(())
}
