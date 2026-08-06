use crate::TelemetrySource;
use std::collections::{BTreeMap, BTreeSet};

const GPS_WEEK_MS: u64 = 604_800_000;

#[derive(Debug, Clone, PartialEq)]
pub struct LapMetadata {
    pub number: i64,
    pub start_ns: u64,
    pub end_ns: u64,
    pub duration_ns: u64,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DriverStint {
    pub driver_id: i64,
    pub start_ns: u64,
    pub end_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsoluteTimeRange {
    pub clock: String,
    pub start_ns: u64,
    pub end_ns: u64,
    pub session_hint: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceIdentity {
    pub driver: String,
    pub vehicle: String,
    pub venue: String,
    pub event: String,
    pub session: String,
    pub date: String,
    pub time: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoReference {
    pub file_index: Option<u32>,
    pub sync_time: Option<f64>,
    pub frame_index: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileMetadata {
    pub path: String,
    pub format: String,
    pub channel_count: usize,
    pub sampled_channel_count: usize,
    pub sample_count: u64,
    pub duration_ns: u64,
    pub schema_hash: u64,
    pub session_key: Option<String>,
    pub absolute_clock: Option<String>,
    pub absolute_start_ns: Option<u64>,
    pub absolute_end_ns: Option<u64>,
    pub clock_offset_ns: Option<i128>,
    pub identity: SourceIdentity,
    pub driver_ids: Vec<i64>,
    pub driver_stints: Vec<DriverStint>,
    pub laps: Vec<LapMetadata>,
    pub fastest_lap: Option<LapMetadata>,
    pub video_frame_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionMetadata {
    pub session_key: String,
    pub files: Vec<usize>,
    pub absolute_start_ns: Option<u64>,
    pub absolute_end_ns: Option<u64>,
    pub duration_ns: u64,
    pub driver_stints: Vec<DriverStint>,
    pub laps: Vec<LapMetadata>,
    pub fastest_lap: Option<LapMetadata>,
}

fn normalized(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn channel_index(source: &dyn TelemetrySource, names: &[&str]) -> Option<usize> {
    source.channels().iter().position(|channel| {
        channel.sample_count > 0 && names.contains(&normalized(&channel.name).as_str())
    })
}

fn samples(source: &dyn TelemetrySource, channel_index: usize) -> Vec<(u64, f64)> {
    let channel = &source.channels()[channel_index];
    let mut values = Vec::with_capacity(channel.sample_count as usize);
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        for local_index in 0..chunk.sample_count {
            values.push((
                source.sample_time_ns(channel_index, chunk_index, local_index),
                source.decode(channel_index, chunk_index, local_index),
            ));
        }
    }
    values
}

fn integer_runs(values: &[(u64, f64)], duration_ns: u64) -> Vec<(i64, u64, u64)> {
    let mut runs = Vec::new();
    let mut current: Option<(i64, u64)> = None;
    for &(time_ns, value) in values {
        if !value.is_finite() {
            continue;
        }
        let integer = value.round() as i64;
        if current.is_some_and(|(before, _)| before == integer) {
            continue;
        }
        if let Some((before, start_ns)) = current.replace((integer, time_ns)) {
            runs.push((before, start_ns, time_ns));
        }
    }
    if let Some((value, start_ns)) = current {
        runs.push((value, start_ns, duration_ns.max(start_ns)));
    }
    runs
}

fn schema_hash(source: &dyn TelemetrySource) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for channel in source.channels() {
        for byte in channel.name.to_ascii_lowercase().bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        for byte in channel.unit.to_ascii_lowercase().bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= u64::from(channel.sample_type.code());
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn read_source_metadata(source: &dyn TelemetrySource) -> FileMetadata {
    let duration_ns = source
        .channels()
        .iter()
        .map(|channel| channel.duration_ns)
        .max()
        .unwrap_or(0);
    let hash = schema_hash(source);
    let explicit_absolute = source.absolute_time_range();
    let absolute = channel_index(source, &["gpsweek"]).and_then(|week_index| {
        let week = samples(source, week_index)
            .into_iter()
            .find_map(|(_, value)| value.is_finite().then_some(value.round() as u64))?;
        let itow_index = channel_index(source, &["gpsitow"])?;
        let itow = samples(source, itow_index);
        let &(first_time, first_value) = itow.first()?;
        let &(_last_time, last_value) = itow.last()?;
        let start_ns = week
            .saturating_mul(GPS_WEEK_MS)
            .saturating_add(first_value.round().max(0.0) as u64)
            .saturating_mul(1_000_000);
        let end_ns = week
            .saturating_mul(GPS_WEEK_MS)
            .saturating_add(last_value.round().max(0.0) as u64)
            .saturating_mul(1_000_000);
        Some((week, first_time, start_ns, end_ns))
    });
    let (absolute_clock, absolute_start_ns, absolute_end_ns, clock_offset_ns, session_key) =
        if let Some(range) = explicit_absolute {
            (
                Some(range.clock),
                Some(range.start_ns),
                Some(range.end_ns),
                Some(i128::from(range.start_ns)),
                Some(format!("{}:{hash:016x}", range.session_hint)),
            )
        } else if let Some((week, first_time, start_ns, end_ns)) = absolute {
            (
                Some("gps".into()),
                Some(start_ns),
                Some(end_ns),
                Some(i128::from(start_ns) - i128::from(first_time)),
                Some(format!("gps:{week}:{hash:016x}")),
            )
        } else {
            (None, None, None, None, None)
        };
    let raw_driver_runs = channel_index(source, &["driverid", "driver", "driverindex"])
        .map(|index| integer_runs(&samples(source, index), duration_ns))
        .unwrap_or_default();
    let driver_runs = raw_driver_runs
        .iter()
        .copied()
        .filter(|(_, start_ns, end_ns)| {
            raw_driver_runs.len() == 1 || end_ns.saturating_sub(*start_ns) >= 1_000_000_000
        })
        .collect::<Vec<_>>();
    let driver_ids = driver_runs
        .iter()
        .map(|(driver, _, _)| *driver)
        .filter(|driver| *driver >= 0)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let driver_stints = driver_runs
        .into_iter()
        .filter(|(driver, _, _)| *driver >= 0)
        .map(|(driver_id, start_ns, end_ns)| DriverStint {
            driver_id,
            start_ns,
            end_ns,
        })
        .collect::<Vec<_>>();

    let lap_channel_index = channel_index(
        source,
        &[
            "lapnumber",
            "lapnum",
            "lapcount",
            "lapcounter",
            "currentlap",
            "lap",
        ],
    );
    let lap_runs = lap_channel_index
        .map(|index| integer_runs(&samples(source, index), duration_ns))
        .unwrap_or_default();
    let timer_resets = channel_index(
        source,
        &[
            "currentlaptime",
            "lapcurrentlaptime",
            "laptime",
            "laptimerunning",
        ],
    )
    .map(|index| {
        let values = samples(source, index);
        let max_value = values
            .iter()
            .map(|(_, value)| *value)
            .filter(|value| value.is_finite())
            .fold(0.0_f64, f64::max);
        let reset_threshold = if max_value > 1_000.0 { 5_000.0 } else { 5.0 };
        values
            .windows(2)
            .filter_map(|pair| {
                let before = pair[0].1;
                let after = pair[1].1;
                (before.is_finite() && after.is_finite() && before - after > reset_threshold)
                    .then_some(pair[1].0)
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    let laps = if timer_resets.len() >= 2 {
        let mut boundaries = Vec::with_capacity(timer_resets.len() + 2);
        boundaries.push(0);
        boundaries.extend(timer_resets);
        boundaries.push(duration_ns);
        let count = boundaries.len() - 1;
        boundaries
            .windows(2)
            .enumerate()
            .filter_map(|(index, pair)| {
                let number = lap_channel_index
                    .and_then(|channel| source.sample_at(channel, pair[0], false))
                    .filter(|value| value.is_finite())
                    .map(|value| value.round() as i64)
                    .unwrap_or(index as i64 + 1);
                (number > 0).then_some(LapMetadata {
                    number,
                    start_ns: pair[0],
                    end_ns: pair[1],
                    duration_ns: pair[1].saturating_sub(pair[0]),
                    complete: index > 0 && index + 1 < count,
                })
            })
            .collect::<Vec<_>>()
    } else {
        let lap_run_count = lap_runs.len();
        lap_runs
            .into_iter()
            .enumerate()
            .filter(|(_, (number, _, _))| *number > 0)
            .map(|(index, (number, start_ns, end_ns))| LapMetadata {
                number,
                start_ns,
                end_ns,
                duration_ns: end_ns.saturating_sub(start_ns),
                complete: index > 0 && index + 1 < lap_run_count,
            })
            .collect::<Vec<_>>()
    };
    let reference_lap_ns =
        channel_index(source, &["reflaptime", "referencelaptime"]).and_then(|index| {
            let values = samples(source, index);
            let max_value = values
                .iter()
                .map(|(_, value)| *value)
                .filter(|value| value.is_finite())
                .fold(0.0_f64, f64::max);
            let scale = if max_value > 1_000.0 {
                1_000_000.0
            } else {
                1_000_000_000.0
            };
            values
                .into_iter()
                .map(|(_, value)| value)
                .find(|value| value.is_finite() && *value > 0.0)
                .map(|value| (value * scale).round() as u64)
        });
    let plausible_lap = |duration_ns: u64| {
        duration_ns >= 10_000_000_000
            && reference_lap_ns.is_none_or(|reference| {
                duration_ns >= reference / 2 && duration_ns <= reference.saturating_mul(3) / 2
            })
    };
    let mut fastest_lap = laps
        .iter()
        .filter(|lap| lap.complete && plausible_lap(lap.duration_ns))
        .min_by_key(|lap| lap.duration_ns)
        .cloned();
    if let Some(previous_lap_index) =
        channel_index(source, &["previouslt", "previouslaptime", "lastlaptime"])
    {
        let values = samples(source, previous_lap_index);
        let max_value = values
            .iter()
            .map(|(_, value)| *value)
            .filter(|value| value.is_finite())
            .fold(0.0_f64, f64::max);
        let scale = if max_value > 1_000.0 {
            1_000_000.0
        } else {
            1_000_000_000.0
        };
        let reported = values
            .into_iter()
            .filter(|(_, value)| value.is_finite() && *value > 0.0)
            .map(|(time_ns, value)| (time_ns, (value * scale).round() as u64))
            .filter(|(_, duration_ns)| plausible_lap(*duration_ns))
            .min_by_key(|(_, duration_ns)| *duration_ns);
        if let Some((time_ns, duration_ns)) = reported {
            let number = lap_channel_index
                .and_then(|channel| source.sample_at(channel, time_ns, false))
                .filter(|value| value.is_finite())
                .map(|value| value.round() as i64 - 1)
                .unwrap_or(0);
            fastest_lap = Some(LapMetadata {
                number,
                start_ns: time_ns.saturating_sub(duration_ns),
                end_ns: time_ns,
                duration_ns,
                complete: true,
            });
        }
    }

    FileMetadata {
        path: source.path().to_owned(),
        format: source.format().to_owned(),
        channel_count: source.channels().len(),
        sampled_channel_count: source
            .channels()
            .iter()
            .filter(|channel| channel.sample_count > 0)
            .count(),
        sample_count: source
            .channels()
            .iter()
            .map(|channel| channel.sample_count)
            .sum(),
        duration_ns,
        schema_hash: hash,
        session_key,
        absolute_clock,
        absolute_start_ns,
        absolute_end_ns,
        clock_offset_ns,
        identity: source.identity(),
        driver_ids,
        driver_stints,
        laps,
        fastest_lap,
        video_frame_count: source.video_frame_count(),
    }
}

fn absolute_time(metadata: &FileMetadata, relative_ns: u64) -> Option<u64> {
    let offset = metadata.clock_offset_ns?;
    u64::try_from(i128::from(relative_ns) + offset).ok()
}

pub fn group_sessions(files: &[FileMetadata], max_gap_ns: u64) -> Vec<SessionMetadata> {
    let mut indexed = files
        .iter()
        .enumerate()
        .collect::<Vec<(usize, &FileMetadata)>>();
    indexed.sort_by_key(|(_, file)| (file.session_key.clone(), file.absolute_start_ns));
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, file) in indexed {
        let joins_previous = groups
            .last()
            .and_then(|group| group.last())
            .is_some_and(|before| {
                let previous = &files[*before];
                previous.session_key == file.session_key
                    && previous.session_key.is_some()
                    && previous
                        .absolute_end_ns
                        .zip(file.absolute_start_ns)
                        .is_some_and(|(end, start)| start <= end.saturating_add(max_gap_ns))
            });
        if joins_previous {
            groups.last_mut().unwrap().push(index);
        } else {
            groups.push(vec![index]);
        }
    }

    groups
        .into_iter()
        .enumerate()
        .map(|(group_index, indices)| {
            let start = indices
                .iter()
                .filter_map(|index| files[*index].absolute_start_ns)
                .min();
            let end = indices
                .iter()
                .filter_map(|index| files[*index].absolute_end_ns)
                .max();
            let base = start.unwrap_or(0);

            let mut driver_segments = Vec::new();
            for index in &indices {
                let file = &files[*index];
                for stint in &file.driver_stints {
                    if let (Some(from), Some(to)) = (
                        absolute_time(file, stint.start_ns),
                        absolute_time(file, stint.end_ns),
                    ) {
                        driver_segments.push(DriverStint {
                            driver_id: stint.driver_id,
                            start_ns: from.saturating_sub(base),
                            end_ns: to.saturating_sub(base),
                        });
                    }
                }
            }
            driver_segments.sort_by_key(|stint| stint.start_ns);
            let mut driver_stints: Vec<DriverStint> = Vec::new();
            for stint in driver_segments {
                if let Some(previous) = driver_stints.last_mut() {
                    if previous.driver_id == stint.driver_id
                        && stint.start_ns <= previous.end_ns.saturating_add(max_gap_ns)
                    {
                        previous.end_ns = previous.end_ns.max(stint.end_ns);
                        continue;
                    }
                }
                driver_stints.push(stint);
            }

            let mut lap_segments = Vec::new();
            for index in &indices {
                let file = &files[*index];
                for lap in &file.laps {
                    if let (Some(from), Some(to)) = (
                        absolute_time(file, lap.start_ns),
                        absolute_time(file, lap.end_ns),
                    ) {
                        lap_segments.push(LapMetadata {
                            number: lap.number,
                            start_ns: from.saturating_sub(base),
                            end_ns: to.saturating_sub(base),
                            duration_ns: to.saturating_sub(from),
                            complete: false,
                        });
                    }
                }
            }
            lap_segments.sort_by_key(|lap| lap.start_ns);
            let mut laps: Vec<LapMetadata> = Vec::new();
            for lap in lap_segments {
                if let Some(previous) = laps.last_mut() {
                    if previous.number == lap.number
                        && lap.start_ns <= previous.end_ns.saturating_add(max_gap_ns)
                    {
                        previous.end_ns = previous.end_ns.max(lap.end_ns);
                        previous.duration_ns = previous.end_ns.saturating_sub(previous.start_ns);
                        continue;
                    }
                }
                laps.push(lap);
            }
            let lap_count = laps.len();
            for (index, lap) in laps.iter_mut().enumerate() {
                lap.complete = index > 0 && index + 1 < lap_count;
            }
            let inferred_fastest = laps
                .iter()
                .filter(|lap| lap.complete && lap.duration_ns >= 10_000_000_000)
                .min_by_key(|lap| lap.duration_ns)
                .cloned();
            let reported_fastest = indices
                .iter()
                .filter_map(|index| files[*index].fastest_lap.as_ref())
                .min_by_key(|lap| lap.duration_ns)
                .cloned();
            let fastest_lap = reported_fastest.or(inferred_fastest);

            let candidate = indices
                .first()
                .and_then(|index| files[*index].session_key.clone())
                .unwrap_or_else(|| format!("unkeyed:{group_index}"));
            SessionMetadata {
                session_key: format!("{candidate}:{group_index}"),
                files: indices,
                absolute_start_ns: start,
                absolute_end_ns: end,
                duration_ns: end
                    .zip(start)
                    .map_or(0, |(end, start)| end.saturating_sub(start)),
                driver_stints,
                laps,
                fastest_lap,
            }
        })
        .collect()
}

pub fn driver_histogram(source: &dyn TelemetrySource) -> BTreeMap<i64, u64> {
    let Some(index) = channel_index(source, &["driverid", "driver", "driverindex"]) else {
        return BTreeMap::new();
    };
    let mut counts = BTreeMap::new();
    for (_, value) in samples(source, index) {
        if value.is_finite() {
            *counts.entry(value.round() as i64).or_default() += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Channel, Chunk, SampleType, UnitSource};

    struct MetadataSource {
        path: String,
        channels: Vec<Channel>,
        values: Vec<Vec<f64>>,
        absolute_start_ns: u64,
    }

    impl TelemetrySource for MetadataSource {
        fn path(&self) -> &str {
            &self.path
        }

        fn format(&self) -> &'static str {
            "synthetic"
        }

        fn channels(&self) -> &[Channel] {
            &self.channels
        }

        fn decode(&self, channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
            self.values[channel_index][local_index as usize]
        }

        fn absolute_time_range(&self) -> Option<AbsoluteTimeRange> {
            Some(AbsoluteTimeRange {
                clock: "test".into(),
                start_ns: self.absolute_start_ns,
                end_ns: self.absolute_start_ns + 40_000_000_000,
                session_hint: "test-session".into(),
            })
        }
    }

    fn metadata_source(path: &str, start_ns: u64, driver: i64) -> MetadataSource {
        let names = ["DRIVER_ID", "Lap_Number", "Previous_LT", "Ref_Lap_Time"];
        let values = vec![
            vec![driver as f64; 4],
            vec![1.0, 2.0, 3.0, 4.0],
            vec![0.0, 20_000.0, 18_000.0, 19_000.0],
            vec![20_000.0; 4],
        ];
        let channels = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| Channel {
                id: index as u32,
                name: name.into(),
                unit: String::new(),
                unit_source: UnitSource::Unknown,
                sample_type: SampleType::F64,
                chunks: vec![Chunk {
                    sample_period_ns: 10_000_000_000,
                    sample_count: 4,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
                sample_count: 4,
                duration_ns: 40_000_000_000,
            })
            .collect();
        MetadataSource {
            path: path.into(),
            channels,
            values,
            absolute_start_ns: start_ns,
        }
    }

    #[test]
    fn summarizes_driver_laps_and_fastest_complete_lap() {
        let source = metadata_source("part-1", 1_000_000_000_000, 3);
        let metadata = read_source_metadata(&source);
        assert_eq!(metadata.driver_ids, [3]);
        assert_eq!(metadata.laps.len(), 4);
        assert_eq!(
            metadata.fastest_lap.as_ref().unwrap().duration_ns,
            18_000_000_000
        );
        assert!(metadata
            .session_key
            .as_deref()
            .unwrap()
            .starts_with("test-session:"));
    }

    #[test]
    fn groups_contiguous_files_and_merges_driver_stints() {
        let first = read_source_metadata(&metadata_source("part-1", 1_000_000_000_000, 3));
        let second = read_source_metadata(&metadata_source("part-2", 1_045_000_000_000, 3));
        let sessions = group_sessions(&[first, second], 10_000_000_000);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].files, [0, 1]);
        assert_eq!(sessions[0].driver_stints.len(), 1);
        assert_eq!(sessions[0].driver_stints[0].driver_id, 3);
    }
}
