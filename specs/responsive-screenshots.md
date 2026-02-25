# Spec: Responsive screenshot command

## Problem

Agents need to verify layouts across mobile, tablet, and desktop breakpoints. Currently this requires manually resizing the viewport via CDP eval calls between screenshots — error-prone, verbose, and easy to forget. Most frontend bugs are breakpoint-specific, so single-viewport screenshots miss the majority of layout issues.

## Proposal

Add a `--responsive` flag to `sauron screenshot` that captures three screenshots at standard breakpoints in one command.

### CLI surface

```bash
sauron screenshot --responsive [--path <DIR>]
```

Output: three PNG files saved to the specified directory (or current directory):
```
screenshot-mobile.png     # 390x844  (iPhone 14 Pro)
screenshot-tablet.png     # 820x1180 (iPad Air)
screenshot-desktop.png    # 1440x900 (Standard laptop)
```

JSON output:
```json
{
  "ok": true,
  "command": "screenshot",
  "data": {
    "responsive": true,
    "screenshots": [
      { "preset": "mobile",  "width": 390,  "height": 844,  "path": "/tmp/screenshot-mobile.png",  "saved": true },
      { "preset": "tablet",  "width": 820,  "height": 1180, "path": "/tmp/screenshot-tablet.png",  "saved": true },
      { "preset": "desktop", "width": 1440, "height": 900,  "path": "/tmp/screenshot-desktop.png", "saved": true }
    ]
  }
}
```

### Presets

| Preset | Width | Height | DPR | Model |
|--------|-------|--------|-----|-------|
| mobile | 390 | 844 | 1 | iPhone 14 Pro |
| tablet | 820 | 1180 | 1 | iPad Air |
| desktop | 1440 | 900 | 1 | Standard laptop |

DPR is always 1 for agent screenshots (file size efficiency). The presets match real device dimensions used in Chrome DevTools.

### Implementation

1. Add `--responsive` bool flag to the `Screenshot` command struct.

2. When `--responsive` is set, for each preset:
   a. Call `Emulation.setDeviceMetricsOverride` with the preset dimensions:
      ```json
      { "width": W, "height": H, "deviceScaleFactor": 1, "mobile": M }
      ```
      Where `mobile: true` for the mobile preset (triggers touch events, viewport meta handling).
   b. Wait for `Page.frameResized` event or a short delay (200ms) for layout reflow.
   c. Call `Page.captureScreenshot`.
   d. Save to `{path}/screenshot-{preset}.png`.

3. After all captures, restore the original viewport by calling `Emulation.setDeviceMetricsOverride` with the session's configured viewport (from `--viewport` flag or default 1280x800).

4. If `--path` is a file path (not a directory), error with a helpful message suggesting `--path <DIR>` since responsive mode produces multiple files.

### Integration with `collect`

The `collect` command could also support a `responsive-screenshots` action:
```bash
sauron collect snapshot responsive-screenshots
```

This would run the accessibility snapshot and responsive screenshots in parallel where possible (snapshot first, then resize+capture cycle).

### Future extensions

- `--presets mobile,desktop` to select specific breakpoints
- Custom presets via `--preset name:WxH` syntax
- `--dpr 2` for retina captures when pixel-level detail matters
