mod parser;

use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use duckdb_loadable_macros::duckdb_entrypoint_c_api;
use libduckdb_sys as ffi;
use parser::{open_paths, parse_channel_filter, PdsFile, TICK_NS};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const VECTOR_SIZE: u64 = 2048;
const SAMPLES_COLUMN_COUNT: u64 = 8;
const CHANNELS_COLUMN_COUNT: u64 = 11;

fn ty(id: LogicalTypeId) -> LogicalTypeHandle {
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

// ── pds_samples ─────────────────────────────────────────────────────

struct SamplesBind {
    files: Vec<Arc<PdsFile>>,
    channel_filter: HashSet<String>,
    start_ns: u64,
    end_ns: u64,
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
        let files = open_paths(&pattern)?;
        let filter_value = named_string(bind, "channel");
        let channel_filter = parse_channel_filter(filter_value.as_deref());
        if !channel_filter.is_empty() {
            let found = files
                .iter()
                .flat_map(|file| &file.channels)
                .map(|channel| channel.name.to_ascii_lowercase())
                .collect::<HashSet<_>>();
            let missing = channel_filter
                .difference(&found)
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(format!("PDS channel(s) not found: {}", missing.join(", ")).into());
            }
        }
        let start_ns = named_i64(bind, "start_ns").unwrap_or(0).max(0) as u64;
        let end_ns = named_i64(bind, "end_ns").unwrap_or(i64::MAX).max(0) as u64;
        if end_ns < start_ns {
            return Err("end_ns must be greater than or equal to start_ns".into());
        }

        bind.add_result_column("file", ty(LogicalTypeId::Varchar));
        bind.add_result_column("channel_id", ty(LogicalTypeId::UInteger));
        bind.add_result_column("channel", ty(LogicalTypeId::Varchar));
        bind.add_result_column("unit", ty(LogicalTypeId::Varchar));
        bind.add_result_column("frequency_hz", ty(LogicalTypeId::Double));
        bind.add_result_column("sample_index", ty(LogicalTypeId::UBigint));
        bind.add_result_column("time_ns", ty(LogicalTypeId::Bigint));
        bind.add_result_column("value", ty(LogicalTypeId::Double));

        let cardinality = files
            .iter()
            .flat_map(|f| &f.channels)
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
        })
    }

    fn init(init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        let bind = unsafe { &*init.get_bind_data::<SamplesBind>() };
        let mut segments = Vec::new();
        for (file_idx, file) in bind.files.iter().enumerate() {
            for (channel_idx, channel) in file.channels.iter().enumerate() {
                if !bind.channel_filter.is_empty()
                    && !bind
                        .channel_filter
                        .contains(&channel.name.to_ascii_lowercase())
                {
                    continue;
                }
                for (chunk_idx, chunk) in channel.chunks.iter().enumerate() {
                    let period_ns = chunk.sample_period_ticks as u64 * TICK_NS;
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
        let channel = &file.channels[segment.channel];
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
                        v.insert(row, file.path.as_str());
                    }
                }
                1 => output.flat_vector(out_col).as_mut_slice::<u32>()[..n].fill(channel.id),
                2 => {
                    let v = output.flat_vector(out_col);
                    for row in 0..n {
                        v.insert(row, channel.name.as_str());
                    }
                }
                3 => {
                    let v = output.flat_vector(out_col);
                    for row in 0..n {
                        v.insert(row, channel.unit.as_str());
                    }
                }
                4 => output.flat_vector(out_col).as_mut_slice::<f64>()[..n]
                    .fill(10_000_000.0 / chunk.sample_period_ticks as f64),
                5 => {
                    let mut vector = output.flat_vector(out_col);
                    let dst = &mut vector.as_mut_slice::<u64>()[..n];
                    for (row, value) in dst.iter_mut().enumerate() {
                        *value = chunk.sample_base + segment.local_start + row as u64;
                    }
                }
                6 => {
                    let mut vector = output.flat_vector(out_col);
                    let dst = &mut vector.as_mut_slice::<i64>()[..n];
                    let period = chunk.sample_period_ticks as u64 * TICK_NS;
                    for (row, value) in dst.iter_mut().enumerate() {
                        *value = (chunk.time_base_ns + (segment.local_start + row as u64) * period)
                            as i64;
                    }
                }
                7 => {
                    let mut vector = output.flat_vector(out_col);
                    let dst = &mut vector.as_mut_slice::<f64>()[..n];
                    for (row, value) in dst.iter_mut().enumerate() {
                        *value = file.decode(channel, chunk, segment.local_start + row as u64);
                    }
                }
                _ => output.flat_vector(out_col).as_mut_slice::<i64>()[..n].fill(index as i64),
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
        ])
    }
}

// ── pds_channels ────────────────────────────────────────────────────

struct ChannelsBind {
    files: Vec<Arc<PdsFile>>,
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
        let files = open_paths(&bind.get_parameter(0).to_string())?;
        for (name, logical) in [
            ("file", LogicalTypeId::Varchar),
            ("channel_id", LogicalTypeId::UInteger),
            ("name", LogicalTypeId::Varchar),
            ("unit", LogicalTypeId::Varchar),
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
            .flat_map(|(fi, f)| (0..f.channels.len()).map(move |ci| (fi, ci)))
            .collect::<Vec<_>>();
        bind.set_cardinality(rows.len() as u64, true);
        Ok(ChannelsBind { files, rows })
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
                let channel = &file.channels[ci];
                match original {
                    0 => vector.insert(row, file.path.as_str()),
                    1 => vector.as_mut_slice::<u32>()[row] = channel.id,
                    2 => vector.insert(row, channel.name.as_str()),
                    3 => vector.insert(row, channel.unit.as_str()),
                    4 => vector.as_mut_slice::<u32>()[row] = channel.sample_type.code(),
                    5 => vector.insert(row, channel.sample_type.name()),
                    6 => {
                        if let Some(value) = channel.frequency_hz() {
                            vector.as_mut_slice::<f64>()[row] = value
                        } else {
                            vector.set_null(row)
                        }
                    }
                    7 => {
                        if let Some(ticks) = channel.first_period_ticks() {
                            vector.as_mut_slice::<u64>()[row] = ticks as u64 * TICK_NS
                        } else {
                            vector.set_null(row)
                        }
                    }
                    8 => vector.as_mut_slice::<u64>()[row] = channel.sample_count,
                    9 => vector.as_mut_slice::<u64>()[row] = channel.chunks.len() as u64,
                    10 => vector.as_mut_slice::<u64>()[row] = channel.duration_ns,
                    _ => vector.as_mut_slice::<i64>()[row] = (start + row) as i64,
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

// ── read_pds: projected, resampled wide relation ────────────────────

struct WideBind {
    files: Vec<Arc<PdsFile>>,
    names: Vec<String>,
    // [file][wide channel column] -> source channel index
    source_channels: Vec<Vec<Option<usize>>>,
    ranges: Vec<(u64, u64, u64)>, // start_ns, end_ns, row_count
    rate: u64,
    linear: bool,
    include_filename: bool,
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
        let files = open_paths(&bind.get_parameter(0).to_string())?;
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

        let mut names = Vec::new();
        let mut keys = HashSet::new();
        for file in &files {
            for channel in &file.channels {
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
                return Err(format!("PDS channel(s) not found: {}", missing.join(", ")).into());
            }
        }

        if include_filename {
            bind.add_result_column("filename", ty(LogicalTypeId::Varchar));
        }
        bind.add_result_column("time_ns", ty(LogicalTypeId::Bigint));
        for name in &names {
            bind.add_result_column(name, ty(LogicalTypeId::Double));
        }

        let source_channels = files
            .iter()
            .map(|file| {
                let map = file
                    .channels
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

        let mut ranges = Vec::with_capacity(files.len());
        let mut total_rows = 0u64;
        for (fi, file) in files.iter().enumerate() {
            let duration = source_channels[fi]
                .iter()
                .flatten()
                .map(|&ci| file.channels[ci].duration_ns)
                .max()
                .unwrap_or(0);
            let end = requested_end.min(duration);
            let rows = if end <= start_ns {
                0
            } else {
                ((end - start_ns) as u128 * rate as u128).div_ceil(1_000_000_000) as u64
            };
            ranges.push((start_ns, end, rows));
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
        let fixed_columns = 1 + usize::from(bind.include_filename);
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
            let time_column = u64::from(bind.include_filename);
            let channel_offset = time_column + 1;
            match original {
                0 if bind.include_filename => {
                    let vector = output.flat_vector(out_col);
                    for row in 0..n {
                        vector.insert(row, file.path.as_str());
                    }
                }
                col if col == time_column => {
                    let mut vector = output.flat_vector(out_col);
                    let dst = &mut vector.as_mut_slice::<i64>()[..n];
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
                        let channel = &file.channels[channel_idx];
                        for row in 0..n {
                            let source_row = segment.row_start + row as u64;
                            let time_ns = (start_ns as u128
                                + source_row as u128 * 1_000_000_000u128 / bind.rate as u128)
                                as u64;
                            if let Some(value) = file.sample_at(channel, time_ns, bind.linear) {
                                vector.as_mut_slice::<f64>()[row] = value;
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
                _ => {
                    output.flat_vector(out_col).as_mut_slice::<i64>()[..n].fill(segment_idx as i64)
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
            ("rate".into(), ty(LogicalTypeId::Bigint)),
            ("channels".into(), ty(LogicalTypeId::Varchar)),
            ("start_ns".into(), ty(LogicalTypeId::Bigint)),
            ("end_ns".into(), ty(LogicalTypeId::Bigint)),
            ("interpolate".into(), ty(LogicalTypeId::Varchar)),
            ("filename".into(), ty(LogicalTypeId::Boolean)),
            ("add_filename_as_column".into(), ty(LogicalTypeId::Boolean)),
        ])
    }
}

#[duckdb_entrypoint_c_api(ext_name = "pds", min_duckdb_version = "v1.2.0")]
pub fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    con.register_table_function::<ChannelsVTab>("pds_metadata")?;
    con.register_table_function::<ChannelsVTab>("pds_channels")?;
    con.register_table_function::<SamplesVTab>("pds_samples")?;
    con.register_table_function::<WideVTab>("read_pds")?;
    Ok(())
}
