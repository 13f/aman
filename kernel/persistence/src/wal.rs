// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::event::Event;
use kernel::{AmanResult, Error};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

const CHECKPOINT_FILE: &str = "replay_checkpoint.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalSync {
    Fsync,
    Batch,
}

#[derive(Debug)]
pub struct WriteAheadLog {
    dir: PathBuf,
    rotate_bytes: u64,
    sync_mode: WalSync,
    state: WalState,
}

#[derive(Debug)]
struct WalState {
    active_segment_id: u64,
    active_segment_size: u64,
    next_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct WalEntry {
    offset: u64,
    event: Event,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
struct ReplayCheckpoint {
    offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SegmentMeta {
    id: u64,
    path: PathBuf,
}

impl WriteAheadLog {
    pub fn new(
        dir: impl Into<PathBuf>,
        rotate_bytes: u64,
        sync_mode: WalSync,
    ) -> AmanResult<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let segments = list_segments(&dir)?;
        let (active_segment_id, active_segment_size, next_offset) = if let Some(last) = segments.last() {
            let next = max_offset_in_segment(&last.path)?.map_or(0, |offset| offset.saturating_add(1));
            let size = fs::metadata(&last.path)?.len();
            (last.id, size, next)
        } else {
            (0, 0, 0)
        };

        Ok(Self {
            dir,
            rotate_bytes,
            sync_mode,
            state: WalState {
                active_segment_id,
                active_segment_size,
                next_offset,
            },
        })
    }

    pub fn append(&mut self, event: Event) -> AmanResult<u64> {
        let offset = self.state.next_offset;
        let line = serde_json::to_vec(&WalEntry { offset, event })?;
        let line_len = u64::try_from(line.len()).unwrap_or(u64::MAX).saturating_add(1);

        if self.rotate_bytes > 0
            && self.state.active_segment_size > 0
            && self.state.active_segment_size.saturating_add(line_len) > self.rotate_bytes
        {
            self.rotate_segment()?;
        }

        let path = self.segment_path(self.state.active_segment_id);
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(&line)?;
        file.write_all(b"\n")?;
        if matches!(self.sync_mode, WalSync::Fsync) {
            file.sync_data()?;
        }

        self.state.active_segment_size = self.state.active_segment_size.saturating_add(line_len);
        self.state.next_offset = self.state.next_offset.saturating_add(1);
        Ok(offset)
    }

    pub fn checkpoint(&self, offset: u64) -> AmanResult<()> {
        let path = self.dir.join(CHECKPOINT_FILE);
        let data = serde_json::to_vec(&ReplayCheckpoint { offset })?;
        fs::write(path, data)?;
        Ok(())
    }

    pub fn final_checkpoint(&self, offset: u64) -> AmanResult<()> {
        self.checkpoint(offset)
    }

    #[must_use]
    pub fn last_offset_written(&self) -> Option<u64> {
        if self.state.next_offset == 0 {
            None
        } else {
            Some(self.state.next_offset.saturating_sub(1))
        }
    }

    #[must_use]
    pub fn checkpoint_offset(&self) -> u64 {
        self.read_checkpoint_offset()
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    pub fn replay_from_checkpoint(&self) -> AmanResult<Vec<Event>> {
        self.replay_from_checkpoint_with_limit(usize::MAX)
    }

    pub fn replay_from_checkpoint_with_limit(&self, max_events: usize) -> AmanResult<Vec<Event>> {
        let checkpoint = self.read_checkpoint_offset()?;
        let mut events = Vec::new();
        let segments = list_segments(&self.dir)?;
        if max_events == 0 {
            return Ok(events);
        }
        for segment in segments {
            let file = File::open(segment.path)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let entry: WalEntry = serde_json::from_str(&line)?;
                if checkpoint.is_none_or(|offset| entry.offset > offset) {
                    events.push(entry.event);
                    if events.len() >= max_events {
                        return Ok(events);
                    }
                }
            }
        }
        Ok(events)
    }

    fn rotate_segment(&mut self) -> AmanResult<()> {
        self.state.active_segment_id = self.state.active_segment_id.saturating_add(1);
        self.state.active_segment_size = 0;
        let path = self.segment_path(self.state.active_segment_id);
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(())
    }

    fn segment_path(&self, segment_id: u64) -> PathBuf {
        self.dir.join(format!("wal-{segment_id:020}.log"))
    }

    fn read_checkpoint_offset(&self) -> AmanResult<Option<u64>> {
        let path = self.dir.join(CHECKPOINT_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read(path)?;
        let checkpoint: ReplayCheckpoint = serde_json::from_slice(&data).map_err(Error::from)?;
        Ok(Some(checkpoint.offset))
    }
}

fn list_segments(dir: &Path) -> AmanResult<Vec<SegmentMeta>> {
    let mut segments = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if let Some(id) = parse_segment_id(file_name) {
            segments.push(SegmentMeta { id, path });
        }
    }
    segments.sort();
    Ok(segments)
}

fn parse_segment_id(file_name: &str) -> Option<u64> {
    let stripped = file_name
        .strip_prefix("wal-")?
        .strip_suffix(".log")?;
    stripped.parse::<u64>().ok()
}

fn max_offset_in_segment(path: &Path) -> AmanResult<Option<u64>> {
    let file = File::open(path)?;
    let mut max_offset = None;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: WalEntry = serde_json::from_str(&line)?;
        max_offset = Some(max_offset.map_or(entry.offset, |current: u64| current.max(entry.offset)));
    }
    Ok(max_offset)
}

#[cfg(test)]
mod tests {
    use super::{WalSync, WriteAheadLog};
    use kernel::event::{Event, EventType};
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("aman-{name}-{nanos}"))
    }

    fn event(seq: u64) -> Event {
        Event::new("wal:test", EventType::MessageReceived, json!({ "seq": seq }))
    }

    #[test]
    fn append_and_replay_from_default_checkpoint() {
        let dir = test_dir("wal-replay");
        let mut wal = WriteAheadLog::new(&dir, 1024 * 1024, WalSync::Batch).expect("create wal");
        wal.append(event(1)).expect("append");
        wal.append(event(2)).expect("append");

        let replayed = wal.replay_from_checkpoint().expect("replay");
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].payload["seq"], 1);
        assert_eq!(replayed[1].payload["seq"], 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkpoint_skips_already_processed_offsets() {
        let dir = test_dir("wal-checkpoint");
        let mut wal = WriteAheadLog::new(&dir, 1024 * 1024, WalSync::Batch).expect("create wal");
        let first_offset = wal.append(event(1)).expect("append first");
        wal.append(event(2)).expect("append second");
        wal.checkpoint(first_offset).expect("write checkpoint");

        let replayed = wal.replay_from_checkpoint().expect("replay");
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].payload["seq"], 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotates_segments_when_threshold_reached() {
        let dir = test_dir("wal-rotate");
        let mut wal = WriteAheadLog::new(&dir, 1, WalSync::Batch).expect("create wal");
        wal.append(event(1)).expect("append first");
        wal.append(event(2)).expect("append second");

        let segment_count = fs::read_dir(&dir)
            .expect("read wal dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .unwrap_or_default()
                    .starts_with("wal-")
            })
            .count();
        assert!(segment_count >= 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_honors_max_events_buffer_limit() {
        let dir = test_dir("wal-buffer-limit");
        let mut wal = WriteAheadLog::new(&dir, 1024 * 1024, WalSync::Batch).expect("create wal");
        wal.append(event(1)).expect("append");
        wal.append(event(2)).expect("append");
        wal.append(event(3)).expect("append");

        let replayed = wal
            .replay_from_checkpoint_with_limit(2)
            .expect("replay with limit");
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].payload["seq"], 1);
        assert_eq!(replayed[1].payload["seq"], 2);
        let _ = fs::remove_dir_all(&dir);
    }
}
