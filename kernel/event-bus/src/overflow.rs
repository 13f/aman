// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::event::Event;
use kernel::{AmanResult, Error};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Directory-based overflow storage for events when the in-memory queue
/// reaches Level 4A (98%) backpressure.
///
/// Events are stored as individual JSON files named by event UUID, allowing
/// atomic writes and safe crash recovery via directory scan.
#[derive(Debug)]
pub struct OverflowDir {
    dir: PathBuf,
    max_bytes: u64,
}

impl OverflowDir {
    pub fn new(dir: impl AsRef<Path>, max_bytes: u64) -> AmanResult<Self> {
        let dir = dir.as_ref().to_path_buf();
        if !dir.exists() {
            fs::create_dir_all(&dir).map_err(Error::Io)?;
        }
        Ok(Self { dir, max_bytes })
    }

    /// Write a single event to the overflow directory as a JSON file.
    /// Fails with `BusFull` if the directory exceeds `max_bytes`.
    pub fn write_event(&self, event: &Event) -> AmanResult<()> {
        let current_size = self.current_size()?;
        if current_size >= self.max_bytes {
            return Err(Error::BusFull);
        }

        let json = serde_json::to_string(event).map_err(Error::SerdeJson)?;
        let filename = format!("{}.json", event.id);
        let path = self.dir.join(&filename);
        let tmp_path = self.dir.join(format!("{}.tmp", event.id));

        // Atomic write via temp file + rename
        {
            let mut file = fs::File::create(&tmp_path).map_err(Error::Io)?;
            file.write_all(json.as_bytes()).map_err(Error::Io)?;
            file.sync_all().map_err(Error::Io)?;
        }
        fs::rename(&tmp_path, &path).map_err(Error::Io)?;

        Ok(())
    }

    /// Scan all events in the overflow directory and return them sorted
    /// by timestamp for ordered replay after crash recovery.
    pub fn scan(&self) -> AmanResult<Vec<Event>> {
        let mut events = Vec::new();

        if !self.dir.exists() {
            return Ok(events);
        }

        for entry in fs::read_dir(&self.dir).map_err(Error::Io)? {
            let entry = entry.map_err(Error::Io)?;
            let path = entry.path();

            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| ext == "json")
            {
                let content = fs::read_to_string(&path).map_err(Error::Io)?;
                let event: Event = serde_json::from_str(&content).map_err(Error::SerdeJson)?;
                events.push(event);
            }
        }

        // Sort by timestamp for ordered replay (preserves original enqueue order
        // for same-source events since UUID v7 is time-ordered)
        events.sort_by_key(|e| e.timestamp);
        Ok(events)
    }

    /// Remove a specific event file after successful replay.
    pub fn remove_event(&self, event_id: &uuid::Uuid) -> AmanResult<()> {
        let filename = format!("{}.json", event_id);
        let path = self.dir.join(filename);
        if path.exists() {
            fs::remove_file(&path).map_err(Error::Io)?;
        }
        Ok(())
    }

    /// Calculate the current total size of the overflow directory in bytes.
    pub fn current_size(&self) -> AmanResult<u64> {
        let mut total = 0u64;
        if !self.dir.exists() {
            return Ok(0);
        }
        for entry in fs::read_dir(&self.dir).map_err(Error::Io)? {
            let entry = entry.map_err(Error::Io)?;
            let meta = entry.metadata().map_err(Error::Io)?;
            if meta.is_file() {
                total += meta.len();
            }
        }
        Ok(total)
    }

    /// Returns the current overflow usage as a ratio [0.0, 1.0].
    pub fn usage_ratio(&self) -> AmanResult<f32> {
        let size = self.current_size()?;
        if self.max_bytes == 0 {
            return Ok(0.0);
        }
        Ok(size as f32 / self.max_bytes as f32)
    }

    /// Returns `true` when overflow directory usage exceeds the given
    /// threshold ratio (e.g., 0.8 for 80%).
    #[must_use]
    pub fn is_over_threshold(&self, threshold: f32) -> bool {
        self.usage_ratio().is_ok_and(|ratio| ratio >= threshold)
    }

    /// Clear all events from the overflow directory.
    pub fn clear(&self) -> AmanResult<()> {
        if self.dir.exists() {
            for entry in fs::read_dir(&self.dir).map_err(Error::Io)? {
                let entry = entry.map_err(Error::Io)?;
                let path = entry.path();
                if path.is_file() {
                    fs::remove_file(&path).map_err(Error::Io)?;
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    #[must_use]
    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::OverflowDir;
    use kernel::event::{Event, EventType};
    use serde_json::json;
    use std::fs;

    fn temp_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aman-overflow-test-{}-{}",
            std::process::id(),
            id
        ));
        dir
    }

    #[test]
    fn write_and_scan_single_event() {
        let dir = temp_dir();
        let overflow = OverflowDir::new(&dir, 1_000_000).expect("create overflow dir");

        let event = Event::new(
            "overflow:test",
            EventType::FileCreated,
            json!({"file": "a.txt"}),
        );
        overflow.write_event(&event).expect("write event");

        let scanned = overflow.scan().expect("scan events");
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].source.as_str(), "overflow:test");

        // Cleanup
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_returns_events_sorted_by_timestamp() {
        let dir = temp_dir();
        let overflow = OverflowDir::new(&dir, 1_000_000).expect("create overflow dir");

        // Create events with different timestamps
        for i in 0..5 {
            let mut event = Event::new("overflow:test", EventType::FileCreated, json!({"seq": i}));
            event.timestamp = kernel::types::Timestamp::from_millis(i * 100);
            overflow.write_event(&event).expect("write event");
        }

        let scanned = overflow.scan().expect("scan events");
        assert_eq!(scanned.len(), 5);
        // Should be sorted by timestamp ascending
        for i in 1..scanned.len() {
            assert!(scanned[i - 1].timestamp.as_millis() <= scanned[i].timestamp.as_millis());
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_fails_when_over_max_bytes() {
        let dir = temp_dir();
        // max_bytes=0 means any write should fail immediately
        let overflow = OverflowDir::new(&dir, 0).expect("create overflow dir");

        let event = Event::new(
            "overflow:test",
            EventType::FileCreated,
            json!({"file": "a.txt"}),
        );
        let result = overflow.write_event(&event);
        assert!(result.is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_event_deletes_file() {
        let dir = temp_dir();
        let overflow = OverflowDir::new(&dir, 1_000_000).expect("create overflow dir");

        let event = Event::new(
            "overflow:test",
            EventType::FileCreated,
            json!({"file": "a.txt"}),
        );
        let event_id = event.id;
        overflow.write_event(&event).expect("write event");

        assert_eq!(overflow.scan().expect("scan").len(), 1);

        overflow.remove_event(&event_id).expect("remove event");
        assert_eq!(overflow.scan().expect("scan").len(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_ratio_tracks_file_size() {
        let dir = temp_dir();
        let overflow = OverflowDir::new(&dir, 1_000_000).expect("create overflow dir");

        assert_eq!(overflow.usage_ratio().expect("ratio"), 0.0);

        let event = Event::new(
            "overflow:test",
            EventType::FileCreated,
            json!({"file": "a.txt"}),
        );
        overflow.write_event(&event).expect("write event");

        let ratio = overflow.usage_ratio().expect("ratio");
        assert!(ratio > 0.0);
        assert!(overflow.is_over_threshold(0.0));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_over_threshold_detects_high_usage() {
        let dir = temp_dir();
        let overflow = OverflowDir::new(&dir, 100).expect("create overflow dir");

        assert!(!overflow.is_over_threshold(0.8));

        let event = Event::new(
            "overflow:test",
            EventType::FileCreated,
            json!({"file": "a.txt"}),
        );
        let _ = overflow.write_event(&event);

        // The event JSON is > 80 bytes so it should exceed 80% of 100 bytes
        assert!(overflow.is_over_threshold(0.8));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_removes_all_events() {
        let dir = temp_dir();
        let overflow = OverflowDir::new(&dir, 1_000_000).expect("create overflow dir");

        for i in 0..3 {
            let event = Event::new("overflow:test", EventType::FileCreated, json!({"seq": i}));
            overflow.write_event(&event).expect("write event");
        }

        assert_eq!(overflow.scan().expect("scan").len(), 3);

        overflow.clear().expect("clear");
        assert_eq!(overflow.scan().expect("scan").len(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trip_event_preserves_all_fields() {
        let dir = temp_dir();
        let overflow = OverflowDir::new(&dir, 1_000_000).expect("create overflow dir");

        let original = Event::new(
            "overflow:test",
            EventType::FileCreated,
            json!({"file": "a.txt"}),
        );
        overflow.write_event(&original).expect("write event");

        let scanned = overflow.scan().expect("scan events");
        let recovered = &scanned[0];

        assert_eq!(recovered.id, original.id);
        assert_eq!(recovered.source.as_str(), original.source.as_str());
        assert_eq!(recovered.event_type, original.event_type);
        assert_eq!(recovered.payload, original.payload);

        let _ = fs::remove_dir_all(&dir);
    }
}
