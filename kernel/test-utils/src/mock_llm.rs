use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

/// The minimal interface an LLM provider must implement.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, prompt: &str) -> LlmResult;
    async fn chat(&self, messages: &[LlmMessage], tools: &[LlmToolDef]) -> LlmResult;
}

#[derive(Debug, Clone)]
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone)]
pub struct LlmToolDef {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct LlmResult {
    pub content: String,
    pub tool_calls: Vec<LlmToolCall>,
    pub finish_reason: LlmFinishReason,
}

#[derive(Debug, Clone)]
pub struct LlmToolCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmFinishReason {
    Stop,
    ToolCall,
    Error,
    Length,
}

#[derive(Debug, Clone, Default)]
pub struct MockCallConfig {
    pub response_text: String,
    pub tool_calls: Vec<LlmToolCall>,
    pub delay_ms: u64,
    pub should_error: bool,
}

#[derive(Debug, Clone)]
pub struct MockCallRecord {
    pub prompt: String,
    pub messages: Vec<LlmMessage>,
    pub timestamp_ms: u64,
}

/// A mock LLM provider for testing. Supports predefined responses,
/// delayed responses, and configurable error-on-Nth-call mode.
pub struct MockLLMProvider {
    default_response: String,
    call_configs: Mutex<Vec<MockCallConfig>>,
    call_history: Mutex<Vec<MockCallRecord>>,
    call_count: AtomicUsize,
    error_on_call: AtomicUsize,
    delay_ms: AtomicU64,
    tick: AtomicUsize,
}

impl MockLLMProvider {
    pub fn new(default_response: &str) -> Self {
        Self {
            default_response: default_response.to_owned(),
            call_configs: Mutex::new(Vec::new()),
            call_history: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
            error_on_call: AtomicUsize::new(0),
            delay_ms: AtomicU64::new(0),
            tick: AtomicUsize::new(0),
        }
    }

    pub fn set_error_on_call(&self, n: usize) {
        self.error_on_call.store(n, Ordering::SeqCst);
    }

    pub fn set_delay_ms(&self, ms: u64) {
        self.delay_ms.store(ms, Ordering::SeqCst);
    }

    pub fn set_call_configs(&self, configs: Vec<MockCallConfig>) {
        *self.call_configs.lock().unwrap() = configs;
    }

    pub fn call_history(&self) -> Vec<MockCallRecord> {
        self.call_history.lock().unwrap().clone()
    }

    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    fn simulate_delay(&self) {
        let ms = self.delay_ms.load(Ordering::SeqCst);
        if ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }

    fn record_call(&self, prompt: &str, messages: Vec<LlmMessage>) {
        let tick = self.tick.fetch_add(1, Ordering::SeqCst);
        self.call_history.lock().unwrap().push(MockCallRecord {
            prompt: prompt.to_owned(),
            messages,
            timestamp_ms: tick as u64,
        });
        self.call_count.fetch_add(1, Ordering::SeqCst);
    }

    fn build_result(&self, cfg: Option<MockCallConfig>) -> LlmResult {
        if let Some(cfg) = cfg {
            if cfg.should_error {
                return LlmResult {
                    content: String::new(),
                    tool_calls: vec![],
                    finish_reason: LlmFinishReason::Error,
                };
            }
            let tool_calls = cfg.tool_calls;
            let has_tc = !tool_calls.is_empty();
            return LlmResult {
                content: cfg.response_text,
                tool_calls,
                finish_reason: if has_tc { LlmFinishReason::ToolCall } else { LlmFinishReason::Stop },
            };
        }
        LlmResult {
            content: self.default_response.clone(),
            tool_calls: vec![],
            finish_reason: LlmFinishReason::Stop,
        }
    }
}

#[async_trait]
impl LlmProvider for MockLLMProvider {
    async fn complete(&self, prompt: &str) -> LlmResult {
        self.simulate_delay();
        let idx = self.call_count.load(Ordering::SeqCst);
        let should_err = self.error_on_call.load(Ordering::SeqCst) > 0
            && idx + 1 == self.error_on_call.load(Ordering::SeqCst);

        if should_err {
            self.record_call(prompt, vec![]);
            return LlmResult {
                content: String::new(),
                tool_calls: vec![],
                finish_reason: LlmFinishReason::Error,
            };
        }

        let cfg = {
            let configs = self.call_configs.lock().unwrap();
            configs.get(idx).cloned()
        };

        self.record_call(prompt, vec![]);
        self.build_result(cfg)
    }

    async fn chat(&self, messages: &[LlmMessage], tools: &[LlmToolDef]) -> LlmResult {
        self.simulate_delay();
        let idx = self.call_count.load(Ordering::SeqCst);
        let should_err = self.error_on_call.load(Ordering::SeqCst) > 0
            && idx + 1 == self.error_on_call.load(Ordering::SeqCst);

        let prompt_summary = messages
            .iter()
            .map(|m| {
                format!(
                    "{}: {}",
                    match m.role {
                        LlmRole::User => "user",
                        LlmRole::Assistant => "assistant",
                        LlmRole::System => "system",
                    },
                    m.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        if should_err {
            self.record_call(&prompt_summary, messages.to_vec());
            return LlmResult {
                content: String::new(),
                tool_calls: vec![],
                finish_reason: LlmFinishReason::Error,
            };
        }

        let cfg = {
            let configs = self.call_configs.lock().unwrap();
            configs.get(idx).cloned()
        };

        self.record_call(&prompt_summary, messages.to_vec());

        // If config has no explicit tool_calls but tools were provided,
        // simulate the LLM calling all of them
        let mut result = self.build_result(cfg);
        if result.tool_calls.is_empty() && !tools.is_empty() {
            result.tool_calls = tools
                .iter()
                .map(|t| LlmToolCall {
                    name: t.name.clone(),
                    arguments: "{}".to_owned(),
                })
                .collect();
            result.finish_reason = LlmFinishReason::ToolCall;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_default_response() {
        let mock = MockLLMProvider::new("Hello, world!");
        let result = mock.complete("test").await;
        assert_eq!(result.content, "Hello, world!");
        assert_eq!(result.finish_reason, LlmFinishReason::Stop);
    }

    #[tokio::test]
    async fn records_call_history() {
        let mock = MockLLMProvider::new("ok");
        mock.complete("first call").await;
        mock.complete("second call").await;
        assert_eq!(mock.call_count(), 2);
        let history = mock.call_history();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].prompt, "first call");
        assert_eq!(history[1].prompt, "second call");
    }

    #[tokio::test]
    async fn error_on_nth_call() {
        let mock = MockLLMProvider::new("ok");
        mock.set_error_on_call(3);

        let r1 = mock.complete("one").await;
        assert_eq!(r1.finish_reason, LlmFinishReason::Stop);

        let r2 = mock.complete("two").await;
        assert_eq!(r2.finish_reason, LlmFinishReason::Stop);

        let r3 = mock.complete("three").await;
        assert_eq!(r3.finish_reason, LlmFinishReason::Error);

        let r4 = mock.complete("four").await;
        assert_eq!(r4.finish_reason, LlmFinishReason::Stop);
    }

    #[tokio::test]
    async fn uses_per_call_config() {
        let mock = MockLLMProvider::new("default");
        mock.set_call_configs(vec![
            MockCallConfig {
                response_text: "first response".to_owned(),
                ..Default::default()
            },
            MockCallConfig {
                response_text: "second response".to_owned(),
                tool_calls: vec![LlmToolCall {
                    name: "get_weather".to_owned(),
                    arguments: r#"{"city": "London"}"#.to_owned(),
                }],
                ..Default::default()
            },
        ]);

        let r1 = mock.complete("hello").await;
        assert_eq!(r1.content, "first response");

        let r2 = mock.complete("weather?").await;
        assert_eq!(r2.content, "second response");
        assert_eq!(r2.tool_calls.len(), 1);
        assert_eq!(r2.tool_calls[0].name, "get_weather");

        let r3 = mock.complete("bye").await;
        assert_eq!(r3.content, "default");
    }

    #[tokio::test]
    async fn chat_sends_tool_defs() {
        let mock = MockLLMProvider::new("I'll use a tool");
        let msgs = vec![LlmMessage {
            role: LlmRole::User,
            content: "What's the weather?".to_owned(),
        }];
        let tools = vec![LlmToolDef {
            name: "get_weather".to_owned(),
            description: "Get weather for a city".to_owned(),
        }];
        let result = mock.chat(&msgs, &tools).await;
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "get_weather");
    }

    #[tokio::test]
    async fn error_config_generates_error() {
        let mock = MockLLMProvider::new("ok");
        mock.set_call_configs(vec![MockCallConfig {
            response_text: "won't see this".to_owned(),
            should_error: true,
            ..Default::default()
        }]);
        let result = mock.complete("hello").await;
        assert_eq!(result.finish_reason, LlmFinishReason::Error);
    }
}
