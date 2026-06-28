use async_trait::async_trait;
use kernel::AmanResult;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, warn};

use super::Adapter;
use crate::types::{InfoItem, InfoSearchInput};

pub struct DbAdapter {
    source_name: String,
    runtime: String,
    script: String,
    db_path: Option<String>,
    timeout_ms: u64,
}

impl DbAdapter {
    pub fn new(
        source_name: String,
        runtime: String,
        script: String,
        db_path: Option<String>,
        timeout_ms: u64,
    ) -> Self {
        Self {
            source_name,
            runtime,
            script: super::expand_tilde(&script),
            db_path: db_path.map(|p| super::expand_tilde(&p)),
            timeout_ms,
        }
    }
}

#[async_trait]
impl Adapter for DbAdapter {
    async fn search(&self, input: &InfoSearchInput) -> AmanResult<Vec<InfoItem>> {
        let stdin_payload = serde_json::json!({
            "query": input.query,
            "limit": input.limit,
            "offset": input.offset,
            "since": input.since,
            "db_path": self.db_path,
        });

        debug!(source = %self.source_name, runtime = %self.runtime, script = %self.script, "info-hub db script");

        let mut child = Command::new(&self.runtime)
            .arg(&self.script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| kernel::Error::config_invalid(format!("spawn {} {}: {e}", self.runtime, self.script)))?;

        if let Some(mut stdin) = child.stdin.take() {
            let payload = serde_json::to_string(&stdin_payload).unwrap_or_default();
            let _ = stdin.write_all(payload.as_bytes()).await;
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_millis(self.timeout_ms),
            child.wait_with_output(),
        )
        .await;

        match output {
            Ok(Ok(out)) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                match serde_json::from_str::<Vec<InfoItem>>(&stdout) {
                    Ok(mut items) => {
                        for item in &mut items {
                            item.source = self.source_name.clone();
                        }
                        Ok(items)
                    }
                    Err(e) => {
                        warn!(source = %self.source_name, %e, "db script stdout parse failed");
                        Ok(Vec::new())
                    }
                }
            }
            Ok(Ok(out)) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                warn!(source = %self.source_name, status = %out.status, %stderr, "db script exited non-zero");
                Ok(Vec::new())
            }
            Ok(Err(e)) => {
                warn!(source = %self.source_name, %e, "db script wait failed");
                Ok(Vec::new())
            }
            Err(_) => {
                warn!(source = %self.source_name, "db script timed out");
                Ok(Vec::new())
            }
        }
    }
}
