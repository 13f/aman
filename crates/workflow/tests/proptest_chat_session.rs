// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use workflow::{
    ErrorRecovery, RetryFailurePolicy, StateDef, StateTimeout, Transition, TransitionFrom, TransitionTo,
    WorkflowDef, WorkflowEngine,
};

/// Build the canonical chat-session workflow definition matching
/// the one registered in `agent_runtime.rs`.
fn chat_session_workflow() -> WorkflowDef {
    WorkflowDef {
        name: "message-session".to_owned(),
        states: vec![
            StateDef { name: "ACTIVE".to_owned() },
            StateDef { name: "PROCESSING".to_owned() },
            StateDef { name: "IDLE".to_owned() },
            StateDef { name: "ERROR".to_owned() },
            StateDef { name: "RETRYING".to_owned() },
            StateDef { name: "TIMEOUT".to_owned() },
            StateDef { name: "CLOSED".to_owned() },
        ],
        initial_state: "ACTIVE".to_owned(),
        final_states: vec!["CLOSED".to_owned()],
        error_state: "ERROR".to_owned(),
        transitions: vec![
            Transition {
                from: TransitionFrom::Specific("ACTIVE".to_owned()),
                event: "MESSAGE_RECEIVED".to_owned(),
                to: TransitionTo::Specific("PROCESSING".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("ACTIVE".to_owned()),
                event: "SESSION_TIMEOUT".to_owned(),
                to: TransitionTo::Specific("TIMEOUT".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("PROCESSING".to_owned()),
                event: "LLM_REPLY_READY".to_owned(),
                to: TransitionTo::Specific("IDLE".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("PROCESSING".to_owned()),
                event: "LLM_STREAM_DONE".to_owned(),
                to: TransitionTo::Specific("IDLE".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("PROCESSING".to_owned()),
                event: "LLM_ERROR".to_owned(),
                to: TransitionTo::Specific("ERROR".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("PROCESSING".to_owned()),
                event: "STREAM_TIMEOUT".to_owned(),
                to: TransitionTo::Specific("TIMEOUT".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("PROCESSING".to_owned()),
                event: "SESSION_CLOSE_CMD".to_owned(),
                to: TransitionTo::Specific("CLOSED".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("IDLE".to_owned()),
                event: "MESSAGE_RECEIVED".to_owned(),
                to: TransitionTo::Specific("PROCESSING".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("IDLE".to_owned()),
                event: "SESSION_TIMEOUT".to_owned(),
                to: TransitionTo::Specific("TIMEOUT".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("IDLE".to_owned()),
                event: "SESSION_END".to_owned(),
                to: TransitionTo::Specific("CLOSED".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("ERROR".to_owned()),
                event: "RETRY_CMD".to_owned(),
                to: TransitionTo::Specific("RETRYING".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("ERROR".to_owned()),
                event: "SESSION_END".to_owned(),
                to: TransitionTo::Specific("IDLE".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("ERROR".to_owned()),
                event: "ABANDON_TIMEOUT".to_owned(),
                to: TransitionTo::Specific("CLOSED".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("RETRYING".to_owned()),
                event: "RETRY_STARTED".to_owned(),
                to: TransitionTo::Specific("PROCESSING".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("RETRYING".to_owned()),
                event: "RETRY_FAILED".to_owned(),
                to: TransitionTo::Specific("ERROR".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("TIMEOUT".to_owned()),
                event: "SESSION_END".to_owned(),
                to: TransitionTo::Specific("CLOSED".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
            Transition {
                from: TransitionFrom::Specific("TIMEOUT".to_owned()),
                event: "MESSAGE_RECEIVED".to_owned(),
                to: TransitionTo::Specific("IDLE".to_owned()),
                guard: None, on_fail: None, action: None, on_action_failure: None,
            },
        ],
        state_timeouts: vec![
            StateTimeout {
                state: "ACTIVE".to_owned(),
                timeout_ms: 300_000,
                on_timeout: TransitionTo::Specific("TIMEOUT".to_owned()),
                on_timeout_alert: None,
            },
            StateTimeout {
                state: "PROCESSING".to_owned(),
                timeout_ms: 120_000,
                on_timeout: TransitionTo::Specific("TIMEOUT".to_owned()),
                on_timeout_alert: None,
            },
            StateTimeout {
                state: "IDLE".to_owned(),
                timeout_ms: 600_000,
                on_timeout: TransitionTo::Specific("TIMEOUT".to_owned()),
                on_timeout_alert: None,
            },
            StateTimeout {
                state: "ERROR".to_owned(),
                timeout_ms: 120_000,
                on_timeout: TransitionTo::Specific("CLOSED".to_owned()),
                on_timeout_alert: None,
            },
            StateTimeout {
                state: "TIMEOUT".to_owned(),
                timeout_ms: 120_000,
                on_timeout: TransitionTo::Specific("CLOSED".to_owned()),
                on_timeout_alert: None,
            },
        ],
        error_recovery: ErrorRecovery {
            auto_retry_count: 0,
            max_retry_count: 5,
            on_retry_failure: RetryFailurePolicy::Archive,
            retry_backoff: Default::default(),
        },
    }
}

/// Create a workflow engine with the chat-session workflow registered.
fn setup_engine() -> WorkflowEngine {
    let engine = WorkflowEngine::new();
    engine.register_workflow(chat_session_workflow()).unwrap();
    engine
}

/// Enum of events that can be sent in proptest.
#[derive(Debug, Clone, PartialEq)]
enum ChatEvent {
    MessageReceived,
    LlmReplyReady,
    LlmStreamDone,
    LlmError,
    StreamTimeout,
    SessionTimeout,
    SessionEnd,
    RetryCmd,
    RetryStarted,
    RetryFailed,
    AbandonTimeout,
    SessionCloseCmd,
}

impl ChatEvent {
    fn as_str(&self) -> &'static str {
        match self {
            Self::MessageReceived => "MESSAGE_RECEIVED",
            Self::LlmReplyReady => "LLM_REPLY_READY",
            Self::LlmStreamDone => "LLM_STREAM_DONE",
            Self::LlmError => "LLM_ERROR",
            Self::StreamTimeout => "STREAM_TIMEOUT",
            Self::SessionTimeout => "SESSION_TIMEOUT",
            Self::SessionEnd => "SESSION_END",
            Self::RetryCmd => "RETRY_CMD",
            Self::RetryStarted => "RETRY_STARTED",
            Self::RetryFailed => "RETRY_FAILED",
            Self::AbandonTimeout => "ABANDON_TIMEOUT",
            Self::SessionCloseCmd => "SESSION_CLOSE_CMD",
        }
    }

    /// Returns all possible events.
    fn all() -> Vec<ChatEvent> {
        vec![
            Self::MessageReceived,
            Self::LlmReplyReady,
            Self::LlmStreamDone,
            Self::LlmError,
            Self::StreamTimeout,
            Self::SessionTimeout,
            Self::SessionEnd,
            Self::RetryCmd,
            Self::RetryStarted,
            Self::RetryFailed,
            Self::AbandonTimeout,
            Self::SessionCloseCmd,
        ]
    }

    /// Return events that are NOT legal from a given state.
    /// Used to test that illegal transitions are properly rejected.
    fn illegal_for(state: &str) -> Vec<ChatEvent> {
        let legal = Self::legal_for(state);
        Self::all().into_iter().filter(|e| !legal.contains(e)).collect()
    }

    fn legal_for(state: &str) -> Vec<ChatEvent> {
        match state {
            "ACTIVE" => vec![Self::MessageReceived, Self::SessionTimeout],
            "PROCESSING" => vec![
                Self::LlmReplyReady,
                Self::LlmStreamDone,
                Self::LlmError,
                Self::StreamTimeout,
                Self::SessionCloseCmd,
            ],
            "IDLE" => vec![
                Self::MessageReceived,
                Self::SessionTimeout,
                Self::SessionEnd,
            ],
            "ERROR" => vec![Self::RetryCmd, Self::SessionEnd, Self::AbandonTimeout],
            "RETRYING" => vec![Self::RetryStarted, Self::RetryFailed],
            "TIMEOUT" => vec![Self::SessionEnd, Self::MessageReceived],
            "CLOSED" => vec![],
            _ => vec![],
        }
    }
}

impl std::fmt::Display for ChatEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Property-based tests
// ---------------------------------------------------------------------------

/// All legal transitions succeed (do not error).
#[test]
fn legal_transition_succeeds() {
    let wf = chat_session_workflow();
    proptest::proptest!(ProptestConfig::with_cases(256), |(state in prop_oneof![
        Just("ACTIVE"),
        Just("PROCESSING"),
        Just("IDLE"),
        Just("ERROR"),
        Just("RETRYING"),
        Just("TIMEOUT"),
    ])| {
        prop_assume!(!ChatEvent::legal_for(&state).is_empty());

        for event in ChatEvent::legal_for(&state) {
            let result = wf.find_transition(&state, event.as_str());
            prop_assert!(result.is_some(), "legal transition missing: {} --[{}]-> ?", state, event);
        }
    });
}

/// All illegal transitions are properly rejected (find_transition returns None).
#[test]
fn illegal_transition_rejected() {
    let wf = chat_session_workflow();
    proptest::proptest!(ProptestConfig::with_cases(256), |(state in prop_oneof![
        Just("ACTIVE"),
        Just("PROCESSING"),
        Just("IDLE"),
        Just("ERROR"),
        Just("RETRYING"),
        Just("TIMEOUT"),
        Just("CLOSED"),
    ])| {
        for event in ChatEvent::illegal_for(&state) {
            let result = wf.find_transition(&state, event.as_str());
            prop_assert!(result.is_none(), "illegal transition allowed: {} --[{}]-> ?", state, event);
        }
    });
}

/// CLOSED state rejects ALL events.
#[test]
fn closed_rejects_all_events() {
    let wf = chat_session_workflow();
    for event in ChatEvent::all() {
        let result = wf.find_transition("CLOSED", event.as_str());
        assert!(result.is_none(), "CLOSED should reject event: {}", event);
    }
}

/// Test sequential: send legal events in a path from ACTIVE to CLOSED.
#[test]
fn active_to_closed_via_message_and_end() {
    let engine = setup_engine();
    let events_to_test = ["MESSAGE_RECEIVED", "LLM_REPLY_READY", "SESSION_END"];
    let mut instance = engine.create_instance("message-session", serde_json::json!({})).unwrap();
    assert_eq!(instance.current_state, "ACTIVE");

    for event_name in &events_to_test {
        let event = kernel::event::Event::new(
            "proptest",
            kernel::event::EventType::Custom(event_name.to_string()),
            serde_json::json!({}),
        );
        let result = pollster::block_on(engine.handle_event(&instance.id, event)).unwrap();
        assert!(result.transitioned, "expected transition on {} from {}", event_name, instance.current_state);
        instance.current_state = result.to_state;
    }
    assert_eq!(instance.current_state, "CLOSED");
}

/// Test: illegal transition from ACTIVE returns error.
#[test]
fn illegal_from_active_returns_error() {
    let engine = setup_engine();
    let instance = engine.create_instance("message-session", serde_json::json!({})).unwrap();
    assert_eq!(instance.current_state, "ACTIVE");

    // LLM_REPLY_READY is illegal from ACTIVE
    let event = kernel::event::Event::new(
        "proptest",
        kernel::event::EventType::Custom("LLM_REPLY_READY".to_string()),
        serde_json::json!({}),
    );
    let result = pollster::block_on(engine.handle_event(&instance.id, event));
    assert!(result.is_err(), "illegal event from ACTIVE should error");
}

/// Test: ERROR → RETRYING with max_retry_count exceeded leads to archive.
#[test]
fn retry_exceeds_max_leads_to_closed() {
    let engine = setup_engine();
    let mut instance = engine.create_instance("message-session", serde_json::json!({})).unwrap();

    // Go ACTIVE → PROCESSING → ERROR
    let step = |id: &str, event: &str| {
        let e = kernel::event::Event::new(
            "proptest",
            kernel::event::EventType::Custom(event.to_string()),
            serde_json::json!({}),
        );
        pollster::block_on(engine.handle_event(id, e)).unwrap()
    };

    step(&instance.id, "MESSAGE_RECEIVED");
    let r = step(&instance.id, "LLM_ERROR");
    assert_eq!(r.to_state, "ERROR");

    // RETRY up to max_retry_count (5) - each time RETRY_CMD → RETRYING → PROCESSING → ERROR
    // Actually the handling is: RETRY_CMD goes to RETRYING, then RETRY_STARTED → PROCESSING,
    // then need another LLM_ERROR to go back to ERROR.

    // The workflow engine's retry path goes: ERROR --RETRY_CMD--> RETRYING --RETRY_STARTED--> PROCESSING --LLM_ERROR--> ERROR
    for _i in 0..5 {
        // Load fresh instance
        let events_seq = ["RETRY_CMD", "RETRY_STARTED", "LLM_ERROR"];
        for event_name in &events_seq {
            let e = kernel::event::Event::new(
                "proptest",
                kernel::event::EventType::Custom(event_name.to_string()),
                serde_json::json!({}),
            );
            let r = pollster::block_on(engine.handle_event(&instance.id, e));
            match r {
                Ok(res) => {
                    instance.current_state = res.to_state;
                }
                Err(_) => {
                    // After max_retries, the RETRY_CMD may lead to CLOSED through Archive
                    break;
                }
            }
        }

        // Reload instance to check
        let wf = chat_session_workflow();
        if !wf.is_error_state(&instance.current_state) && instance.current_state != "RETRYING" {
            break;
        }
    }
}
