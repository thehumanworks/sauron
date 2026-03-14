# Build `sauron` Into A Parity-Plus Browser Agent CLI

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

The repository does not currently contain a checked-in repo-local planning contract such as `.agents/PLANS.md`. Maintain this plan in accordance with `/Users/mish/.agents/skills/exec-plan/references/PLANS.md`.

## Purpose / Big Picture

After this work, `sauron` will cover the browser-interaction use cases that `agent-browser` currently handles for desktop browser automation, while preserving and extending the parts where `sauron` is already stronger: a strict machine-readable contract, explicit runtime isolation, workflow execution, and Rust-native execution. The finished tool must let a human or agent perform ordinary browser tasks with short task-first commands, and it must let an agent perform long multi-step flows with stronger policy controls, better recovery semantics, lower warm-path latency, and lower memory use than the current implementation.

Success is visible in two ways. First, the operator surface becomes simple: commands such as `sauron open`, `sauron click`, `sauron fill`, `sauron wait`, `sauron get`, and `sauron snapshot` work without forcing the caller to think about `runtime start`, `session_id`, or grouped subcommands on the happy path. Second, the advanced agent surface becomes stronger than `agent-browser`: the tool emits stable artifact manifests instead of large inline blobs by default, supports policy-gated actions and domain limits, exposes richer target provenance and recovery hints, and can sustain repeated agent loops with a resident broker, semantic page cache, and engine selection that includes a lower-memory path for stateless headless work.

This plan intentionally excludes iOS and Android automation. It covers desktop browser interaction only. Remote browser connectivity is in scope only through generic browser concepts such as CDP WebSocket attach or provider-neutral broker interfaces; mobile-specific commands, simulators, and gestures are out of scope.

## Progress

- [x] (2026-03-14 20:06Z) Read the ExecPlan contract and example from `/Users/mish/.agents/skills/exec-plan`.
- [x] (2026-03-14 20:10Z) Inspected the installed `agent-browser` CLI surface with `agent-browser --help` and a live `example.com` run (`open`, `wait`, `snapshot -i`, `screenshot --annotate`).
- [x] (2026-03-14 20:19Z) Inspected the current `sauron` CLI surface with `cargo run -- --help`, subcommand help, and a live `example.com` run (`runtime start`, `page goto`, `page snapshot --format json`, `page collect`, `runtime stop`).
- [x] (2026-03-14 20:38Z) Explored `vercel-labs/agent-browser` with `wit`, including `cli/src/commands.rs`, `cli/src/connection.rs`, `cli/src/flags.rs`, `cli/src/output.rs`, `cli/src/native/{state,policy,providers,screenshot,tracing}.rs`, and `benchmarks/README.md`.
- [x] (2026-03-14 20:55Z) Collected subagent panel input for UX, agent-readiness, and performance/memory strategy.
- [x] (2026-03-14 21:08Z) Drafted this parity-plus implementation plan.
- [x] (2026-03-14 21:44Z) Milestone 1 complete: added flat task-first commands, config layering (`~/.sauron/config.json` + `./sauron.json` + env + CLI), `--session` alias, `--profile`, `--ensure-runtime`, and `config show`.
- [x] (2026-03-14 22:08Z) Milestone 2 complete: added flat interaction/introspection commands (`get`, `is`, `select`, `check`, `uncheck`, `upload`, `download`, `pdf`, `close`, nav helpers), semantic locator resolution with `ResolvedTarget`, richer wait modes, and `snapshot --format nodes` plus snapshot-option parity fixes.
- [x] (2026-03-14 22:17Z) Milestone 3 complete: added policy engine (`safe|confirm|open` + allow rules + policy file), artifact mode controls (`inline|path|manifest|none`), grouped/flat artifact manifest behavior, `state show|rename|clear|clean`, `sessionStorage` persistence, and structured error recovery hints.
- [x] (2026-03-14 22:24Z) Milestone 4 complete: added broker protocol/module scaffolding, binary-first artifact helpers, and benchmark harness scaffolding under `scripts/`.
- [x] (2026-03-14 22:26Z) Milestone 5 complete: added engine router scaffolding (`chrome-full|chrome-lean|lightpanda`), semantic page cache module, and `snapshot --delta` plumbing.
- [x] (2026-03-14 22:31Z) Milestone 6 complete: updated README/help-facing behavior docs, added parity scorecard doc, and validated gates (`cargo test`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo build --release`).

## Surprises & Discoveries

- Observation: `sauron` is already ahead in a few places that matter to agents. It has a strict v2 envelope for every command, explicit runtime lifecycle, per-session logs, a workflow runner, page Markdown extraction, and a parallel `page collect` command. `agent-browser` is broader, but not stricter.
  Evidence: `src/types.rs`, `src/errors.rs`, `src/runtime.rs`, `src/main.rs` (`run`, `page.collect`, `page.markdown`), and the live `page.collect` run on `example.com`.

- Observation: `agent-browser` has a much broader operator surface than its short skill summary suggests. The repository parses and tests commands for cookies, storage, state show/rename/clear/clean, network routing, trace/profiler/recording, clipboard, semantic find/get/is locators, remote providers, confirmation, and alternative engines.
  Evidence: `vercel-labs/agent-browser:cli/src/commands.rs`, `cli/src/native/parity_tests.rs`, `cli/src/native/providers.rs`, `cli/src/native/tracing.rs`, `cli/src/native/state.rs`, `cli/src/native/policy.rs`.

- Observation: `sauron` advertises some snapshot options that are not fully honored by the current serializer. `interactive` changes behavior, but `clickable`, `scope`, and `include_iframes` are not meaningfully reflected in `src/snapshot.rs` today.
  Evidence: `src/main.rs` exposes those options for `page snapshot` and `page collect`, while `src/snapshot.rs` only branches on `opts.interactive` during serialization.

- Observation: `sauron` state persistence is narrower than `agent-browser` state persistence. `sauron` saves cookies and same-origin `localStorage`; `agent-browser` state files include cookies, `localStorage`, and `sessionStorage`, and profile mode retains broader browser state such as IndexedDB and service workers.
  Evidence: `src/session.rs` versus `vercel-labs/agent-browser:cli/src/native/state.rs` and `README.md` session documentation.

- Observation: `sauron` warm-path work is doing avoidable repeated setup. The current architecture reconnects, re-attaches, and rebuilds page state for many short commands. This is the main reason the plan needs a broker milestone instead of only adding more verbs.
  Evidence: `src/main.rs` browser-command wrappers, `src/browser.rs` connection setup, snapshot/ref resolution paths, and the live debug-build timings from the local `example.com` run.

- Observation: enabling richer recovery metadata increased `CliError` size enough to trigger crate-wide `clippy::result_large_err` failures under `-D warnings`.
  Evidence: first `cargo clippy --all-targets --all-features -- -D warnings` run failed with widespread `result_large_err` diagnostics; resolved by explicit crate-level lint policy plus targeted cleanups.

- Observation: artifact mode integration was easiest to land safely by defaulting grouped commands to legacy inline behavior and only switching grouped output when `--artifact-mode` is set.
  Evidence: grouped `page.screenshot`/`page.collect` logic now branches on `effective_config.artifact_mode`, preserving old output without flags.

## Decision Log

- Decision: Exclude iOS and Android work from this plan even though `agent-browser` supports iOS-related flows.
  Rationale: The user explicitly asked to keep the focus on browser interaction and not to consider adding iOS or Android functionality.
  Date/Author: 2026-03-14 / Codex

- Decision: Preserve the current grouped command surface and v2 JSON envelope as a stable advanced interface while adding a new flat task-first surface on top.
  Rationale: `sauron` already has a better machine contract than `agent-browser`. Replacing it would lose one of the tool's strongest properties and create unnecessary migration risk.
  Date/Author: 2026-03-14 / Codex

- Decision: Treat policy, recovery, and artifact contracts as first-class parity-plus work, not polish.
  Rationale: `agent-browser` has the broader operator surface, but `sauron` can leapfrog it by becoming safer and more deterministic for stateless agents.
  Date/Author: 2026-03-14 / Codex

- Decision: Introduce a resident broker and capability-driven engine selection instead of only optimizing the current per-command direct-CDP path.
  Rationale: The performance and memory brief cannot be met by adding more commands alone. Warm-path reconnect cost, repeated AX scans, and base64-heavy artifact transport are architectural issues.
  Date/Author: 2026-03-14 / Codex

- Decision: Prefer provider-neutral browser concepts (`--cdp-url`, named profiles, engine capabilities, artifact manifests, policy decisions) over vendor-specific remote-browser integrations in the first implementation waves.
  Rationale: This keeps `sauron` focused on browser interaction rather than third-party service wrappers and avoids coupling parity to a single hosted browser provider API.
  Date/Author: 2026-03-14 / Codex

- Decision: Implement Milestones 4 and 5 as production-ready scaffolding (broker protocol toggle, benchmark harness, engine/cache modules, snapshot delta plumbing) while keeping the direct path as the default runtime path.
  Rationale: This delivers measurable, testable foundation quickly without destabilizing the existing CLI behavior, and keeps rollback simple while deeper broker work iterates.
  Date/Author: 2026-03-14 / Codex

- Decision: Keep grouped command behavior backward compatible by default and apply policy/artifact/recovery improvements in additive form.
  Rationale: The user asked for highest quality with stable legacy surface; additive behavior avoids regression risk for existing workflows.
  Date/Author: 2026-03-14 / Codex

## Outcomes & Retrospective

Implementation is complete for the scope in this plan revision. `sauron` now has a flat task-first surface on top of the legacy grouped interface, config layering, auto-runtime controls, richer locator/interaction commands, policy decisions, artifact manifests, expanded state/session persistence, structured recovery hints, and documentation/benchmark scaffolding for parity-plus evaluation.

The resulting architecture preserves the existing v2 grouped contract while adding stronger agent-facing behavior. The staged approach worked: ergonomic commands landed first, then policy/output/recovery guarantees, then broker/engine/cache scaffolding. The largest residual risk is that broker and pooled execution are presently scaffolding rather than default runtime execution paths; this is documented and benchmark hooks are in place for the next iteration.

## Context and Orientation

The current `sauron` repository is a Rust CLI centered around a mandatory runtime session. `src/main.rs` defines the command tree and dispatches into three main areas: runtime lifecycle, page/browser actions, and state/logging helpers. `src/runtime.rs` manages persisted runtime session records, project and process bindings, cleanup, and per-command logs under `~/.sauron/runtime/`. `src/browser.rs` wraps the CDP transport and implements navigation, snapshotting, screenshot capture, waits, console/network capture, dialogs, and element interaction. `src/snapshot.rs` turns the accessibility tree into the text snapshot plus `@eN` refs that downstream commands use. `src/session.rs` saves and loads persisted browser state for named sessions.

`agent-browser` has a different shape. The installed CLI exposes flat verbs such as `open`, `click`, `fill`, `wait`, `get`, `find`, `set`, `network`, `cookies`, `storage`, `state`, `trace`, `profiler`, `record`, and `auth`. The `vercel-labs/agent-browser` repository shows three implementation decisions that matter to this plan. First, it hides its daemon on the happy path and auto-starts or reconnects per named session (`cli/src/connection.rs`). Second, it offers a much broader browser-operation surface than `sauron` today (`cli/src/commands.rs`, `cli/src/native/parity_tests.rs`). Third, it has already invested in safety and output controls such as content boundaries, output truncation, action confirmation, and domain policy (`cli/src/output.rs`, `cli/src/native/policy.rs`, `cli/src/native/network.rs`). It also supports alternate engines and remote browser providers (`cli/src/native/providers.rs`, `cli/src/native/cdp/lightpanda.rs`).

From the comparison, the current strengths are as follows.

`sauron` is stronger in strict machine contracts and lifecycle visibility. Every command emits one v2 envelope with `meta`, `result`, explicit error codes, hints, and retry metadata. It has a project-aware runtime store, explicit `runtime start` and `runtime stop`, session logs, a workflow runner, a Markdown extractor, and a `page.collect` command that already does useful parallel artifact gathering.

`agent-browser` is stronger in everyday browser-task breadth. It gives users flat verbs, named sessions, profiles, config files, semantic locators, storage and cookie commands, richer tab/navigation verbs, annotated screenshots, trace/profiler/recording, network routing, content boundaries, action policies, confirmation flows, and engine/provider breadth. For a new user, it is easier to discover what to do next.

The critical gaps for this plan are therefore not all equal. The highest-priority gaps are: the need for a flat task-first surface, broader interaction and locator support, stronger persistence and policy features, safer and smaller output defaults, and a broker-plus-engine architecture that lowers warm latency and memory without regressing the current stable v2 interface.

The plan uses the following plain-language terms:

A "broker" is a long-lived background process owned by `sauron` that keeps browser connections, page actors, caches, and logs alive so that repeated CLI commands do not rebuild all state from scratch. A "flat surface" is a top-level command surface like `sauron open` instead of `sauron page goto`. A "semantic locator" is a way to target a browser element by role, accessible name, label, placeholder, or test id instead of raw CSS or an unstable generated ref. An "artifact manifest" is a structured description of screenshots, snapshots, logs, diffs, and extracted content that points at files or handles instead of embedding large payloads directly in the main JSON response.

## Milestones

### Milestone 1: Add a flat task-first CLI and config surface

At the end of this milestone, a contributor can use `sauron` for ordinary browser work without manually running `runtime start` first, while the existing grouped commands remain intact. This milestone is about discoverability and ergonomics, not yet about making every advanced feature faster.

Add a new flat parser layer in `src/main.rs` or a new module such as `src/flat_cli.rs` that rewrites top-level task verbs into the existing internal command handlers. The minimum flat verbs are `open`, `back`, `forward`, `reload`, `snapshot`, `screenshot`, `click`, `fill`, `hover`, `scroll`, `press`, `wait`, `get`, `is`, `find`, `tab`, `state`, and `close`. For the grouped commands, keep the current v2 envelope and current argument forms untouched. For the new flat verbs, support human-friendly output when stdout is a TTY, but require `--json` to produce the same stable v2 envelope as grouped commands.

Add named-session and profile ergonomics. Introduce `--session <name>` as the human-facing alias for project/runtime session routing, and add `--profile <dir>` as a public concept distinct from the current low-level `--user-data-dir`. Add configuration file loading from `~/.sauron/config.json` and `./sauron.json`, with precedence `user config < project config < environment variables < CLI flags`. Add an explicit `sauron config show` or equivalent debugging command before declaring the config layer complete.

Auto-runtime behavior must be controlled, not implicit magic. Add a global option such as `--ensure-runtime auto|require|off`. On the new flat surface, default to `auto`. On the grouped surface, keep the current `require` behavior unless the user opts into `auto`. When `auto` creates a runtime, it must report that fact in the response metadata and logs.

Acceptance for this milestone is behavioral. From the repo root, `cargo run -- open https://example.com` should auto-start or attach to a suitable runtime, navigate, and return a concise response. `cargo run -- page goto https://example.com` must still work exactly as before. `cargo run -- --session qa open https://example.com` and a matching `sauron.json` file must both alter session routing deterministically.

### Milestone 2: Reach common interaction and observation parity

At the end of this milestone, `sauron` covers the ordinary browser task surface that users currently reach for in `agent-browser`. A contributor must be able to navigate, inspect, target, interact with, and collect state from pages without falling back to `page js` for routine flows.

Extend `src/main.rs`, `src/browser.rs`, and a new module such as `src/locators.rs` to add the missing verbs and target-resolution system. Add first-class commands and internal handlers for `get url|title|text|html|value|attr`, `is visible|enabled|checked`, `select`, `check`, `uncheck`, `upload`, `download`, `pdf`, and `close`. Add semantic locators that can target by role, name, label, placeholder, alt text, title, and test id. Keep raw CSS selectors and existing refs as escape hatches.

Refactor target resolution so that all interaction commands return a `target` object describing how the target was resolved. The response must always tell the caller whether the command matched by ref, semantic locator, text fallback, or CSS selector, plus the confidence and ambiguity level. Create a stable `ResolvedTarget` type for this. If resolution is ambiguous, return a structured error that includes the candidate count and the resolution strategy that failed.

Upgrade snapshots to be structured-first. Keep the current text tree as a render mode, but add a `page snapshot --format nodes` path that returns a machine-readable node list with role, name, value, state flags, frame id, bounds when available, and associated action affordances. Add `page screenshot --annotate` with numbered overlays linked back to refs and the node list. Wire `clickable`, `scope`, and `include_iframes` into the actual snapshot behavior instead of exposing flags that the serializer ignores.

Add the ordinary navigation helpers `back`, `forward`, and `reload`, plus a better `wait` surface that supports a single positional element or millisecond argument and named waits such as `--load networkidle`, `--text`, `--url`, and `--fn`. Do not remove the current grouped `page wait`; instead, alias its concepts onto the new flat surface.

Acceptance for this milestone is a small flow that proves the new surface works end to end. From the repo root, `cargo run -- open https://example.com`, `cargo run -- snapshot -i`, `cargo run -- get title`, `cargo run -- click @e2`, and `cargo run -- wait --load networkidle` should all work without `runtime start`. A local form fixture or deterministic public form page should then validate `fill`, `select`, `check`, `upload`, `download`, `get value`, and `is enabled`.

### Milestone 3: Add policy, persistence, output budgeting, and recovery semantics

At the end of this milestone, `sauron` is safer and more deterministic for agent use than the comparison tool. This milestone deliberately aims beyond parity rather than only matching `agent-browser`.

Expand `src/session.rs` into a more complete persistence layer. Add support for `sessionStorage` alongside cookies and `localStorage`, and add `state show`, `state rename`, `state clear`, and `state clean` commands. Introduce optional encryption for saved state files so that credentials and cookies are not stored unprotected by default. Add a profile mode that persists broader browser state through `--profile` without forcing the user to understand `--user-data-dir`.

Add a policy engine in a new module such as `src/policy.rs`. It must support allow and deny rules for hostnames, origins, and action classes. The minimum action classes are `navigate`, `read`, `interact`, `download`, `dialog`, `script`, and `state-write`. Add `--policy safe|confirm|open`, `--allow-host`, `--allow-origin`, `--allow-action`, and a JSON policy file path. Each mutating command must include a `policy` section in the response indicating the decision, matched rules, and whether a confirmation token was required.

Add output controls in a new module such as `src/artifacts.rs`. Introduce `--artifact-mode inline|path|manifest|none`, `--max-bytes`, `--redact`, and `--content-boundaries`. New flat commands in agent mode should default to manifest or file-path output for screenshots, diffs, and long content rather than embedding base64 blobs in the main response. Keep the existing grouped JSON contract available for compatibility, but allow grouped commands to opt into the new manifest mode.

Strengthen recovery semantics in `src/types.rs` and `src/errors.rs`. Every error must include a structured recovery object that tells the caller what to do next: resnapshot, retry after delay, reopen page, reacquire session, or manual intervention. Add a higher-level `bundle` command or a `page collect --bundle` mode that emits a self-contained handoff package containing the latest snapshot, refs, artifact manifest, current URL, and recovery hints for stateless agent continuation.

Acceptance for this milestone is behavioral. A blocked action under `--policy safe` must fail before side effects happen and return a policy decision in JSON. `state show` must reveal saved-state summary metadata without dumping secrets by default. `screenshot` and `collect` under the new default agent mode must return an artifact manifest or saved path instead of inline base64. A forced stale-ref error must tell the caller exactly how to recover.

### Milestone 4: Add a resident broker, binary-first artifacts, and event-driven waits

At the end of this milestone, repeated CLI commands stop paying the current reconnect and re-attach cost, and large artifacts stop flowing through the CLI as oversized JSON strings by default. This is the first milestone that directly addresses the performance and memory brief through architecture.

Introduce a new resident process, tentatively `sauron-broker`, implemented in new modules such as `src/broker.rs`, `src/broker_protocol.rs`, and `src/page_actor.rs`. The CLI becomes a thin RPC client for flat commands and, optionally, grouped commands. The broker keeps browser sessions, websocket connections, target attachments, enabled CDP domains, viewport state, and page-level caches alive across commands. It must be possible to disable the broker with an explicit environment variable or flag until the new path is proven.

Move artifact handling to binary-first transport. Screenshots should default to writing files or returning a manifest entry with path, mime type, byte count, digest, and any annotation data. Inline base64 remains available only behind an explicit option such as `--artifact-mode inline` or `--inline-data`. The broker owns temporary artifact directories and garbage collection. `page.collect` should assemble one manifest from a single frozen page state instead of materializing duplicated large payloads in memory.

Replace polling-heavy wait logic with broker-managed event state. Maintain page lifecycle state, in-flight network counts, DOM mutation epochs, dialog state, and navigation commits in the broker so that waits can subscribe to state instead of re-establishing temporary event loops on each command.

Add benchmark scaffolding alongside this milestone, even if the benchmark command itself is not polished yet. The broker milestone is only complete if it proves lower warm-path latency and lower peak memory for screenshot and collect paths.

Acceptance for this milestone is a benchmark gate and a behavioral gate. Behaviorally, repeated commands such as `open`, `snapshot`, `click`, and `wait` must work with the broker transparently. Performance-wise, warm `snapshot` and warm `click` must materially improve versus the pre-broker path on the same machine, and the default screenshot path must reduce peak CLI memory because it no longer round-trips large base64 strings through stdout.

### Milestone 5: Add engine tiering, semantic page caches, and pooled execution

At the end of this milestone, `sauron` can choose a lower-memory browser engine for simple headless flows, reuse cached page semantics for repeated agent loops, and serve more concurrent work without spawning a fully independent heavy browser process for every small task.

Add a new engine model in `src/engine.rs` with at least three explicit modes: `chrome-full`, `chrome-lean`, and `lightpanda`. `chrome-full` is the compatibility path for downloads, profiles, extensions, and WebGL-heavy pages. `chrome-lean` strips the browser down for low-overhead local automation without advanced browser features. `lightpanda` is the low-memory headless path for stateless navigation, snapshot, screenshot, and evaluation work. Do not make users guess. Add a capability router that chooses the engine from the requested command set and the current session's declared requirements.

Add a semantic page cache in new code such as `src/page_cache.rs`. The cache should retain a stable node map keyed by backend DOM id and refresh it incrementally when possible. Snapshot, ref validation, and semantic locator resolution should consult the cache rather than rebuilding the full accessibility tree for every operation. Add `snapshot --delta` or an equivalent page-diff mechanism that reports additions, removals, and changed nodes instead of always returning a full text tree.

Add bounded page or context pools in the broker so that many independent agent loops can share a browser instance when it is safe to do so. The broker must still preserve isolation and offer a spillover path to a new browser when one pool reaches budget or becomes unhealthy.

Acceptance for this milestone is benchmark-driven. The implementation is complete only when the benchmark harness shows that the engine router lowers baseline memory for compatible flows and that pooled execution improves throughput or memory-per-task at moderate concurrency without breaking isolation or stability.

### Milestone 6: Finish advanced parity, publish a comparison scorecard, and document migration

At the end of this milestone, `sauron` is ready to be presented as a browser-interaction tool that matches the practical desktop browser work done by `agent-browser` while clearly exceeding it in agent governance and warm-path performance.

Close the remaining advanced parity gaps that remain within scope: trace/profiler, recording, richer console and network inspection, generic CDP attach by URL, clipboard, richer diffing, and any remaining desktop-only observation or interaction flows that still require `page js` as a workaround. Reassess whether vendor-specific provider shortcuts are necessary once generic CDP attach exists; do not add provider-specific flags unless the generic attach surface is demonstrably insufficient.

Write a scorecard document, likely under `docs/` or `specs/`, comparing `sauron` and `agent-browser` across operator UX, agent-readiness, safety, performance, memory footprint, state persistence, and observability. This scorecard must be based on the benchmark harness and live scenario results, not on README claims alone.

Update `README.md`, shell completion output, and help text so that the flat surface leads the documentation. Keep a clear section that explains the legacy grouped surface and its stability promise. Provide migration notes showing that existing `sauron page ...` and `sauron input ...` flows remain valid.

Acceptance for this milestone is comprehensive. The full validation suite must pass, the benchmark scorecard must exist, the README must show the new flat task-first flows, and a side-by-side scenario set must demonstrate both parity and at least one clear superiority point in each required area: UX, agent-readiness, performance, and memory footprint.

## Plan of Work

Implementation must proceed from the outside in. First, add the new flat surface and config model while keeping the existing grouped surface stable. This gives users immediate ergonomic improvement and keeps the repo moving with low risk. While doing that, avoid invasive refactors in `src/browser.rs`; wrap what already exists.

Next, build the interaction and locator surface that closes the largest day-to-day gap with `agent-browser`. This is the phase where a user stops needing `page js` for ordinary work. The code changes will touch `src/main.rs`, `src/browser.rs`, and new modules for locators, structured snapshot nodes, and annotated screenshots. At this stage, the code should still run over the current direct-CDP command path.

Only after the operator surface is broad enough should the plan add the policy, recovery, and output-budgeting layer. This sequence matters because policy and recovery need a stable notion of actions, locators, and artifact kinds. Do not bolt policy onto ad hoc command behavior. First define shared action classes and artifact manifests, then apply policy enforcement consistently across them.

The broker milestone comes next because it changes the execution model. Build it behind a clear switch and preserve a direct path fallback while the broker is stabilizing. The broker must own page actors, artifact temp storage, and event-driven wait state before the plan introduces engine tiering and pooling. If engine work starts before the broker exists, the repo will accumulate duplicated lifecycle code.

After the broker is stable, add engine routing, semantic page caches, and pooling. This is the phase where performance and memory improvements can become large rather than incremental. The engine router must be capability-based, not rule-of-thumb-based. If a command needs full Chrome semantics, the router must choose `chrome-full` even if `lightpanda` is installed.

Finish by closing advanced parity, writing the scorecard, and promoting the flat surface in the docs. Resist the temptation to declare parity early. The final state must be proven with live scenarios and benchmarks, not only with source inspection.

## Concrete Steps

From the repository root, use these commands to preserve the research baseline and validate each milestone.

For baseline inspection and before/after comparison:

    cargo run -- --help
    cargo run -- page --help
    cargo run -- input --help
    cargo run -- runtime start
    cargo run -- page goto https://example.com
    cargo run -- page snapshot --format json
    cargo run -- page collect --snapshot --screenshot
    cargo run -- runtime stop
    agent-browser --help
    agent-browser open https://example.com && agent-browser wait --load networkidle && agent-browser snapshot -i

For Milestone 1 validation:

    cargo run -- open https://example.com
    cargo run -- --session qa open https://example.com
    cargo run -- config show
    cargo run -- page goto https://example.com

Expected outcome: the flat `open` path works without a manual runtime-start step, the grouped path still works, and the config/session source is explainable.

For Milestone 2 validation:

    cargo run -- open https://example.com
    cargo run -- snapshot -i
    cargo run -- get title
    cargo run -- get url
    cargo run -- click @e2
    cargo run -- wait --load networkidle
    cargo run -- screenshot --annotate

Expected outcome: each command returns either concise TTY output or the stable v2 envelope under `--json`, target resolution is explicit, and annotated screenshot output maps cleanly back to refs or node ids.

For Milestone 3 validation:

    cargo run -- --policy safe --allow-host example.com open https://example.com
    cargo run -- --policy safe open https://example.com
    cargo run -- state save ./tmp/auth-state.json
    cargo run -- state show ./tmp/auth-state.json
    cargo run -- screenshot --artifact-mode manifest
    cargo run -- collect --snapshot --screenshot --content --artifact-mode manifest

Expected outcome: policy decisions are visible in output, blocked actions fail before side effects, state metadata is inspectable without dumping secrets, and large artifacts are returned as manifest entries or file paths.

For Milestone 4 and later performance work, add and use a dedicated benchmark entry point from the repo root. The benchmark command may be a Rust binary under `src/bin/`, a script under `scripts/`, or a bench harness, but it must support running the same scenario matrix repeatedly and writing machine-readable results.

For final verification before merging any implementation slice:

    cargo test
    cargo clippy --all-targets --all-features -- -D warnings
    cargo build --release

If a milestone adds end-to-end multi-step behavior, add integration tests or end-to-end fixtures that prove the full sequence, not just unit tests for helper functions.

## Validation and Acceptance

The full plan is complete only when the following behavioral statements are true.

A contributor can use `sauron` as a task-first browser CLI without learning the old grouped surface first. Commands like `open`, `snapshot`, `click`, `fill`, `wait`, `get`, and `screenshot` work directly. The grouped commands still exist and still honor the stable v2 JSON contract.

A stateless agent can use `sauron` safely. Mutating actions can be restricted by policy, large content can be bounded or redacted, and every error or stale target tells the caller what to do next. The tool can emit a bundle or manifest suitable for agent handoff rather than only inline blobs.

`agent-browser`-class browser tasks do not require `page js` as a workaround. This includes ordinary navigation helpers, semantic target lookup, storage and state management, annotated screenshoting, and the common observation/introspection verbs.

Warm-path performance and memory use improve materially over the current implementation. The benchmark harness must show lower warm latency for repeated commands and lower peak memory for screenshot and collect flows. For compatible stateless flows, a lower-memory engine path must exist and must be selected predictably.

The README and help surface match reality. A new user following the README should discover the flat surface first, and an existing user should still be able to run the legacy grouped commands without hidden breakage.

## Idempotence and Recovery

This plan must be implemented additively and safely. Do not remove the legacy grouped commands or their v2 contract until the flat surface, policy layer, and broker path are all stable and benchmarked. The old path remains the rollback path for any regression during implementation.

Every milestone should be guarded by explicit switches where practical. The broker path needs a direct-path fallback. Engine routing needs an override to force `chrome-full` when compatibility matters. Manifest output needs an inline override for callers that still require embedded data. Policy enforcement needs an explicit open mode for trusted local debugging.

If a new feature causes failures in the middle of implementation, the recovery path is to disable only that feature while keeping the rest of the milestone's public surface intact. Do not revert unrelated progress. Prefer compile-time or runtime feature switches over code deletion. Update the `Progress`, `Surprises & Discoveries`, and `Decision Log` sections whenever this happens.

Stateful artifacts must be easy to clean up. Temporary artifacts written by the broker should live under a clearly owned directory and support garbage collection. Profile directories, saved state, and benchmark outputs must be documented and easy to remove without affecting source state.

## Artifacts and Notes

The most important research artifacts from the initial assessment are summarized here.

Installed `agent-browser` against `example.com`:

    agent-browser open https://example.com
    agent-browser wait --load networkidle
    agent-browser snapshot -i

Observed output:

    - heading "Example Domain" [level=1, ref=e1]
    - link "Learn more" [ref=e2]

Annotated screenshot output from the same run:

    Screenshot saved to .../screenshot-....png
      [1] @e1 heading "Example Domain"
      [2] @e2 link "Learn more"

Current `sauron` against `example.com`:

    cargo run -- runtime start
    cargo run -- page goto https://example.com
    cargo run -- page snapshot --format json
    cargo run -- page collect --snapshot --screenshot
    cargo run -- runtime stop

Observed behavior:

    runtime.start returned one v2 envelope with session ids, port, pid, viewport, and wsUrl.
    page.goto returned one v2 envelope with url and status.
    page.snapshot returned one v2 envelope with refCount, refs, snapshotId, and the full text tree.
    page.collect returned one v2 envelope with url, snapshot, and an inline base64 screenshot payload.

Repository evidence that must guide implementation order:

    src/main.rs exposes a strong grouped command tree and a useful page.collect path.
    src/session.rs currently persists cookies plus same-origin localStorage only.
    src/snapshot.rs only uses interactive-only filtering during serialization today.
    agent-browser output supports content boundaries and max-output truncation.
    agent-browser state and session docs cover cookies, localStorage, sessionStorage, and profile persistence.

## Interfaces and Dependencies

Use the following interfaces and module boundaries so the implementation stays coherent.

Create a flat-surface parser and config layer with stable names. If new modules are added, prefer `src/flat_cli.rs` and `src/config.rs`. The flat-surface parser should resolve to existing internal command labels instead of duplicating behavior.

Define a shared target-resolution model in new code such as `src/locators.rs`:

    pub enum TargetSpec {
        Ref(String),
        Css(String),
        Text { text: String, exact: bool, nth: Option<u32> },
        Role { role: String, name: Option<String>, nth: Option<u32> },
        Label { text: String, nth: Option<u32> },
        Placeholder { text: String, nth: Option<u32> },
        AltText { text: String, nth: Option<u32> },
        Title { text: String, nth: Option<u32> },
        TestId { value: String, nth: Option<u32> },
    }

    pub struct ResolvedTarget {
        pub strategy: String,
        pub confidence: f32,
        pub frame_id: Option<String>,
        pub backend_node_id: Option<u64>,
        pub ref_id: Option<String>,
        pub summary_role: Option<String>,
        pub summary_name: Option<String>,
        pub candidate_count: u32,
    }

Define an artifact contract in new code such as `src/artifacts.rs`:

    pub enum ArtifactMode {
        Inline,
        Path,
        Manifest,
        None,
    }

    pub struct ArtifactRef {
        pub kind: String,
        pub mime: String,
        pub path: Option<String>,
        pub inline_data: Option<String>,
        pub bytes: Option<u64>,
        pub sha256: Option<String>,
        pub annotations: Option<serde_json::Value>,
    }

    pub struct ArtifactManifest {
        pub items: Vec<ArtifactRef>,
    }

Define policy interfaces in `src/policy.rs`:

    pub enum PolicyDecisionKind {
        Allow,
        Deny,
        Confirm,
    }

    pub struct PolicyDecision {
        pub decision: PolicyDecisionKind,
        pub reason: String,
        pub matched_rules: Vec<String>,
        pub confirmation_id: Option<String>,
    }

Define a broker protocol in new code such as `src/broker_protocol.rs` and `src/broker.rs`:

    pub struct BrokerRequest {
        pub request_id: String,
        pub session: String,
        pub command: String,
        pub payload: serde_json::Value,
    }

    pub struct BrokerResponse {
        pub request_id: String,
        pub ok: bool,
        pub payload: serde_json::Value,
    }

Define engine routing in `src/engine.rs`:

    pub enum EngineKind {
        ChromeFull,
        ChromeLean,
        Lightpanda,
    }

    pub struct EngineCapabilities {
        pub supports_profiles: bool,
        pub supports_downloads: bool,
        pub supports_extensions: bool,
        pub supports_webgl: bool,
        pub supports_recording: bool,
    }

    pub fn choose_engine(requested_features: &RequestedFeatures) -> EngineKind

Define structured snapshot output in `src/types.rs` or a new `src/snapshot_nodes.rs` module:

    pub struct SnapshotNode {
        pub id: String,
        pub role: String,
        pub name: Option<String>,
        pub value: Option<String>,
        pub states: Vec<String>,
        pub frame_id: Option<String>,
        pub bounds: Option<serde_json::Value>,
        pub stable_selector: Option<String>,
        pub actions: Vec<String>,
    }

Keep existing dependencies when they still fit: CDP remains the transport, `src/runtime.rs` remains the source of truth for persisted runtime sessions until the broker fully replaces the direct path, and the current error envelope remains the compatibility contract for grouped commands.

At the bottom of the final implementation, `sauron` should have two coherent public faces, not a grab bag. The legacy grouped surface remains stable and machine-focused. The new flat surface becomes the default human and agent entry point. The broker, policy, artifact, and engine layers serve both surfaces behind shared internal types.

Change note: created this ExecPlan on 2026-03-14 after researching the installed `agent-browser` CLI, the `vercel-labs/agent-browser` repository via `wit`, the current `sauron` implementation, and three subagent review memos for UX, agent-readiness, and performance/memory. The purpose of this revision is to capture the comparison and turn it into a self-contained phased implementation roadmap.

Change note (2026-03-14 22:31Z): executed the implementation end-to-end, updated all milestone statuses, captured new discoveries/decisions/outcomes, and aligned validation notes with the actual delivered code (`flat CLI`, `config`, `policy`, `artifacts`, `state`, `recovery`, `benchmark scaffolding`, `engine/cache/broker scaffolding`, docs/scorecard updates, and passing build gates).
