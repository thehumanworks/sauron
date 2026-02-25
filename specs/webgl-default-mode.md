# Spec: Make `--webgl` the default mode

## Problem

Modern web apps use Canvas, WebGL, Three.js, R3F, and GPU-heavy rendering extensively. The current default (no WebGL, GPU disabled) fails to render these pages correctly — agents see blank canvases or broken layouts. Every WebGL-heavy project requires the user to remember `sauron start --webgl`, which is friction for the common case.

The `--webgl` flag already implies `--enable-gpu`, so it is the strictly more capable mode. Headless mode works correctly for JS-heavy content including full R3F scenes with custom GLSL shaders — headed mode provides no additional rendering fidelity.

## Proposal

Make WebGL-enabled headless the default behavior of `sauron start`.

### Behavioral changes

| Current | Proposed |
|---------|----------|
| `sauron start` → headless, no GPU, no WebGL | `sauron start` → headless, GPU enabled, WebGL enabled |
| `sauron start --webgl` → headless, GPU, WebGL | Same (now a no-op, kept for backward compat) |
| `sauron start --headed` → headed, no GPU | `sauron start --headed` → headed, GPU, WebGL |

### New opt-out flags

For the rare case where WebGL/GPU is not wanted (e.g. testing pure HTML/CSS, minimizing resource usage on CI):

```bash
sauron start --no-webgl      # disable WebGL/SwiftShader
sauron start --no-gpu        # disable GPU process entirely
```

### Implementation

1. In `DaemonOpts` / `StartOpts`, change `webgl` default from `false` to `true`.

2. Change `disable_gpu` logic: GPU is now enabled by default (since WebGL implies it). The `--no-gpu` flag explicitly disables it.

3. Keep `--webgl` as an accepted flag (no-op when already default) so existing scripts and agent instructions don't break.

4. Add `--no-webgl` and `--no-gpu` flags:
   ```rust
   #[arg(long)]
   no_webgl: bool,

   #[arg(long)]
   no_gpu: bool,
   ```

5. Update Chrome args in `daemon.rs`:
   - Default: include SwiftShader/WebGL flags + GPU enabled
   - `--no-webgl`: omit SwiftShader flags
   - `--no-gpu`: add `--disable-gpu`

6. Update `README.md` to reflect the new defaults.

### Security note

The current `--webgl` help text warns "SwiftShader is less secure; use only with trusted content." Since sauron is an agent tool navigating to localhost dev servers and known URLs, this is acceptable for the default. The `--no-webgl` escape hatch exists for untrusted content if needed. Consider adding a note to `sauron start` output when WebGL is active: `WebGL: enabled (use --no-webgl to disable)`.

### Migration

- Existing `sauron start --webgl` invocations continue to work (flag becomes a no-op).
- Existing `sauron start` invocations get upgraded behavior automatically — this is the desired outcome.
- No breaking changes to JSON output contract or session record format.
