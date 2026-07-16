use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use thiserror::Error;

use motorsport_telemetry_core::{Channel, Chunk, SampleType, TelemetrySource};

pub const TICK_NS: u64 = 100;
const MARKER: u64 = 0x7c72;

#[derive(Debug, Error)]
pub enum CosworthError {
    #[error("I/O error for {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid PDS file {path}: {message}")]
    Invalid { path: String, message: String },
}

#[derive(Debug)]
pub struct CosworthFile {
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

fn invalid(path: &str, message: impl Into<String>) -> CosworthError {
    CosworthError::Invalid {
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

fn find_layout(
    entries: &[DirEntry],
    file_size: usize,
    path: &str,
) -> Result<Layout, CosworthError> {
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
                    sample_type: SampleType::from_pds_code(u32le(data, pos + 0xd8).unwrap_or(6)),
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
            sample_type: SampleType::from_pds_code(code),
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

impl CosworthFile {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CosworthError> {
        let path = path.as_ref();
        let display = path.to_string_lossy().into_owned();
        let file = File::open(path).map_err(|source| CosworthError::Io {
            path: display.clone(),
            source,
        })?;
        let data = unsafe { Mmap::map(&file) }.map_err(|source| CosworthError::Io {
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
                        sample_period_ns: chunk.sample_period_ticks as u64 * TICK_NS,
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
        Ok(Self {
            path: display,
            channels,
            data,
        })
    }
}

impl TelemetrySource for CosworthFile {
    fn path(&self) -> &str {
        &self.path
    }

    fn format(&self) -> &'static str {
        "pds"
    }

    fn channels(&self) -> &[Channel] {
        &self.channels
    }

    #[inline]
    fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
        let channel = &self.channels[channel_index];
        let chunk = &channel.chunks[chunk_index];
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn u32_at(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn utf16_at(data: &mut [u8], offset: usize, value: &str) {
        for (index, unit) in value.encode_utf16().enumerate() {
            data[offset + index * 2..offset + index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
    }
    fn directory(data: &mut [u8], at: usize, offset: u32, count: u32, class_b: u32, next: u32) {
        u32_at(data, at, offset);
        u32_at(data, at + 8, count);
        u32_at(data, at + 0x10, 8);
        u32_at(data, at + 0x14, class_b);
        u32_at(data, at + 0x18, next);
    }
    fn fixture() -> tempfile::NamedTempFile {
        let mut data = vec![0u8; 0x700];
        let defs = 0x200;
        let chunks = 0x380;
        directory(&mut data, 0x80, defs, 2, 1, 4);
        directory(&mut data, 0xa0, chunks, 4, 3, 0);
        directory(&mut data, 0xc0, 0x480, 0, 1, 0);
        for (index, (id, name)) in [(1, "Speed"), (2, "Gear")].into_iter().enumerate() {
            let at = defs as usize + index * 0xc0;
            u32_at(&mut data, at, id);
            utf16_at(&mut data, at + 8, name);
        }
        for (index, (order, id, values)) in [
            (100, 1, [10.0_f64, 11.0]),
            (200, 2, [3.0, 3.0]),
            (1, 1, [12.0, 13.0]),
            (2, 2, [4.0, 4.0]),
        ]
        .into_iter()
        .enumerate()
        {
            let at = chunks as usize + index * 0x40;
            let ptr = 0x580 + index * 0x20;
            u32_at(&mut data, at, order);
            u32_at(&mut data, at + 4, id);
            u32_at(&mut data, at + 8, id);
            u32_at(&mut data, at + 0x18, 10_000_000);
            u32_at(&mut data, at + 0x1c, 2);
            u32_at(&mut data, at + 0x38, ptr as u32);
            for (sample, value) in values.into_iter().enumerate() {
                data[ptr + sample * 8..ptr + sample * 8 + 8].copy_from_slice(&value.to_le_bytes());
            }
        }
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&data).unwrap();
        file
    }

    #[test]
    fn preserves_chunk_table_order_and_interpolates() {
        let fixture = fixture();
        let file = CosworthFile::open(fixture.path()).unwrap();
        assert_eq!(file.channels.len(), 2);
        assert_eq!(file.channels[0].sample_count, 4);
        let values = (0..4)
            .map(|index| file.decode(0, usize::from(index >= 2), index % 2))
            .collect::<Vec<_>>();
        assert_eq!(values, [10.0, 11.0, 12.0, 13.0]);
        assert_eq!(file.sample_at(0, 1_500_000_000, true), Some(11.5));
        assert_eq!(file.sample_at(1, 2_500_000_000, true), Some(4.0));
    }

    #[test]
    fn decodes_native_markerless_type_codes() {
        let defs = 0x200usize;
        let stride = 0xe0usize;
        let chunks = defs + stride * 201;
        let end = chunks + 0x80;
        let mut data = vec![0u8; end + 0x80];
        directory(&mut data, 0x80, defs as u32, 201, 1, 2);
        directory(&mut data, 0xa0, chunks as u32, 2, 3, 0);
        directory(&mut data, 0xc0, end as u32, 0, 1, 0);
        for (index, (id, name, type_code)) in [(1, "Float channel", 6), (2, "Signed channel", 2)]
            .into_iter()
            .enumerate()
        {
            let at = defs + index * stride;
            u32_at(&mut data, at, id);
            utf16_at(&mut data, at + 8, name);
            u32_at(&mut data, at + 0xd0, type_code);
            let chunk = chunks + index * 0x40;
            let ptr = end + index * 0x20;
            u32_at(&mut data, chunk, index as u32);
            u32_at(&mut data, chunk + 4, id);
            u32_at(&mut data, chunk + 8, id);
            u32_at(&mut data, chunk + 0x18, 10_000_000);
            u32_at(&mut data, chunk + 0x1c, 2);
            u32_at(&mut data, chunk + 0x38, ptr as u32);
            if type_code == 6 {
                for (sample, value) in [1.5_f32, -2.25].into_iter().enumerate() {
                    data[ptr + sample * 4..ptr + sample * 4 + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
            } else {
                for (sample, value) in [-30_000_i16, 30_000].into_iter().enumerate() {
                    data[ptr + sample * 2..ptr + sample * 2 + 2]
                        .copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        let mut fixture = tempfile::NamedTempFile::new().unwrap();
        fixture.write_all(&data).unwrap();
        let file = CosworthFile::open(fixture.path()).unwrap();
        assert_eq!(file.channels[0].sample_type, SampleType::F32);
        assert_eq!(file.decode(0, 0, 1), -2.25);
        assert_eq!(file.channels[1].sample_type, SampleType::I16);
        assert_eq!(file.decode(1, 0, 0), -30_000.0);
    }

    #[test]
    fn rejects_short_files() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"not pds").unwrap();
        assert!(matches!(
            CosworthFile::open(file.path()),
            Err(CosworthError::Invalid { .. })
        ));
    }
}
