pub mod channel_map;
mod unit_sql;

#[cfg(not(target_os = "emscripten"))]
use aim_telemetry::AimFile;
use channel_map::ChannelMap;
use chrono::{DateTime, NaiveDate, NaiveDateTime};
use cosworth_telemetry::CosworthFile;
use duckdb::{
    core::{DataChunkHandle, FlatVector, Inserter, LogicalTypeHandle, LogicalTypeId},
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use duckdb_loadable_macros::duckdb_entrypoint_c_api;
use glob::glob;
#[cfg(target_os = "emscripten")]
use libduckdb_sys as ffi;
use motec_telemetry::{
    motec_sidecar_path, write_motec_bytes, write_motec_ldx_bytes, MotecFile, MotecMetadata,
};
use motorsport_telemetry_core::{
    group_sessions, read_source_metadata, units, Channel, FileMetadata, SampleType, SourceRef,
    TelemetrySource, UnitSource,
};
use std::collections::{HashMap, HashSet};
use std::error::Error;
#[cfg(target_os = "emscripten")]
use std::ffi::{c_void, CString};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use vbo_telemetry::VboFile;

pub(crate) const VECTOR_SIZE: u64 = 2048;
const SAMPLES_COLUMN_COUNT: u64 = 10;
const CHANNELS_COLUMN_COUNT: u64 = 15;
const SESSION_FIXED_COLUMNS: u64 = 9;

pub(crate) trait FlatVectorExt {
    fn typed_slice<T>(&mut self) -> &mut [T];
}
impl FlatVectorExt for FlatVector<'_> {
    fn typed_slice<T>(&mut self) -> &mut [T] {
        unsafe { self.as_mut_slice::<T>() }
    }
}

pub(crate) fn ty(id: LogicalTypeId) -> LogicalTypeHandle {
    LogicalTypeHandle::from(id)
}
fn named_string(bind: &BindInfo, name: &str) -> Option<String> {
    bind.get_named_parameter(name).map(|v| v.to_string())
}
fn named_i64(bind: &BindInfo, name: &str) -> Option<i64> {
    bind.get_named_parameter(name).map(|v| v.to_int64())
}
fn named_bool(bind: &BindInfo, name: &str) -> Option<bool> {
    bind.get_named_parameter(name).map(|v| v.to_int64() != 0)
}
fn named_string_list(bind: &BindInfo, name: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let Some(value) = bind.get_named_parameter(name) else {
        return Ok(Vec::new());
    };
    let values = value
        .to_list()
        .ok_or_else(|| format!("{name} must be a list of strings"))?;
    Ok(values.into_iter().map(|value| value.to_string()).collect())
}

/// Read a SQL `VARCHAR[][]` named argument without introducing another mapping
/// language. Each inner list is one declarative row in the SQL file.
fn named_string_rows(
    bind: &BindInfo,
    name: &str,
    width: usize,
) -> Result<Vec<Vec<String>>, Box<dyn Error>> {
    let Some(value) = bind.get_named_parameter(name) else {
        return Ok(Vec::new());
    };
    let rows = value
        .to_list()
        .ok_or_else(|| format!("{name} must be a list of lists"))?;
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            let cells = row
                .to_list()
                .ok_or_else(|| format!("{name} row {} must be a list", index + 1))?;
            if cells.len() != width {
                return Err(format!(
                    "{name} row {} must contain {width} strings, got {}",
                    index + 1,
                    cells.len()
                )
                .into());
            }
            Ok(cells.into_iter().map(|cell| cell.to_string()).collect())
        })
        .collect()
}

/// Read the `channel_map` named argument.
///
/// Accepts inline rules or a path to a rules file, so a team's mapping can
/// live in version control instead of being pasted into every query. A value
/// containing no `->` and naming an existing file is treated as a path.
fn named_channel_map(bind: &BindInfo) -> Result<ChannelMap, Box<dyn Error>> {
    let Some(value) = named_string(bind, "channel_map") else {
        return Ok(ChannelMap::default());
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(ChannelMap::default());
    }
    if !trimmed.contains("->") && Path::new(trimmed).is_file() {
        let text = std::fs::read_to_string(trimmed)
            .map_err(|error| format!("cannot read channel_map file '{trimmed}': {error}"))?;
        return ChannelMap::parse(&text);
    }
    ChannelMap::parse(trimmed)
}
fn named_timestamp(bind: &BindInfo, name: &str) -> Result<Option<i64>, Box<dyn Error>> {
    let Some(value) = bind.get_named_parameter(name) else {
        return Ok(None);
    };
    let text = value.to_string();
    let micros = DateTime::parse_from_rfc3339(&text)
        .map(|value| value.timestamp_micros())
        .or_else(|_| {
            NaiveDateTime::parse_from_str(&text, "%Y-%m-%d %H:%M:%S%.f")
                .map(|value| value.and_utc().timestamp_micros())
        })
        .or_else(|_| {
            NaiveDate::parse_from_str(&text, "%Y-%m-%d").map(|value| {
                value
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp_micros()
            })
        })
        .map_err(|_| format!("invalid {name} timestamp: {text}"))?;
    Ok(Some(micros))
}
fn projected(init: &InitInfo, total: u64) -> Vec<u64> {
    let cols = init.get_column_indices();
    if cols.is_empty() {
        (0..total).collect()
    } else {
        cols
    }
}
fn ceil_div(value: u64, divisor: u64) -> u64 {
    value.div_ceil(divisor)
}
fn worker_count(tasks: usize) -> u64 {
    let cpus = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    tasks.min(cpus).max(1) as u64
}

fn normalized_channel_name(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn matching_channel(source: &dyn TelemetrySource, names: &[&str]) -> Option<usize> {
    source.channels().iter().position(|channel| {
        channel.sample_count > 0 && names.contains(&normalized_channel_name(&channel.name).as_str())
    })
}

fn parse_channel_filter(value: Option<&str>) -> HashSet<String> {
    value
        .unwrap_or("")
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn expand_paths(pattern: &str) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let expansion = if let Some(start) = pattern.find("{pds,ld,vbo,mp4}") {
        Some((
            start,
            "{pds,ld,vbo,mp4}".len(),
            vec!["pds", "ld", "vbo", "mp4"],
        ))
    } else if let Some(start) = pattern.find("{pds,ld,vbo}") {
        Some((start, "{pds,ld,vbo}".len(), vec!["pds", "ld", "vbo"]))
    } else {
        None
    };
    let patterns = if let Some((start, width, extensions)) = expansion {
        extensions
            .into_iter()
            .map(|extension| {
                format!(
                    "{}{}{}",
                    &pattern[..start],
                    extension,
                    &pattern[start + width..]
                )
            })
            .collect()
    } else {
        vec![pattern.to_owned()]
    };
    let has_magic = pattern
        .bytes()
        .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b'{'));
    let mut paths = Vec::new();
    for candidate in patterns {
        if candidate
            .bytes()
            .any(|byte| matches!(byte, b'*' | b'?' | b'['))
        {
            paths.extend(
                glob(&candidate)?
                    .filter_map(Result::ok)
                    .filter(|path| path.is_file()),
            );
        } else {
            paths.push(PathBuf::from(candidate));
        }
    }
    if has_magic {
        paths.retain(|path| {
            matches!(
                path.extension()
                    .and_then(|value| value.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("pds" | "ld" | "vbo" | "mp4")
            )
        });
    }
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Err(format!("no telemetry files matched {pattern}").into());
    }
    Ok(paths)
}

#[derive(Clone)]
struct InputFile {
    source: SourceRef,
    create_date_micros: i64,
    modified_at_micros: i64,
}

impl Deref for InputFile {
    type Target = dyn motorsport_telemetry_core::TelemetrySource;
    fn deref(&self) -> &Self::Target {
        self.source.as_ref()
    }
}

#[derive(Clone, Copy)]
struct ReaderConfig {
    format: Option<&'static str>,
    session: bool,
}

fn system_time_micros(timestamp: Option<std::time::SystemTime>) -> i64 {
    timestamp
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_micros().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn file_timestamps(path: &Path) -> (i64, i64) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return (0, 0);
    };
    let modified = metadata.modified().ok();
    let created = metadata.created().ok().or(modified);
    (system_time_micros(created), system_time_micros(modified))
}

#[cfg(target_os = "emscripten")]
fn read_vfs(bind: &BindInfo, path: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    // BindInfo is a one-pointer wrapper in duckdb-rs. The public wrapper does not
    // expose that pointer, while DuckDB's VFS API requires the raw bind info.
    debug_assert_eq!(
        std::mem::size_of::<BindInfo>(),
        std::mem::size_of::<ffi::duckdb_bind_info>()
    );
    let info = unsafe { *(bind as *const BindInfo as *const ffi::duckdb_bind_info) };
    let mut context: ffi::duckdb_client_context = std::ptr::null_mut();
    unsafe { ffi::duckdb_table_function_get_client_context(info, &mut context) };
    if context.is_null() {
        return Err("DuckDB did not expose a client context".into());
    }
    let mut filesystem = unsafe { ffi::duckdb_client_context_get_file_system(context) };
    if filesystem.is_null() {
        unsafe { ffi::duckdb_destroy_client_context(&mut context) };
        return Err("DuckDB did not expose its virtual filesystem".into());
    }
    let c_path = CString::new(path)?;
    let mut options = unsafe { ffi::duckdb_create_file_open_options() };
    unsafe {
        ffi::duckdb_file_open_options_set_flag(
            options,
            ffi::duckdb_file_flag_DUCKDB_FILE_FLAG_READ,
            true,
        )
    };
    let mut handle: ffi::duckdb_file_handle = std::ptr::null_mut();
    let state =
        unsafe { ffi::duckdb_file_system_open(filesystem, c_path.as_ptr(), options, &mut handle) };
    unsafe { ffi::duckdb_destroy_file_open_options(&mut options) };
    if state != ffi::DuckDBSuccess || handle.is_null() {
        unsafe {
            ffi::duckdb_destroy_file_system(&mut filesystem);
            ffi::duckdb_destroy_client_context(&mut context);
        }
        return Err(format!("cannot open telemetry file {path} through DuckDB's VFS").into());
    }
    let size = unsafe { ffi::duckdb_file_handle_size(handle) };
    if size < 0 || size > 4 * 1024 * 1024 * 1024_i64 {
        unsafe {
            ffi::duckdb_destroy_file_handle(&mut handle);
            ffi::duckdb_destroy_file_system(&mut filesystem);
            ffi::duckdb_destroy_client_context(&mut context);
        }
        return Err(format!("invalid or excessive telemetry file size for {path}: {size}").into());
    }
    let mut data = vec![0u8; size as usize];
    let mut read = 0usize;
    while read < data.len() {
        let count = unsafe {
            ffi::duckdb_file_handle_read(
                handle,
                data[read..].as_mut_ptr().cast::<c_void>(),
                (data.len() - read) as i64,
            )
        };
        if count <= 0 {
            break;
        }
        read += count as usize;
    }
    unsafe {
        ffi::duckdb_destroy_file_handle(&mut handle);
        ffi::duckdb_destroy_file_system(&mut filesystem);
        ffi::duckdb_destroy_client_context(&mut context);
    }
    if read != data.len() {
        return Err(format!("short read for {path}: {read} of {} bytes", data.len()).into());
    }
    Ok(data)
}

fn open_paths(
    _bind: &BindInfo,
    pattern: &str,
    required_format: Option<&str>,
    create_date_from: Option<i64>,
    create_date_to: Option<i64>,
) -> Result<Vec<InputFile>, Box<dyn Error>> {
    let mut result = Vec::new();
    for path in expand_paths(pattern)? {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let format = match extension.as_str() {
            "pds" => "pds",
            "ld" => "motec",
            "vbo" => "vbo",
            "mp4" => "aimd",
            _ => continue,
        };
        if required_format.is_some_and(|required| required != format) {
            continue;
        }
        let (created, modified) = file_timestamps(&path);
        if create_date_from.is_some_and(|from| created < from)
            || create_date_to.is_some_and(|to| created >= to)
        {
            continue;
        }
        #[cfg(not(target_os = "emscripten"))]
        let source: SourceRef = match format {
            "pds" => Arc::new(CosworthFile::open(&path)?),
            "motec" => Arc::new(MotecFile::open(&path)?),
            "vbo" => Arc::new(VboFile::open(&path)?),
            "aimd" => Arc::new(AimFile::open(&path)?),
            _ => unreachable!(),
        };
        #[cfg(target_os = "emscripten")]
        let source: SourceRef = {
            let display = path.to_string_lossy().into_owned();
            let bytes = read_vfs(_bind, &display)?;
            match format {
                "pds" => Arc::new(CosworthFile::from_bytes(display, bytes)?) as SourceRef,
                "motec" => Arc::new(MotecFile::from_bytes(display, bytes)?) as SourceRef,
                "vbo" => Arc::new(VboFile::from_bytes(display, bytes)?) as SourceRef,
                "aimd" => return Err("AiM MP4 is not supported in the WebAssembly build".into()),
                _ => unreachable!(),
            }
        };
        result.push(InputFile {
            source,
            create_date_micros: created,
            modified_at_micros: modified,
        });
    }
    if result.is_empty() {
        return Err(
            format!("no telemetry files remained after format/date pruning for {pattern}").into(),
        );
    }
    Ok(result)
}

// ── telemetry_samples ───────────────────────────────────────────────

struct SamplesBind {
    files: Vec<InputFile>,
    channel_filter: HashSet<String>,
    start_ns: u64,
    end_ns: u64,
    /// Opt-in renaming / unit conversion. Empty means pass through untouched.
    map: ChannelMap,
}

#[derive(Clone, Copy)]
struct SampleSegment {
    file: usize,
    channel: usize,
    chunk: usize,
    local_start: u64,
    len: u64,
}

struct SamplesInit {
    next: AtomicUsize,
    segments: Vec<SampleSegment>,
    projected: Vec<u64>,
}

struct SamplesVTab;

impl VTab for SamplesVTab {
    type BindData = SamplesBind;
    type InitData = SamplesInit;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        let pattern = bind.get_parameter(0).to_string();
        let files = open_paths(bind, &pattern, None, None, None)?;
        let filter_value = named_string(bind, "channel");
        let channel_filter = parse_channel_filter(filter_value.as_deref());
        if !channel_filter.is_empty() {
            let found = files
                .iter()
                .flat_map(|file| file.channels())
                .map(|channel| channel.name.to_ascii_lowercase())
                .collect::<HashSet<_>>();
            let missing = channel_filter
                .difference(&found)
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(
                    format!("telemetry channel(s) not found: {}", missing.join(", ")).into(),
                );
            }
        }
        let start_ns = named_i64(bind, "start_ns").unwrap_or(0).max(0) as u64;
        let end_ns = named_i64(bind, "end_ns").unwrap_or(i64::MAX).max(0) as u64;
        if end_ns < start_ns {
            return Err("end_ns must be greater than or equal to start_ns".into());
        }

        let map = named_channel_map(bind)?;
        if !map.is_empty() {
            let available: Vec<String> = files
                .iter()
                .flat_map(|file| file.channels())
                .map(|channel| channel.name.clone())
                .collect();
            map.validate(&available)?;
        }

        bind.add_result_column("file", ty(LogicalTypeId::Varchar));
        bind.add_result_column("format", ty(LogicalTypeId::Varchar));
        bind.add_result_column("channel_id", ty(LogicalTypeId::UInteger));
        bind.add_result_column("channel", ty(LogicalTypeId::Varchar));
        bind.add_result_column("unit", ty(LogicalTypeId::Varchar));
        bind.add_result_column("unit_source", ty(LogicalTypeId::Varchar));
        bind.add_result_column("frequency_hz", ty(LogicalTypeId::Double));
        bind.add_result_column("sample_index", ty(LogicalTypeId::UBigint));
        bind.add_result_column("time_ns", ty(LogicalTypeId::Bigint));
        bind.add_result_column("value", ty(LogicalTypeId::Double));

        let cardinality = files
            .iter()
            .flat_map(|file| file.channels())
            .filter(|c| {
                channel_filter.is_empty() || channel_filter.contains(&c.name.to_ascii_lowercase())
            })
            .map(|c| c.sample_count)
            .sum();
        bind.set_cardinality(cardinality, start_ns == 0 && end_ns == i64::MAX as u64);
        Ok(SamplesBind {
            files,
            channel_filter,
            start_ns,
            end_ns,
            map,
        })
    }

    fn init(init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        let bind = unsafe { &*init.get_bind_data::<SamplesBind>() };
        let mut segments = Vec::new();
        for (file_idx, file) in bind.files.iter().enumerate() {
            for (channel_idx, channel) in file.channels().iter().enumerate() {
                if !bind.channel_filter.is_empty()
                    && !bind
                        .channel_filter
                        .contains(&channel.name.to_ascii_lowercase())
                {
                    continue;
                }
                for (chunk_idx, chunk) in channel.chunks.iter().enumerate() {
                    let period_ns = chunk.sample_period_ns;
                    let first = if bind.start_ns <= chunk.time_base_ns {
                        0
                    } else {
                        ceil_div(bind.start_ns - chunk.time_base_ns, period_ns)
                    }
                    .min(chunk.sample_count);
                    let past_end = if bind.end_ns <= chunk.time_base_ns {
                        0
                    } else {
                        ceil_div(bind.end_ns - chunk.time_base_ns, period_ns)
                    }
                    .min(chunk.sample_count);
                    let mut at = first;
                    while at < past_end {
                        let len = (past_end - at).min(VECTOR_SIZE);
                        segments.push(SampleSegment {
                            file: file_idx,
                            channel: channel_idx,
                            chunk: chunk_idx,
                            local_start: at,
                            len,
                        });
                        at += len;
                    }
                }
            }
        }
        init.set_max_threads(worker_count(segments.len()));
        Ok(SamplesInit {
            next: AtomicUsize::new(0),
            segments,
            projected: projected(init, SAMPLES_COLUMN_COUNT),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let state = func.get_init_data();
        let index = state.next.fetch_add(1, Ordering::Relaxed);
        let Some(segment) = state.segments.get(index) else {
            output.set_len(0);
            return Ok(());
        };
        let bind = func.get_bind_data();
        let file = &bind.files[segment.file];
        let channel = &file.channels()[segment.channel];
        let chunk = &channel.chunks[segment.chunk];
        let n = segment.len as usize;

        for (out_col, original) in state.projected.iter().copied().enumerate() {
            if out_col >= output.num_columns() {
                break;
            }
            match original {
                0 => {
                    let v = output.flat_vector(out_col);
                    for row in 0..n {
                        v.insert(row, file.path());
                    }
                }
                1 => {
                    let vector = output.flat_vector(out_col);
                    for row in 0..n {
                        vector.insert(row, file.format());
                    }
                }
                2 => output.flat_vector(out_col).typed_slice::<u32>()[..n].fill(channel.id),
                3 => {
                    let vector = output.flat_vector(out_col);
                    let name = bind.map.name_for(&channel.name);
                    for row in 0..n {
                        vector.insert(row, name);
                    }
                }
                4 => {
                    let vector = output.flat_vector(out_col);
                    let (unit, _) =
                        bind.map
                            .unit_for(&channel.name, &channel.unit, channel.unit_source);
                    for row in 0..n {
                        vector.insert(row, unit.as_str());
                    }
                }
                5 => {
                    let vector = output.flat_vector(out_col);
                    let (_, source) =
                        bind.map
                            .unit_for(&channel.name, &channel.unit, channel.unit_source);
                    for row in 0..n {
                        vector.insert(row, source.name());
                    }
                }
                6 => output.flat_vector(out_col).typed_slice::<f64>()[..n]
                    .fill(1e9 / chunk.sample_period_ns as f64),
                7 => {
                    let mut vector = output.flat_vector(out_col);
                    let dst = &mut vector.typed_slice::<u64>()[..n];
                    for (row, value) in dst.iter_mut().enumerate() {
                        *value = chunk.sample_base + segment.local_start + row as u64;
                    }
                }
                8 => {
                    let mut vector = output.flat_vector(out_col);
                    let dst = &mut vector.typed_slice::<i64>()[..n];
                    for (row, value) in dst.iter_mut().enumerate() {
                        *value = file.sample_time_ns(
                            segment.channel,
                            segment.chunk,
                            segment.local_start + row as u64,
                        ) as i64;
                    }
                }
                9 => {
                    let mut vector = output.flat_vector(out_col);
                    let rule = bind.map.rule_for(&channel.name);
                    let dst = &mut vector.typed_slice::<f64>()[..n];
                    for (row, value) in dst.iter_mut().enumerate() {
                        let raw = file.decode(
                            segment.channel,
                            segment.chunk,
                            segment.local_start + row as u64,
                        );
                        // Only mapped channels are converted; everything else
                        // passes through byte-for-byte as decoded.
                        *value = match rule {
                            Some(rule) => rule.convert(raw),
                            None => raw,
                        };
                    }
                }
                _ => output.flat_vector(out_col).typed_slice::<i64>()[..n].fill(index as i64),
            }
        }
        output.set_len(n);
        Ok(())
    }

    fn supports_pushdown() -> bool {
        true
    }
    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![ty(LogicalTypeId::Varchar)])
    }
    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        Some(vec![
            ("channel".into(), ty(LogicalTypeId::Varchar)),
            ("start_ns".into(), ty(LogicalTypeId::Bigint)),
            ("end_ns".into(), ty(LogicalTypeId::Bigint)),
            // Inline rules or a path to a rules file.
            ("channel_map".into(), ty(LogicalTypeId::Varchar)),
        ])
    }
}

// ── telemetry_metadata ──────────────────────────────────────────────

struct ChannelsBind {
    files: Vec<InputFile>,
    map: ChannelMap,
    rows: Vec<(usize, usize)>,
}
struct ChannelsInit {
    next: AtomicUsize,
    projected: Vec<u64>,
}
struct ChannelsVTab;

impl VTab for ChannelsVTab {
    type BindData = ChannelsBind;
    type InitData = ChannelsInit;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        let files = open_paths(bind, &bind.get_parameter(0).to_string(), None, None, None)?;
        let map = named_channel_map(bind)?;
        if !map.is_empty() {
            let available: Vec<String> = files
                .iter()
                .flat_map(|file| file.channels())
                .map(|channel| channel.name.clone())
                .collect();
            map.validate(&available)?;
        }
        for (name, logical) in [
            ("file", LogicalTypeId::Varchar),
            ("format", LogicalTypeId::Varchar),
            ("channel_id", LogicalTypeId::UInteger),
            ("name", LogicalTypeId::Varchar),
            ("unit", LogicalTypeId::Varchar),
            // Provenance of `unit`: declared / spec_default / unknown. Exposed
            // so queries can tell a unit the file stated from one implied by
            // the format spec, and skip channels with no unit at all.
            ("unit_source", LogicalTypeId::Varchar),
            ("canonical_unit", LogicalTypeId::Varchar),
            ("dimension", LogicalTypeId::Varchar),
            ("type_code", LogicalTypeId::UInteger),
            ("data_type", LogicalTypeId::Varchar),
            ("frequency_hz", LogicalTypeId::Double),
            ("sample_period_ns", LogicalTypeId::UBigint),
            ("sample_count", LogicalTypeId::UBigint),
            ("chunk_count", LogicalTypeId::UBigint),
            ("duration_ns", LogicalTypeId::UBigint),
        ] {
            bind.add_result_column(name, ty(logical));
        }
        let rows = files
            .iter()
            .enumerate()
            .flat_map(|(fi, file)| (0..file.channels().len()).map(move |ci| (fi, ci)))
            .collect::<Vec<_>>();
        bind.set_cardinality(rows.len() as u64, true);
        Ok(ChannelsBind { files, map, rows })
    }

    fn init(init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        let bind = unsafe { &*init.get_bind_data::<ChannelsBind>() };
        init.set_max_threads(worker_count(bind.rows.len()));
        Ok(ChannelsInit {
            next: AtomicUsize::new(0),
            projected: projected(init, CHANNELS_COLUMN_COUNT),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let state = func.get_init_data();
        let start = state
            .next
            .fetch_add(VECTOR_SIZE as usize, Ordering::Relaxed);
        let bind = func.get_bind_data();
        if start >= bind.rows.len() {
            output.set_len(0);
            return Ok(());
        }
        let end = (start + VECTOR_SIZE as usize).min(bind.rows.len());
        let n = end - start;
        for (out_col, original) in state.projected.iter().copied().enumerate() {
            if out_col >= output.num_columns() {
                break;
            }
            let mut vector = output.flat_vector(out_col);
            for (row, &(fi, ci)) in bind.rows[start..end].iter().enumerate() {
                let file = &bind.files[fi];
                let channel = &file.channels()[ci];
                match original {
                    0 => vector.insert(row, file.path()),
                    1 => vector.insert(row, file.format()),
                    2 => vector.typed_slice::<u32>()[row] = channel.id,
                    3 => vector.insert(row, bind.map.name_for(&channel.name)),
                    4 => {
                        let (unit, _) =
                            bind.map
                                .unit_for(&channel.name, &channel.unit, channel.unit_source);
                        vector.insert(row, unit.as_str())
                    }
                    5 => {
                        let (_, source) =
                            bind.map
                                .unit_for(&channel.name, &channel.unit, channel.unit_source);
                        vector.insert(row, source.name())
                    }
                    // Registry view of the unit: canonical spelling and physical
                    // dimension. NULL when the unit is absent or unrecognised,
                    // so an unknown unit reads as unknown rather than guessed.
                    6 => {
                        let (unit, _) =
                            bind.map
                                .unit_for(&channel.name, &channel.unit, channel.unit_source);
                        match motorsport_telemetry_core::units::normalize(&unit) {
                            Some(canonical) => vector.insert(row, canonical),
                            None => vector.set_null(row),
                        }
                    }
                    7 => {
                        let (unit, _) =
                            bind.map
                                .unit_for(&channel.name, &channel.unit, channel.unit_source);
                        match motorsport_telemetry_core::units::lookup(&unit) {
                            Some(def) => vector.insert(row, def.dimension.name()),
                            None => vector.set_null(row),
                        }
                    }
                    8 => vector.typed_slice::<u32>()[row] = channel.sample_type.code(),
                    9 => vector.insert(row, channel.sample_type.name()),
                    10 => {
                        if let Some(value) = channel.frequency_hz() {
                            vector.typed_slice::<f64>()[row] = value
                        } else {
                            vector.set_null(row)
                        }
                    }
                    11 => {
                        if let Some(period) = channel.first_period_ns() {
                            vector.typed_slice::<u64>()[row] = period
                        } else {
                            vector.set_null(row)
                        }
                    }
                    12 => vector.typed_slice::<u64>()[row] = channel.sample_count,
                    13 => vector.typed_slice::<u64>()[row] = channel.chunks.len() as u64,
                    14 => vector.typed_slice::<u64>()[row] = channel.duration_ns,
                    _ => vector.typed_slice::<i64>()[row] = (start + row) as i64,
                }
            }
        }
        output.set_len(n);
        Ok(())
    }

    fn supports_pushdown() -> bool {
        true
    }
    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![ty(LogicalTypeId::Varchar)])
    }
    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        Some(vec![
            ("channel_map".into(), ty(LogicalTypeId::Varchar)),
            ("channels".into(), ty(LogicalTypeId::Varchar)),
        ])
    }
}

#[cfg(not(target_os = "emscripten"))]
fn fast_file_metadata(pattern: &str) -> Result<Vec<FileMetadata>, Box<dyn Error>> {
    expand_paths(pattern)?
        .into_iter()
        .map(|path| {
            match path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str()
            {
                "pds" => cosworth_telemetry::read_metadata(&path).map_err(Into::into),
                "ld" => motec_telemetry::read_metadata(&path).map_err(Into::into),
                "vbo" => vbo_telemetry::read_metadata(&path).map_err(Into::into),
                "mp4" => aim_telemetry::read_metadata(&path).map_err(Into::into),
                extension => Err(format!("unsupported telemetry extension {extension}").into()),
            }
        })
        .collect()
}

// ── telemetry_file_metadata: one fast summary row per file ─────────

struct FileMetadataBind {
    metadata: Vec<FileMetadata>,
}

struct FileMetadataInit {
    next: AtomicUsize,
    projected: Vec<u64>,
}

struct FileMetadataVTab;

impl VTab for FileMetadataVTab {
    type BindData = FileMetadataBind;
    type InitData = FileMetadataInit;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        #[cfg(not(target_os = "emscripten"))]
        let metadata = fast_file_metadata(&bind.get_parameter(0).to_string())?;
        #[cfg(target_os = "emscripten")]
        let metadata = open_paths(bind, &bind.get_parameter(0).to_string(), None, None, None)?
            .iter()
            .map(|file| read_source_metadata(file.deref()))
            .collect::<Vec<_>>();
        for (name, logical) in [
            ("file", LogicalTypeId::Varchar),
            ("format", LogicalTypeId::Varchar),
            ("session_key", LogicalTypeId::Varchar),
            ("absolute_start_ns", LogicalTypeId::UBigint),
            ("absolute_end_ns", LogicalTypeId::UBigint),
            ("duration_ns", LogicalTypeId::UBigint),
            ("channel_count", LogicalTypeId::UBigint),
            ("sampled_channel_count", LogicalTypeId::UBigint),
            ("sample_count", LogicalTypeId::UBigint),
            ("schema_hash", LogicalTypeId::UBigint),
            ("driver_ids", LogicalTypeId::Varchar),
            ("lap_count", LogicalTypeId::UBigint),
            ("fastest_lap_number", LogicalTypeId::Bigint),
            ("fastest_lap_time_ns", LogicalTypeId::UBigint),
            ("video_frame_count", LogicalTypeId::UBigint),
            ("driver", LogicalTypeId::Varchar),
            ("vehicle", LogicalTypeId::Varchar),
            ("venue", LogicalTypeId::Varchar),
            ("event", LogicalTypeId::Varchar),
            ("session", LogicalTypeId::Varchar),
            ("date", LogicalTypeId::Varchar),
            ("time", LogicalTypeId::Varchar),
            ("absolute_clock", LogicalTypeId::Varchar),
        ] {
            bind.add_result_column(name, ty(logical));
        }
        bind.set_cardinality(metadata.len() as u64, true);
        Ok(FileMetadataBind { metadata })
    }

    fn init(init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        let bind = unsafe { &*init.get_bind_data::<FileMetadataBind>() };
        init.set_max_threads(worker_count(bind.metadata.len()));
        Ok(FileMetadataInit {
            next: AtomicUsize::new(0),
            projected: projected(init, 23),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let state = func.get_init_data();
        let start = state
            .next
            .fetch_add(VECTOR_SIZE as usize, Ordering::Relaxed);
        let bind = func.get_bind_data();
        if start >= bind.metadata.len() {
            output.set_len(0);
            return Ok(());
        }
        let end = (start + VECTOR_SIZE as usize).min(bind.metadata.len());
        let n = end - start;
        for (out_col, original) in state.projected.iter().copied().enumerate() {
            if out_col >= output.num_columns() {
                break;
            }
            let mut vector = output.flat_vector(out_col);
            for (row, metadata) in bind.metadata[start..end].iter().enumerate() {
                match original {
                    0 => vector.insert(row, metadata.path.as_str()),
                    1 => vector.insert(row, metadata.format.as_str()),
                    2 => match &metadata.session_key {
                        Some(value) => vector.insert(row, value.as_str()),
                        None => vector.set_null(row),
                    },
                    3 => match metadata.absolute_start_ns {
                        Some(value) => vector.typed_slice::<u64>()[row] = value,
                        None => vector.set_null(row),
                    },
                    4 => match metadata.absolute_end_ns {
                        Some(value) => vector.typed_slice::<u64>()[row] = value,
                        None => vector.set_null(row),
                    },
                    5 => vector.typed_slice::<u64>()[row] = metadata.duration_ns,
                    6 => vector.typed_slice::<u64>()[row] = metadata.channel_count as u64,
                    7 => vector.typed_slice::<u64>()[row] = metadata.sampled_channel_count as u64,
                    8 => vector.typed_slice::<u64>()[row] = metadata.sample_count,
                    9 => vector.typed_slice::<u64>()[row] = metadata.schema_hash,
                    10 => {
                        let value = metadata
                            .driver_ids
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(",");
                        vector.insert(row, value.as_str())
                    }
                    11 => vector.typed_slice::<u64>()[row] = metadata.laps.len() as u64,
                    12 => match &metadata.fastest_lap {
                        Some(lap) => vector.typed_slice::<i64>()[row] = lap.number,
                        None => vector.set_null(row),
                    },
                    13 => match &metadata.fastest_lap {
                        Some(lap) => vector.typed_slice::<u64>()[row] = lap.duration_ns,
                        None => vector.set_null(row),
                    },
                    14 => match metadata.video_frame_count {
                        Some(value) => vector.typed_slice::<u64>()[row] = value,
                        None => vector.set_null(row),
                    },
                    15 => vector.insert(row, metadata.identity.driver.as_str()),
                    16 => vector.insert(row, metadata.identity.vehicle.as_str()),
                    17 => vector.insert(row, metadata.identity.venue.as_str()),
                    18 => vector.insert(row, metadata.identity.event.as_str()),
                    19 => vector.insert(row, metadata.identity.session.as_str()),
                    20 => vector.insert(row, metadata.identity.date.as_str()),
                    21 => vector.insert(row, metadata.identity.time.as_str()),
                    22 => match &metadata.absolute_clock {
                        Some(value) => vector.insert(row, value.as_str()),
                        None => vector.set_null(row),
                    },
                    _ => unreachable!(),
                }
            }
        }
        output.set_len(n);
        Ok(())
    }

    fn supports_pushdown() -> bool {
        true
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![ty(LogicalTypeId::Varchar)])
    }
}

// ── telemetry_session_metadata: grouped multi-file summaries ───────

struct SessionMetadataBind {
    sessions: Vec<motorsport_telemetry_core::SessionMetadata>,
    files: Vec<FileMetadata>,
}

struct SessionMetadataInit {
    next: AtomicUsize,
    projected: Vec<u64>,
}

struct SessionMetadataVTab;

impl VTab for SessionMetadataVTab {
    type BindData = SessionMetadataBind;
    type InitData = SessionMetadataInit;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        #[cfg(not(target_os = "emscripten"))]
        let files = fast_file_metadata(&bind.get_parameter(0).to_string())?;
        #[cfg(target_os = "emscripten")]
        let files = open_paths(bind, &bind.get_parameter(0).to_string(), None, None, None)?
            .iter()
            .map(|file| read_source_metadata(file.deref()))
            .collect::<Vec<_>>();
        let max_gap_seconds = named_i64(bind, "max_gap_seconds").unwrap_or(60);
        if !(0..=86_400).contains(&max_gap_seconds) {
            return Err("max_gap_seconds must be between 0 and 86400".into());
        }
        let sessions = group_sessions(&files, max_gap_seconds as u64 * 1_000_000_000);
        for (name, logical) in [
            ("session_key", LogicalTypeId::Varchar),
            ("file_count", LogicalTypeId::UBigint),
            ("absolute_start_ns", LogicalTypeId::UBigint),
            ("absolute_end_ns", LogicalTypeId::UBigint),
            ("duration_ns", LogicalTypeId::UBigint),
            ("driver_ids", LogicalTypeId::Varchar),
            ("stint_count", LogicalTypeId::UBigint),
            ("lap_count", LogicalTypeId::UBigint),
            ("fastest_lap_number", LogicalTypeId::Bigint),
            ("fastest_lap_time_ns", LogicalTypeId::UBigint),
        ] {
            bind.add_result_column(name, ty(logical));
        }
        bind.set_cardinality(sessions.len() as u64, true);
        Ok(SessionMetadataBind { sessions, files })
    }

    fn init(init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        Ok(SessionMetadataInit {
            next: AtomicUsize::new(0),
            projected: projected(init, 10),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let state = func.get_init_data();
        let start = state
            .next
            .fetch_add(VECTOR_SIZE as usize, Ordering::Relaxed);
        let bind = func.get_bind_data();
        if start >= bind.sessions.len() {
            output.set_len(0);
            return Ok(());
        }
        let end = (start + VECTOR_SIZE as usize).min(bind.sessions.len());
        let n = end - start;
        for (out_col, original) in state.projected.iter().copied().enumerate() {
            if out_col >= output.num_columns() {
                break;
            }
            let mut vector = output.flat_vector(out_col);
            for (row, session) in bind.sessions[start..end].iter().enumerate() {
                match original {
                    0 => vector.insert(row, session.session_key.as_str()),
                    1 => vector.typed_slice::<u64>()[row] = session.files.len() as u64,
                    2 => match session.absolute_start_ns {
                        Some(value) => vector.typed_slice::<u64>()[row] = value,
                        None => vector.set_null(row),
                    },
                    3 => match session.absolute_end_ns {
                        Some(value) => vector.typed_slice::<u64>()[row] = value,
                        None => vector.set_null(row),
                    },
                    4 => vector.typed_slice::<u64>()[row] = session.duration_ns,
                    5 => {
                        let value = session
                            .files
                            .iter()
                            .flat_map(|index| bind.files[*index].driver_ids.iter().copied())
                            .collect::<std::collections::BTreeSet<_>>()
                            .into_iter()
                            .map(|driver| driver.to_string())
                            .collect::<Vec<_>>()
                            .join(",");
                        vector.insert(row, value.as_str())
                    }
                    6 => vector.typed_slice::<u64>()[row] = session.driver_stints.len() as u64,
                    7 => vector.typed_slice::<u64>()[row] = session.laps.len() as u64,
                    8 => match &session.fastest_lap {
                        Some(lap) => vector.typed_slice::<i64>()[row] = lap.number,
                        None => vector.set_null(row),
                    },
                    9 => match &session.fastest_lap {
                        Some(lap) => vector.typed_slice::<u64>()[row] = lap.duration_ns,
                        None => vector.set_null(row),
                    },
                    _ => unreachable!(),
                }
            }
        }
        output.set_len(n);
        Ok(())
    }

    fn supports_pushdown() -> bool {
        true
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![ty(LogicalTypeId::Varchar)])
    }

    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        Some(vec![("max_gap_seconds".into(), ty(LogicalTypeId::Bigint))])
    }
}

// ── read_telemetry: projected, resampled wide relation ─────────────

struct WideBind {
    files: Vec<InputFile>,
    names: Vec<String>,
    // [file][wide channel column] -> source channel index
    source_channels: Vec<Vec<Option<usize>>>,
    ranges: Vec<(u64, u64, u64)>, // start_ns, end_ns, row_count
    rate: u64,
    linear: bool,
    include_filename: bool,
    include_create_date: bool,
    include_modified_at: bool,
    /// Opt-in renaming / unit conversion applied to the wide columns.
    /// Retained so `telemetry_column_comments` and the bind-time column names
    /// agree on what each column is called.
    #[allow(dead_code)]
    map: ChannelMap,
    /// Per wide column, the (scale, offset) to apply. None = pass through.
    conversions: Vec<Option<(f64, f64)>>,
    session_mode: bool,
    session_key: String,
    session_offsets_ns: Vec<i128>,
    driver_channels: Vec<Option<usize>>,
    lap_channels: Vec<Option<usize>>,
}

#[derive(Clone, Copy)]
struct WideSegment {
    file: usize,
    row_start: u64,
    len: u64,
}
struct WideInit {
    next: AtomicUsize,
    segments: Vec<WideSegment>,
    projected: Vec<u64>,
}
struct WideVTab;

impl VTab for WideVTab {
    type BindData = WideBind;
    type InitData = WideInit;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        let config = unsafe { &*bind.get_extra_info::<ReaderConfig>() };
        let pattern = bind.get_parameter(0).to_string();
        let create_date_from = named_timestamp(bind, "create_date_from")?;
        let create_date_to = named_timestamp(bind, "create_date_to")?;
        if matches!((create_date_from, create_date_to), (Some(from), Some(to)) if to < from) {
            return Err("create_date_to must be greater than or equal to create_date_from".into());
        }
        let mut files = open_paths(
            bind,
            &pattern,
            config.format,
            create_date_from,
            create_date_to,
        )?;
        let mut metadata = files
            .iter()
            .map(|file| read_source_metadata(file.deref()))
            .collect::<Vec<_>>();
        let mut session_key = String::new();
        let mut session_base_ns = 0u64;
        if config.session {
            let max_gap_seconds = named_i64(bind, "max_gap_seconds").unwrap_or(60);
            if !(0..=86_400).contains(&max_gap_seconds) {
                return Err("max_gap_seconds must be between 0 and 86400".into());
            }
            let sessions = group_sessions(&metadata, max_gap_seconds as u64 * 1_000_000_000);
            if sessions.len() != 1 {
                return Err(format!(
                    "read_telemetry_session matched {} sessions; narrow the glob or adjust max_gap_seconds",
                    sessions.len()
                )
                .into());
            }
            let session = &sessions[0];
            session_key = session.session_key.clone();
            session_base_ns = session.absolute_start_ns.unwrap_or(0);
            files = session
                .files
                .iter()
                .map(|index| files[*index].clone())
                .collect();
            metadata = session
                .files
                .iter()
                .map(|index| metadata[*index].clone())
                .collect();
        }
        let rate = named_i64(bind, "rate").unwrap_or(100);
        if !(1..=5000).contains(&rate) {
            return Err("rate must be between 1 and 5000 Hz".into());
        }
        let start_ns = named_i64(bind, "start_ns").unwrap_or(0).max(0) as u64;
        let requested_end = named_i64(bind, "end_ns").unwrap_or(i64::MAX).max(0) as u64;
        if requested_end < start_ns {
            return Err("end_ns must be greater than or equal to start_ns".into());
        }
        let interpolation = named_string(bind, "interpolate")
            .unwrap_or_else(|| "linear".into())
            .to_ascii_lowercase();
        if interpolation != "previous" && interpolation != "linear" {
            return Err("interpolate must be 'previous' or 'linear'".into());
        }
        let filter = parse_channel_filter(named_string(bind, "channels").as_deref());
        // `filename` follows DuckDB's read_json/read_csv convention. Keep the
        // more explicit spelling as an alias for callers that use it already.
        let include_filename = named_bool(bind, "filename").unwrap_or(false)
            || named_bool(bind, "add_filename_as_column").unwrap_or(false);
        let include_timestamps = named_bool(bind, "timestamps").unwrap_or(false);
        // Unit aliases are strict DuckDB logical types, so opt in: they make
        // provenance available to telemetry_convert_column but intentionally
        // require an explicit ::DOUBLE before generic numeric functions.
        let tag_units = named_bool(bind, "unit_tags").unwrap_or(false);
        let include_create_date =
            include_timestamps || named_bool(bind, "add_create_date_as_column").unwrap_or(false);
        let include_modified_at =
            include_timestamps || named_bool(bind, "add_modified_at_as_column").unwrap_or(false);

        let mut names = Vec::new();
        let mut keys = HashSet::new();
        for file in &files {
            for channel in file.channels() {
                let key = channel.name.to_ascii_lowercase();
                if channel.sample_count == 0
                    || (!filter.is_empty() && !filter.contains(&key))
                    || !keys.insert(key)
                {
                    continue;
                }
                names.push(channel.name.clone());
            }
        }
        if !filter.is_empty() {
            let found = names
                .iter()
                .map(|n| n.to_ascii_lowercase())
                .collect::<HashSet<_>>();
            let missing = filter.difference(&found).cloned().collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(
                    format!("telemetry channel(s) not found: {}", missing.join(", ")).into(),
                );
            }
        }

        // Opt-in renaming / unit conversion. Parsed before columns are
        // declared because a rename changes the SQL column name itself.
        let channel_map = named_channel_map(bind)?;
        if !channel_map.is_empty() {
            let available: Vec<String> = files
                .iter()
                .flat_map(|file| file.channels())
                .map(|channel| channel.name.clone())
                .collect();
            channel_map.validate(&available)?;
        }
        // Precompute per-column conversions so the hot loop stays a multiply.
        let conversions: Vec<Option<(f64, f64)>> = names
            .iter()
            .map(|name| {
                channel_map
                    .rule_for(name)
                    .filter(|rule| !rule.is_identity())
                    .map(|rule| (rule.scale, rule.offset))
            })
            .collect();

        if config.session {
            for (name, logical) in [
                ("session_key", LogicalTypeId::Varchar),
                ("time_ns", LogicalTypeId::Bigint),
                ("file_time_ns", LogicalTypeId::Bigint),
                ("source_file", LogicalTypeId::Varchar),
                ("video_file_index", LogicalTypeId::UInteger),
                ("video_frame_index", LogicalTypeId::UBigint),
                ("video_sync_time", LogicalTypeId::Double),
                ("driver_id", LogicalTypeId::Bigint),
                ("lap_number", LogicalTypeId::Bigint),
            ] {
                bind.add_result_column(name, ty(logical));
            }
        } else {
            if include_filename {
                bind.add_result_column("filename", ty(LogicalTypeId::Varchar));
            }
            if include_create_date {
                bind.add_result_column("create_date", ty(LogicalTypeId::Timestamp));
            }
            if include_modified_at {
                bind.add_result_column("modified_at", ty(LogicalTypeId::Timestamp));
            }
            bind.add_result_column("time_ns", ty(LogicalTypeId::Bigint));
        }
        // Wide telemetry columns remain physically DOUBLE, but known units are
        // carried as logical aliases. telemetry_convert_column() reads this
        // tag, while ordinary DuckDB arithmetic continues to work normally.
        for name in &names {
            let declared_units = files
                .iter()
                .flat_map(|file| file.channels())
                .filter(|channel| channel.name.eq_ignore_ascii_case(name))
                .map(|channel| {
                    channel_map
                        .unit_for(&channel.name, &channel.unit, channel.unit_source)
                        .0
                })
                .collect::<HashSet<_>>();
            let logical = if tag_units && declared_units.len() == 1 {
                unit_sql::tagged_double(declared_units.iter().next().unwrap())
            } else {
                // A mixed-file query with conflicting units has no truthful
                // single tag. Leave it untagged so inference fails loudly.
                ty(LogicalTypeId::Double)
            };
            bind.add_result_column(channel_map.name_for(name), logical);
        }

        let source_channels = files
            .iter()
            .map(|file| {
                let map = file
                    .channels()
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (c.name.to_ascii_lowercase(), i))
                    .collect::<HashMap<_, _>>();
                names
                    .iter()
                    .map(|name| map.get(&name.to_ascii_lowercase()).copied())
                    .collect()
            })
            .collect::<Vec<Vec<Option<usize>>>>();
        let session_offsets_ns = metadata
            .iter()
            .map(|file| file.clock_offset_ns.unwrap_or(0) - i128::from(session_base_ns))
            .collect::<Vec<_>>();
        let driver_channels = files
            .iter()
            .map(|file| matching_channel(file.deref(), &["driverid", "driver", "driverindex"]))
            .collect::<Vec<_>>();
        let lap_channels = files
            .iter()
            .map(|file| {
                matching_channel(
                    file.deref(),
                    &[
                        "lapnumber",
                        "lapnum",
                        "lapcount",
                        "lapcounter",
                        "currentlap",
                        "lap",
                    ],
                )
            })
            .collect::<Vec<_>>();

        let mut ranges = Vec::with_capacity(files.len());
        let mut total_rows = 0u64;
        for (fi, file) in files.iter().enumerate() {
            let duration = source_channels[fi]
                .iter()
                .flatten()
                .map(|&ci| file.channels()[ci].duration_ns)
                .max()
                .unwrap_or(0);
            let (local_start, local_requested_end) = if config.session {
                let offset = u64::try_from(session_offsets_ns[fi]).unwrap_or(0);
                (
                    start_ns.saturating_sub(offset),
                    requested_end.saturating_sub(offset),
                )
            } else {
                (start_ns, requested_end)
            };
            let end = local_requested_end.min(duration);
            let rows = if end <= local_start {
                0
            } else {
                ((end - local_start) as u128 * rate as u128).div_ceil(1_000_000_000) as u64
            };
            ranges.push((local_start, end, rows));
            total_rows = total_rows.saturating_add(rows);
        }
        bind.set_cardinality(total_rows, true);
        Ok(WideBind {
            files,
            names,
            source_channels,
            ranges,
            rate: rate as u64,
            linear: interpolation == "linear",
            include_filename,
            include_create_date,
            include_modified_at,
            map: channel_map,
            conversions,
            session_mode: config.session,
            session_key,
            session_offsets_ns,
            driver_channels,
            lap_channels,
        })
    }

    fn init(init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        let bind = unsafe { &*init.get_bind_data::<WideBind>() };
        let mut segments = Vec::new();
        for (file, &(_, _, rows)) in bind.ranges.iter().enumerate() {
            let mut row = 0;
            while row < rows {
                let len = (rows - row).min(VECTOR_SIZE);
                segments.push(WideSegment {
                    file,
                    row_start: row,
                    len,
                });
                row += len;
            }
        }
        init.set_max_threads(worker_count(segments.len()));
        let fixed_columns = if bind.session_mode {
            SESSION_FIXED_COLUMNS as usize
        } else {
            1 + usize::from(bind.include_filename)
                + usize::from(bind.include_create_date)
                + usize::from(bind.include_modified_at)
        };
        Ok(WideInit {
            next: AtomicUsize::new(0),
            segments,
            projected: projected(init, (bind.names.len() + fixed_columns) as u64),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let state = func.get_init_data();
        let segment_idx = state.next.fetch_add(1, Ordering::Relaxed);
        let Some(segment) = state.segments.get(segment_idx) else {
            output.set_len(0);
            return Ok(());
        };
        let bind = func.get_bind_data();
        let file = &bind.files[segment.file];
        let start_ns = bind.ranges[segment.file].0;
        let n = segment.len as usize;
        for (out_col, original) in state.projected.iter().copied().enumerate() {
            if out_col >= output.num_columns() {
                break;
            }
            if bind.session_mode {
                let file_time_ns = |row: usize| {
                    let source_row = segment.row_start + row as u64;
                    (start_ns as u128 + source_row as u128 * 1_000_000_000u128 / bind.rate as u128)
                        as u64
                };
                match original {
                    0 => {
                        let vector = output.flat_vector(out_col);
                        for row in 0..n {
                            vector.insert(row, bind.session_key.as_str());
                        }
                    }
                    1 => {
                        let offset = bind.session_offsets_ns[segment.file];
                        let mut vector = output.flat_vector(out_col);
                        let dst = &mut vector.typed_slice::<i64>()[..n];
                        for (row, value) in dst.iter_mut().enumerate() {
                            *value = i64::try_from(i128::from(file_time_ns(row)) + offset)
                                .unwrap_or(i64::MAX);
                        }
                    }
                    2 => {
                        let mut vector = output.flat_vector(out_col);
                        let dst = &mut vector.typed_slice::<i64>()[..n];
                        for (row, value) in dst.iter_mut().enumerate() {
                            *value = file_time_ns(row) as i64;
                        }
                    }
                    3 => {
                        let vector = output.flat_vector(out_col);
                        for row in 0..n {
                            vector.insert(row, file.path());
                        }
                    }
                    4..=6 => {
                        let mut vector = output.flat_vector(out_col);
                        for row in 0..n {
                            let reference = file.video_reference_at(file_time_ns(row));
                            match original {
                                4 => match reference.file_index {
                                    Some(value) => vector.typed_slice::<u32>()[row] = value,
                                    None => vector.set_null(row),
                                },
                                5 => match reference.frame_index {
                                    Some(value) => vector.typed_slice::<u64>()[row] = value,
                                    None => vector.set_null(row),
                                },
                                6 => match reference.sync_time {
                                    Some(value) => vector.typed_slice::<f64>()[row] = value,
                                    None => vector.set_null(row),
                                },
                                _ => unreachable!(),
                            }
                        }
                    }
                    7 | 8 => {
                        let channel_index = if original == 7 {
                            bind.driver_channels[segment.file]
                        } else {
                            bind.lap_channels[segment.file]
                        };
                        let mut vector = output.flat_vector(out_col);
                        for row in 0..n {
                            match channel_index
                                .and_then(|index| file.sample_at(index, file_time_ns(row), false))
                                .filter(|value| value.is_finite())
                            {
                                Some(value) => {
                                    vector.typed_slice::<i64>()[row] = value.round() as i64
                                }
                                None => vector.set_null(row),
                            }
                        }
                    }
                    col if col >= SESSION_FIXED_COLUMNS
                        && (col - SESSION_FIXED_COLUMNS) < bind.names.len() as u64 =>
                    {
                        let wide_idx = (col - SESSION_FIXED_COLUMNS) as usize;
                        let mut vector = output.flat_vector(out_col);
                        if let Some(channel_idx) = bind.source_channels[segment.file][wide_idx] {
                            let conversion = bind.conversions.get(wide_idx).copied().flatten();
                            for row in 0..n {
                                if let Some(value) =
                                    file.sample_at(channel_idx, file_time_ns(row), bind.linear)
                                {
                                    vector.typed_slice::<f64>()[row] = match conversion {
                                        Some((scale, offset)) => value * scale + offset,
                                        None => value,
                                    };
                                } else {
                                    vector.set_null(row);
                                }
                            }
                        } else {
                            for row in 0..n {
                                vector.set_null(row);
                            }
                        }
                    }
                    _ => unreachable!(),
                }
                continue;
            }
            let create_date_column = u64::from(bind.include_filename);
            let modified_at_column = create_date_column + u64::from(bind.include_create_date);
            let time_column = modified_at_column + u64::from(bind.include_modified_at);
            let channel_offset = time_column + 1;
            match original {
                0 if bind.include_filename => {
                    let vector = output.flat_vector(out_col);
                    for row in 0..n {
                        vector.insert(row, file.path());
                    }
                }
                col if bind.include_create_date && col == create_date_column => {
                    output.flat_vector(out_col).typed_slice::<i64>()[..n]
                        .fill(file.create_date_micros);
                }
                col if bind.include_modified_at && col == modified_at_column => {
                    output.flat_vector(out_col).typed_slice::<i64>()[..n]
                        .fill(file.modified_at_micros);
                }
                col if col == time_column => {
                    let mut vector = output.flat_vector(out_col);
                    let dst = &mut vector.typed_slice::<i64>()[..n];
                    for (row, value) in dst.iter_mut().enumerate() {
                        let source_row = segment.row_start + row as u64;
                        *value = (start_ns as u128
                            + source_row as u128 * 1_000_000_000u128 / bind.rate as u128)
                            as i64;
                    }
                }
                col if col >= channel_offset
                    && (col - channel_offset) < bind.names.len() as u64 =>
                {
                    let wide_idx = (col - channel_offset) as usize;
                    let mut vector = output.flat_vector(out_col);
                    if let Some(channel_idx) = bind.source_channels[segment.file][wide_idx] {
                        // Only mapped channels are converted; everything else
                        // passes through exactly as decoded.
                        let conversion = bind.conversions.get(wide_idx).copied().flatten();
                        for row in 0..n {
                            let source_row = segment.row_start + row as u64;
                            let time_ns = (start_ns as u128
                                + source_row as u128 * 1_000_000_000u128 / bind.rate as u128)
                                as u64;
                            if let Some(value) = file.sample_at(channel_idx, time_ns, bind.linear) {
                                vector.typed_slice::<f64>()[row] = match conversion {
                                    Some((scale, offset)) => value * scale + offset,
                                    None => value,
                                };
                            } else {
                                vector.set_null(row);
                            }
                        }
                    } else {
                        for row in 0..n {
                            vector.set_null(row);
                        }
                    }
                }
                _ => output.flat_vector(out_col).typed_slice::<i64>()[..n].fill(segment_idx as i64),
            }
        }
        output.set_len(n);
        Ok(())
    }

    fn supports_pushdown() -> bool {
        true
    }
    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![ty(LogicalTypeId::Varchar)])
    }
    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        Some(vec![
            ("rate".into(), ty(LogicalTypeId::Bigint)),
            ("channels".into(), ty(LogicalTypeId::Varchar)),
            ("start_ns".into(), ty(LogicalTypeId::Bigint)),
            ("end_ns".into(), ty(LogicalTypeId::Bigint)),
            ("interpolate".into(), ty(LogicalTypeId::Varchar)),
            ("max_gap_seconds".into(), ty(LogicalTypeId::Bigint)),
            ("filename".into(), ty(LogicalTypeId::Boolean)),
            ("add_filename_as_column".into(), ty(LogicalTypeId::Boolean)),
            ("timestamps".into(), ty(LogicalTypeId::Boolean)),
            ("unit_tags".into(), ty(LogicalTypeId::Boolean)),
            (
                "add_create_date_as_column".into(),
                ty(LogicalTypeId::Boolean),
            ),
            (
                "add_modified_at_as_column".into(),
                ty(LogicalTypeId::Boolean),
            ),
            ("create_date_from".into(), ty(LogicalTypeId::Timestamp)),
            ("create_date_to".into(), ty(LogicalTypeId::Timestamp)),
            // Advanced: opt-in channel renaming and unit conversion. Inline
            // rules or a path to a rules file.
            ("channel_map".into(), ty(LogicalTypeId::Varchar)),
        ])
    }
}

// ── telemetry_column_comments: unit metadata as DDL ────────────────
//
// DuckDB cannot attach comments to a table function's result columns, so a
// query that wants unit metadata to persist has to materialise a table and
// then comment its columns. This emits the exact `COMMENT ON COLUMN`
// statements for that, so units travel with the table instead of living only
// in a query someone has to remember to re-read.
//
//   CREATE TABLE laps AS SELECT * FROM read_telemetry('run.pds');
//   -- then execute each statement from:
//   SELECT ddl FROM telemetry_column_comments('run.pds', 'laps');

struct CommentsBind {
    /// (column, unit, comment DDL, KV_METADATA payload, channel_map rule)
    statements: Vec<(String, String, String, String, String)>,
}

struct CommentsInit {
    next: AtomicUsize,
}

struct CommentsVTab;

impl VTab for CommentsVTab {
    type BindData = CommentsBind;
    type InitData = CommentsInit;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        let pattern = bind.get_parameter(0).to_string();
        let table = bind.get_parameter(1).to_string();
        let files = open_paths(bind, &pattern, None, None, None)?;
        let map = named_channel_map(bind)?;
        // Scope the output to a channel subset, so the generated DDL matches a
        // table materialised with the same `channels` filter instead of naming
        // columns that table does not have.
        let filter = parse_channel_filter(named_string(bind, "channels").as_deref());

        for (name, logical) in [
            ("column_name", LogicalTypeId::Varchar),
            ("unit", LogicalTypeId::Varchar),
            // COMMENT ON COLUMN: lives in the DuckDB catalog, queryable via
            // duckdb_columns(), but lost by COPY ... TO parquet.
            ("ddl", LogicalTypeId::Varchar),
            // KV_METADATA payload: survives export to Parquet, where a column
            // comment would not. Use with COPY ... (FORMAT PARQUET, KV_METADATA ...).
            ("kv_metadata", LogicalTypeId::Varchar),
            // The channel_map rule that reproduces this column's unit, so a
            // map can be recovered from the file rather than written by hand.
            ("channel_map_rule", LogicalTypeId::Varchar),
        ] {
            bind.add_result_column(name, ty(logical));
        }

        // Deduplicate by column name: the same channel can appear in several
        // files of a glob, and a table has one column per name.
        let mut seen = HashSet::new();
        let mut columns: Vec<(String, String, UnitSource, f64, u64)> = Vec::new();
        for file in &files {
            for channel in file.channels() {
                let column = map.name_for(&channel.name).to_owned();
                // Match on either the source or the mapped name, so a filter
                // works whether the caller thinks in file or mapped names.
                if !filter.is_empty()
                    && !filter.contains(&channel.name.to_ascii_lowercase())
                    && !filter.contains(&column.to_ascii_lowercase())
                {
                    continue;
                }
                let (unit, source) =
                    map.unit_for(&channel.name, &channel.unit, channel.unit_source);
                if !seen.insert(column.clone()) {
                    continue;
                }
                columns.push((
                    column,
                    unit,
                    source,
                    channel.frequency_hz().unwrap_or(0.0),
                    channel.first_period_ns().unwrap_or(0),
                ));
            }
        }

        let statements = columns
            .into_iter()
            .map(|(column, unit, source, frequency_hz, sample_period_ns)| {
                let mut payload = channel_map::unit_payload(&unit, source);
                payload.push_str(&format!(
                    "; native_frequency_hz={frequency_hz}; native_sample_period_ns={sample_period_ns}"
                ));
                let ddl = format!(
                    "COMMENT ON COLUMN {} IS '{}';",
                    channel_map::quote_qualified(&table, &column),
                    channel_map::escape_literal(&payload)
                );
                let rule = if unit.is_empty() {
                    String::new()
                } else {
                    format!("{column} -> {column} [{unit}]")
                };
                (column, unit, ddl, payload, rule)
            })
            .collect::<Vec<_>>();
        bind.set_cardinality(statements.len() as u64, true);
        Ok(CommentsBind { statements })
    }

    fn init(_init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        Ok(CommentsInit {
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
        if start >= bind.statements.len() {
            output.set_len(0);
            return Ok(());
        }
        let end = (start + VECTOR_SIZE as usize).min(bind.statements.len());
        for (row, (column, unit, ddl, payload, rule)) in
            bind.statements[start..end].iter().enumerate()
        {
            output.flat_vector(0).insert(row, column.as_str());
            output.flat_vector(1).insert(row, unit.as_str());
            output.flat_vector(2).insert(row, ddl.as_str());
            output.flat_vector(3).insert(row, payload.as_str());
            output.flat_vector(4).insert(row, rule.as_str());
        }
        output.set_len(end - start);
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![ty(LogicalTypeId::Varchar), ty(LogicalTypeId::Varchar)])
    }

    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        Some(vec![
            ("channel_map".into(), ty(LogicalTypeId::Varchar)),
            ("channels".into(), ty(LogicalTypeId::Varchar)),
        ])
    }
}

// ── write_telemetry: export a source to a telemetry file ───────────
//
// Counterpart to `read_telemetry`. Only MoTeC LD output is supported today;
// the `format` argument exists so PDS/VBO writers can slot in without a
// breaking signature change.

struct WriteBind {
    files: Vec<InputFile>,
    output: String,
    metadata: MotecMetadata,
    /// SQL rows: [source, target name, target unit].
    channel_mapping: Vec<Vec<String>>,
    /// SQL rows: [left source, right source, target name, target unit].
    sum_channels: Vec<Vec<String>>,
    include_unmapped: bool,
    exclude_channels: Vec<String>,
}

enum ExportInputs {
    Direct(usize),
    Sum(usize, usize),
}

struct ExportChannel {
    inputs: ExportInputs,
    scale: f64,
    offset: f64,
}

/// A SQL-declared projection over a telemetry source. It gives the existing LD
/// writer renamed, converted and derived channels without teaching the writer
/// about SQL or any particular car schema.
struct ExportSource {
    source: SourceRef,
    channels: Vec<Channel>,
    exports: Vec<ExportChannel>,
}

fn affine_conversion(from: &str, to: &str) -> Result<(f64, f64), Box<dyn Error>> {
    if from.eq_ignore_ascii_case(to) || units::normalize(from) == units::normalize(to) {
        return Ok((1.0, 0.0));
    }
    let offset = units::convert(0.0, from, to)?;
    let scale = units::convert(1.0, from, to)? - offset;
    Ok((scale, offset))
}

impl ExportSource {
    fn new(
        source: SourceRef,
        mapping: &[Vec<String>],
        sums: &[Vec<String>],
        include_unmapped: bool,
        exclude_channels: &[String],
    ) -> Result<Self, Box<dyn Error>> {
        if mapping.is_empty() && sums.is_empty() && exclude_channels.is_empty() {
            return Ok(Self {
                channels: source.channels().to_vec(),
                exports: (0..source.channels().len())
                    .map(|index| ExportChannel {
                        inputs: ExportInputs::Direct(index),
                        scale: 1.0,
                        offset: 0.0,
                    })
                    .collect(),
                source,
            });
        }
        let find = |name: &str| {
            source
                .channels()
                .iter()
                .position(|channel| channel.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("export source channel not found: {name}"))
        };
        let mut channels = Vec::with_capacity(source.channels().len() + sums.len());
        let mut exports = Vec::with_capacity(source.channels().len() + sums.len());
        let mut target_names = HashSet::new();
        let mut mapped_sources = HashSet::new();
        let excluded = exclude_channels
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<HashSet<_>>();

        for row in mapping {
            let index = find(&row[0])?;
            mapped_sources.insert(index);
            let original = &source.channels()[index];
            let target_name = row[1].trim();
            let target_unit = row[2].trim();
            if target_name.is_empty() {
                return Err(format!("empty target name for {}", row[0]).into());
            }
            if !target_names.insert(target_name.to_ascii_lowercase()) {
                return Err(format!("duplicate export target channel: {target_name}").into());
            }
            let unit = if target_unit.is_empty() {
                original.unit.as_str()
            } else {
                target_unit
            };
            let (scale, offset) = affine_conversion(&original.unit, unit)?;
            let mut channel = original.clone();
            channel.name = target_name.to_owned();
            channel.unit = unit.to_owned();
            if !target_unit.is_empty() {
                channel.unit_source = UnitSource::Declared;
            }
            if scale != 1.0 || offset != 0.0 {
                channel.sample_type = SampleType::F64;
            }
            channels.push(channel);
            exports.push(ExportChannel {
                inputs: ExportInputs::Direct(index),
                scale,
                offset,
            });
        }

        if include_unmapped {
            for (index, original) in source.channels().iter().enumerate() {
                if mapped_sources.contains(&index)
                    || excluded.contains(&original.name.to_ascii_lowercase())
                {
                    continue;
                }
                if !target_names.insert(original.name.to_ascii_lowercase()) {
                    return Err(
                        format!("duplicate export target channel: {}", original.name).into(),
                    );
                }
                channels.push(original.clone());
                exports.push(ExportChannel {
                    inputs: ExportInputs::Direct(index),
                    scale: 1.0,
                    offset: 0.0,
                });
            }
        }

        for row in sums {
            let left_index = find(&row[0])?;
            let right_index = find(&row[1])?;
            let left = &source.channels()[left_index];
            let right = &source.channels()[right_index];
            if left.chunks.len() != right.chunks.len()
                || left.sample_count != right.sample_count
                || left.chunks.iter().zip(&right.chunks).any(|(a, b)| {
                    a.sample_period_ns != b.sample_period_ns
                        || a.sample_count != b.sample_count
                        || a.time_base_ns != b.time_base_ns
                })
            {
                return Err(
                    format!("cannot sum {} and {}: sample clocks differ", row[0], row[1]).into(),
                );
            }
            if units::normalize(&left.unit) != units::normalize(&right.unit) {
                return Err(format!(
                    "cannot sum {} [{}] and {} [{}]: units differ",
                    row[0], left.unit, row[1], right.unit
                )
                .into());
            }
            let target_name = row[2].trim();
            let target_unit = row[3].trim();
            if target_name.is_empty() || target_unit.is_empty() {
                return Err("sum_channels target name and unit cannot be empty".into());
            }
            if !target_names.insert(target_name.to_ascii_lowercase()) {
                return Err(format!("duplicate export target channel: {target_name}").into());
            }
            let (scale, offset) = affine_conversion(&left.unit, target_unit)?;
            let mut channel = left.clone();
            channel.name = target_name.to_owned();
            channel.unit = target_unit.to_owned();
            channel.unit_source = UnitSource::Declared;
            channel.sample_type = SampleType::F64;
            channels.push(channel);
            exports.push(ExportChannel {
                inputs: ExportInputs::Sum(left_index, right_index),
                scale,
                offset,
            });
        }

        Ok(Self {
            source,
            channels,
            exports,
        })
    }
}

impl TelemetrySource for ExportSource {
    fn path(&self) -> &str {
        self.source.path()
    }
    fn format(&self) -> &'static str {
        self.source.format()
    }
    fn channels(&self) -> &[Channel] {
        &self.channels
    }
    fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
        let export = &self.exports[channel_index];
        let value = match export.inputs {
            ExportInputs::Direct(index) => self.source.decode(index, chunk_index, local_index),
            ExportInputs::Sum(left, right) => {
                self.source.decode(left, chunk_index, local_index)
                    + self.source.decode(right, chunk_index, local_index)
            }
        };
        value * export.scale + export.offset
    }
}

struct WriteInit {
    done: AtomicUsize,
}

struct WriteVTab;

impl VTab for WriteVTab {
    type BindData = WriteBind;
    type InitData = WriteInit;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        let pattern = bind.get_parameter(0).to_string();
        let output = bind.get_parameter(1).to_string();

        // Default to the output file's extension, then fall back to motec.
        let requested = named_string(bind, "format").unwrap_or_else(|| {
            match Path::new(&output)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str()
            {
                "pds" => "pds".into(),
                "vbo" => "vbo".into(),
                _ => "motec".into(),
            }
        });
        let format = requested.to_ascii_lowercase();
        if format != "motec" && format != "ld" {
            return Err(format!(
                "write_telemetry currently supports format 'motec' only, got '{requested}'"
            )
            .into());
        }

        let files = open_paths(bind, &pattern, None, None, None)?;
        if files.len() != 1 {
            return Err(format!(
                "write_telemetry writes one file at a time but {} matched {pattern}",
                files.len()
            )
            .into());
        }
        for (name, logical) in [
            ("source", LogicalTypeId::Varchar),
            ("output", LogicalTypeId::Varchar),
            ("format", LogicalTypeId::Varchar),
            ("channels", LogicalTypeId::UBigint),
            ("samples", LogicalTypeId::UBigint),
            ("bytes", LogicalTypeId::UBigint),
            ("sidecar", LogicalTypeId::Varchar),
            ("sidecar_bytes", LogicalTypeId::UBigint),
        ] {
            bind.add_result_column(name, ty(logical));
        }
        bind.set_cardinality(1, true);
        let channel_mapping = named_string_rows(bind, "channel_mapping", 3)?;
        let sum_channels = named_string_rows(bind, "sum_channels", 4)?;
        let include_unmapped = named_bool(bind, "include_unmapped").unwrap_or(false);
        let exclude_channels = named_string_list(bind, "exclude_channels")?;
        let metadata = MotecMetadata {
            driver: named_string(bind, "driver").unwrap_or_default(),
            vehicle: named_string(bind, "vehicle").unwrap_or_default(),
            vehicle_number: named_string(bind, "vehicle_number").unwrap_or_default(),
            team: named_string(bind, "team").unwrap_or_default(),
            venue: named_string(bind, "venue").unwrap_or_default(),
            event: named_string(bind, "event").unwrap_or_default(),
            session: named_string(bind, "session").unwrap_or_default(),
            short_comment: named_string(bind, "comment").unwrap_or_default(),
            date: named_string(bind, "date").unwrap_or_default(),
            time: named_string(bind, "time").unwrap_or_default(),
            ..Default::default()
        };
        Ok(WriteBind {
            files,
            output,
            metadata,
            channel_mapping,
            sum_channels,
            include_unmapped,
            exclude_channels,
        })
    }

    fn init(_init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        Ok(WriteInit {
            done: AtomicUsize::new(0),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let state = func.get_init_data();
        // Writing has a side effect, so run it exactly once.
        if state.done.fetch_add(1, Ordering::SeqCst) != 0 {
            output.set_len(0);
            return Ok(());
        }
        let bind = func.get_bind_data();
        let input = &bind.files[0];
        let source = ExportSource::new(
            Arc::clone(&input.source),
            &bind.channel_mapping,
            &bind.sum_channels,
            bind.include_unmapped,
            &bind.exclude_channels,
        )?;
        let bytes = write_motec_bytes(&source, &bind.metadata)?;
        std::fs::write(&bind.output, &bytes)?;
        let sidecar = motec_sidecar_path(&bind.output);
        // Infer beacons from the full input even when the SQL projection omits
        // lap channels from the LD output.
        let sidecar_bytes = write_motec_ldx_bytes(input.source.as_ref(), &bind.metadata);
        std::fs::write(&sidecar, &sidecar_bytes)?;

        let channels = source
            .channels()
            .iter()
            .filter(|channel| channel.sample_count > 0)
            .count() as u64;
        let samples = source
            .channels()
            .iter()
            .map(|channel| channel.sample_count)
            .sum::<u64>();

        output.flat_vector(0).insert(0, input.path());
        output.flat_vector(1).insert(0, bind.output.as_str());
        output.flat_vector(2).insert(0, "motec");
        output.flat_vector(3).typed_slice::<u64>()[0] = channels;
        output.flat_vector(4).typed_slice::<u64>()[0] = samples;
        output.flat_vector(5).typed_slice::<u64>()[0] = bytes.len() as u64;
        output
            .flat_vector(6)
            .insert(0, sidecar.to_string_lossy().as_ref());
        output.flat_vector(7).typed_slice::<u64>()[0] = sidecar_bytes.len() as u64;
        output.set_len(1);
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![ty(LogicalTypeId::Varchar), ty(LogicalTypeId::Varchar)])
    }

    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        let string_list = LogicalTypeHandle::list(&ty(LogicalTypeId::Varchar));
        let string_rows = LogicalTypeHandle::list(&string_list);
        Some(vec![
            ("format".into(), ty(LogicalTypeId::Varchar)),
            ("channel_mapping".into(), string_rows),
            ("include_unmapped".into(), ty(LogicalTypeId::Boolean)),
            ("exclude_channels".into(), string_list),
            (
                "sum_channels".into(),
                LogicalTypeHandle::list(&LogicalTypeHandle::list(&ty(LogicalTypeId::Varchar))),
            ),
            ("driver".into(), ty(LogicalTypeId::Varchar)),
            ("vehicle".into(), ty(LogicalTypeId::Varchar)),
            ("vehicle_number".into(), ty(LogicalTypeId::Varchar)),
            ("team".into(), ty(LogicalTypeId::Varchar)),
            ("venue".into(), ty(LogicalTypeId::Varchar)),
            ("event".into(), ty(LogicalTypeId::Varchar)),
            ("session".into(), ty(LogicalTypeId::Varchar)),
            ("comment".into(), ty(LogicalTypeId::Varchar)),
            ("date".into(), ty(LogicalTypeId::Varchar)),
            ("time".into(), ty(LogicalTypeId::Varchar)),
        ])
    }
}

#[cfg_attr(
    not(target_os = "emscripten"),
    duckdb_entrypoint_c_api(ext_name = "motorsport_telemetry", min_duckdb_version = "v1.2.0")
)]
#[cfg_attr(
    target_os = "emscripten",
    duckdb_entrypoint_c_api(ext_name = "motorsport_telemetry", min_duckdb_version = "v1.2.0")
)]
pub fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    con.register_table_function::<ChannelsVTab>("telemetry_metadata")?;
    con.register_table_function::<SessionMetadataVTab>("telemetry_session_metadata")?;
    con.register_table_function::<FileMetadataVTab>("telemetry_file_metadata")?;
    con.register_table_function::<SamplesVTab>("telemetry_samples")?;
    con.register_table_function_with_extra_info::<WideVTab, _>(
        "read_telemetry",
        &ReaderConfig {
            format: None,
            session: false,
        },
    )?;
    con.register_table_function_with_extra_info::<WideVTab, _>(
        "read_telemetry_session",
        &ReaderConfig {
            format: None,
            session: true,
        },
    )?;
    con.register_table_function_with_extra_info::<WideVTab, _>(
        "read_aim",
        &ReaderConfig {
            format: Some("aimd"),
            session: false,
        },
    )?;
    con.register_table_function_with_extra_info::<WideVTab, _>(
        "read_aimd",
        &ReaderConfig {
            format: Some("aimd"),
            session: false,
        },
    )?;
    con.register_table_function_with_extra_info::<WideVTab, _>(
        "read_cosworth",
        &ReaderConfig {
            format: Some("pds"),
            session: false,
        },
    )?;
    con.register_table_function_with_extra_info::<WideVTab, _>(
        "read_motec",
        &ReaderConfig {
            format: Some("motec"),
            session: false,
        },
    )?;
    con.register_table_function_with_extra_info::<WideVTab, _>(
        "read_vbo",
        &ReaderConfig {
            format: Some("vbo"),
            session: false,
        },
    )?;
    con.register_table_function::<WriteVTab>("write_telemetry")?;
    con.register_table_function::<CommentsVTab>("telemetry_column_comments")?;
    con.register_table_function::<unit_sql::UnitsVTab>("telemetry_units")?;
    con.register_scalar_function::<unit_sql::ConvertScalar>("telemetry_convert")?;
    con.register_scalar_function::<unit_sql::ConvertColumnScalar>("telemetry_convert_column")?;
    con.register_scalar_function::<unit_sql::CanConvertScalar>("telemetry_can_convert")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use motorsport_telemetry_core::Chunk;

    struct Synthetic {
        channels: Vec<Channel>,
        values: Vec<Vec<Vec<f64>>>,
    }

    impl TelemetrySource for Synthetic {
        fn path(&self) -> &str {
            "synthetic"
        }
        fn format(&self) -> &'static str {
            "synthetic"
        }
        fn channels(&self) -> &[Channel] {
            &self.channels
        }
        fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
            self.values[channel_index][chunk_index][local_index as usize]
        }
    }

    /// A single-chunk channel at 1 Hz holding `values`, named/unit-tagged later.
    fn named(name: &str, unit: &str, values: &[f64]) -> (Channel, Vec<Vec<f64>>) {
        let count = values.len() as u64;
        (
            Channel {
                id: 1,
                name: name.into(),
                unit: unit.into(),
                unit_source: if unit.is_empty() {
                    UnitSource::Unknown
                } else {
                    UnitSource::Declared
                },
                sample_type: SampleType::F64,
                chunks: vec![Chunk {
                    sample_period_ns: 1_000_000_000,
                    sample_count: count,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
                sample_count: count,
                duration_ns: count * 1_000_000_000,
            },
            vec![values.to_vec()],
        )
    }

    fn source(parts: Vec<(Channel, Vec<Vec<f64>>)>) -> SourceRef {
        let (channels, values) = parts.into_iter().unzip();
        Arc::new(Synthetic { channels, values })
    }

    // ── affine_conversion ────────────────────────────────────────────

    #[test]
    fn affine_conversion_reproduces_units_convert_exactly() {
        // The writer applies decode's projected value as `v * scale + offset`;
        // that must equal `units::convert(v, from, to)` for every value, or
        // projected exports would drift from the truthful conversion.
        for (from, to) in [
            ("m/s", "km/h"),
            ("km/h", "m/s"),
            ("°C", "°F"),
            ("°F", "°C"),
            ("Pa", "bar"),
            ("m", "mm"),
        ] {
            let (scale, offset) = affine_conversion(from, to).unwrap();
            for value in [-100.0, -12.5, 0.0, 0.2, 100.0, 273.15, 1e3] {
                let linear = value * scale + offset;
                let conv = units::convert(value, from, to).unwrap();
                assert!(
                    (linear - conv).abs() <= 1e-9 * (1.0 + conv.abs()),
                    "{from}->{to} at {value}: linear {linear} vs convert {conv}"
                );
            }
        }
    }

    #[test]
    fn affine_conversion_is_identity_for_same_unit_and_spellings() {
        assert_eq!(affine_conversion("m/s", "m/s").unwrap(), (1.0, 0.0));
        // Case-insensitive.
        assert_eq!(affine_conversion("M/S", "m/s").unwrap(), (1.0, 0.0));
        // Alias vs canonical of the same unit.
        assert_eq!(affine_conversion("°C", "Celsius").unwrap(), (1.0, 0.0));
    }

    #[test]
    fn affine_conversion_rejects_unknown_and_mismatched_units() {
        assert!(affine_conversion("m/s", "nonexistent").is_err());
        assert!(affine_conversion("nonexistent", "m/s").is_err());
        // Different dimensions (speed vs pressure) must not silently convert.
        assert!(affine_conversion("m/s", "bar").is_err());
    }

    // ── ExportSource ─────────────────────────────────────────────────

    #[test]
    fn direct_mapping_renames_converts_and_follows_unity() {
        let (mut spd, sv) = named("Speed", "m/s", &[10.0, 20.0]);
        spd.sample_type = SampleType::I32; // integer raw storage stays integer-ish
        let src = source(vec![(spd, sv)]);
        let mapping = vec![vec!["Speed".into(), "Ground Speed".into(), "km/h".into()]];
        let ext = ExportSource::new(Arc::clone(&src), &mapping, &[], false, &[]).unwrap();
        assert_eq!(ext.channels().len(), 1);
        let ch = &ext.channels()[0];
        assert_eq!(ch.name, "Ground Speed");
        assert_eq!(ch.unit, "km/h");
        assert_eq!(ch.unit_source, UnitSource::Declared);
        // A non-identity conversion forces f64 so the scale cannot truncate.
        assert_eq!(ch.sample_type, SampleType::F64);
        assert_eq!(ext.decode(0, 0, 0), 36.0);
        assert_eq!(ext.decode(0, 0, 1), 72.0);
    }

    #[test]
    fn sum_channel_adds_sources_and_converts_the_total() {
        let (mut l, lv) = named("Left", "m/s", &[1.0, 2.0]);
        l.unit = "m/s".into();
        l.unit_source = UnitSource::Declared;
        let (mut r, rv) = named("Right", "m/s", &[3.0, 4.0]);
        r.unit = "m/s".into();
        r.unit_source = UnitSource::Declared;
        let src = source(vec![(l, lv), (r, rv)]);
        let sums = vec![vec![
            "Left".into(),
            "Right".into(),
            "Total".into(),
            "km/h".into(),
        ]];
        let ext = ExportSource::new(Arc::clone(&src), &[], &sums, false, &[]).unwrap();
        assert_eq!(ext.channels().len(), 1);
        assert_eq!(ext.channels()[0].name, "Total");
        assert_eq!(ext.channels()[0].unit, "km/h");
        assert_eq!(ext.channels()[0].sample_type, SampleType::F64);
        // (1+3) m/s -> 14.4 km/h; (2+4) m/s -> 21.6 km/h.
        assert!((ext.decode(0, 0, 0) - 14.4).abs() < 1e-9);
        assert!((ext.decode(0, 0, 1) - 21.6).abs() < 1e-9);
    }

    #[test]
    fn channels_and_exports_stay_aligned_under_mixed_projection() {
        let mk = |name: &str, unit: &str, vals: &[f64]| named(name, unit, vals);
        let src = source(vec![
            mk("Speed", "m/s", &[10.0, 20.0]),
            mk("Steer", "rad", &[0.5, 1.0]),
            mk("T1", "K", &[300.0, 310.0]),
            mk("T2", "K", &[260.0, 270.0]),
        ]);
        let mapping = vec![vec!["Speed".into(), "Ground Speed".into(), "km/h".into()]];
        let sums = vec![vec!["T1".into(), "T2".into(), "Tsum".into(), "°C".into()]];
        let ext = ExportSource::new(Arc::clone(&src), &mapping, &sums, true, &[]).unwrap();
        let names: Vec<_> = ext.channels().iter().map(|c| c.name.as_str()).collect();
        // Mappings first, then unmapped (Steer, T1, T2), then sums.
        assert_eq!(names, vec!["Ground Speed", "Steer", "T1", "T2", "Tsum"]);

        // Each exported index must decode against its own export entry, proving
        // the channels[]/exports[] vectors never drift out of alignment.
        assert_eq!(ext.decode(0, 0, 0), 36.0); // Speed converted
        assert_eq!(ext.decode(1, 0, 0), 0.5); // Steer passthrough
        assert_eq!(ext.decode(2, 0, 0), 300.0); // T1 passthrough
        assert_eq!(ext.decode(3, 0, 0), 260.0); // T2 passthrough
        let expected = units::convert(560.0, "K", "°C").unwrap();
        assert!((ext.decode(4, 0, 0) - expected).abs() < 1e-9); // (T1+T2)
    }

    #[test]
    fn empty_projection_passes_every_channel_through_untouched() {
        let src = source(vec![
            named("A", "m/s", &[1.0, 2.0]),
            named("B", "m/s", &[3.0, 4.0]),
        ]);
        let ext = ExportSource::new(Arc::clone(&src), &[], &[], false, &[]).unwrap();
        assert_eq!(ext.channels().len(), 2);
        assert_eq!(ext.channels()[0].name, "A");
        assert_eq!(ext.channels()[1].unit, "m/s");
        assert_eq!(ext.channels()[1].unit_source, UnitSource::Declared);
        assert_eq!(ext.decode(0, 0, 0), 1.0);
        assert_eq!(ext.decode(1, 0, 1), 4.0);
    }

    #[test]
    fn include_unmapped_and_exclude_control_the_projection() {
        let src = source(vec![
            named("A", "m/s", &[1.0, 2.0]),
            named("B", "m/s", &[3.0, 4.0]),
            named("C", "m/s", &[5.0, 6.0]),
        ]);
        let mapping = vec![vec!["A".into(), "X".into(), "m/s".into()]];

        let only_mapped = ExportSource::new(Arc::clone(&src), &mapping, &[], false, &[]).unwrap();
        let names: Vec<_> = only_mapped
            .channels()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["X"]);

        let all = ExportSource::new(Arc::clone(&src), &mapping, &[], true, &[]).unwrap();
        let names: Vec<_> = all.channels().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["X", "B", "C"]);

        // Exclusion is case-insensitive and drops the unmapped channel only.
        let excluded =
            ExportSource::new(Arc::clone(&src), &mapping, &[], true, &["b".into()]).unwrap();
        let names: Vec<_> = excluded
            .channels()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["X", "C"]);
    }

    #[test]
    fn export_rejects_bad_projections() {
        let (mut x, xv) = named("A", "m/s", &[1.0, 2.0]);
        x.unit_source = UnitSource::Declared;
        let (mut y, yv) = named("B", "m/s", &[3.0, 4.0]);
        y.unit_source = UnitSource::Declared;
        let (mut z, zv) = named("C", "kg", &[5.0, 6.0]);
        z.unit_source = UnitSource::Declared;
        let src = source(vec![
            (x.clone(), xv.clone()),
            (y.clone(), yv.clone()),
            (z.clone(), zv.clone()),
        ]);

        // Unknown source channel.
        assert!(ExportSource::new(
            Arc::clone(&src),
            &[vec!["Nope".into(), "X".into(), "m/s".into()]],
            &[],
            false,
            &[]
        )
        .is_err());
        // Empty target name.
        assert!(ExportSource::new(
            Arc::clone(&src),
            &[vec!["A".into(), "  ".into(), "m/s".into()]],
            &[],
            false,
            &[]
        )
        .is_err());
        // Duplicate target names, even under different casing.
        assert!(ExportSource::new(
            Arc::clone(&src),
            &[
                vec!["A".into(), "X".into(), "m/s".into()],
                vec!["B".into(), "x".into(), "m/s".into()]
            ],
            &[],
            false,
            &[]
        )
        .is_err());
        // Mapping across incompatible dimensions.
        assert!(ExportSource::new(
            Arc::clone(&src),
            &[vec!["A".into(), "X".into(), "kg".into()]],
            &[],
            false,
            &[]
        )
        .is_err());
        // Sum sources on different sample clocks.
        let (mut slow, slowv) = named("Slow", "m/s", &[7.0, 8.0]);
        slow.chunks[0].sample_period_ns = 2_000_000_000;
        slow.unit_source = UnitSource::Declared;
        let src2 = source(vec![(x.clone(), xv.clone()), (slow, slowv)]);
        assert!(ExportSource::new(
            Arc::clone(&src2),
            &[],
            &[vec!["A".into(), "Slow".into(), "Sum".into(), "m/s".into()]],
            false,
            &[]
        )
        .is_err());
        // Sum sources with incompatible dimensions.
        let src3 = source(vec![(x.clone(), xv.clone()), (z.clone(), zv.clone())]);
        assert!(ExportSource::new(
            Arc::clone(&src3),
            &[],
            &[vec!["A".into(), "C".into(), "Sum".into(), "m/s".into()]],
            false,
            &[]
        )
        .is_err());
    }

    #[test]
    fn sum_with_identical_units_and_clocks_needs_no_conversion() {
        let (mut l, lv) = named("L", "m/s", &[1.0, 2.0]);
        l.unit_source = UnitSource::Declared;
        let (mut r, rv) = named("R", "m/s", &[3.0, 4.0]);
        r.unit_source = UnitSource::Declared;
        let src = source(vec![(l, lv), (r, rv)]);
        let sums = vec![vec!["L".into(), "R".into(), "LR".into(), "m/s".into()]];
        let ext = ExportSource::new(Arc::clone(&src), &[], &sums, false, &[]).unwrap();
        let ch = &ext.channels()[0];
        assert_eq!(ch.unit, "m/s");
        assert_eq!(ch.unit_source, UnitSource::Declared);
        assert_eq!(ext.decode(0, 0, 0), 4.0);
    }
}
