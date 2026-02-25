# Spec: `--viewport` flag on `sauron start`

## Problem

The default headless viewport is 756x469 (Chrome's default), which is too small for meaningful layout analysis. Agents comparing screenshots to user-reported issues get confused by layout differences caused by the viewport mismatch, not actual bugs. There is currently no way to set viewport size without manual `Emulation.setDeviceMetricsOverride` CDP calls via `sauron eval`.

## Proposal

Add `--viewport <WxH>` to `sauron start`.

### CLI surface

```
sauron start [--viewport <WxH>]
```

Examples:
```bash
sauron start --webgl                          # uses default 1280x800
sauron start --webgl --viewport 1440x900      # explicit desktop
sauron start --webgl --viewport 390x844       # iPhone 14 Pro
```

### Default

**1280x800** when `--viewport` is not specified. Rationale:
- Covers most desktop layouts without being wastefully large for screenshot file sizes
- Standard laptop proportion (16:10)
- Matches common Chromium headless testing defaults
- Large enough that responsive breakpoints (typically 768px, 1024px) are correctly triggered

### Implementation

1. Add `--viewport` option to `StartOpts` in `main.rs`:
   ```rust
   #[arg(long, default_value = "1280x800")]
   viewport: String,
   ```

2. Parse `WxH` string into `(u32, u32)` with validation (both dimensions > 0, reasonable max like 3840x2160).

3. In `daemon.rs`, replace the hardcoded `--window-size=1,1` with `--window-size={w},{h}` for headless mode. For headed mode, apply as initial window size.

4. After CDP connection is established, call `Emulation.setDeviceMetricsOverride` to set the viewport precisely:
   ```json
   {
     "width": 1280,
     "height": 800,
     "deviceScaleFactor": 1,
     "mobile": false
   }
   ```
   This ensures the viewport matches regardless of Chrome's window chrome.

5. Store the viewport dimensions in `RuntimeSessionRecord` so other commands can reference them.

### DPR

Default `deviceScaleFactor: 1`. This keeps screenshot file sizes manageable for agent context windows. Could add `--dpr <N>` later if pixel-level detail is needed, but 1x is sufficient for layout and visual checks.
