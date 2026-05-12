use runtime::AgentRuntime;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct AppState {
    pub runtime: Arc<Mutex<Option<Arc<AgentRuntime>>>>,
}

impl AppState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
