# ADR 0002: Agent-Ready Browser Workspace Startup

- Status: Accepted
- Date: 2026-02-20

## Context

Sauron starts a Chromium CDP sandbox and returns connection details for browser clients.
To support AI agents working directly against real codebases, startup now needs to:

1. Provision a runtime with Node tooling.
2. Inject a pre-existing Modal secret for private GitHub access.
3. Clone a repository before browser automation begins.
4. Optionally start a development server in parallel with CDP.
5. Return connection details that Playwright/Puppeteer can consume without custom glue.

## Decision

1. Extend `start` with repo/dev/runtime/secret flags and map them through manager options.
2. Add `--from-dotenv` so local project dotenv values can be injected as an ephemeral Modal secret.
3. Default runtime to Node 20 image and include `chromium`, `socat`, `git`, and `gh` in the built image.
4. Resolve the named Modal secret (`--secret`, for example `github`) and attach it in `SandboxCreateParams.Secrets`.
5. Parse dotenv entries locally and append them using `client.Secrets.FromMap(...)`.
6. Run a bootstrap script inside sandbox startup that:
   - configures GitHub auth for git/gh
   - clones repo + checks out optional ref
   - starts optional dev command in the background
   - starts Chromium with remote debugging on loopback (`127.0.0.1:9223`) and forwards tunneled port `9222` via `socat`.
7. Expose CDP on encrypted tunnel port `9222` (no connect token flow) and return that URL in startup output.
8. Expose optional dev server port via `H2Ports` and return tunnel URL when available.
9. Discover browser websocket URL from `/json/version` during startup and return it in `StartResult`.
10. Force `Host: localhost` for CDP discovery requests; Chromium rejects non-localhost host headers on devtools endpoints.

## Consequences

- Agents can connect directly with `playwright.chromium.connectOverCDP(...)`, `puppeteer.connect(...)`, or `agent-browser --cdp ...` using CLI JSON output.
- `agent-browser --cdp <https_url>` may fail on Chromium host-header checks; prefer the returned `browser_ws_url` for headerless CLI compatibility.
- Private repository cloning works without extra manual auth setup when the Modal secret carries GitHub token env vars.
- Startup is more capable but has more moving parts (secret resolution, repo clone, tunnel lookup, websocket discovery), so failures are surfaced with clearer context.
