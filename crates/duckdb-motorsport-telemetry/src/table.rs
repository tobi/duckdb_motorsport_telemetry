//! Shared telemetry scan lifecycle on DuckDB's v2 C API.
//!
//! Schemas are declared independently of a connection and resolved with the
//! callback's factory. DuckDB owns all callback state and vector storage.
use super::{ReaderConfig, VECTOR_SIZE};
use duckdb_neo::{
    bind_arguments::BindArgument,
    connection::{Context, FFILink},
    data_chunk::DataChunkRef,
    extension::Extension,
    logical_type::{LogicalType, LogicalTypeID},
    signature::{
        FunctionDocumentationBuilder, FunctionParameter, FunctionParameterKind, SignatureBuilder,
    },
    table_function::{
        BindFunctionHandle, ExecColumnInfo, InitColumnData, OrderPreservation,
        TableFunctionBuilder, TableFunctionCallbacks,
    },
    value::Value,
    vector::{StorageKind, Unknown, Vector},
    Parameters, ToValue,
};
use std::{any::Any, cell::Cell, error::Error, marker::PhantomData};

type ScanResult<T> = Result<T, Box<dyn Error>>;

#[derive(Clone)]
pub enum SqlType {
    Varchar,
    Double,
    Boolean,
    UInteger,
    UBigint,
    Bigint,
    Timestamp,
    List(Box<Self>),
    Alias(Box<Self>, String),
}
impl SqlType {
    pub fn list(child: &Self) -> Self {
        Self::List(Box::new(child.clone()))
    }
    pub fn resolve(&self, link: &(impl FFILink + ?Sized)) -> duckdb_neo::Result<LogicalType> {
        use duckdb_neo::ffi::DUCKDB_V2_LOGICAL_TYPE_ID::*;
        let id = match self {
            Self::Varchar => DUCKDB_V2_LOGICAL_TYPE_ID_VARCHAR,
            Self::Double => DUCKDB_V2_LOGICAL_TYPE_ID_DOUBLE,
            Self::Boolean => DUCKDB_V2_LOGICAL_TYPE_ID_BOOLEAN,
            Self::UInteger => DUCKDB_V2_LOGICAL_TYPE_ID_UINTEGER,
            Self::UBigint => DUCKDB_V2_LOGICAL_TYPE_ID_UBIGINT,
            Self::Bigint => DUCKDB_V2_LOGICAL_TYPE_ID_BIGINT,
            Self::Timestamp => DUCKDB_V2_LOGICAL_TYPE_ID_TIMESTAMP,
            Self::List(child) => {
                let child = Value::from_logical_type(link, &child.resolve(link)?)?;
                return LogicalType::create_from_id(
                    link,
                    DUCKDB_V2_LOGICAL_TYPE_ID_LIST,
                    Parameters::positional(&[&child]),
                );
            }
            Self::Alias(inner, name) => return inner.resolve(link)?.with_alias(link, name),
        };
        LogicalType::create_from_id(link, id, Parameters::None)
    }
}

// Defaults must carry the declared type: Rust's `()` is an empty SQL tuple,
// not NULL, and an arbitrary typed NULL is not valid for every nested type.
struct NullDefault(SqlType);
impl ToValue for NullDefault {
    fn value<C: FFILink + ?Sized>(&self, link: &C) -> duckdb_neo::Result<Value> {
        Value::null(link, &self.0.resolve(link)?)
    }
}

pub struct BindInfo<'a> {
    pub context: &'a Context,
    handle: BindFunctionHandle<'a>,
    arguments: Vec<Option<Value>>,
    positional: usize,
    named: Vec<String>,
    config: Option<&'a ReaderConfig>,
}
impl BindInfo<'_> {
    pub fn get_parameter(&self, index: usize) -> ScanResult<&Value> {
        self.arguments
            .get(index)
            .and_then(Option::as_ref)
            .ok_or_else(|| "path must not be NULL".into())
    }
    pub fn get_named_parameter(&self, name: &str) -> Option<&Value> {
        self.named
            .iter()
            .position(|n| n == name)
            .and_then(|index| self.arguments[self.positional + index].as_ref())
    }
    pub fn add_result_column(&self, name: &str, ty: SqlType) -> duckdb_neo::Result<()> {
        self.handle
            .add_result_column(name, &ty.resolve(self.context)?)
    }
    pub fn set_cardinality(&self, cardinality: u64, exact: bool) -> duckdb_neo::Result<()> {
        self.handle.set_cardinality(cardinality as usize, exact)
    }
    pub fn reader_config(&self) -> ScanResult<&ReaderConfig> {
        self.config
            .ok_or_else(|| "reader configuration missing".into())
    }
}

pub struct InitInfo<'a> {
    bind: &'a (dyn Any + Send + Sync),
    columns: Vec<u64>,
    threads: Cell<usize>,
}
impl InitInfo<'_> {
    pub fn get_bind_data<T: Any>(&self) -> &T {
        self.bind.downcast_ref().expect("scan bind type")
    }
    pub fn get_column_indices(&self) -> Vec<u64> {
        self.columns.clone()
    }
    pub fn set_max_threads(&self, threads: u64) {
        self.threads.set(threads as usize);
    }
}

pub struct TableFunctionInfo<'a, T: TelemetryTable> {
    bind: &'a T::BindData,
    state: &'a T::InitData,
}
impl<T: TelemetryTable> TableFunctionInfo<'_, T> {
    pub fn get_bind_data(&self) -> &T::BindData {
        self.bind
    }
    pub fn get_init_data(&self) -> &T::InitData {
        self.state
    }
}

/// Telemetry-specific operations shared by all readers; registration and
/// ownership are supplied by `TableFunctionCallbacks` below.
pub trait TelemetryTable: Sized + Send + Sync + 'static {
    type BindData: Any + Send + Sync;
    type InitData: Any + Send + Sync;
    fn bind(bind: &BindInfo) -> ScanResult<Self::BindData>;
    fn init(init: &InitInfo) -> ScanResult<Self::InitData>;
    fn func(func: &TableFunctionInfo<Self>, output: &mut ChunkWriter) -> ScanResult<()>;
    fn parameters() -> Option<Vec<SqlType>>;
    fn named_parameters() -> Option<Vec<(String, SqlType)>> {
        None
    }
    fn supports_pushdown() -> bool {
        false
    }
}
struct Scan<T> {
    config: Option<ReaderConfig>,
    _scan: PhantomData<T>,
}
fn scan_error(error: Box<dyn Error>) -> duckdb_neo::Error {
    match error.downcast::<duckdb_neo::Error>() {
        Ok(error) => *error,
        Err(error) => duckdb_neo::Error::invalid_input(error.to_string()),
    }
}
impl<T: TelemetryTable> TableFunctionCallbacks for Scan<T> {
    type BindData = T::BindData;
    type GlobalState = T::InitData;
    type LocalState = ();
    fn bind(
        &self,
        context: &Context,
        arguments: Vec<BindArgument>,
        handle: BindFunctionHandle<'_>,
    ) -> duckdb_neo::Result<Self::BindData> {
        let arguments = arguments
            .into_iter()
            .map(|arg| {
                let value = arg.value.ok_or_else(|| {
                    duckdb_neo::Error::invalid_input("telemetry arguments must be constant")
                })?;
                Ok(if value.is_null()? { None } else { Some(value) })
            })
            .collect::<duckdb_neo::Result<Vec<_>>>()?;
        handle.set_order(OrderPreservation::None)?;
        let bind = BindInfo {
            context,
            handle,
            arguments,
            positional: T::parameters().unwrap_or_default().len(),
            named: T::named_parameters()
                .unwrap_or_default()
                .into_iter()
                .map(|(name, _)| name)
                .collect(),
            config: self.config.as_ref(),
        };
        T::bind(&bind).map_err(scan_error)
    }
    fn init_global_state(
        &self,
        bind_data: &Self::BindData,
        _context: &Context,
        columns: InitColumnData<'_>,
    ) -> duckdb_neo::Result<(Option<Self::GlobalState>, Option<usize>)> {
        let columns = (0..columns.count()?)
            .map(|i| columns.get_column_index(i).map(|c| c as u64))
            .collect::<duckdb_neo::Result<Vec<_>>>()?;
        let init = InitInfo {
            bind: bind_data,
            columns,
            threads: Cell::new(1),
        };
        let state = T::init(&init).map_err(scan_error)?;
        Ok((Some(state), Some(init.threads.get())))
    }
    fn exec(
        &self,
        bind: &Self::BindData,
        state: Option<&Self::GlobalState>,
        _local: Option<&mut ()>,
        _context: &Context,
        output: DataChunkRef<'_>,
        _columns: ExecColumnInfo<'_>,
    ) -> duckdb_neo::Result<()> {
        let state =
            state.ok_or_else(|| duckdb_neo::Error::api_error("telemetry scan state missing"))?;
        T::func(
            &TableFunctionInfo::<T> { bind, state },
            &mut ChunkWriter(output),
        )
        .map_err(scan_error)
    }
}

pub fn register<T: TelemetryTable>(
    extension: &Extension,
    name: &str,
    config: Option<ReaderConfig>,
) -> duckdb_neo::Result<()> {
    let mut signature = SignatureBuilder::new();
    for (index, ty) in T::parameters().unwrap_or_default().into_iter().enumerate() {
        let parameter = match (name, index) {
            (_, 0) => "path",
            ("telemetry_column_comments", _) => "table_name",
            _ => "output_path",
        };
        signature = signature.param(FunctionParameter::with_type(
            parameter,
            ty.resolve(extension)?,
        ));
    }
    for (name, ty) in T::named_parameters().unwrap_or_default() {
        let logical = ty.resolve(extension)?;
        signature = signature.param(
            FunctionParameter::with_type(name, logical)
                .kind(FunctionParameterKind::Named)
                .default_value(NullDefault(ty)),
        );
    }
    let description = match name {
        "telemetry_metadata" => {
            "Channel metadata, units and native sample rates from telemetry files."
        }
        "telemetry_samples" => {
            "Exact native-rate telemetry samples, with channel and time pruning."
        }
        "telemetry_laps" => "Session-monotonic laps, stints and lap kinds from telemetry files.",
        "telemetry_file_metadata" => "File identity, track, session and lap summaries.",
        "telemetry_session_metadata" => "Groups related telemetry files into sessions.",
        "read_telemetry_normalized" => "Normalized telemetry channels at each native instant.",
        "write_telemetry" => {
            "Exports telemetry to MoTeC LD with optional channel and unit mappings."
        }
        "telemetry_column_comments" => {
            "SQL column comments preserving telemetry units and provenance."
        }
        "telemetry_units" => {
            "Discover canonical telemetry units, aliases and compatible dimensions."
        }
        "read_telemetry_session" => {
            "Synchronized wide telemetry across related files in a session."
        }
        _ => "Projected wide telemetry with type-aware interpolation and optional unit tags.",
    };
    let example = match name {
        "telemetry_units" => "SELECT * FROM telemetry_units();".to_owned(),
        "write_telemetry" => "SELECT * FROM write_telemetry('run.pds', 'run.ld');".to_owned(),
        "telemetry_column_comments" => {
            "SELECT * FROM telemetry_column_comments('run.pds', 'run');".to_owned()
        }
        "read_aim" | "read_aimd" => format!("SELECT * FROM {name}('run.mp4');"),
        "read_motec" => "SELECT * FROM read_motec('run.ld');".to_owned(),
        "read_vbo" => "SELECT * FROM read_vbo('run.vbo');".to_owned(),
        _ => format!("SELECT * FROM {name}('run.pds');"),
    };
    signature = signature.documentation(
        FunctionDocumentationBuilder::new()
            .description(description)
            .add_category("telemetry")
            .add_example("", example),
    );
    TableFunctionBuilder::new(
        name,
        signature,
        Scan::<T> {
            config,
            _scan: PhantomData,
        },
    )
    .with_projection_pushdown(T::supports_pushdown())
    .register(extension)
}

pub struct ChunkWriter<'a>(DataChunkRef<'a>);
impl ChunkWriter<'_> {
    pub fn num_columns(&self) -> usize {
        self.0.col_count()
    }
    pub fn flat_vector(&mut self, index: usize) -> duckdb_neo::Result<VectorWriter<'_>> {
        let mut vector = self.0.get_vector_at_mut::<Unknown>(index)?;
        vector.set_size(VECTOR_SIZE as usize)?;
        Ok(
            if vector.logical_type().id()? == LogicalTypeID::DUCKDB_V2_LOGICAL_TYPE_ID_VARCHAR {
                VectorWriter::Text(vector.cast::<String>()?)
            } else {
                VectorWriter::Numeric(vector)
            },
        )
    }
    pub fn set_len(&mut self, len: usize) -> duckdb_neo::Result<()> {
        for i in 0..self.0.col_count() {
            self.0.get_vector_at_mut::<Unknown>(i)?.set_size(len)?;
        }
        Ok(())
    }
}
pub enum VectorWriter<'a> {
    Text(Vector<'a, String>),
    Numeric(Vector<'a, Unknown>),
}
impl VectorWriter<'_> {
    pub fn insert(&mut self, row: usize, value: &str) -> duckdb_neo::Result<()> {
        match self {
            Self::Text(vector) => vector.write(row, Some(value)),
            Self::Numeric(_) => Err(duckdb_neo::Error::api_error(
                "string written to numeric output",
            )),
        }
    }
    pub fn set_null(&mut self, row: usize) -> duckdb_neo::Result<()> {
        match self {
            Self::Text(vector) => vector.write(row, None),
            Self::Numeric(vector) => vector.set_null_slow(row),
        }
    }
    pub fn typed_slice<T: NumericStorage>(&mut self) -> duckdb_neo::Result<&mut [T]> {
        let Self::Numeric(vector) = self else {
            return Err(duckdb_neo::Error::api_error("numeric write to text output"));
        };
        if !T::accepts(vector.logical_type().id()?)
            || !vector.is_writable()
            || vector.storage_kind() != StorageKind::Flat
        {
            return Err(duckdb_neo::Error::api_error(
                "invalid numeric output vector",
            ));
        }
        let view = vector
            .get_view()
            .ok_or_else(|| duckdb_neo::Error::api_error("missing numeric output view"))?;
        let ptr = view.as_ptr() as *mut T;
        if ptr.is_null() {
            return Err(duckdb_neo::Error::api_error("null numeric output buffer"));
        }
        // SAFETY: the sealed storage implementations below match the checked
        // logical type, and a writable flat vector owns `len` physical slots.
        // This mutable borrow prevents other access to this vector view.
        Ok(unsafe { std::slice::from_raw_parts_mut(ptr, vector.len()) })
    }
}

// Only fixed-width primitives with matching DuckDB storage may enter this API.
pub trait NumericStorage: private::Sealed {
    fn accepts(id: LogicalTypeID) -> bool;
}
mod private {
    pub trait Sealed {}
    impl Sealed for bool {}
    impl Sealed for u32 {}
    impl Sealed for u64 {}
    impl Sealed for i64 {}
    impl Sealed for f64 {}
}
macro_rules! numeric_storage { ($ty:ty, $($id:ident)|+) => {
    impl NumericStorage for $ty { fn accepts(id: LogicalTypeID) -> bool { matches!(id, $(LogicalTypeID::$id)|+) } }
}; }
numeric_storage!(bool, DUCKDB_V2_LOGICAL_TYPE_ID_BOOLEAN);
numeric_storage!(u32, DUCKDB_V2_LOGICAL_TYPE_ID_UINTEGER);
numeric_storage!(u64, DUCKDB_V2_LOGICAL_TYPE_ID_UBIGINT);
numeric_storage!(
    i64,
    DUCKDB_V2_LOGICAL_TYPE_ID_BIGINT | DUCKDB_V2_LOGICAL_TYPE_ID_TIMESTAMP
);
numeric_storage!(f64, DUCKDB_V2_LOGICAL_TYPE_ID_DOUBLE);
