# Spec: Sauron CLI V2 Integration (No Backward Compatibility)

## Decision

- `v2` is the only supported CLI and response contract.
- No compatibility mode.
- No legacy aliases for v1 command names or flags.
- No mixed output modes: every command returns a JSON envelope.
- Existing v1 automation/scripts must migrate.

## Current State

- Command surface is namespaced (`runtime/page/input/tab/state/ref/logs/console/network/run`) in `src/main.rs`.
- All commands return the v2 `meta/result` envelope.
- Runtime lifecycle commands are JSON-only (`runtime start/stop/status/cleanup`).
- Selector-based input targeting and state/count-based waits are implemented.
- Console and network capture commands are implemented for fixed durations.

## V2 End State

### 1) Canonical Command Taxonomy

Use namespaced command groups with verb actions:

- `runtime`: `start`, `stop`, `status`, `cleanup`
- `page`: `goto`, `snapshot`, `screenshot`, `collect`, `markdown`, `wait`, `js`, `diff`, `dialog`
- `input`: `click`, `fill`, `hover`, `scroll`, `press`
- `tab`: `list`, `open`, `switch`, `close`
- `state`: `save`, `load`, `list`, `delete`
- `ref`: `list`, `show`, `validate`
- `logs`: `list`, `tail`
- `console`: `capture`
- `network`: `capture`
- `run`: workflow executor

### 2) Canonical Flag Conventions

- Time units are explicit: `--timeout-ms`, `--delay-ms`, `--for-ms`.
- Boolean toggles use explicit polarity pairs: `--gpu` / `--no-gpu`, `--webgl` / `--no-webgl`.
- Output path semantics are explicit: `--output` for single artifact file and `--output-dir` for multi-artifact output.
- Target-selection is consistent: `--selector`, `--ref`, `--text`, `--match-index`.
- Snapshot flags use include-style booleans: `--include-iframes`, `--interactive-only`, `--clickable-only`.

### 3) New Interaction Points

Add first-class AI-agent workflow primitives:

1. Lifecycle JSON mode becomes default behavior (no text output path).
2. Selector-based targeting across `input` actions.
3. Enhanced wait semantics: state/count-based waits.
4. Structured `page snapshot --format json`.
5. `ref` namespace for ref introspection/validation.
6. Range diff support: `page diff --from --to`.
7. Runtime log query/tail commands.
8. Console capture command.
9. Network capture command.
10. Workflow runner command (`run --file`).

### 4) V2 Response Contract

All commands return:

```json
{
  "meta": {
    "requestId": "uuid",
    "timestamp": "RFC3339",
    "durationMs": 0,
    "session": {
      "sessionId": "sess-...",
      "instanceId": "inst-...",
      "clientId": "client-..."
    }
  },
  "result": {
    "ok": true,
    "command": "page.goto",
    "data": {}
  }
}
```

Error shape:

```json
{
  "meta": {
    "requestId": "uuid",
    "timestamp": "RFC3339",
    "durationMs": 0
  },
  "result": {
    "ok": false,
    "command": "input.click",
    "error": {
      "code": "REF_STALE",
      "message": "Ref @e12 could not be resolved on the current page",
      "hint": "Run page snapshot first",
      "recoverable": true,
      "exitCode": 1,
      "category": "state",
      "retry": {
        "retryable": true,
        "afterMs": 0,
        "strategy": "after_command",
        "requires": ["page.snapshot"]
      }
    }
  }
}
```

## v1 to v2 Command Mapping (Breaking)

- `start` -> `runtime start`
- `terminate` -> `runtime stop`
- `status` -> `runtime status`
- `navigate` -> `page goto`
- `content` / `markdown` -> `page markdown`
- `eval` / `js` -> `page js`
- `key` -> `input press`
- `click` -> `input click`
- `fill` -> `input fill`
- `hover` -> `input hover`
- `scroll` -> `input scroll`
- `wait` -> `page wait`
- `diff` -> `page diff`
- `session *` -> `state *`

## Related specs

- [remove-response-version-key.md](remove-response-version-key.md): Removed `v` key from response envelope (versioning is pointless when there is no other version).

## Implementation Checklist (Completed)

1. Replaced clap command tree in `src/main.rs` with v2 taxonomy only.
2. Removed v1 aliases and legacy flat command names.
3. Introduced a single v2 envelope type in `src/types.rs`.
4. Updated `src/errors.rs` and command handlers to emit v2 `result.error`.
5. Converted lifecycle handlers (`runtime`) to JSON output.
6. Implemented `ref`, `logs`, `console`, `network`, and `run` namespaces.
7. Added selector-first target resolution in `src/browser.rs`.
8. Added structured snapshot output mode and response mode markers.
9. Updated README command examples to v2 syntax.
10. Updated tests to assert namespaced command labels and v2 envelopes.

## Verification Checklist

- `cargo test`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo build --release`
- Post-migration manual smoke (run after integration steps are complete):
- `sauron runtime start`
- `sauron page goto https://example.com`
- `sauron page snapshot --format json`
- `sauron input click --ref @e1`
- `sauron runtime stop`
