# sauron (Rust)

A fully Rust-native CLI for AI agents to control Chrome via the **Chrome DevTools Protocol (CDP)**.

This is a rewrite of the attached Bun/TypeScript `sauron` project as a compiled Rust binary.

## Goals

- **Agent-friendly** JSON output for all browser-facing commands
- **Fast** startup and execution (single static-ish binary)
- **Process-safe concurrency** with mandatory runtime sessions (`start` before browser commands)
- **Per-session isolation** with generated `session_id`, `instance`, and `client` IDs by default
- Optional runtime state backend in filesystem or Valkey
- Uses Chrome **`--headless=new`** by default (or `--headed` with non-obstructive window flags)

## Install

```bash
cargo install --path .
```

Or run in-place:

```bash
cargo run -- --help
```

## Quick start

Each shell/process must start a runtime session first:

```bash
sauron start
```

`start` prints:

- Session ID
- Generated instance ID
- Generated client ID
- Project binding confirmation (no export needed)

Then run browser commands from the same project directory:

```bash
sauron navigate https://example.com
sauron snapshot
sauron click @e1
```

Clean up with:

```bash
sauron terminate
```

(`sauron stop` is an alias for `sauron terminate`.)

## Mandatory session lifecycle

- Non-`start` commands require an active runtime session.
- Session resolution order is:
  - explicit `--session-id`
  - current process binding
  - current project binding
  - `SAURON_SESSION_ID` fallback
- If none resolve to an active session, commands fail with `SESSION_REQUIRED`.
- `start` auto-generates:
  - `session_id` (`sess-...`)
  - `instance` (`inst-...`)
  - `client` (`client-...`)
- You can still override IDs:

```bash
sauron --session-id mysession --instance work --client alice start
```

## Concurrent session workflow

Terminal A:

```bash
sauron start
sauron navigate https://example.com
sauron session save logged-in
```

Terminal B (independent shell/process):

```bash
sauron start
sauron navigate https://news.ycombinator.com
sauron session save baseline
```

Both sessions are isolated and can run concurrently without conflicts.

If you previously exported `SAURON_SESSION_ID`, clear it to avoid overriding project-aware routing:

```bash
unset SAURON_SESSION_ID
```

## Runtime state backends

Default backend: filesystem under `~/.sauron/runtime/`.

Valkey backend:

```bash
sauron --session-store valkey --valkey-url redis://127.0.0.1:6379/ start
sauron --session-store valkey --valkey-url redis://127.0.0.1:6379/ navigate https://example.com
sauron --session-store valkey --valkey-url redis://127.0.0.1:6379/ terminate
```

When using Valkey, use the same `--session-store` and `--valkey-url` on all commands for that session.

## Session logs

Each session writes NDJSON logs to:

`~/.sauron/runtime/logs/<session_id>.ndjson`

Each line includes timestamp, session metadata, command name, status, and error details when present.

## CLI flag placement

Global flags (`--session-id`, `--session-store`, `--valkey-url`, `--port`, etc.) must be placed before the subcommand:

```bash
sauron --session-id mysession navigate https://example.com
```

## Output contract

All **browser commands** emit exactly one JSON object to stdout:

- Success:

```json
{ "ok": true, "command": "snapshot", "data": { /* ... */ } }
```

- Error:

```json
{ "ok": false, "command": "click", "error": { "code": "ELEMENT_NOT_FOUND", "message": "...", "hint": "...", "recoverable": true } }
```

Lifecycle commands (`start`, `status`, `terminate`) print human-readable output.

## Notes

- You need a local Chrome/Chromium install.
- The daemon uses `--remote-debugging-port=<port>`.
