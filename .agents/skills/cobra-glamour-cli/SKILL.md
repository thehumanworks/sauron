---
name: cobra-glamour-cli
description: |
  Build and evolve Sauron's Go CLI command tree with Cobra and terminal markdown
  rendering via glamour. Use this when adding commands, flags, or human-friendly output.
---

# cobra-glamour-cli

Use `wit` lookups to keep command patterns aligned with upstream examples:

1. Find Cobra command patterns:
   - `wit rg -C 5 -g 'site/content/user_guide.md' 'Use:|RunE:|Execute\\(' spf13/cobra`
2. Find glamour renderer patterns:
   - `wit rg -C 5 -g 'README.md' 'NewTermRenderer|WithAutoStyle|WithWordWrap|Render\\(' charmbracelet/glamour`
   - `wit cat charmbracelet/glamour examples/custom_renderer/main.go`

## Sauron conventions
- Keep command definitions in `internal/cli/`.
- Prefer `RunE` so command failures bubble as errors.
- Support machine output with `--json`; use glamour-rendered markdown otherwise.
- Keep output concise and explicit for `start`, `stop`, and `health`.

## External dependencies
- `github.com/spf13/cobra`
- `github.com/charmbracelet/glamour`
