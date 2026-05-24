use async_trait::async_trait;
use kernel::AmanResult;
use tokio::process::Command;
use tracing::{debug, warn};

use super::{replace_placeholders, Adapter};
use crate::types::{InfoItem, InfoSearchInput};

pub struct CliAdapter {
    source_name: String,
    command: String,
    args_template: Vec<String>,
    timeout_ms: u64,
}

impl CliAdapter {
    pub fn new(
        source_name: String,
        command: String,
        args_template: Vec<String>,
        timeout_ms: u64,
    ) -> Self {
        Self {
            source_name,
            command: super::expand_tilde(&command),
            args_template,
            timeout_ms,
        }
    }
}

#[async_trait]
impl Adapter for CliAdapter {
    async fn search(&self, input: &InfoSearchInput) -> AmanResult<Vec<InfoItem>> {
        let args: Vec<String> = self
            .args_template
            .iter()
            .map(|arg| replace_placeholders(arg, input))
            .collect();

        debug!(source = %self.source_name, command = %self.command, ?args, "info-hub cli execute");

        let output = Command::new(&self.command)
            .args(&args)
            .kill_on_drop(true)
            .output();

        let output = tokio::time::timeout(
            std::time::Duration::from_millis(self.timeout_ms),
            output,
        )
        .await;

        match output {
            Ok(Ok(out)) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                match serde_json::from_str::<Vec<InfoItem>>(&stdout) {
                    Ok(items) => Ok(items),
                    Err(e) => {
                        warn!(source = %self.source_name, %e, "cli stdout parse failed");
                        Ok(Vec::new())
                    }
                }
            }
            Ok(Ok(out)) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                warn!(source = %self.source_name, status = %out.status, %stderr, "cli exited non-zero");
                Ok(Vec::new())
            }
            Ok(Err(e)) => {
                warn!(source = %self.source_name, %e, "cli spawn failed");
                Ok(Vec::new())
            }
            Err(_) => {
                warn!(source = %self.source_name, "cli timed out");
                Ok(Vec::new())
            }
        }
    }
}
