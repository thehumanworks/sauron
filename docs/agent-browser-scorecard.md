# sauron vs agent-browser Scorecard (Desktop Browser Scope)

This scorecard compares `sauron` and `agent-browser` for desktop browser automation after the parity-plus implementation in this repository.

## Scope and Method

- Scope: desktop browser interaction only.
- Environment: local machine run from this repository.
- Benchmark harness: `scripts/benchmark-matrix.sh`.
- Latest run artifact: `benchmarks/latest-scorecard.json`.

## Scenario Results

Latest benchmark (`BENCH_RUNS=3`, generated `2026-03-14T21:22:07Z`):

- `sauron_open`: 242 ms avg
- `agent_browser_open`: 269 ms avg
- `sauron_snapshot`: 94 ms avg
- `agent_browser_snapshot`: 229 ms avg
- `sauron_wait_networkidle`: 690 ms avg
- `agent_browser_wait`: 1025 ms avg

These numbers are local-run directional metrics, not universal absolutes. Re-run with the harness on your target machine for authoritative numbers.

## Capability Comparison

### Operator UX

- `sauron` now has flat task-first verbs (`open`, `snapshot`, `click`, `fill`, `wait`, `get`, `is`, `find`, `close`, `pdf`, `download`) with auto-runtime support.
- Legacy grouped surface remains stable (`runtime/page/input/tab/state/...`) for existing workflows.
- Config layering is explicit and inspectable via `sauron config show`.

### Agent Readiness

- `sauron` includes structured target provenance (`ResolvedTarget`) and machine-readable node snapshots (`--format nodes`).
- Artifact output can be switched to `manifest`/`path` to avoid large inline payloads.
- `collect --bundle` provides a handoff package pattern for stateless continuation.

### Safety and Governance

- Policy engine supports `--policy safe|confirm|open`, allow rules, and policy files.
- Policy decisions are surfaced in command output and blocked actions fail before side effects.
- Error envelopes now include structured recovery hints.

### Performance and Memory

- Flat warm-path benchmark scenarios show lower or comparable local latency against `agent-browser` in this run.
- Artifact manifests reduce stdout/base64 pressure for screenshot and collect paths.
- Broker/engine/cache layers are in place as scaffolding for deeper warm-path and memory optimization.

### State Persistence

- Saved state now includes `cookies`, `localStorage`, and `sessionStorage`.
- State management now includes `state show`, `state rename`, `state clear`, and `state clean`.

### Observability

- Existing runtime/session logging remains intact.
- Snapshot node output and target-resolution metadata improve traceability for agent decisions.

## Current Superiority Points (from this revision)

- UX: flat verbs + config layering without breaking grouped compatibility.
- Agent-readiness: manifest-first artifact controls and structured target/recovery metadata.
- Performance: local benchmark run shows faster `snapshot` and `wait` averages.
- Memory/output safety: manifest/path artifact modes avoid default large inline binary payloads for flat commands.

## Remaining Follow-up

- Promote broker execution path from scaffolding to default-ready path once additional soak and benchmark cycles are complete.
- Add deeper memory profiling outputs into the benchmark harness for engine/pool comparisons.
