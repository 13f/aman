use event_bus::PublicOverflowDir;
use kernel::event::Event;
use kernel::AmanResult;
use std::path::Path;
use uuid::Uuid;

#[derive(Debug)]
pub struct OverflowDir {
    inner: PublicOverflowDir,
}

impl OverflowDir {
    pub fn new(path: impl AsRef<Path>, max_bytes: u64) -> AmanResult<Self> {
        Ok(Self {
            inner: PublicOverflowDir::new(path.as_ref(), max_bytes)?,
        })
    }

    pub fn write_event(&self, event: &Event) -> AmanResult<()> {
        self.inner.write_event(event)
    }

    pub fn scan(&self) -> AmanResult<Vec<Event>> {
        self.inner.scan()
    }

    pub fn remove_event(&self, event_id: &Uuid) -> AmanResult<()> {
        self.inner.remove_event(event_id)
    }

    pub fn usage_ratio(&self) -> AmanResult<f32> {
        self.inner.usage_ratio()
    }
}

#[cfg(test)]
mod tests {
    use super::OverflowDir;
    use kernel::event::{Event, EventType};
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("aman-overflow-{name}-{nanos}"))
    }

    #[test]
    fn write_scan_remove_roundtrip() {
        let dir = test_dir("roundtrip");
        let overflow = OverflowDir::new(&dir, 1024 * 1024).expect("create overflow dir");
        let event = Event::new("overflow:test", EventType::MessageReceived, json!({"id": 1}));
        overflow.write_event(&event).expect("write event");

        let scanned = overflow.scan().expect("scan");
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].id, event.id);

        overflow.remove_event(&event.id).expect("remove");
        let scanned_again = overflow.scan().expect("scan again");
        assert!(scanned_again.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
