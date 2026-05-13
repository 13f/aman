# Workflow Definition Guide

Workflows model long-running business processes as state machines with timeouts, guards, and error recovery.

## Workflow Definition

Workflows are defined programmatically using the `WorkflowDef` builder:

```rust
use aman_sdk::prelude::*;

let approval_workflow = WorkflowDef::new("invoice-approval")
    .initial_state("PENDING")
    .final_states(["APPROVED", "REJECTED"])
    .error_state("ERROR")
    .state(StateDef::new("PENDING"))
    .state(StateDef::new("REVIEWING")
        .timeout(StateTimeout::new(Duration::from_secs(86400))
            .on_timeout("REJECTED")
            .on_timeout_alert(true)))
    .state(StateDef::new("APPROVED"))
    .state(StateDef::new("REJECTED"))
    .state(StateDef::new("ERROR")
        .recovery(ErrorRecovery::new()
            .max_retries(3)
            .retry_backoff(RetryBackoff::Exponential)
            .on_failure(RetryFailurePolicy::Archive)))
    .transition(Transition::new("SUBMIT")
        .from("PENDING")
        .to("REVIEWING"))
    .transition(Transition::new("APPROVE")
        .from("REVIEWING")
        .to("APPROVED")
        .guard("has_permission"))
    .transition(Transition::new("REJECT")
        .from("REVIEWING")
        .to("REJECTED"))
    .transition(Transition::new("RETRY")
        .from("ERROR")
        .to("REVIEWING"))
    .transition(Transition::new("CANCEL")
        .from("ERROR")
        .to("REJECTED"));
```

## State Machine Diagram

```
            SUBMIT
  PENDING ──────────▶ REVIEWING ──────APPROVE─────▶ APPROVED
                       │  ▲              │
                       │  │              │
                  REJECT│  │RETRY        │ (30d timeout)
                       │  │              │
                       ▼  │              ▼
                     REJECTED          ARCHIVED

                       ERROR ────RETRY────▶ REVIEWING
                          │                  ▲
                          │                  │
                       CANCEL                │
                          │                  │
                          ▼                  │
                       REJECTED ─────────────┘
```

## Timeouts

States can define timeout transitions that fire when a state is occupied too long:

```rust
StateDef::new("REVIEWING")
    .timeout(StateTimeout::new(Duration::from_secs(86400))  // 24h
        .on_timeout("REJECTED")                              // auto-reject
        .on_timeout_alert(true))                             // emit alert
```

Timeouts are paused when the instance enters an `ERROR` state and resumed on `RETRY`.

## Error Recovery

When a Pipeline action fails, the workflow enters the `ERROR` state:

```rust
ErrorRecovery::new()
    .max_retries(3)                    // Total retry limit
    .retry_backoff(RetryBackoff::Exponential)  // Delay strategy
    .on_failure(RetryFailurePolicy::Archive)   // What to do after exhausting retries
```

Recovery strategies:

| Strategy | Behavior |
|---|---|
| `Archive` | Mark instance as archived after max retries |
| `ManualOnly` | Require manual intervention |

## Guards

Guards conditionally allow or deny transitions:

```rust
Transition::new("APPROVE")
    .from("REVIEWING")
    .to("APPROVED")
    .guard("has_permission")              // Built-in permission guard
    .on_fail(TransitionTo::Specific("REJECTED"))  // Where to go if denied
```

Built-in guards:

| Guard | Description |
|---|---|
| `has_permission` | Checks operator permissions |
| `max_retry` | Prevents retry beyond limit |

## API Operations

```bash
# List workflow instances
curl http://localhost:9090/workflow/instances

# Get workflow definition
curl http://localhost:9090/workflow/def/invoice-approval

# Retry failed instance
curl -X POST http://localhost:9090/workflow/123/retry \
  -H "x-aman-confirm: yes" \
  -H "x-aman-operator: alice"

# Cancel instance
curl -X POST http://localhost:9090/workflow/123/cancel \
  -H "x-aman-confirm: yes"
```
