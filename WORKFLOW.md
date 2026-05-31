# Workflow Definition Guide

Workflows model long-running business processes as state machines with timeouts, guards, and error recovery.

## Workflow Definition

Workflows are defined programmatically using the `WorkflowDef` struct:

```rust
use aman_sdk::prelude::*;

let approval_workflow = WorkflowDef {
    name: "invoice-approval".to_owned(),
    states: vec![
        StateDef { name: "PENDING".to_owned() },
        StateDef { name: "REVIEWING".to_owned() },
        StateDef { name: "APPROVED".to_owned() },
        StateDef { name: "REJECTED".to_owned() },
        StateDef { name: "ERROR".to_owned() },
        StateDef { name: "ARCHIVED".to_owned() },
    ],
    initial_state: "PENDING".to_owned(),
    final_states: vec![
        "APPROVED".to_owned(),
        "REJECTED".to_owned(),
        "ARCHIVED".to_owned(),
    ],
    error_state: "ERROR".to_owned(),
    transitions: vec![
        Transition {
            from: TransitionFrom::Specific("PENDING".to_owned()),
            event: "SUBMIT".to_owned(),
            to: TransitionTo::Specific("REVIEWING".to_owned()),
            guard: None, on_fail: None, action: None, on_action_failure: None,
        },
        Transition {
            from: TransitionFrom::Specific("REVIEWING".to_owned()),
            event: "APPROVE".to_owned(),
            to: TransitionTo::Specific("APPROVED".to_owned()),
            guard: Some("has_permission".to_owned()),
            on_fail: Some(TransitionTo::Specific("REJECTED".to_owned())),
            action: None, on_action_failure: None,
        },
        Transition {
            from: TransitionFrom::Specific("REVIEWING".to_owned()),
            event: "REJECT".to_owned(),
            to: TransitionTo::Specific("REJECTED".to_owned()),
            guard: None, on_fail: None, action: None, on_action_failure: None,
        },
        Transition {
            from: TransitionFrom::Specific("ERROR".to_owned()),
            event: "RETRY".to_owned(),
            to: TransitionTo::LastActiveState,
            guard: None, on_fail: None, action: None, on_action_failure: None,
        },
        Transition {
            from: TransitionFrom::Specific("ERROR".to_owned()),
            event: "CANCEL".to_owned(),
            to: TransitionTo::Specific("REJECTED".to_owned()),
            guard: None, on_fail: None, action: None, on_action_failure: None,
        },
    ],
    state_timeouts: vec![
        StateTimeout {
            state: "REVIEWING".to_owned(),
            timeout_ms: 86_400_000,  // 24h in ms
            on_timeout: TransitionTo::Specific("REJECTED".to_owned()),
            on_timeout_alert: Some("review-timeout".to_owned()),
        },
    ],
    error_recovery: ErrorRecovery {
        auto_retry_count: 0,
        max_retry_count: 3,
        retry_backoff: RetryBackoff::Exponential,
        on_retry_failure: RetryFailurePolicy::Archive,
    },
};
```

## State Machine Diagram

```
            SUBMIT
  PENDING ──────────▶ REVIEWING ──────APPROVE─────▶ APPROVED
                       │                              │
                  REJECT│                              │ (30d archive)
                       │                              │
                       ▼                              ▼
                     REJECTED                     ARCHIVED

                       ERROR ────RETRY────▶ REVIEWING
                          │
                       CANCEL
                          │
                          ▼
                       REJECTED
```

## Transitions

Each `Transition` defines a possible state change triggered by an event:

| Field | Type | Description |
|---|---|---|
| `from` | `TransitionFrom` | Source state (`Specific(name)` or `Any`) |
| `event` | `String` | Event that triggers this transition |
| `to` | `TransitionTo` | Target state (`Specific(name)` or `LastActiveState`) |
| `guard` | `Option<String>` | Name of a registered guard |
| `on_fail` | `Option<TransitionTo>` | Where to go if the guard rejects |
| `action` | `Option<TransitionAction>` | Pipeline or Skill to run on transition |
| `on_action_failure` | `Option<TransitionTo>` | Where to go if the action fails (defaults to error_state) |

### Wildcard transitions

Use `TransitionFrom::Any` to match any state. Use `TransitionTo::LastActiveState` to return to the state the instance was in before entering `ERROR`.

### Actions

Transitions can run a Pipeline or Skill as a side effect:

```rust
Transition {
    from: TransitionFrom::Specific("REVIEWING".to_owned()),
    event: "APPROVE".to_owned(),
    to: TransitionTo::Specific("APPROVED".to_owned()),
    guard: None,
    on_fail: None,
    action: Some(TransitionAction::Pipeline("approval-pipeline".to_owned())),
    on_action_failure: Some(TransitionTo::Specific("ERROR".to_owned())),
}
```

If an action fails, the instance transitions to `on_action_failure` (or the workflow's `error_state` by default). Pipeline actions with `partial_rollback` set require an `idempotency_key` on retry.

## Timeouts

States can define timeout transitions that fire when a state is occupied too long:

```rust
StateTimeout {
    state: "REVIEWING".to_owned(),
    timeout_ms: 86_400_000,  // 24h in ms
    on_timeout: TransitionTo::Specific("REJECTED".to_owned()),
    on_timeout_alert: Some("review-timeout".to_owned()),
}
```

Timeouts use an elapsed-time clock with three exit modes:
- **Pause** (default on state change) — remaining time is preserved when leaving the state
- **Reset** — remaining time resets to the full duration on re-entry
- **Continue** — remaining time is not adjusted on exit

When a pipeline action fails and the instance enters the `ERROR` state, the timeout for the last active state is paused. On `RETRY`, the timeout resumes with the remaining time intact.

## Error Recovery

When a Pipeline action fails, the workflow enters the `ERROR` state:

```rust
ErrorRecovery {
    auto_retry_count: 0,
    max_retry_count: 3,
    retry_backoff: RetryBackoff::Exponential,
    on_retry_failure: RetryFailurePolicy::Archive,
}
```

`ErrorRecovery` fields:

| Field | Type | Default | Description |
|---|---|---|---|
| `auto_retry_count` | `u32` | `0` | Number of automatic retries before requiring an external event |
| `max_retry_count` | `u32` | `3` | Maximum retries across the instance lifetime |
| `retry_backoff` | `RetryBackoff` | `Immediate` | Delay strategy between retries |
| `on_retry_failure` | `RetryFailurePolicy` | `ManualOnly` | What to do after exhausting retries |

`RetryBackoff` variants:

| Variant | Behavior |
|---|---|
| `Immediate` | No delay |
| `Fixed(ms)` | Constant delay in ms |
| `Exponential` | Doubling delay: 100, 200, 400, ... ms |
| `Sequence(ms_vec)` | Step-by-step delays from list |

Recovery policies:

| Policy | Behavior |
|---|---|
| `Archive` | Move instance to the `ARCHIVED` state after max retries |
| `ManualOnly` | Leave instance in `ERROR` state for external intervention |

## Guards

Guards conditionally allow or deny transitions:

```rust
Transition {
    from: TransitionFrom::Specific("REVIEWING".to_owned()),
    event: "APPROVE".to_owned(),
    to: TransitionTo::Specific("APPROVED".to_owned()),
    guard: Some("has_permission".to_owned()),
    on_fail: Some(TransitionTo::Specific("REJECTED".to_owned())),
    action: None,
    on_action_failure: None,
}
```

Built-in guards:

| Guard | Description |
|---|---|
| `has_permission` | Checks event payload for `has_permission: true` |
| `max_retry` | Rejects retry when `total_retry_count` >= the guard's `max_retry_count` |

Custom guards can be registered on the `WorkflowEngine` via `register_guard()`.

## API Operations

```bash
# List workflow definitions
curl http://localhost:9090/workflows

# Get workflow definition
curl http://localhost:9090/workflow/invoice-approval

# Create a workflow instance
curl -X POST http://localhost:9090/workflow/invoice-approval/create \
  -H "Content-Type: application/json" \
  -d '{"data": {"ticket": "A-1"}}'

# List workflow instances
curl http://localhost:9090/workflow-instances

# Get workflow instance
curl http://localhost:9090/workflow-instance/INSTANCE_ID

# Retry failed instance
curl -X POST http://localhost:9090/workflow-instance/INSTANCE_ID/retry \
  -H "x-aman-confirm: yes" \
  -H "x-aman-operator: alice"

# Cancel instance
curl -X POST http://localhost:9090/workflow-instance/INSTANCE_ID/cancel \
  -H "x-aman-confirm: yes"
```
