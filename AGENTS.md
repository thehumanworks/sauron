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
