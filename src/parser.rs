use glob::glob;
use memmap2::Mmap;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

pub const TICK_NS: u64 = 100;
const MARKER: u64 = 0x7c72;

#[derive(Debug, Error)]
pub enum PdsError {
    #[error("I/O error for {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid glob {pattern}: {source}")]
    Glob {
        pattern: String,
        source: glob::PatternError,
    },
    #[error("no files matched {0}")]
    NoFiles(String),
    #[error("invalid PDS file {path}: {message}")]
    Invalid { path: String, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleType {
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl SampleType {
    pub fn from_code(code: u32) -> Self {
        match code {
            1 => Self::U8,
            2 => Self::I16,
            3 => Self::U16,
            4 => Self::I32,
            5 => Self::U32,
            7 => Self::F64,
            _ => Self::F32,
        }
    }

    pub fn code(self) -> u32 {
        match self {
            Self::U8 => 1,
            Self::I16 => 2,
            Self::U16 => 3,
            Self::I32 => 4,
            Self::U32 => 5,
            Self::F32 => 6,
            Self::F64 => 7,
        }
    }

    pub fn byte_width(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::U8 => "uint8",
            Self::I16 => "int16",
            Self::U16 => "uint16",
            Self::I32 => "int32",
            Self::U32 => "uint32",
            Self::F32 => "float32",
            Self::F64 => "float64",
        }
    }

    pub fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub sample_period_ticks: u32,
    pub sample_count: u64,
    pub data_ptr: u64,
    pub sample_base: u64,
    pub time_base_ns: u64,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub id: u32,
    pub name: String,
    pub unit: String,
    pub sample_type: SampleType,
    pub chunks: Vec<Chunk>,
    pub sample_count: u64,
    pub duration_ns: u64,
}

impl Channel {
    pub fn first_period_ticks(&self) -> Option<u32> {
        self.chunks.first().map(|c| c.sample_period_ticks)
    }

    pub fn frequency_hz(&self) -> Option<f64> {
        self.first_period_ticks().map(|p| 10_000_000.0 / p as f64)
    }
}

#[derive(Debug)]
pub struct PdsFile {
    pub path: String,
    pub channels: Vec<Channel>,
    data: Mmap,
}

#[derive(Clone, Copy)]
struct DirEntry {
    offset: u64,
    count: u32,
    class_b: u32,
    next_count: u32,
}

#[derive(Clone, Copy)]
struct Layout {
    defs_offset: usize,
    defs_count: usize,
    chunk_offset: usize,
    next_offset: usize,
    chunk_count: usize,
}

#[derive(Clone)]
struct ChannelDef {
    id: u32,
    name: String,
    unit: String,
    sample_type: SampleType,
}

#[derive(Clone, Copy)]
struct RawChunk {
    channel_id: u32,
    sample_period_ticks: u32,
    sample_count: u64,
    data_ptr: u64,
}

fn invalid(path: &str, message: impl Into<String>) -> PdsError {
    PdsError::Invalid {
        path: path.to_owned(),
        message: message.into(),
    }
}

fn u16le(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn u32le(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn u64le(data: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        data.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn utf16le(data: &[u8], offset: usize, max_bytes: usize) -> String {
    let end = offset.saturating_add(max_bytes).min(data.len());
    let mut units = Vec::with_capacity((end.saturating_sub(offset)) / 2);
    let mut pos = offset;
    while pos + 1 < end {
        let code = u16le(data, pos).unwrap_or(0);
        if code == 0 {
            break;
        }
        units.push(code);
        pos += 2;
    }
    String::from_utf16_lossy(&units).trim().to_owned()
}

fn infer_unit(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    if n.contains("speed") || n.contains("vel") {
        "m/s".into()
    } else if n.contains("steer") {
        "deg".into()
    } else if n.contains("accel") {
        "g".into()
    } else if n.contains("damper") {
        "mm".into()
    } else if (n.contains("brake") && n.contains("press"))
        || n.contains("p_f_brake")
        || n.contains("p_r_brake")
        || n.contains("p_tyre")
        || n.contains("tire") && n.contains("press")
        || n.contains("tyre") && n.contains("press")
    {
        "pa".into()
    } else {
        String::new()
    }
}

fn read_entries_at(data: &[u8], start: usize) -> Vec<DirEntry> {
    (0..20)
        .filter_map(|i| {
            let base = start + i * 32;
            if base + 32 > data.len() {
                return None;
            }
            let lo = u32le(data, base)? as u64;
            let hi = u32le(data, base + 4)? as u64;
            Some(DirEntry {
                offset: lo | hi << 32,
                count: u32le(data, base + 8)?,
                class_b: u32le(data, base + 0x14)?,
                next_count: u32le(data, base + 0x18)?,
            })
        })
        .collect()
}

fn find_directory(data: &[u8]) -> Vec<DirEntry> {
    let mut best = Vec::new();
    let mut best_score = i32::MIN;
    for start in [0x80, 0x78, 0x70, 0x68, 0x60, 0x58, 0x50, 0x48, 0x40] {
        let entries = read_entries_at(data, start);
        let score = entries
            .iter()
            .map(|e| {
                if e.class_b <= 3 && e.offset > 0 && e.offset < data.len() as u64 {
                    2
                } else if e.class_b <= 3 {
                    1
                } else {
                    0
                }
            })
            .sum();
        if score > best_score {
            best = entries;
            best_score = score;
        }
    }
    best
}

fn find_layout(entries: &[DirEntry], file_size: usize, path: &str) -> Result<Layout, PdsError> {
    for window in entries.windows(3) {
        let defs = window[0];
        let chunks = window[1];
        let next = window[2];
        if !(defs.offset < chunks.offset
            && chunks.offset < next.offset
            && next.offset <= file_size as u64)
            || defs.class_b != 1
            || defs.count == 0
        {
            continue;
        }
        let span = (next.offset - chunks.offset) as usize;
        let plausible = |count: u32| -> Option<usize> {
            if count == 0 || span % count as usize != 0 {
                return None;
            }
            let width = span / count as usize;
            (48..=512).contains(&width).then_some(count as usize)
        };
        let chunk_count = plausible(defs.next_count).or_else(|| plausible(chunks.count));
        if let Some(chunk_count) = chunk_count {
            return Ok(Layout {
                defs_offset: defs.offset as usize,
                defs_count: defs.count as usize,
                chunk_offset: chunks.offset as usize,
                next_offset: next.offset as usize,
                chunk_count,
            });
        }
    }
    Err(invalid(path, "no valid definitions/chunk layout found"))
}

fn marker_defs(data: &[u8], layout: Layout) -> Vec<ChannelDef> {
    let scan_end = layout
        .chunk_offset
        .min(layout.defs_offset.saturating_add(8192))
        .min(data.len());
    let marker_pos = (layout.defs_offset..scan_end.saturating_sub(7))
        .step_by(2)
        .find(|&pos| u64le(data, pos) == Some(MARKER));
    let Some(first) = marker_pos else {
        return Vec::new();
    };
    let probe_end = layout.chunk_offset.min(first + 1024).min(data.len());
    let record_size = ((first + 16)..probe_end.saturating_sub(7))
        .step_by(2)
        .find(|&pos| u64le(data, pos) == Some(MARKER))
        .map(|pos| pos - first)
        .unwrap_or(304);
    if record_size < 0xdc {
        return Vec::new();
    }

    let mut defs = Vec::new();
    let mut pos = first;
    while pos + 0xdc <= layout.chunk_offset.min(data.len()) {
        if u64le(data, pos) == Some(MARKER) {
            let id = u32le(data, pos + 8).unwrap_or(0);
            let name = utf16le(data, pos + 0x10, 112);
            if id != 0 && !name.is_empty() {
                let raw_unit = utf16le(data, pos + 0x98, 32);
                let unit = if raw_unit.is_empty() {
                    infer_unit(&name)
                } else {
                    raw_unit
                };
                defs.push(ChannelDef {
                    id,
                    name,
                    unit,
                    sample_type: SampleType::from_code(u32le(data, pos + 0xd8).unwrap_or(6)),
                });
            }
        }
        pos += record_size;
    }
    defs
}

fn markerless_defs(data: &[u8], layout: Layout, is_export: bool) -> Vec<ChannelDef> {
    if layout.defs_count == 0 {
        return Vec::new();
    }
    let span = layout.chunk_offset.saturating_sub(layout.defs_offset);
    let record_size = span / layout.defs_count;
    if !(100..=1024).contains(&record_size) {
        return Vec::new();
    }
    let mut defs = Vec::new();
    for i in 0..layout.defs_count {
        let pos = layout.defs_offset + i * record_size;
        if pos + 16 > data.len() {
            break;
        }
        let id = u32le(data, pos).unwrap_or(0);
        let name = utf16le(data, pos + 8, 112);
        if name.is_empty() {
            continue;
        }
        let raw_unit = if record_size >= 0xb8 {
            utf16le(data, pos + 0x98, 32)
        } else {
            String::new()
        };
        let unit = if raw_unit.is_empty() {
            infer_unit(&name)
        } else {
            raw_unit
        };
        let code = if !is_export && record_size >= 0xd4 {
            u32le(data, pos + 0xd0)
                .filter(|v| (1..=7).contains(v))
                .unwrap_or(7)
        } else {
            7
        };
        defs.push(ChannelDef {
            id,
            name,
            unit,
            sample_type: SampleType::from_code(code),
        });
    }
    defs
}

fn parse_chunks(data: &[u8], layout: Layout, is_export: bool) -> Vec<RawChunk> {
    let span = layout.next_offset - layout.chunk_offset;
    let width = span / layout.chunk_count;
    if width < 0x3c {
        return Vec::new();
    }

    let aligned = (0..4096.min(span))
        .step_by(4)
        .map(|n| layout.chunk_offset + n)
        .find(|&pos| {
            pos + 0x3c <= data.len()
                && u32le(data, pos + 4).unwrap_or(0) > 0
                && u32le(data, pos + 4) == u32le(data, pos + 8)
                && u32le(data, pos + 0x1c).unwrap_or(0) > 0
        });
    if let Some(mut start) = aligned {
        if is_export {
            while start >= layout.chunk_offset + width {
                let p = start - width;
                if u32le(data, p + 4) == u32le(data, p + 8)
                    && u32le(data, p + 0x1c).unwrap_or(0) > 0
                {
                    start = p;
                } else {
                    break;
                }
            }
        }
        let count = layout.chunk_count.min((layout.next_offset - start) / width);
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let pos = start + i * width;
            let channel_id = u32le(data, pos + 4).unwrap_or(0);
            let duplicate = u32le(data, pos + 8).unwrap_or(u32::MAX);
            let period = u32le(data, pos + 0x18).unwrap_or(0);
            let count = u32le(data, pos + 0x1c).unwrap_or(0) as u64;
            let ptr = u32le(data, pos + 0x38).unwrap_or(u32::MAX) as u64;
            if channel_id == duplicate
                && (channel_id > 0 || is_export)
                && period > 0
                && count > 0
                && ptr < data.len() as u64
            {
                out.push(RawChunk {
                    channel_id,
                    sample_period_ticks: period,
                    sample_count: count,
                    data_ptr: ptr,
                });
            }
        }
        if !out.is_empty() {
            return out;
        }
    }

    // Compact Pi Toolbox exports omit the duplicate channel id. Their chunk
    // directory is channel-interleaved in definition order.
    let mut out = Vec::new();
    for i in 0..layout.chunk_count {
        let pos = layout.chunk_offset + i * width;
        if pos + width > data.len() {
            break;
        }
        let period = u32le(data, pos + 0x18).unwrap_or(0);
        let count = u32le(data, pos + 0x1c).unwrap_or(0) as u64;
        let ptr = u32le(data, pos + 0x38).unwrap_or(0) as u64;
        if period > 0 && count > 0 && ptr > 0 && ptr < data.len() as u64 {
            out.push(RawChunk {
                channel_id: (i % layout.defs_count.max(1)) as u32,
                sample_period_ticks: period,
                sample_count: count,
                data_ptr: ptr,
            });
        }
    }
    out
}

impl PdsFile {
    pub fn open(path: &Path) -> Result<Arc<Self>, PdsError> {
        let display = path.to_string_lossy().into_owned();
        let file = File::open(path).map_err(|source| PdsError::Io {
            path: display.clone(),
            source,
        })?;
        let data = unsafe { Mmap::map(&file) }.map_err(|source| PdsError::Io {
            path: display.clone(),
            source,
        })?;
        if data.len() < 0x100 {
            return Err(invalid(&display, "file is smaller than 256 bytes"));
        }
        let entries = find_directory(&data);
        if entries.len() < 3 {
            return Err(invalid(&display, "directory has fewer than three entries"));
        }
        let layout = find_layout(&entries, data.len(), &display)?;
        let marked = marker_defs(&data, layout);
        let is_export = marked.is_empty() && layout.defs_count <= 200;
        let defs = if marked.is_empty() {
            markerless_defs(&data, layout, is_export)
        } else {
            marked
        };
        if defs.is_empty() {
            return Err(invalid(&display, "no channel definitions found"));
        }
        let raw_chunks = parse_chunks(&data, layout, is_export);

        let mut grouped: HashMap<u32, Vec<RawChunk>> = HashMap::new();
        for chunk in raw_chunks {
            grouped.entry(chunk.channel_id).or_default().push(chunk);
        }
        let mut channels = Vec::with_capacity(defs.len());
        for def in defs {
            let mut chunks = Vec::new();
            let mut sample_base = 0u64;
            let mut time_base_ns = 0u64;
            if let Some(raw) = grouped.remove(&def.id) {
                // Deliberately preserve chunk-index table order. `order` and
                // data_ptr are not temporal keys in interrupted native logs.
                for chunk in raw {
                    let width = def.sample_type.byte_width() as u64;
                    let max_count = (data.len() as u64).saturating_sub(chunk.data_ptr) / width;
                    let count = chunk.sample_count.min(max_count);
                    if count == 0 {
                        continue;
                    }
                    chunks.push(Chunk {
                        sample_period_ticks: chunk.sample_period_ticks,
                        sample_count: count,
                        data_ptr: chunk.data_ptr,
                        sample_base,
                        time_base_ns,
                    });
                    sample_base = sample_base.saturating_add(count);
                    time_base_ns = time_base_ns.saturating_add(
                        count
                            .saturating_mul(chunk.sample_period_ticks as u64)
                            .saturating_mul(TICK_NS),
                    );
                }
            }
            channels.push(Channel {
                id: def.id,
                name: def.name,
                unit: def.unit,
                sample_type: def.sample_type,
                chunks,
                sample_count: sample_base,
                duration_ns: time_base_ns,
            });
        }
        Ok(Arc::new(Self {
            path: display,
            channels,
            data,
        }))
    }

    #[inline]
    pub fn decode(&self, channel: &Channel, chunk: &Chunk, local_index: u64) -> f64 {
        let offset =
            chunk.data_ptr as usize + local_index as usize * channel.sample_type.byte_width();
        match channel.sample_type {
            SampleType::U8 => self.data[offset] as f64,
            SampleType::I16 => {
                i16::from_le_bytes(self.data[offset..offset + 2].try_into().unwrap()) as f64
            }
            SampleType::U16 => {
                u16::from_le_bytes(self.data[offset..offset + 2].try_into().unwrap()) as f64
            }
            SampleType::I32 => {
                i32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap()) as f64
            }
            SampleType::U32 => {
                u32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap()) as f64
            }
            SampleType::F32 => {
                f32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap()) as f64
            }
            SampleType::F64 => {
                f64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap())
            }
        }
    }

    pub fn sample_at(&self, channel: &Channel, time_ns: u64, linear: bool) -> Option<f64> {
        if time_ns >= channel.duration_ns || channel.chunks.is_empty() {
            return None;
        }
        let idx = channel.chunks.partition_point(|c| {
            c.time_base_ns
                .saturating_add(c.sample_count * c.sample_period_ticks as u64 * TICK_NS)
                <= time_ns
        });
        let chunk = channel.chunks.get(idx)?;
        let period_ns = chunk.sample_period_ticks as u64 * TICK_NS;
        let relative = time_ns.saturating_sub(chunk.time_base_ns);
        let sample = (relative / period_ns).min(chunk.sample_count - 1);
        let a = self.decode(channel, chunk, sample);
        if !linear || !channel.sample_type.is_float() || sample + 1 >= chunk.sample_count {
            return Some(a);
        }
        let b = self.decode(channel, chunk, sample + 1);
        let fraction = (relative % period_ns) as f64 / period_ns as f64;
        Some(a + (b - a) * fraction)
    }
}

pub fn expand_paths(pattern: &str) -> Result<Vec<PathBuf>, PdsError> {
    let has_magic = pattern
        .as_bytes()
        .iter()
        .any(|b| matches!(b, b'*' | b'?' | b'['));
    let mut paths = if has_magic {
        glob(pattern)
            .map_err(|source| PdsError::Glob {
                pattern: pattern.into(),
                source,
            })?
            .filter_map(Result::ok)
            .filter(|p| p.is_file())
            .collect::<Vec<_>>()
    } else {
        vec![PathBuf::from(pattern)]
    };
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Err(PdsError::NoFiles(pattern.into()));
    }
    Ok(paths)
}

pub fn open_paths(pattern: &str) -> Result<Vec<Arc<PdsFile>>, PdsError> {
    expand_paths(pattern)?
        .iter()
        .map(|p| PdsFile::open(p))
        .collect()
}

pub fn parse_channel_filter(value: Option<&str>) -> HashSet<String> {
    value
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}
