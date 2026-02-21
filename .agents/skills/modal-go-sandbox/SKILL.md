---
name: modal-go-sandbox
description: |
  Translate Sauron sandbox lifecycle behavior to Modal's Go SDK.
  Use this when updating browser sandbox startup, connect token flow,
  or stop/health behavior backed by modal-labs/libmodal.
---

# modal-go-sandbox

Use `wit` to ground implementation details before changing lifecycle code:

1. Start with files-only search:
   - `wit rg -l 'CreateConnectToken|SandboxCreate|Terminate|Poll|FromID' modal-labs/libmodal`
2. Open the reference example:
   - `wit cat modal-labs/libmodal modal-go/examples/sandbox-connect-token/main.go`
3. Confirm SDK behavior in core implementation:
   - `wit sed -n '280,980p' modal-labs/libmodal modal-go/sandbox.go`
   - `wit sed -n '55,120p' modal-labs/libmodal modal-go/app.go`
   - `wit sed -n '170,235p' modal-labs/libmodal modal-go/image.go`
   - `wit sed -n '1,180p' modal-labs/libmodal modal-go/secret.go`
   - `wit sed -n '1,120p' modal-labs/libmodal modal-go/doc.go`

## Sauron mapping
- App lookup: `client.Apps.FromName(..., CreateIfMissing: true)`
- Sandbox start: `client.Sandboxes.Create(...)` with chromium bootstrap command
- Secret injection: `client.Secrets.FromName(...)` + `SandboxCreateParams.Secrets`
- Dotenv injection: parse local `.env` and append `client.Secrets.FromMap(...)` result to `SandboxCreateParams.Secrets`
- Direct CDP access: expose encrypted tunnel port `9222` via `SandboxCreateParams.H2Ports`, run Chromium debug on loopback (`127.0.0.1:9223`), and forward via `socat` (still no connect-token proxying)
- Dev server tunnels: `SandboxCreateParams.H2Ports` + `sb.Tunnels(...)`
- Credentials (legacy proxy mode only): `sb.CreateConnectToken(...)`
- Browser WS discovery for Playwright/Puppeteer: `GET <cdp_tunnel_url>/json/version` with `Host: localhost` (Chromium rejects non-localhost host headers on devtools HTTP endpoints)
- Stop: `sb.Terminate(...)`
- Liveness check: `sb.Poll(...)` (nil exit code means still running)

## External dependency
- Go package: `github.com/modal-labs/libmodal/modal-go`

## Verbose output note
- There is no direct `modal.enable_output()` equivalent in `modal-go`.
- `modal-go/image.go` currently ignores build stream log/progress entries in `waitForBuildIteration`.
- To expose lifecycle details, create the client with `modal.NewClientWithOptions` and pass a debug logger (`ClientParams.Logger`) or set `MODAL_LOGLEVEL=DEBUG`.
- SDK logs default to stderr unless you provide a custom logger output destination.
