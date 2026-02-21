# Sauron Agent Guide

## Project shape
- CLI entrypoint: `cmd/sauron/main.go`
- Command wiring/UI: `internal/cli/`
- Sandbox lifecycle logic: `internal/sauron/`
- Architecture decisions: `adrs/`
- Reusable project skills: `.agents/skills/`

## Quality gates
- Run `gofmt -w` on touched Go files.
- Run `go test ./...` before finishing.

## Skill iteration rule
- Treat `.agents/skills/*/SKILL.md` as living project memory for external dependencies.
- When you learn new stable usage patterns for Modal Go SDK, Cobra, or glamour, update the relevant skill in the same change set.
- Keep skill docs focused on concrete lookup targets (repos/files/commands), not general theory.

## CDP tunnel reliability
- For CDP discovery via `/json/version` over Modal tunnels, force HTTP/1.1 and send `Host: localhost`; surface that host header in returned `connect_headers`.
- When exposing CDP externally, proxy Chromium’s loopback listener to `0.0.0.0` (for example with `socat`) and verify with a live `agent-browser` or `curl` smoke test after tunnel changes.
- Guard Modal SDK RPCs used by `start` with hard timeouts and make client shutdown non-blocking so CLI does not hang; keep a timeout regression test covering tunnel lookup.
- If CDP discovery times out, still return `browser_ws_url` + `connect_headers` with a clear error message; do not block CLI on discovery.
- Reject `https://` browse URLs for CDP with an explicit error pointing users to `browser_ws_url` and required headers.

## CLI robustness + operator behavior
- Avoid intermediate status narration while researching or implementing; deliver one consolidated update unless the user explicitly requests step-by-step updates.
- Do not default new CLI flags to values that assume optional external resources (for example secrets); keep new inputs opt-in and preserve prior behavior by default.
- Timebox network discovery/retry loops in CLI paths with a hard cap to prevent hangs; prefer returning partial data with a clear error.
