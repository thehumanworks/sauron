# ADR 0001: Go CLI Architecture for Sauron

- Status: Accepted
- Date: 2026-02-20

## Context

Sauron started as a Python script that directly created a Modal sandbox and printed CDP connect credentials.  
The project now needs a maintainable Go CLI with explicit commands (`start`, `stop`, `health`) and testable lifecycle logic.

## Decision

1. Build the CLI with Cobra.
2. Keep command handlers thin and push lifecycle behavior into `internal/sauron`.
3. Isolate Modal SDK calls behind a `SandboxService` interface so manager logic is testable without network calls.
4. Persist active sandbox state in a local JSON file to support `stop` and `health` across separate CLI invocations.
5. Render human-oriented output with glamour (Markdown -> terminal), with `--json` for machine-readable output.

## Consequences

- We can unit test start/stop/health orchestration without mocking gRPC internals.
- `health` can return a deterministic hard-timeout countdown (`non-idle remaining`) based on saved start timestamp.
- Modal-specific behavior is centralized in one adapter (`internal/sauron/modal_service.go`), reducing coupling to Cobra code.
