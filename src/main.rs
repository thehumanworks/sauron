#![allow(clippy::result_large_err, clippy::too_many_arguments)]

mod artifacts;
mod broker;
mod broker_protocol;
mod browser;
mod cdp;
mod config;
mod context;
mod daemon;
mod diff;
mod engine;
mod errors;
mod locators;
mod page_cache;
mod policy;
mod runtime;
mod session;
mod snapshot;
mod snapshot_nodes;
#[cfg(test)]
mod test_support;
mod types;

use base64::Engine as _;
use browser::BrowserClient;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use context::AppContext;
use errors::{
    make_error, make_error_with_meta, make_success, make_success_with_meta, print_result, CliError,
};
use regex::Regex;
use runtime::{
    activate_session, cleanup_session_state, cleanup_stale_state, create_session_record,
    resolve_active_session, resolve_project_root_path, session_required_error, terminate_session,
    CleanupStats, RuntimeSessionRecord, RuntimeStore,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(
    name = "sauron",
    version,
    about = "Rust-native CLI for controlling Chrome via CDP"
)]
struct Cli {
    /// Runtime session id override (otherwise resolved by process binding, project context, then SAURON_SESSION_ID)
    #[arg(long = "session-id", alias = "session", global = true)]
    session_id: Option<String>,

    /// Optional instance id override for `start` (auto-generated when omitted)
    #[arg(long, global = true)]
    instance: Option<String>,

    /// Optional client id override for `start` (auto-generated when omitted)
    #[arg(long, global = true)]
    client: Option<String>,

    /// Chrome DevTools debugging port (overrides pidfile)
    #[arg(long, global = true)]
    port: Option<u16>,

    /// Optional override for pidfile location
    #[arg(long, global = true)]
    pid_path: Option<PathBuf>,

    /// Optional override for Chrome user data dir
    #[arg(long, global = true)]
    user_data_dir: Option<PathBuf>,

    /// Named browser profile directory (human-friendly alias for user-data-dir semantics).
    #[arg(long, global = true)]
    profile: Option<PathBuf>,

    /// Optional timeout in milliseconds (command-specific defaults apply when omitted)
    #[arg(long, global = true)]
    timeout_ms: Option<u64>,

    /// Sleep for N milliseconds before executing the subcommand.
    ///
    /// This is useful for agent loops that need deterministic pacing.
    #[arg(long, global = true)]
    delay_ms: Option<u64>,

    /// Viewport in WIDTHxHEIGHT format (e.g. 1440x900).
    ///
    /// - `sauron runtime start`: sets the session default viewport.
    /// - Browser commands: overrides the viewport for this invocation.
    #[arg(long, global = true)]
    viewport: Option<String>,

    /// Runtime handling strategy for commands that need browser connectivity.
    #[arg(long, value_enum, global = true)]
    ensure_runtime: Option<EnsureRuntimeArg>,

    /// Force machine-readable JSON output.
    #[arg(long, global = true)]
    json: bool,

    /// Policy mode for browser actions.
    #[arg(long, value_enum, global = true)]
    policy: Option<policy::PolicyMode>,

    /// Allow policy host(s); can be repeated.
    #[arg(long, global = true)]
    allow_host: Vec<String>,

    /// Allow policy origin(s); can be repeated.
    #[arg(long, global = true)]
    allow_origin: Vec<String>,

    /// Allow policy action class(es); can be repeated.
    #[arg(long, global = true)]
    allow_action: Vec<String>,

    /// Optional JSON policy file path.
    #[arg(long, global = true)]
    policy_file: Option<PathBuf>,

    /// Artifact output strategy for large payloads.
    #[arg(long, value_enum, global = true)]
    artifact_mode: Option<artifacts::ArtifactMode>,

    /// Maximum bytes to include for content fields in inline output.
    #[arg(long, global = true)]
    max_bytes: Option<u64>,

    /// Redact sensitive values in output where supported.
    #[arg(long, global = true)]
    redact: bool,

    /// Emit content boundary metadata for long text payloads.
    #[arg(long, global = true)]
    content_boundaries: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Runtime lifecycle commands
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommands,
    },

    /// Page-level browser commands
    Page {
        #[command(subcommand)]
        command: PageCommands,
    },

    /// Input interaction commands
    Input {
        #[command(subcommand)]
        command: InputCommands,
    },

    /// Tab management
    Tab {
        #[command(subcommand)]
        command: TabCommands,
    },

    /// Save/load/list/delete browser state
    State {
        #[command(subcommand)]
        command: StateCommands,
    },

    /// Snapshot ref inspection
    Ref {
        #[command(subcommand)]
        command: RefCommands,
    },

    /// Runtime log inspection
    Logs {
        #[command(subcommand)]
        command: LogCommands,
    },

    /// Console capture controls
    Console {
        #[command(subcommand)]
        command: ConsoleCommands,
    },

    /// Network capture controls
    Network {
        #[command(subcommand)]
        command: NetworkCommands,
    },

    /// Show resolved configuration and config sources
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Navigate to a URL (flat task-first alias for `page goto`)
    Open {
        url: String,
        #[arg(long, default_value = "load")]
        until: String,
    },

    /// Navigate backward in history
    Back,

    /// Navigate forward in history
    Forward,

    /// Reload the current page
    Reload,

    /// Snapshot the current page
    Snapshot {
        #[arg(short = 'i', long)]
        interactive: bool,
        #[arg(long)]
        clickable: bool,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        include_iframes: bool,
        #[arg(long, value_enum, default_value = "text")]
        format: SnapshotFormatArg,
        #[arg(long)]
        delta: bool,
    },

    /// Capture a screenshot
    Screenshot {
        #[arg(long)]
        full: bool,
        #[arg(long)]
        responsive: bool,
        #[arg(long, value_enum, default_value = "high")]
        quality: ScreenshotQualityArg,
        #[arg(long)]
        annotate: bool,
        #[arg(long, conflicts_with = "output_dir")]
        output: Option<PathBuf>,
        #[arg(long, conflicts_with = "output")]
        output_dir: Option<PathBuf>,
    },

    /// Click a target
    Click {
        #[command(flatten)]
        target: FlatTargetArgs,
        #[arg(long)]
        double: bool,
    },

    /// Fill a target with a value
    Fill {
        #[command(flatten)]
        target: FlatTargetArgs,
        value: String,
    },

    /// Hover a target
    Hover {
        #[command(flatten)]
        target: FlatTargetArgs,
    },

    /// Scroll the page or target
    Scroll {
        #[arg(long)]
        direction: Option<ScrollDirectionArg>,
        #[arg(long, default_value_t = 500)]
        amount: i64,
        #[command(flatten)]
        target: FlatTargetArgs,
    },

    /// Press a key combo
    Press { combo: String },

    /// Get a page or element value
    Get {
        subject: String,
        #[command(flatten)]
        target: FlatTargetArgs,
        #[arg(long)]
        attr: Option<String>,
    },

    /// Evaluate a boolean condition about a target
    Is {
        predicate: String,
        #[command(flatten)]
        target: FlatTargetArgs,
    },

    /// Select an option in a select element
    Select {
        #[command(flatten)]
        target: FlatTargetArgs,
        value: String,
    },

    /// Check a checkbox/radio element
    Check {
        #[command(flatten)]
        target: FlatTargetArgs,
    },

    /// Uncheck a checkbox/radio element
    Uncheck {
        #[command(flatten)]
        target: FlatTargetArgs,
    },

    /// Upload file(s) to a file input element
    Upload {
        #[command(flatten)]
        target: FlatTargetArgs,
        files: Vec<String>,
    },

    /// Download a URL to a local file path
    Download {
        url: String,
        #[arg(long)]
        output: PathBuf,
    },

    /// Export current page as PDF
    Pdf {
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Wait for a condition
    Wait {
        /// Positional shorthand: milliseconds for a sleep wait.
        arg: Option<String>,
        #[arg(long)]
        load: Option<String>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        selector: Option<String>,
        #[arg(long)]
        state: Option<SelectorStateArg>,
        #[arg(long)]
        count: Option<u32>,
        #[arg(long)]
        idle: bool,
        #[arg(long = "fn")]
        function: Option<String>,
    },

    /// Find target(s) without acting on them
    Find {
        #[command(flatten)]
        target: FlatTargetArgs,
    },

    /// Collect multiple artifacts in one command
    Collect {
        #[arg(long)]
        snapshot: bool,
        #[arg(long)]
        screenshot: bool,
        #[arg(long)]
        content: bool,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        bundle: bool,
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Close current tab (or explicit index)
    Close {
        #[arg(long)]
        index: Option<usize>,
    },

    /// Execute a workflow file
    Run {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        stop_on_error: bool,
    },

    /// Print shell completion scripts to stdout (bash/zsh).
    ///
    /// Examples:
    ///   sauron completions --shell zsh > _sauron
    ///   sauron completions --shell bash > sauron.bash
    Completions {
        #[arg(long, value_enum)]
        shell: CompletionShell,
    },
}

#[derive(Subcommand, Debug)]
enum RuntimeCommands {
    /// Start a new runtime session and Chrome daemon
    Start {
        /// Enable WebGL-friendly rendering flags.
        #[arg(long, conflicts_with = "no_webgl")]
        webgl: bool,

        /// Disable WebGL-friendly rendering flags.
        #[arg(long, conflicts_with = "webgl")]
        no_webgl: bool,

        /// Enable GPU acceleration.
        #[arg(long, conflicts_with = "no_gpu")]
        gpu: bool,

        /// Disable GPU acceleration.
        #[arg(long, conflicts_with = "gpu")]
        no_gpu: bool,
    },

    /// Stop the active runtime session
    Stop,

    /// Show runtime daemon status
    Status,

    /// Remove stale runtime artifacts
    Cleanup,
}

#[derive(Subcommand, Debug)]
enum ConfigCommands {
    Show,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EnsureRuntimeArg {
    Auto,
    Require,
    Off,
}

impl EnsureRuntimeArg {
    fn as_str(self) -> &'static str {
        match self {
            EnsureRuntimeArg::Auto => "auto",
            EnsureRuntimeArg::Require => "require",
            EnsureRuntimeArg::Off => "off",
        }
    }
}

#[derive(Subcommand, Debug)]
enum PageCommands {
    /// Navigate to a URL
    Goto {
        url: String,
        #[arg(long, default_value = "load")]
        until: String,
    },

    /// Snapshot the current page accessibility tree (agent-friendly)
    Snapshot {
        #[arg(long)]
        interactive: bool,
        #[arg(long)]
        clickable: bool,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        include_iframes: bool,
        #[arg(long, value_enum, default_value = "text")]
        format: SnapshotFormatArg,
    },

    /// Take a screenshot
    Screenshot {
        #[arg(long)]
        full: bool,
        /// Capture mobile/tablet/desktop screenshots in one command.
        #[arg(long)]
        responsive: bool,
        /// Image quality profile.
        #[arg(long, value_enum, default_value = "high")]
        quality: ScreenshotQualityArg,
        #[arg(long, conflicts_with = "output_dir")]
        output: Option<PathBuf>,
        #[arg(long, conflicts_with = "output")]
        output_dir: Option<PathBuf>,
    },

    /// Collect multiple artifacts in one command (runs actions in parallel)
    Collect {
        /// Include an accessibility snapshot (same as `snapshot`)
        #[arg(long)]
        snapshot: bool,

        /// Include a screenshot (same as `screenshot`)
        #[arg(long)]
        screenshot: bool,

        /// Include Markdown content (same as `markdown`)
        #[arg(long)]
        content: bool,

        // --- Snapshot options (only used if --snapshot is set) ---
        #[arg(long)]
        interactive: bool,
        #[arg(long)]
        clickable: bool,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        include_iframes: bool,

        // --- Screenshot options (only used if --screenshot is set) ---
        #[arg(long)]
        full: bool,
        /// Image quality profile for the screenshot action.
        #[arg(long, value_enum, default_value = "high")]
        quality: ScreenshotQualityArg,
        /// Optional file path to write the screenshot image.
        /// If omitted, screenshot data is returned as base64 in JSON.
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Extract the full text content of the current page as Markdown
    Markdown,

    /// Wait for a condition
    Wait {
        #[arg(long)]
        for_ms: Option<u64>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        selector: Option<String>,
        #[arg(long)]
        state: Option<SelectorStateArg>,
        #[arg(long)]
        count: Option<u32>,
        #[arg(long)]
        idle: bool,
    },

    /// Run JavaScript in the page context
    Js { expression: String },

    /// Diff the last two snapshots
    Diff {
        #[arg(long)]
        from: Option<u64>,
        #[arg(long)]
        to: Option<u64>,
        #[arg(long, default_value = "unified")]
        format: String,
    },

    /// Handle the next JavaScript dialog (accept/dismiss)
    Dialog {
        action: String,
        #[arg(long)]
        text: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum InputCommands {
    /// Click an element by ref (@e1) or text
    Click {
        #[command(flatten)]
        target: TargetArgs,
        #[arg(long)]
        double: bool,
    },

    /// Fill an input/select by ref (@e1) or text
    Fill {
        #[command(flatten)]
        target: TargetArgs,
        value: String,
    },

    /// Hover an element by ref (@e1) or text
    Hover {
        #[command(flatten)]
        target: TargetArgs,
    },

    /// Scroll
    Scroll {
        #[arg(long)]
        direction: Option<ScrollDirectionArg>,
        /// Amount in pixels for directional scroll
        #[arg(long, default_value_t = 500)]
        amount: i64,
        #[command(flatten)]
        target: TargetArgs,
    },

    /// Press a key or key combination (e.g. Enter, Control+A)
    Press { combo: String },
}

#[derive(Subcommand, Debug)]
enum TabCommands {
    List,
    Open { url: String },
    Switch { index: usize },
    Close { index: usize },
}

#[derive(Subcommand, Debug)]
enum StateCommands {
    Save { name: String },
    Load { name: String },
    List,
    Delete { name: String },
    Show { name: String },
    Rename { from: String, to: String },
    Clear,
    Clean,
}

#[derive(Subcommand, Debug)]
enum RefCommands {
    List,
    Show {
        reference: String,
    },
    Validate {
        #[arg(long, short = 'r')]
        reference: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum LogCommands {
    List,
    Tail {
        #[arg(long, default_value_t = 50)]
        lines: usize,
    },
}

#[derive(Subcommand, Debug)]
enum ConsoleCommands {
    Capture {
        #[arg(long, default_value_t = 5_000)]
        duration_ms: u64,
        #[arg(long)]
        level: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum NetworkCommands {
    Capture {
        #[arg(long, default_value_t = 5_000)]
        duration_ms: u64,
        #[arg(long)]
        url_glob: Option<String>,
    },
}

#[derive(Args, Debug, Clone, Default)]
struct TargetArgs {
    #[arg(long)]
    selector: Option<String>,
    #[arg(long = "ref")]
    reference: Option<String>,
    #[arg(long)]
    text: Option<String>,
    #[arg(long)]
    match_index: Option<u32>,
}

impl TargetArgs {
    fn is_empty(&self) -> bool {
        self.selector
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
            && self
                .reference
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            && self.text.as_deref().map(str::trim).unwrap_or("").is_empty()
    }
}

#[derive(Args, Debug, Clone, Default)]
struct FlatTargetArgs {
    /// Positional target shorthand. `@e1` resolves as ref, otherwise text.
    target: Option<String>,
    #[arg(long)]
    selector: Option<String>,
    #[arg(long = "ref")]
    reference: Option<String>,
    #[arg(long)]
    text: Option<String>,
    #[arg(long)]
    role: Option<String>,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    label: Option<String>,
    #[arg(long)]
    placeholder: Option<String>,
    #[arg(long = "alt")]
    alt_text: Option<String>,
    #[arg(long)]
    title: Option<String>,
    #[arg(long = "test-id")]
    test_id: Option<String>,
    #[arg(long)]
    nth: Option<u32>,
    #[arg(long)]
    exact: bool,
}

impl FlatTargetArgs {
    fn is_empty(&self) -> bool {
        self.target
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
            && self
                .selector
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            && self
                .reference
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            && self.text.as_deref().map(str::trim).unwrap_or("").is_empty()
            && self.role.as_deref().map(str::trim).unwrap_or("").is_empty()
            && self
                .label
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            && self
                .placeholder
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            && self
                .alt_text
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            && self
                .title
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            && self
                .test_id
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
    }

    fn to_target_spec(&self) -> Result<locators::TargetSpec, CliError> {
        let mut strategy_count = 0usize;
        if !self
            .selector
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            strategy_count += 1;
        }
        if !self
            .reference
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            strategy_count += 1;
        }
        if !self.text.as_deref().map(str::trim).unwrap_or("").is_empty() {
            strategy_count += 1;
        }
        if !self.role.as_deref().map(str::trim).unwrap_or("").is_empty() {
            strategy_count += 1;
        }
        if !self
            .label
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            strategy_count += 1;
        }
        if !self
            .placeholder
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            strategy_count += 1;
        }
        if !self
            .alt_text
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            strategy_count += 1;
        }
        if !self
            .title
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            strategy_count += 1;
        }
        if !self
            .test_id
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            strategy_count += 1;
        }
        if !self
            .target
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            strategy_count += 1;
        }

        if strategy_count != 1 {
            return Err(CliError::bad_input(
                "Provide exactly one target strategy",
                "Use one of positional target, --selector, --ref, --text, --role, --label, --placeholder, --alt, --title, or --test-id",
            ));
        }

        if let Some(selector) = self
            .selector
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(locators::TargetSpec::Css(selector.to_string()));
        }
        if let Some(reference) = self
            .reference
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(locators::TargetSpec::Ref(reference.to_string()));
        }
        if let Some(text) = self
            .text
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(locators::TargetSpec::Text {
                text: text.to_string(),
                exact: self.exact,
                nth: self.nth,
            });
        }
        if let Some(role) = self
            .role
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let name = self
                .name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            return Ok(locators::TargetSpec::Role {
                role: role.to_string(),
                name,
                nth: self.nth,
            });
        }
        if let Some(label) = self
            .label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(locators::TargetSpec::Label {
                text: label.to_string(),
                nth: self.nth,
            });
        }
        if let Some(placeholder) = self
            .placeholder
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(locators::TargetSpec::Placeholder {
                text: placeholder.to_string(),
                nth: self.nth,
            });
        }
        if let Some(alt_text) = self
            .alt_text
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(locators::TargetSpec::AltText {
                text: alt_text.to_string(),
                nth: self.nth,
            });
        }
        if let Some(title) = self
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(locators::TargetSpec::Title {
                text: title.to_string(),
                nth: self.nth,
            });
        }
        if let Some(test_id) = self
            .test_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(locators::TargetSpec::TestId {
                value: test_id.to_string(),
                nth: self.nth,
            });
        }
        if let Some(target) = self
            .target
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if target.starts_with('@') {
                return Ok(locators::TargetSpec::Ref(target.to_string()));
            }
            return Ok(locators::TargetSpec::Text {
                text: target.to_string(),
                exact: self.exact,
                nth: self.nth,
            });
        }

        Err(CliError::bad_input(
            "No target provided",
            "Provide a positional target or a target flag",
        ))
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SnapshotFormatArg {
    Text,
    Json,
    Nodes,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SelectorStateArg {
    Attached,
    Visible,
    Hidden,
    Detached,
}

impl SelectorStateArg {
    fn to_browser(self) -> browser::SelectorWaitState {
        match self {
            SelectorStateArg::Attached => browser::SelectorWaitState::Attached,
            SelectorStateArg::Visible => browser::SelectorWaitState::Visible,
            SelectorStateArg::Hidden => browser::SelectorWaitState::Hidden,
            SelectorStateArg::Detached => browser::SelectorWaitState::Detached,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ScrollDirectionArg {
    Up,
    Down,
    Top,
    Bottom,
}

#[derive(Debug, Deserialize)]
struct WorkflowFile {
    steps: Vec<WorkflowStep>,
}

#[derive(Debug, Deserialize)]
struct WorkflowStep {
    command: Vec<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CompletionShell {
    Bash,
    Zsh,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ScreenshotQualityArg {
    Low,
    Medium,
    High,
}

impl ScreenshotQualityArg {
    fn to_browser(self) -> browser::ScreenshotQuality {
        match self {
            ScreenshotQualityArg::Low => browser::ScreenshotQuality::Low,
            ScreenshotQualityArg::Medium => browser::ScreenshotQuality::Medium,
            ScreenshotQualityArg::High => browser::ScreenshotQuality::High,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ScreenshotQualityArg::Low => "low",
            ScreenshotQualityArg::Medium => "medium",
            ScreenshotQualityArg::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ResponsivePreset {
    name: &'static str,
    width: u32,
    height: u32,
    mobile: bool,
}

fn responsive_screenshot_presets(desktop: types::Viewport) -> [ResponsivePreset; 3] {
    [
        ResponsivePreset {
            name: "mobile",
            width: 390,
            height: 844,
            mobile: true,
        },
        ResponsivePreset {
            name: "tablet",
            width: 820,
            height: 1180,
            mobile: false,
        },
        ResponsivePreset {
            name: "desktop",
            width: desktop.width,
            height: desktop.height,
            mobile: false,
        },
    ]
}

#[derive(Clone)]
struct ActiveRuntime {
    store: RuntimeStore,
    session: RuntimeSessionRecord,
    ctx: AppContext,
}

enum RuntimeStatusTarget {
    Runtime(Box<ActiveRuntime>),
    Probe(RuntimeStatusProbe),
    Missing,
}

struct RuntimeStatusProbe {
    pid_path: PathBuf,
    port: u16,
}

fn build_runtime_store() -> Result<RuntimeStore, CliError> {
    RuntimeStore::new()
}

fn require_runtime(
    store: &RuntimeStore,
    session_id: Option<String>,
) -> Result<ActiveRuntime, CliError> {
    let session = resolve_active_session(store, session_id)?;
    let ctx = AppContext::new(
        &session.instance,
        &session.client,
        session.pid_path.clone(),
        session.user_data_dir.clone(),
    )?;
    Ok(ActiveRuntime {
        store: store.clone(),
        session,
        ctx,
    })
}

fn is_session_resolution_error(error: &CliError) -> bool {
    matches!(
        error.code,
        types::ErrorCode::SessionRequired
            | types::ErrorCode::SessionInvalid
            | types::ErrorCode::SessionTerminated
    )
}

fn build_runtime_status_probe(
    instance: Option<String>,
    client: Option<String>,
    pid_path: Option<PathBuf>,
    user_data_dir: Option<PathBuf>,
    port: Option<u16>,
) -> Result<Option<RuntimeStatusProbe>, CliError> {
    if let Some(pid_path) = pid_path {
        return Ok(Some(RuntimeStatusProbe {
            pid_path,
            port: port.unwrap_or(9222),
        }));
    }

    if instance.is_some() || client.is_some() || user_data_dir.is_some() {
        let ctx = AppContext::new(
            instance.as_deref().unwrap_or("default"),
            client.as_deref().unwrap_or("default"),
            None,
            user_data_dir,
        )?;
        let port = ctx.resolve_port(port);
        return Ok(Some(RuntimeStatusProbe {
            pid_path: ctx.pid_path,
            port,
        }));
    }

    if let Some(port) = port {
        return Ok(Some(RuntimeStatusProbe {
            pid_path: context::resolve_base_dir()?
                .join("runtime")
                .join(".status-probe.pid"),
            port,
        }));
    }

    Ok(None)
}

fn resolve_runtime_status_target(
    store: &RuntimeStore,
    session_id: Option<String>,
    instance: Option<String>,
    client: Option<String>,
    pid_path: Option<PathBuf>,
    user_data_dir: Option<PathBuf>,
    port: Option<u16>,
) -> Result<RuntimeStatusTarget, CliError> {
    match require_runtime(store, session_id) {
        Ok(runtime) => Ok(RuntimeStatusTarget::Runtime(Box::new(runtime))),
        Err(error) if is_session_resolution_error(&error) => {
            let Some(probe) =
                build_runtime_status_probe(instance, client, pid_path, user_data_dir, port)?
            else {
                return Ok(RuntimeStatusTarget::Missing);
            };
            Ok(RuntimeStatusTarget::Probe(probe))
        }
        Err(error) => Err(error),
    }
}

fn status_payload_from_daemon_status(status: types::DaemonStatus) -> serde_json::Value {
    match status {
        types::DaemonStatus::Running { pid, port, ws_url } => json!({
            "status": "running",
            "pid": pid,
            "port": port,
            "wsUrl": ws_url
        }),
        types::DaemonStatus::Stale { pid, port } => json!({
            "status": "stale",
            "pid": pid,
            "port": port,
            "hint": "Run 'sauron runtime stop' then 'sauron runtime start'"
        }),
        types::DaemonStatus::Stopped => json!({
            "status": "stopped"
        }),
    }
}

fn remove_file_if_present(path: &Path, what: &str) -> Result<(), CliError> {
    match std::fs::remove_file(path) {
        Ok(_) => Ok(()),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(CliError::unknown(
                    format!("Failed to remove {} {}: {}", what, path.display(), e),
                    "Check filesystem permissions",
                ))
            }
        }
    }
}

fn remove_dir_all_if_present(path: &Path, what: &str) -> Result<(), CliError> {
    match std::fs::remove_dir_all(path) {
        Ok(_) => Ok(()),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(CliError::unknown(
                    format!("Failed to remove {} {}: {}", what, path.display(), e),
                    "Check filesystem permissions",
                ))
            }
        }
    }
}

fn remove_dir_if_empty(path: &Path, what: &str) -> Result<(), CliError> {
    match std::fs::remove_dir(path) {
        Ok(_) => Ok(()),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound
                || e.kind() == std::io::ErrorKind::DirectoryNotEmpty
            {
                Ok(())
            } else {
                Err(CliError::unknown(
                    format!("Failed to remove {} {}: {}", what, path.display(), e),
                    "Check filesystem permissions",
                ))
            }
        }
    }
}

fn cleanup_runtime_context(ctx: &AppContext) -> Result<(), CliError> {
    context::remove_pid_file(&ctx.pid_path)?;
    remove_dir_all_if_present(&ctx.user_data_dir, "Chrome user data dir")?;
    remove_file_if_present(&ctx.instance_lock_path, "instance lock")?;
    remove_dir_if_empty(&ctx.instance_dir, "instance dir")?;
    Ok(())
}

fn begin_runtime_command(runtime: &mut ActiveRuntime, command_name: &str) -> Result<(), CliError> {
    // Concurrency-safe update: multiple `sauron` processes may operate on the same session.
    runtime.session = runtime
        .store
        .mark_session_command(&runtime.session.session_id, command_name)?;
    runtime
        .store
        .append_log(&runtime.session, command_name, "start", None)?;
    Ok(())
}

fn finish_runtime_command(
    runtime: &ActiveRuntime,
    command_name: &str,
    ok: bool,
    details: serde_json::Value,
) {
    let status = if ok { "ok" } else { "error" };
    let _ = runtime
        .store
        .append_log(&runtime.session, command_name, status, Some(details));
}

fn ensure_runtime_or_exit(
    store: &RuntimeStore,
    session_id: Option<String>,
    command_name: &str,
) -> ActiveRuntime {
    match require_runtime(store, session_id) {
        Ok(runtime) => runtime,
        Err(mut e) => {
            if matches!(e.code, types::ErrorCode::SessionRequired) {
                e = session_required_error(command_name);
            }
            let res = make_error(command_name, &e);
            print_result(&res);
            std::process::exit(e.exit_code);
        }
    }
}

async fn ensure_runtime_for_flat_command(
    store: &RuntimeStore,
    session_id: Option<String>,
    instance: Option<String>,
    client: Option<String>,
    pid_path: Option<PathBuf>,
    user_data_dir: Option<PathBuf>,
    port: Option<u16>,
    timeout_ms: Option<u64>,
    viewport_override: Option<types::Viewport>,
    ensure_runtime: EnsureRuntimeArg,
    command_name: &str,
) -> Result<(ActiveRuntime, bool), CliError> {
    match require_runtime(store, session_id.clone()) {
        Ok(runtime) => return Ok((runtime, false)),
        Err(error)
            if matches!(ensure_runtime, EnsureRuntimeArg::Auto)
                && is_session_resolution_error(&error) => {}
        Err(error) => {
            if matches!(
                ensure_runtime,
                EnsureRuntimeArg::Off | EnsureRuntimeArg::Require
            ) && matches!(error.code, types::ErrorCode::SessionRequired)
            {
                return Err(session_required_error(command_name));
            }
            return Err(error);
        }
    }

    let mut session =
        create_session_record(store, session_id, instance, client, pid_path, user_data_dir)?;
    let session_viewport = viewport_override.unwrap_or_default();
    session.viewport_width = session_viewport.width;
    session.viewport_height = session_viewport.height;

    let ctx = AppContext::new(
        &session.instance,
        &session.client,
        session.pid_path.clone(),
        session.user_data_dir.clone(),
    )?;
    session.pid_path = Some(ctx.pid_path.clone());
    session.user_data_dir = Some(ctx.user_data_dir.clone());

    let timeout = timeout_ms.unwrap_or(10_000);
    let webgl_enabled = cfg!(target_os = "macos");
    let gpu_enabled = cfg!(target_os = "macos");

    let started = daemon::start(
        ctx.pid_path.clone(),
        ctx.user_data_dir.clone(),
        ctx.instance_lock_path.clone(),
        port,
        timeout,
        daemon::ChromeLaunchOptions {
            headless: true,
            disable_gpu: !gpu_enabled,
            webgl: webgl_enabled,
            viewport: session_viewport,
        },
    )
    .await?;

    activate_session(store, &session)?;

    let runtime = ActiveRuntime {
        store: store.clone(),
        session,
        ctx,
    };
    finish_runtime_command(
        &runtime,
        "runtime.auto_start",
        true,
        json!({
            "port": started.port,
            "pid": started.pid,
            "triggeredBy": command_name
        }),
    );
    Ok((runtime, true))
}

fn build_policy_engine(config: &EffectiveConfig) -> Result<policy::EffectivePolicy, CliError> {
    policy::build_policy(policy::PolicyInputs {
        mode: config.policy_mode,
        allow_hosts: config.allow_host.clone(),
        allow_origins: config.allow_origin.clone(),
        allow_actions: config.allow_action.clone(),
        policy_file: config.policy_file.clone(),
    })
}

fn policy_decision_error(command_name: &str, decision: &policy::PolicyDecision) -> CliError {
    let message = format!("{} blocked by policy ({})", command_name, decision.reason);
    CliError::new(
        types::ErrorCode::BadInput,
        message,
        "Adjust policy mode/rules or choose a permitted action",
        false,
        4,
    )
}

fn truncate_to_max_bytes(
    input: &str,
    max_bytes: Option<u64>,
) -> (String, Option<serde_json::Value>) {
    let Some(limit) = max_bytes else {
        return (input.to_string(), None);
    };
    let limit = limit as usize;
    if input.len() <= limit {
        return (input.to_string(), None);
    }
    let truncated = input
        .chars()
        .scan(0usize, |count, ch| {
            let ch_len = ch.len_utf8();
            if *count + ch_len > limit {
                None
            } else {
                *count += ch_len;
                Some(ch)
            }
        })
        .collect::<String>();
    (
        truncated,
        Some(json!({
            "truncated": true,
            "originalBytes": input.len(),
            "maxBytes": limit
        })),
    )
}

fn runtime_command_label(command: &RuntimeCommands) -> &'static str {
    match command {
        RuntimeCommands::Start { .. } => "runtime.start",
        RuntimeCommands::Stop => "runtime.stop",
        RuntimeCommands::Status => "runtime.status",
        RuntimeCommands::Cleanup => "runtime.cleanup",
    }
}

fn page_command_label(command: &PageCommands) -> &'static str {
    match command {
        PageCommands::Goto { .. } => "page.goto",
        PageCommands::Snapshot { .. } => "page.snapshot",
        PageCommands::Screenshot { .. } => "page.screenshot",
        PageCommands::Collect { .. } => "page.collect",
        PageCommands::Markdown => "page.markdown",
        PageCommands::Wait { .. } => "page.wait",
        PageCommands::Js { .. } => "page.js",
        PageCommands::Diff { .. } => "page.diff",
        PageCommands::Dialog { .. } => "page.dialog",
    }
}

fn input_command_label(command: &InputCommands) -> &'static str {
    match command {
        InputCommands::Click { .. } => "input.click",
        InputCommands::Fill { .. } => "input.fill",
        InputCommands::Hover { .. } => "input.hover",
        InputCommands::Scroll { .. } => "input.scroll",
        InputCommands::Press { .. } => "input.press",
    }
}

fn tab_command_label(command: &TabCommands) -> &'static str {
    match command {
        TabCommands::List => "tab.list",
        TabCommands::Open { .. } => "tab.open",
        TabCommands::Switch { .. } => "tab.switch",
        TabCommands::Close { .. } => "tab.close",
    }
}

fn state_command_label(command: &StateCommands) -> &'static str {
    match command {
        StateCommands::Save { .. } => "state.save",
        StateCommands::Load { .. } => "state.load",
        StateCommands::List => "state.list",
        StateCommands::Delete { .. } => "state.delete",
        StateCommands::Show { .. } => "state.show",
        StateCommands::Rename { .. } => "state.rename",
        StateCommands::Clear => "state.clear",
        StateCommands::Clean => "state.clean",
    }
}

fn ref_command_label(command: &RefCommands) -> &'static str {
    match command {
        RefCommands::List => "ref.list",
        RefCommands::Show { .. } => "ref.show",
        RefCommands::Validate { .. } => "ref.validate",
    }
}

fn log_command_label(command: &LogCommands) -> &'static str {
    match command {
        LogCommands::List => "logs.list",
        LogCommands::Tail { .. } => "logs.tail",
    }
}

fn command_label(command: &Commands) -> &'static str {
    match command {
        Commands::Runtime { command } => runtime_command_label(command),
        Commands::Page { command } => page_command_label(command),
        Commands::Input { command } => input_command_label(command),
        Commands::Tab { command } => tab_command_label(command),
        Commands::State { command } => state_command_label(command),
        Commands::Ref { command } => ref_command_label(command),
        Commands::Logs { command } => log_command_label(command),
        Commands::Console { .. } => "console.capture",
        Commands::Network { .. } => "network.capture",
        Commands::Config { .. } => "config.show",
        Commands::Open { .. } => "open",
        Commands::Back => "back",
        Commands::Forward => "forward",
        Commands::Reload => "reload",
        Commands::Snapshot { .. } => "snapshot",
        Commands::Screenshot { .. } => "screenshot",
        Commands::Click { .. } => "click",
        Commands::Fill { .. } => "fill",
        Commands::Hover { .. } => "hover",
        Commands::Scroll { .. } => "scroll",
        Commands::Press { .. } => "press",
        Commands::Get { .. } => "get",
        Commands::Is { .. } => "is",
        Commands::Select { .. } => "select",
        Commands::Check { .. } => "check",
        Commands::Uncheck { .. } => "uncheck",
        Commands::Upload { .. } => "upload",
        Commands::Download { .. } => "download",
        Commands::Pdf { .. } => "pdf",
        Commands::Wait { .. } => "wait",
        Commands::Find { .. } => "find",
        Commands::Collect { .. } => "collect",
        Commands::Close { .. } => "close",
        Commands::Run { .. } => "run",
        Commands::Completions { .. } => "completions",
    }
}

fn build_response_meta(runtime: Option<&ActiveRuntime>, started: Instant) -> types::ResponseMeta {
    let mut meta = types::ResponseMeta::new(
        uuid::Uuid::now_v7().to_string(),
        chrono::Utc::now().to_rfc3339(),
        started.elapsed().as_millis() as u64,
    );
    if let Some(runtime) = runtime {
        meta = meta.with_session(types::ResponseSessionMeta {
            session_id: runtime.session.session_id.clone(),
            instance_id: runtime.session.instance.clone(),
            client_id: runtime.session.client.clone(),
        });
    }
    meta
}

async fn resolve_backend_from_target(
    page: &browser::PageClient,
    ctx: &AppContext,
    target: &TargetArgs,
) -> Result<(u64, serde_json::Value), CliError> {
    let selector = target
        .selector
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let reference = target
        .reference
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let text = target
        .text
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());

    let mut kinds = 0usize;
    if selector.is_some() {
        kinds += 1;
    }
    if reference.is_some() {
        kinds += 1;
    }
    if text.is_some() {
        kinds += 1;
    }
    if kinds != 1 {
        return Err(CliError::bad_input(
            "Exactly one of --selector, --ref, or --text must be provided",
            "Specify a single targeting strategy for this input command",
        ));
    }

    let match_index = target.match_index.unwrap_or(0);
    if let Some(selector) = selector {
        let backend = page
            .resolve_selector_backend_node_id(selector, Some(match_index))
            .await?;
        return Ok((
            backend,
            json!({
                "strategy": "selector",
                "selector": selector,
                "matchIndex": match_index
            }),
        ));
    }

    if let Some(reference) = reference {
        let normalized = reference.trim_start_matches('@');
        let backend = page
            .resolve_target_backend_node_id(ctx, &format!("@{}", normalized), None)
            .await?;
        return Ok((
            backend,
            json!({
                "strategy": "ref",
                "reference": format!("@{}", normalized)
            }),
        ));
    }

    let text = text.expect("validated");
    let backend = page
        .resolve_target_backend_node_id(ctx, text, Some(match_index))
        .await?;
    Ok((
        backend,
        json!({
            "strategy": "text",
            "text": text,
            "matchIndex": match_index
        }),
    ))
}

fn wildcard_to_regex(pattern: &str) -> Result<Regex, CliError> {
    let escaped = regex::escape(pattern);
    let wildcard_pattern = escaped.replace("\\*", ".*");
    let anchored = format!("^{}$", wildcard_pattern);
    Regex::new(&anchored).map_err(|_| {
        CliError::bad_input(
            format!("Invalid wildcard pattern: {}", pattern),
            "Use '*' as wildcard characters",
        )
    })
}

fn exit_with_error(command_name: &str, err: CliError) -> ! {
    let res = make_error(command_name, &err);
    print_result(&res);
    std::process::exit(err.exit_code);
}

fn normalize_ref_key(reference: &str) -> Result<String, CliError> {
    let normalized = reference.trim().trim_start_matches('@').to_string();
    if normalized.is_empty() {
        return Err(CliError::bad_input(
            "Reference cannot be empty",
            "Provide a ref like @e1",
        ));
    }
    Ok(normalized)
}

fn missing_ref_state_error() -> CliError {
    CliError::bad_input(
        "No refs available",
        "Run 'sauron page snapshot' to capture refs first",
    )
}

fn cleanup_stats_json(stats: CleanupStats) -> serde_json::Value {
    json!({
        "instances": stats.instances,
        "sessions": stats.sessions,
        "logs": stats.logs
    })
}

fn run_cleanup(base_dir: &std::path::Path) -> Result<CleanupStats, CliError> {
    cleanup_stale_state(base_dir)
}

fn cleanup_terminate_log_artifacts(runtime: &ActiveRuntime) -> usize {
    let logs_dir = runtime.ctx.base_dir.join("runtime").join("logs");
    let session_id = runtime.session.session_id.as_str();
    let mut removed = 0usize;

    for extension in ["ndjson", "lock"] {
        let path = logs_dir.join(format!("{}.{}", session_id, extension));
        match std::fs::remove_file(&path) {
            Ok(_) => removed = removed.saturating_add(1),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "Cleanup warning: Failed to remove log artifact {}: {}",
                        path.display(),
                        e
                    );
                }
            }
        }
    }

    removed
}

const MAX_VIEWPORT_WIDTH: u32 = 7_680;
const MAX_VIEWPORT_HEIGHT: u32 = 4_320;

fn parse_viewport(input: &str) -> Result<types::Viewport, CliError> {
    let normalized = input.trim().to_ascii_lowercase();
    let Some((width_raw, height_raw)) = normalized.split_once('x') else {
        return Err(CliError::bad_input(
            format!("Invalid viewport '{}'", input),
            "Use WIDTHxHEIGHT format (for example 1440x900)",
        ));
    };

    let width = width_raw.parse::<u32>().map_err(|_| {
        CliError::bad_input(
            format!("Invalid viewport width '{}'", width_raw),
            "Width must be a positive integer",
        )
    })?;
    let height = height_raw.parse::<u32>().map_err(|_| {
        CliError::bad_input(
            format!("Invalid viewport height '{}'", height_raw),
            "Height must be a positive integer",
        )
    })?;

    if width == 0 || height == 0 {
        return Err(CliError::bad_input(
            format!("Invalid viewport '{}'", input),
            "Viewport dimensions must be greater than zero",
        ));
    }
    if width > MAX_VIEWPORT_WIDTH || height > MAX_VIEWPORT_HEIGHT {
        return Err(CliError::bad_input(
            format!("Viewport '{}' is too large", input),
            format!(
                "Maximum supported viewport is {}x{}",
                MAX_VIEWPORT_WIDTH, MAX_VIEWPORT_HEIGHT
            ),
        ));
    }

    Ok(types::Viewport { width, height })
}

fn parse_optional_viewport(input: Option<&str>) -> Result<Option<types::Viewport>, CliError> {
    input.map(parse_viewport).transpose()
}

#[derive(Clone)]
struct EffectiveConfig {
    session_id: Option<String>,
    instance: Option<String>,
    client: Option<String>,
    port: Option<u16>,
    pid_path: Option<PathBuf>,
    user_data_dir: Option<PathBuf>,
    timeout_ms: Option<u64>,
    viewport: Option<String>,
    ensure_runtime: EnsureRuntimeArg,
    json_output: bool,
    policy_mode: policy::PolicyMode,
    allow_host: Vec<String>,
    allow_origin: Vec<String>,
    allow_action: Vec<String>,
    policy_file: Option<PathBuf>,
    artifact_mode: artifacts::ArtifactMode,
    max_bytes: Option<u64>,
    redact: bool,
    content_boundaries: bool,
    config_layers: config::ConfigLayers,
}

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_bool_value(input: &str) -> Option<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_u16_env(name: &str) -> Option<u16> {
    env_opt(name).and_then(|value| value.parse::<u16>().ok())
}

fn parse_u64_env(name: &str) -> Option<u64> {
    env_opt(name).and_then(|value| value.parse::<u64>().ok())
}

fn parse_path_env(name: &str) -> Option<PathBuf> {
    env_opt(name).map(PathBuf::from)
}

fn parse_vec_env(name: &str) -> Option<Vec<String>> {
    let value = env_opt(name)?;
    let items: Vec<String> = value
        .split(',')
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

fn parse_ensure_runtime_value(value: &str) -> Option<EnsureRuntimeArg> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(EnsureRuntimeArg::Auto),
        "require" => Some(EnsureRuntimeArg::Require),
        "off" => Some(EnsureRuntimeArg::Off),
        _ => None,
    }
}

fn parse_policy_mode_value(value: &str) -> Option<policy::PolicyMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "safe" => Some(policy::PolicyMode::Safe),
        "confirm" => Some(policy::PolicyMode::Confirm),
        "open" => Some(policy::PolicyMode::Open),
        _ => None,
    }
}

fn parse_artifact_mode_value(value: &str) -> Option<artifacts::ArtifactMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "inline" => Some(artifacts::ArtifactMode::Inline),
        "path" => Some(artifacts::ArtifactMode::Path),
        "manifest" => Some(artifacts::ArtifactMode::Manifest),
        "none" => Some(artifacts::ArtifactMode::None),
        _ => None,
    }
}

fn is_flat_command(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Open { .. }
            | Commands::Back
            | Commands::Forward
            | Commands::Reload
            | Commands::Snapshot { .. }
            | Commands::Screenshot { .. }
            | Commands::Click { .. }
            | Commands::Fill { .. }
            | Commands::Hover { .. }
            | Commands::Scroll { .. }
            | Commands::Press { .. }
            | Commands::Get { .. }
            | Commands::Is { .. }
            | Commands::Select { .. }
            | Commands::Check { .. }
            | Commands::Uncheck { .. }
            | Commands::Upload { .. }
            | Commands::Download { .. }
            | Commands::Pdf { .. }
            | Commands::Wait { .. }
            | Commands::Find { .. }
            | Commands::Collect { .. }
            | Commands::Close { .. }
    )
}

fn resolve_effective_config(cli: &Cli) -> Result<EffectiveConfig, CliError> {
    let config_layers = config::load_config_layers()?;
    let file_config = config_layers.merged_file_config();
    let flat_command = is_flat_command(&cli.command);

    let session_id = cli
        .session_id
        .clone()
        .or_else(|| env_opt("SAURON_SESSION_ID"))
        .or_else(|| env_opt("SAURON_SESSION"))
        .or_else(|| file_config.session.clone())
        .or_else(|| file_config.session_id.clone());

    let instance = cli
        .instance
        .clone()
        .or_else(|| env_opt("SAURON_INSTANCE"))
        .or_else(|| file_config.instance.clone());
    let client = cli
        .client
        .clone()
        .or_else(|| env_opt("SAURON_CLIENT"))
        .or_else(|| file_config.client.clone());
    let port = cli
        .port
        .or_else(|| parse_u16_env("SAURON_PORT"))
        .or(file_config.port);
    let pid_path = cli
        .pid_path
        .clone()
        .or_else(|| parse_path_env("SAURON_PID_PATH"))
        .or(file_config.pid_path.clone());

    let profile_path = cli
        .profile
        .clone()
        .or_else(|| parse_path_env("SAURON_PROFILE"))
        .or(file_config.profile.clone());
    let user_data_dir = cli
        .user_data_dir
        .clone()
        .or_else(|| parse_path_env("SAURON_USER_DATA_DIR"))
        .or(file_config.user_data_dir.clone())
        .or(profile_path);

    let timeout_ms = cli
        .timeout_ms
        .or_else(|| parse_u64_env("SAURON_TIMEOUT_MS"))
        .or(file_config.timeout_ms);
    let viewport = cli
        .viewport
        .clone()
        .or_else(|| env_opt("SAURON_VIEWPORT"))
        .or(file_config.viewport.clone());

    let ensure_runtime = cli
        .ensure_runtime
        .or_else(|| {
            env_opt("SAURON_ENSURE_RUNTIME").and_then(|value| parse_ensure_runtime_value(&value))
        })
        .or_else(|| {
            file_config
                .ensure_runtime
                .as_deref()
                .and_then(parse_ensure_runtime_value)
        })
        .unwrap_or(if flat_command {
            EnsureRuntimeArg::Auto
        } else {
            EnsureRuntimeArg::Require
        });

    let json_output = if cli.json {
        true
    } else if let Some(value) = env_opt("SAURON_JSON").and_then(|value| parse_bool_value(&value)) {
        value
    } else {
        file_config.json.unwrap_or(false)
    };

    let policy_mode = cli
        .policy
        .or_else(|| env_opt("SAURON_POLICY").and_then(|value| parse_policy_mode_value(&value)))
        .or_else(|| {
            file_config
                .policy
                .as_deref()
                .and_then(parse_policy_mode_value)
        })
        .unwrap_or(policy::PolicyMode::Open);

    let allow_host = if !cli.allow_host.is_empty() {
        cli.allow_host.clone()
    } else if let Some(values) = parse_vec_env("SAURON_ALLOW_HOST") {
        values
    } else {
        file_config.allow_host.clone().unwrap_or_default()
    };

    let allow_origin = if !cli.allow_origin.is_empty() {
        cli.allow_origin.clone()
    } else if let Some(values) = parse_vec_env("SAURON_ALLOW_ORIGIN") {
        values
    } else {
        file_config.allow_origin.clone().unwrap_or_default()
    };

    let allow_action = if !cli.allow_action.is_empty() {
        cli.allow_action.clone()
    } else if let Some(values) = parse_vec_env("SAURON_ALLOW_ACTION") {
        values
    } else {
        file_config.allow_action.clone().unwrap_or_default()
    };

    let policy_file = cli
        .policy_file
        .clone()
        .or_else(|| parse_path_env("SAURON_POLICY_FILE"));

    let artifact_mode = cli
        .artifact_mode
        .or_else(|| {
            env_opt("SAURON_ARTIFACT_MODE").and_then(|value| parse_artifact_mode_value(&value))
        })
        .or_else(|| {
            file_config
                .artifact_mode
                .as_deref()
                .and_then(parse_artifact_mode_value)
        })
        .unwrap_or(if flat_command {
            artifacts::ArtifactMode::Manifest
        } else {
            artifacts::ArtifactMode::Inline
        });

    let max_bytes = cli
        .max_bytes
        .or_else(|| parse_u64_env("SAURON_MAX_BYTES"))
        .or(file_config.max_bytes);
    let redact = if cli.redact {
        true
    } else if let Some(value) = env_opt("SAURON_REDACT").and_then(|value| parse_bool_value(&value))
    {
        value
    } else {
        file_config.redact.unwrap_or(false)
    };
    let content_boundaries = if cli.content_boundaries {
        true
    } else if let Some(value) =
        env_opt("SAURON_CONTENT_BOUNDARIES").and_then(|value| parse_bool_value(&value))
    {
        value
    } else {
        file_config.content_boundaries.unwrap_or(false)
    };

    Ok(EffectiveConfig {
        session_id,
        instance,
        client,
        port,
        pid_path,
        user_data_dir,
        timeout_ms,
        viewport,
        ensure_runtime,
        json_output,
        policy_mode,
        allow_host,
        allow_origin,
        allow_action,
        policy_file,
        artifact_mode,
        max_bytes,
        redact,
        content_boundaries,
        config_layers,
    })
}

fn screenshot_path_extension_matches(path: &std::path::Path, expected_extension: &str) -> bool {
    let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
        return true;
    };
    let ext = ext.to_ascii_lowercase();
    match expected_extension {
        "jpg" => ext == "jpg" || ext == "jpeg",
        "jpeg" => ext == "jpg" || ext == "jpeg",
        other => ext == other,
    }
}

fn path_looks_like_image_file(path: &std::path::Path) -> bool {
    let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "webp"
    )
}

fn validate_screenshot_output_path(
    path: &std::path::Path,
    expected_extension: &str,
) -> Result<(), CliError> {
    if screenshot_path_extension_matches(path, expected_extension) {
        return Ok(());
    }
    Err(CliError::bad_input(
        format!(
            "Screenshot output extension mismatch for {}",
            path.display()
        ),
        format!(
            "Use a .{} output path for the selected quality profile",
            expected_extension
        ),
    ))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if let Some(ms) = cli.delay_ms {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    if let Commands::Completions { shell } = &cli.command {
        let mut cmd = Cli::command();
        let bin_name = cmd.get_name().to_string();
        let target_shell = match shell {
            CompletionShell::Bash => Shell::Bash,
            CompletionShell::Zsh => Shell::Zsh,
        };
        generate(target_shell, &mut cmd, bin_name, &mut std::io::stdout());
        return;
    }

    let effective_config = match resolve_effective_config(&cli) {
        Ok(config) => config,
        Err(e) => exit_with_error(command_label(&cli.command), e),
    };

    let command = cli.command;
    let session_id_flag = effective_config.session_id.clone();
    let instance_flag = effective_config.instance.clone();
    let client_flag = effective_config.client.clone();
    let port_flag = effective_config.port;
    let pid_path_flag = effective_config.pid_path.clone();
    let user_data_dir_flag = effective_config.user_data_dir.clone();
    let timeout_ms_flag = effective_config.timeout_ms;
    let viewport_override = match parse_optional_viewport(effective_config.viewport.as_deref()) {
        Ok(v) => v,
        Err(e) => exit_with_error(command_label(&command), e),
    };

    let store = match build_runtime_store() {
        Ok(store) => store,
        Err(e) => exit_with_error(command_label(&command), e),
    };
    let policy_engine = match build_policy_engine(&effective_config) {
        Ok(policy) => policy,
        Err(e) => exit_with_error(command_label(&command), e),
    };

    match command {
        Commands::Config { command } => match command {
            ConfigCommands::Show => {
                let started_at = Instant::now();
                let command_name = "config.show";
                let meta = build_response_meta(None, started_at);
                print_result(&make_success_with_meta(
                    command_name,
                    json!({
                        "sources": {
                            "userConfigPath": effective_config.config_layers.user_path.to_string_lossy(),
                            "projectConfigPath": effective_config.config_layers.project_path.to_string_lossy(),
                            "userConfigLoaded": effective_config.config_layers.user.is_some(),
                            "projectConfigLoaded": effective_config.config_layers.project.is_some()
                        },
                        "effective": {
                            "sessionId": effective_config.session_id,
                            "instance": effective_config.instance,
                            "client": effective_config.client,
                            "port": effective_config.port,
                            "pidPath": effective_config.pid_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                            "userDataDir": effective_config.user_data_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
                            "timeoutMs": effective_config.timeout_ms,
                            "viewport": effective_config.viewport,
                            "ensureRuntime": effective_config.ensure_runtime.as_str(),
                            "json": effective_config.json_output,
                            "policy": format!("{:?}", effective_config.policy_mode).to_ascii_lowercase(),
                            "allowHost": effective_config.allow_host,
                            "allowOrigin": effective_config.allow_origin,
                            "allowAction": effective_config.allow_action,
                            "policyFile": effective_config.policy_file.as_ref().map(|p| p.to_string_lossy().to_string()),
                            "artifactMode": format!("{:?}", effective_config.artifact_mode).to_ascii_lowercase(),
                            "maxBytes": effective_config.max_bytes,
                            "redact": effective_config.redact,
                            "contentBoundaries": effective_config.content_boundaries
                        }
                    }),
                    meta,
                ));
            }
        },
        Commands::Open { url, until } => {
            let command_name = "open";
            let decision =
                policy_engine.evaluate(policy::ActionClass::Navigate, Some(url.as_str()));
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }

            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };

            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let decision = decision.clone();
                    async move {
                        let timeout = Duration::from_millis(timeout_ms_flag.unwrap_or(30_000));
                        let outcome = page.navigate(&url, &until, timeout).await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "url": url,
                                "status": outcome.status,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Back => {
            let command_name = "back";
            let decision = policy_engine.evaluate(policy::ActionClass::Navigate, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let decision = decision.clone();
                    async move {
                        page.go_back().await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "navigated": "back",
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Forward => {
            let command_name = "forward";
            let decision = policy_engine.evaluate(policy::ActionClass::Navigate, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let decision = decision.clone();
                    async move {
                        page.go_forward().await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "navigated": "forward",
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Reload => {
            let command_name = "reload";
            let decision = policy_engine.evaluate(policy::ActionClass::Navigate, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let decision = decision.clone();
                    async move {
                        page.reload().await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "reloaded": true,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Snapshot {
            interactive,
            clickable,
            scope,
            include_iframes,
            format,
            delta,
        } => {
            let command_name = "snapshot";
            let decision = policy_engine.evaluate(policy::ActionClass::Read, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    async move {
                        let opts = types::SnapshotOptions {
                            interactive,
                            clickable,
                            scope: scope.clone(),
                            include_iframes,
                        };
                        let snap = page.snapshot_and_persist(&cmd_ctx, opts.clone()).await?;
                        let refs: Vec<serde_json::Value> = snap
                            .refs
                            .iter()
                            .map(|(id, reference)| {
                                json!({
                                    "id": id,
                                    "role": reference.role,
                                    "name": reference.name,
                                    "locator": reference.locator
                                })
                            })
                            .collect();

                        let nodes =
                            if matches!(format, SnapshotFormatArg::Json | SnapshotFormatArg::Nodes)
                                || delta
                            {
                                let ax = page.accessibility_tree().await?;
                                Some(snapshot_nodes::flatten_snapshot_nodes(
                                    &ax,
                                    clickable,
                                    scope.as_deref(),
                                ))
                            } else {
                                None
                            };

                        let delta_payload = if delta {
                            if let Some(nodes) = nodes.clone() {
                                let current =
                                    page_cache::build_cache_entry(snap.snapshot_id, nodes);
                                if let Ok(Some(state)) = browser::load_ref_state(&cmd_ctx).await {
                                    let previous_snapshot_id = state.snapshot_id.saturating_sub(1);
                                    if previous_snapshot_id > 0 {
                                        if let Ok(Some(previous_tree)) =
                                            browser::load_snapshot(&cmd_ctx, previous_snapshot_id)
                                                .await
                                        {
                                            let previous_nodes = previous_tree
                                                .lines()
                                                .enumerate()
                                                .map(|(index, line)| snapshot_nodes::SnapshotNode {
                                                    id: format!("l{}", index + 1),
                                                    role: "line".to_string(),
                                                    name: Some(line.trim().to_string()),
                                                    value: None,
                                                    states: Vec::new(),
                                                    frame_id: None,
                                                    bounds: None,
                                                    stable_selector: None,
                                                    actions: Vec::new(),
                                                })
                                                .collect::<Vec<_>>();
                                            let previous = page_cache::build_cache_entry(
                                                previous_snapshot_id,
                                                previous_nodes,
                                            );
                                            Some(page_cache::diff_entries(&previous, &current))
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        let payload = match format {
                            SnapshotFormatArg::Text => json!({
                                "format": "text",
                                "url": snap.url,
                                "snapshotId": snap.snapshot_id,
                                "refCount": snap.refs.len(),
                                "tree": snap.tree,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started },
                                "delta": delta_payload
                            }),
                            SnapshotFormatArg::Json => json!({
                                "format": "json",
                                "url": snap.url,
                                "snapshotId": snap.snapshot_id,
                                "refCount": snap.refs.len(),
                                "tree": snap.tree,
                                "refs": refs,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started },
                                "delta": delta_payload
                            }),
                            SnapshotFormatArg::Nodes => json!({
                                "format": "nodes",
                                "url": snap.url,
                                "snapshotId": snap.snapshot_id,
                                "refCount": snap.refs.len(),
                                "tree": snap.tree,
                                "refs": refs,
                                "nodes": nodes.unwrap_or_default(),
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started },
                                "delta": delta_payload
                            }),
                        };

                        Ok(make_success(command_name, payload))
                    }
                },
            )
            .await;
        }
        Commands::Screenshot {
            full,
            responsive,
            quality,
            annotate,
            output,
            output_dir,
        } => {
            let command_name = "screenshot";
            let decision = policy_engine.evaluate(policy::ActionClass::Read, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            let artifact_mode = effective_config.artifact_mode;
            let default_viewport = viewport_override.unwrap_or(runtime.session.viewport());
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    async move {
                        if !responsive {
                            let shot = page.capture_screenshot(full, quality.to_browser()).await?;
                            if let Some(path) = output.as_ref() {
                                validate_screenshot_output_path(path, &shot.extension)?;
                            }
                            let annotations = if annotate {
                                browser::load_ref_state(&cmd_ctx)
                                    .await?
                                    .map(|state| {
                                        let mut refs: Vec<_> = state.refs.into_iter().collect();
                                        refs.sort_by(|a, b| a.0.cmp(&b.0));
                                        json!(
                                            refs.into_iter()
                                                .enumerate()
                                                .map(|(idx, (id, item))| json!({
                                                    "index": idx + 1,
                                                    "ref": format!("@{}", id),
                                                    "role": item.role,
                                                    "name": item.name
                                                }))
                                                .collect::<Vec<_>>()
                                        )
                                    })
                            } else {
                                None
                            };

                            let write = artifacts::write_screenshot_artifact(
                                &cmd_ctx.base_dir,
                                &cmd_ctx.instance,
                                artifact_mode,
                                &shot.mime,
                                &shot.extension,
                                &shot.data,
                                output.as_deref(),
                                annotations.clone(),
                            )?;

                            let artifact_payload = match artifact_mode {
                                artifacts::ArtifactMode::Inline => json!({
                                    "mode": "inline",
                                    "artifact": write.reference
                                }),
                                artifacts::ArtifactMode::Path => json!({
                                    "mode": "path",
                                    "path": write.reference.path,
                                    "artifact": write.reference
                                }),
                                artifacts::ArtifactMode::Manifest => json!({
                                    "mode": "manifest",
                                    "manifest": artifacts::ArtifactManifest { items: vec![write.reference] }
                                }),
                                artifacts::ArtifactMode::None => json!({
                                    "mode": "none"
                                }),
                            };

                            return Ok(make_success(
                                command_name,
                                json!({
                                    "responsive": false,
                                    "quality": quality.as_str(),
                                    "policy": decision,
                                    "runtime": { "autoStarted": auto_started },
                                    "annotationRequested": annotate,
                                    "result": artifact_payload
                                }),
                            ));
                        }

                        let output_dir = if let Some(path) = output_dir.or(output.clone()) {
                            path
                        } else {
                            std::env::current_dir().map_err(|e| {
                                CliError::unknown(
                                    format!("Failed to resolve current directory: {}", e),
                                    "Run command from an accessible directory",
                                )
                            })?
                        };
                        tokio::fs::create_dir_all(&output_dir).await.map_err(|e| {
                            CliError::unknown(
                                format!(
                                    "Failed to create screenshot directory {}: {}",
                                    output_dir.display(),
                                    e
                                ),
                                "Check filesystem permissions",
                            )
                        })?;

                        let mut outputs = Vec::new();
                        for preset in responsive_screenshot_presets(default_viewport) {
                            page.set_viewport(preset.width, preset.height, preset.mobile)
                                .await?;
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            let shot = page.capture_screenshot(full, quality.to_browser()).await?;
                            let bytes = artifacts::decode_base64(&shot.data)?;
                            let path = output_dir.join(format!(
                                "screenshot-{}.{}",
                                preset.name, shot.extension
                            ));
                            tokio::fs::write(&path, bytes).await.map_err(|e| {
                                CliError::unknown(
                                    format!("Failed to write screenshot {}: {}", path.display(), e),
                                    "Check filesystem permissions",
                                )
                            })?;
                            outputs.push(json!({
                                "preset": preset.name,
                                "width": preset.width,
                                "height": preset.height,
                                "path": path.to_string_lossy()
                            }));
                        }

                        Ok(make_success(
                            command_name,
                            json!({
                                "responsive": true,
                                "quality": quality.as_str(),
                                "screenshots": outputs,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Click { target, double } => {
            let command_name = "click";
            let decision = policy_engine.evaluate(policy::ActionClass::Interact, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let target_spec = match target.to_target_spec() {
                Ok(spec) => spec,
                Err(err) => exit_with_error(command_name, err),
            };
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let target_spec = target_spec.clone();
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    async move {
                        let (backend, resolved) =
                            locators::resolve_target(&page, &cmd_ctx, &target_spec).await?;
                        page.click(backend, double).await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "clicked": true,
                                "double": double,
                                "target": resolved,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Fill { target, value } => {
            let command_name = "fill";
            let decision = policy_engine.evaluate(policy::ActionClass::Interact, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let target_spec = match target.to_target_spec() {
                Ok(spec) => spec,
                Err(err) => exit_with_error(command_name, err),
            };
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let target_spec = target_spec.clone();
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    async move {
                        let (backend, resolved) =
                            locators::resolve_target(&page, &cmd_ctx, &target_spec).await?;
                        let typ = page.fill(backend, &value).await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "filled": true,
                                "type": typ,
                                "target": resolved,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Hover { target } => {
            let command_name = "hover";
            let decision = policy_engine.evaluate(policy::ActionClass::Interact, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let target_spec = match target.to_target_spec() {
                Ok(spec) => spec,
                Err(err) => exit_with_error(command_name, err),
            };
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let target_spec = target_spec.clone();
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    async move {
                        let (backend, resolved) =
                            locators::resolve_target(&page, &cmd_ctx, &target_spec).await?;
                        page.hover(backend).await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "hovered": true,
                                "target": resolved,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Press { combo } => {
            let command_name = "press";
            let decision = policy_engine.evaluate(policy::ActionClass::Interact, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let decision = decision.clone();
                    async move {
                        page.press_key(&combo).await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "pressed": true,
                                "key": combo,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Scroll {
            direction,
            amount,
            target,
        } => {
            let command_name = "scroll";
            let decision = policy_engine.evaluate(policy::ActionClass::Interact, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    let target = target.clone();
                    async move {
                        if let Some(direction) = direction {
                            if !target.is_empty() {
                                return Err(CliError::bad_input(
                                    "Do not combine --direction with explicit target",
                                    "Use directional scrolling or target scrolling, not both",
                                ));
                            }
                            let expr = match direction {
                                ScrollDirectionArg::Top => "window.scrollTo(0, 0)".to_string(),
                                ScrollDirectionArg::Bottom => {
                                    "window.scrollTo(0, document.body.scrollHeight)".to_string()
                                }
                                ScrollDirectionArg::Up => {
                                    format!("window.scrollBy(0, -{})", amount)
                                }
                                ScrollDirectionArg::Down => {
                                    format!("window.scrollBy(0, {})", amount)
                                }
                            };
                            let _ = page.eval(&expr).await?;
                            return Ok(make_success(
                                command_name,
                                json!({
                                    "direction": format!("{:?}", direction).to_ascii_lowercase(),
                                    "amount": amount,
                                    "policy": decision,
                                    "runtime": { "autoStarted": auto_started }
                                }),
                            ));
                        }

                        let target_spec = target.to_target_spec()?;
                        let (backend, resolved) =
                            locators::resolve_target(&page, &cmd_ctx, &target_spec).await?;
                        page.scroll_into_view(backend).await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "scrolledIntoView": true,
                                "target": resolved,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Get {
            subject,
            target,
            attr,
        } => {
            let command_name = "get";
            let decision = policy_engine.evaluate(policy::ActionClass::Read, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let subject = subject.to_ascii_lowercase();
            let maybe_target_spec =
                if matches!(subject.as_str(), "text" | "html" | "value" | "attr") {
                    match target.to_target_spec() {
                        Ok(spec) => Some(spec),
                        Err(err) => exit_with_error(command_name, err),
                    }
                } else {
                    None
                };
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            let max_bytes = effective_config.max_bytes;
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let decision = decision.clone();
                    let cmd_ctx = cmd_ctx.clone();
                    let maybe_target_spec = maybe_target_spec.clone();
                    let attr = attr.clone();
                    async move {
                        let mut target_info: Option<locators::ResolvedTarget> = None;
                        let value = match subject.as_str() {
                            "url" => page.url().await?,
                            "title" => page.title().await?,
                            "text" => {
                                let spec = maybe_target_spec.as_ref().ok_or_else(|| {
                                    CliError::bad_input(
                                        "get text requires a target",
                                        "Provide a target via positional argument or locator flag",
                                    )
                                })?;
                                let (backend, resolved) =
                                    locators::resolve_target(&page, &cmd_ctx, spec).await?;
                                target_info = Some(resolved);
                                page.get_node_text(backend).await?
                            }
                            "html" => {
                                let spec = maybe_target_spec.as_ref().ok_or_else(|| {
                                    CliError::bad_input(
                                        "get html requires a target",
                                        "Provide a target via positional argument or locator flag",
                                    )
                                })?;
                                let (backend, resolved) =
                                    locators::resolve_target(&page, &cmd_ctx, spec).await?;
                                target_info = Some(resolved);
                                page.get_node_html(backend).await?
                            }
                            "value" => {
                                let spec = maybe_target_spec.as_ref().ok_or_else(|| {
                                    CliError::bad_input(
                                        "get value requires a target",
                                        "Provide a target via positional argument or locator flag",
                                    )
                                })?;
                                let (backend, resolved) =
                                    locators::resolve_target(&page, &cmd_ctx, spec).await?;
                                target_info = Some(resolved);
                                page.get_node_value(backend).await?
                            }
                            "attr" => {
                                let name = attr.as_deref().ok_or_else(|| {
                                    CliError::bad_input(
                                        "get attr requires --attr <name>",
                                        "Provide an attribute name, for example --attr href",
                                    )
                                })?;
                                let spec = maybe_target_spec.as_ref().ok_or_else(|| {
                                    CliError::bad_input(
                                        "get attr requires a target",
                                        "Provide a target via positional argument or locator flag",
                                    )
                                })?;
                                let (backend, resolved) =
                                    locators::resolve_target(&page, &cmd_ctx, spec).await?;
                                target_info = Some(resolved);
                                page.get_node_attr(backend, name).await?.unwrap_or_default()
                            }
                            _ => {
                                return Err(CliError::bad_input(
                                    format!("Unsupported get subject '{}'", subject),
                                    "Use one of: url, title, text, html, value, attr",
                                ))
                            }
                        };
                        let (value, boundaries) = truncate_to_max_bytes(&value, max_bytes);
                        Ok(make_success(
                            command_name,
                            json!({
                                "subject": subject,
                                "value": value,
                                "target": target_info,
                                "contentBoundaries": boundaries,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Is { predicate, target } => {
            let command_name = "is";
            let decision = policy_engine.evaluate(policy::ActionClass::Read, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let target_spec = match target.to_target_spec() {
                Ok(spec) => spec,
                Err(err) => exit_with_error(command_name, err),
            };
            let predicate = predicate.to_ascii_lowercase();
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let target_spec = target_spec.clone();
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    async move {
                        let (backend, resolved) =
                            locators::resolve_target(&page, &cmd_ctx, &target_spec).await?;
                        let value = match predicate.as_str() {
                            "visible" => page.is_node_visible(backend).await?,
                            "enabled" => page.is_node_enabled(backend).await?,
                            "checked" => page.is_node_checked(backend).await?,
                            _ => {
                                return Err(CliError::bad_input(
                                    format!("Unsupported predicate '{}'", predicate),
                                    "Use one of: visible, enabled, checked",
                                ))
                            }
                        };
                        Ok(make_success(
                            command_name,
                            json!({
                                "predicate": predicate,
                                "value": value,
                                "target": resolved,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Select { target, value } => {
            let command_name = "select";
            let decision = policy_engine.evaluate(policy::ActionClass::Interact, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let target_spec = match target.to_target_spec() {
                Ok(spec) => spec,
                Err(err) => exit_with_error(command_name, err),
            };
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let target_spec = target_spec.clone();
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    async move {
                        let (backend, resolved) =
                            locators::resolve_target(&page, &cmd_ctx, &target_spec).await?;
                        page.select_node_value(backend, &value).await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "selected": true,
                                "value": value,
                                "target": resolved,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Check { target } => {
            let command_name = "check";
            let decision = policy_engine.evaluate(policy::ActionClass::Interact, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let target_spec = match target.to_target_spec() {
                Ok(spec) => spec,
                Err(err) => exit_with_error(command_name, err),
            };
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let target_spec = target_spec.clone();
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    async move {
                        let (backend, resolved) =
                            locators::resolve_target(&page, &cmd_ctx, &target_spec).await?;
                        page.set_node_checked(backend, true).await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "checked": true,
                                "target": resolved,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Uncheck { target } => {
            let command_name = "uncheck";
            let decision = policy_engine.evaluate(policy::ActionClass::Interact, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let target_spec = match target.to_target_spec() {
                Ok(spec) => spec,
                Err(err) => exit_with_error(command_name, err),
            };
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let target_spec = target_spec.clone();
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    async move {
                        let (backend, resolved) =
                            locators::resolve_target(&page, &cmd_ctx, &target_spec).await?;
                        page.set_node_checked(backend, false).await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "checked": false,
                                "target": resolved,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Upload { target, files } => {
            let command_name = "upload";
            if files.is_empty() {
                exit_with_error(
                    command_name,
                    CliError::bad_input(
                        "upload requires at least one file path",
                        "Pass one or more file arguments after the target",
                    ),
                );
            }
            let decision = policy_engine.evaluate(policy::ActionClass::Interact, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let target_spec = match target.to_target_spec() {
                Ok(spec) => spec,
                Err(err) => exit_with_error(command_name, err),
            };
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let target_spec = target_spec.clone();
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    let files = files.clone();
                    async move {
                        let (backend, resolved) =
                            locators::resolve_target(&page, &cmd_ctx, &target_spec).await?;
                        page.upload_files(backend, &files).await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "uploaded": true,
                                "count": files.len(),
                                "files": files,
                                "target": resolved,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Download { url, output } => {
            let command_name = "download";
            let decision =
                policy_engine.evaluate(policy::ActionClass::Download, Some(url.as_str()));
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let response = match reqwest::get(&url).await {
                Ok(resp) => resp,
                Err(err) => {
                    exit_with_error(
                        command_name,
                        CliError::unknown(
                            format!("Download request failed: {}", err),
                            "Check network connectivity and URL",
                        ),
                    );
                }
            };
            let status = response.status();
            let bytes = match response.bytes().await {
                Ok(bytes) => bytes,
                Err(err) => {
                    exit_with_error(
                        command_name,
                        CliError::unknown(
                            format!("Failed to read download bytes: {}", err),
                            "Retry the download command",
                        ),
                    );
                }
            };
            if let Err(err) = std::fs::write(&output, &bytes) {
                exit_with_error(
                    command_name,
                    CliError::unknown(
                        format!("Failed to write download to {}: {}", output.display(), err),
                        "Check filesystem permissions and parent directory",
                    ),
                );
            }
            let started_at = Instant::now();
            let meta = build_response_meta(None, started_at);
            print_result(&make_success_with_meta(
                command_name,
                json!({
                    "downloaded": status.is_success(),
                    "status": status.as_u16(),
                    "url": url,
                    "path": output.to_string_lossy(),
                    "bytes": bytes.len(),
                    "policy": decision
                }),
                meta,
            ));
        }
        Commands::Pdf { output } => {
            let command_name = "pdf";
            let decision = policy_engine.evaluate(policy::ActionClass::Read, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let artifact_mode = effective_config.artifact_mode;
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let decision = decision.clone();
                    let output = output.clone();
                    async move {
                        let data = page.print_to_pdf().await?;
                        let bytes = artifacts::decode_base64(&data)?;
                        let sha = artifacts::sha256_hex(&bytes);
                        let path = match artifact_mode {
                            artifacts::ArtifactMode::Path | artifacts::ArtifactMode::Manifest => {
                                let target_path = if let Some(path) = output.clone() {
                                    path
                                } else {
                                    artifacts::write_artifact(
                                        &artifacts::artifact_root(&cmd_ctx.base_dir, &cmd_ctx.instance),
                                        "page",
                                        "pdf",
                                        &bytes,
                                    )?
                                };
                                std::fs::write(&target_path, &bytes).map_err(|e| {
                                    CliError::unknown(
                                        format!("Failed to write PDF {}: {}", target_path.display(), e),
                                        "Check filesystem permissions",
                                    )
                                })?;
                                Some(target_path)
                            }
                            _ => output.clone(),
                        };
                        let artifact = artifacts::ArtifactRef {
                            kind: "pdf".to_string(),
                            mime: "application/pdf".to_string(),
                            path: path.as_ref().map(|p| p.to_string_lossy().to_string()),
                            inline_data: if matches!(artifact_mode, artifacts::ArtifactMode::Inline) {
                                Some(data)
                            } else {
                                None
                            },
                            bytes: Some(bytes.len() as u64),
                            sha256: Some(sha),
                            annotations: None,
                        };
                        let data = match artifact_mode {
                            artifacts::ArtifactMode::Inline => json!({ "mode": "inline", "artifact": artifact }),
                            artifacts::ArtifactMode::Path => json!({ "mode": "path", "artifact": artifact }),
                            artifacts::ArtifactMode::Manifest => json!({ "mode": "manifest", "manifest": artifacts::ArtifactManifest { items: vec![artifact] }}),
                            artifacts::ArtifactMode::None => json!({ "mode": "none" }),
                        };
                        Ok(make_success(
                            command_name,
                            json!({
                                "result": data,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Wait {
            arg,
            load,
            text,
            url,
            selector,
            state,
            count,
            idle,
            function,
        } => {
            let command_name = "wait";
            let decision = policy_engine.evaluate(policy::ActionClass::Read, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let decision = decision.clone();
                    async move {
                        let timeout = Duration::from_millis(timeout_ms_flag.unwrap_or(30_000));
                        let mut selected = 0usize;
                        let positional_ms = arg
                            .as_deref()
                            .and_then(|value| value.parse::<u64>().ok());
                        if positional_ms.is_some() {
                            selected += 1;
                        }
                        if load.is_some() {
                            selected += 1;
                        }
                        if text.is_some() {
                            selected += 1;
                        }
                        if url.is_some() {
                            selected += 1;
                        }
                        if selector.is_some() {
                            selected += 1;
                        }
                        if idle {
                            selected += 1;
                        }
                        if function.is_some() {
                            selected += 1;
                        }
                        if selected != 1 {
                            return Err(CliError::bad_input(
                                "Provide exactly one wait condition",
                                "Use one of positional milliseconds, --load, --text, --url, --selector, --idle, or --fn",
                            ));
                        }
                        if let Some(ms) = positional_ms {
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                            return Ok(make_success(
                                command_name,
                                json!({
                                    "waited": true,
                                    "forMs": ms,
                                    "policy": decision,
                                    "runtime": { "autoStarted": auto_started }
                                }),
                            ));
                        }
                        if let Some(load_state) = load {
                            page.wait_for_load_state(&load_state, timeout).await?;
                            return Ok(make_success(
                                command_name,
                                json!({
                                    "waited": true,
                                    "load": load_state,
                                    "policy": decision,
                                    "runtime": { "autoStarted": auto_started }
                                }),
                            ));
                        }
                        if let Some(needle) = text {
                            page.wait_for_text(&needle, timeout).await?;
                            return Ok(make_success(
                                command_name,
                                json!({
                                    "waited": true,
                                    "text": needle,
                                    "policy": decision,
                                    "runtime": { "autoStarted": auto_started }
                                }),
                            ));
                        }
                        if let Some(pattern) = url {
                            page.wait_for_url(&pattern, timeout).await?;
                            return Ok(make_success(
                                command_name,
                                json!({
                                    "waited": true,
                                    "url": pattern,
                                    "policy": decision,
                                    "runtime": { "autoStarted": auto_started }
                                }),
                            ));
                        }
                        if let Some(sel) = selector {
                            let outcome = page
                                .wait_for_selector_state(
                                    &sel,
                                    state.unwrap_or(SelectorStateArg::Attached).to_browser(),
                                    count,
                                    timeout,
                                )
                                .await?;
                            return Ok(make_success(
                                command_name,
                                json!({
                                    "waited": true,
                                    "selector": outcome.selector,
                                    "state": outcome.state.as_str(),
                                    "count": outcome.count,
                                    "visibleCount": outcome.visible_count,
                                    "expectedCount": outcome.expected_count,
                                    "policy": decision,
                                    "runtime": { "autoStarted": auto_started }
                                }),
                            ));
                        }
                        if idle {
                            page.wait_for_idle(timeout).await?;
                            return Ok(make_success(
                                command_name,
                                json!({
                                    "waited": true,
                                    "idle": true,
                                    "policy": decision,
                                    "runtime": { "autoStarted": auto_started }
                                }),
                            ));
                        }
                        if let Some(expr) = function {
                            page.wait_for_function(&expr, timeout).await?;
                            return Ok(make_success(
                                command_name,
                                json!({
                                    "waited": true,
                                    "function": expr,
                                    "policy": decision,
                                    "runtime": { "autoStarted": auto_started }
                                }),
                            ));
                        }
                        Err(CliError::unknown("No wait condition selected", "Retry with a wait option"))
                    }
                },
            )
            .await;
        }
        Commands::Find { target } => {
            let command_name = "find";
            let decision = policy_engine.evaluate(policy::ActionClass::Read, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let target_spec = match target.to_target_spec() {
                Ok(spec) => spec,
                Err(err) => exit_with_error(command_name, err),
            };
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let target_spec = target_spec.clone();
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    async move {
                        let (_, resolved) =
                            locators::resolve_target(&page, &cmd_ctx, &target_spec).await?;
                        Ok(make_success(
                            command_name,
                            json!({
                                "found": true,
                                "target": resolved,
                                "policy": decision,
                                "runtime": { "autoStarted": auto_started }
                            }),
                        ))
                    }
                },
            )
            .await;
        }
        Commands::Collect {
            snapshot,
            screenshot,
            content,
            full,
            bundle,
            output,
        } => {
            let command_name = "collect";
            let decision = policy_engine.evaluate(policy::ActionClass::Read, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            if !(snapshot || screenshot || content) {
                exit_with_error(
                    command_name,
                    CliError::bad_input(
                        "collect requires at least one action flag",
                        "Use one or more of --snapshot, --screenshot, --content",
                    ),
                );
            }
            let artifact_mode = effective_config.artifact_mode;
            let max_bytes = effective_config.max_bytes;
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let cmd_ctx = cmd_ctx.clone();
                    let decision = decision.clone();
                    async move {
                        let mut output_payload = json!({
                            "policy": decision,
                            "runtime": { "autoStarted": auto_started }
                        });

                        let current_url = page.url().await.unwrap_or_default();
                        output_payload["url"] = json!(current_url);

                        let mut bundle_manifest: Vec<artifacts::ArtifactRef> = Vec::new();

                        if snapshot {
                            let snap = page
                                .snapshot_and_persist(
                                    &cmd_ctx,
                                    types::SnapshotOptions {
                                        interactive: false,
                                        clickable: false,
                                        scope: None,
                                        include_iframes: false,
                                    },
                                )
                                .await?;
                            output_payload["snapshot"] = json!({
                                "snapshotId": snap.snapshot_id,
                                "refCount": snap.refs.len(),
                                "tree": snap.tree
                            });
                        }

                        if screenshot {
                            let shot = page.capture_screenshot(full, browser::ScreenshotQuality::High).await?;
                            let write = artifacts::write_screenshot_artifact(
                                &cmd_ctx.base_dir,
                                &cmd_ctx.instance,
                                artifact_mode,
                                &shot.mime,
                                &shot.extension,
                                &shot.data,
                                output.as_deref(),
                                None,
                            )?;
                            bundle_manifest.push(write.reference.clone());
                            output_payload["screenshot"] = match artifact_mode {
                                artifacts::ArtifactMode::Inline => json!({ "mode": "inline", "artifact": write.reference }),
                                artifacts::ArtifactMode::Path => json!({ "mode": "path", "artifact": write.reference }),
                                artifacts::ArtifactMode::Manifest => json!({ "mode": "manifest", "manifest": artifacts::ArtifactManifest { items: vec![write.reference] } }),
                                artifacts::ArtifactMode::None => json!({ "mode": "none" }),
                            };
                        }

                        if content {
                            let markdown = page.extract_markdown().await?;
                            let (markdown, boundaries) = truncate_to_max_bytes(&markdown, max_bytes);
                            output_payload["content"] = json!(markdown);
                            if let Some(boundaries) = boundaries {
                                output_payload["contentBoundaries"] = boundaries;
                            }
                        }

                        if bundle {
                            output_payload["bundle"] = json!({
                                "url": output_payload["url"],
                                "snapshot": output_payload.get("snapshot").cloned(),
                                "artifacts": artifacts::ArtifactManifest { items: bundle_manifest },
                                "recovery": {
                                    "steps": ["resnapshot", "reacquire-session", "reopen-page"]
                                }
                            });
                        }

                        Ok(make_success(command_name, output_payload))
                    }
                },
            )
            .await;
        }
        Commands::Close { index } => {
            let command_name = "close";
            let decision = policy_engine.evaluate(policy::ActionClass::Interact, None);
            if !matches!(decision.decision, policy::PolicyDecisionKind::Allow) {
                exit_with_error(command_name, policy_decision_error(command_name, &decision));
            }
            let (mut runtime, auto_started) = match ensure_runtime_for_flat_command(
                &store,
                session_id_flag.clone(),
                instance_flag.clone(),
                client_flag.clone(),
                pid_path_flag.clone(),
                user_data_dir_flag.clone(),
                port_flag,
                timeout_ms_flag,
                viewport_override,
                effective_config.ensure_runtime,
                command_name,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(err) => exit_with_error(command_name, err),
            };
            let cmd_ctx = runtime.ctx.clone();
            with_browser_only_command(&mut runtime, port_flag, command_name, move |browser| {
                let cmd_ctx = cmd_ctx.clone();
                let decision = decision.clone();
                async move {
                    let pages = browser.get_targets().await?;
                    let pages: Vec<_> = pages
                        .into_iter()
                        .filter(|target| target.target_type == "page")
                        .collect();
                    if pages.is_empty() {
                        return Err(CliError::bad_input(
                            "No tabs are available to close",
                            "Open a page before running close",
                        ));
                    }

                    let selected_index = if let Some(index) = index {
                        if index >= pages.len() {
                            return Err(CliError::bad_input(
                                format!("Tab index {} out of range ({} tabs)", index, pages.len()),
                                "Run 'sauron tab list' to inspect available tabs",
                            ));
                        }
                        index
                    } else if let Some(bound) = browser::get_bound_target_id(&cmd_ctx)? {
                        pages
                            .iter()
                            .position(|target| target.target_id == bound)
                            .unwrap_or(0)
                    } else {
                        0
                    };

                    let target_id = pages[selected_index].target_id.clone();
                    browser.close_target(&target_id).await?;

                    Ok(make_success(
                        command_name,
                        json!({
                            "closed": true,
                            "index": selected_index,
                            "targetId": target_id,
                            "policy": decision,
                            "runtime": { "autoStarted": auto_started }
                        }),
                    ))
                }
            })
            .await;
        }
        Commands::Runtime { command } => match command {
            RuntimeCommands::Start {
                webgl,
                no_webgl,
                gpu,
                no_gpu,
            } => {
                let started_at = Instant::now();
                let command_name = "runtime.start";
                let mut session = match create_session_record(
                    &store,
                    session_id_flag.clone(),
                    instance_flag.clone(),
                    client_flag.clone(),
                    pid_path_flag.clone(),
                    user_data_dir_flag.clone(),
                ) {
                    Ok(session) => session,
                    Err(e) => exit_with_error(command_name, e),
                };
                let session_viewport = viewport_override.unwrap_or_default();
                session.viewport_width = session_viewport.width;
                session.viewport_height = session_viewport.height;

                let ctx = match AppContext::new(
                    &session.instance,
                    &session.client,
                    session.pid_path.clone(),
                    session.user_data_dir.clone(),
                ) {
                    Ok(c) => c,
                    Err(e) => exit_with_error(command_name, e),
                };
                session.pid_path = Some(ctx.pid_path.clone());
                session.user_data_dir = Some(ctx.user_data_dir.clone());

                let timeout_ms = timeout_ms_flag.unwrap_or(10_000);
                let webgl_enabled = if webgl {
                    true
                } else if cfg!(target_os = "macos") {
                    !no_webgl
                } else {
                    false
                };
                let gpu_enabled = if gpu {
                    true
                } else if cfg!(target_os = "macos") {
                    !no_gpu
                } else {
                    false
                };
                let disable_gpu = !gpu_enabled;

                let _ = store.append_log(&session, command_name, "start", None);
                match daemon::start(
                    ctx.pid_path.clone(),
                    ctx.user_data_dir.clone(),
                    ctx.instance_lock_path.clone(),
                    port_flag,
                    timeout_ms,
                    daemon::ChromeLaunchOptions {
                        headless: true,
                        disable_gpu,
                        webgl: webgl_enabled,
                        viewport: session_viewport,
                    },
                )
                .await
                {
                    Ok(r) => {
                        if let Err(e) = activate_session(&store, &session) {
                            let rollback_summary = if r.reused_existing {
                                "rollback_skipped_reused_daemon".to_string()
                            } else {
                                let rollback = daemon::stop(
                                    &ctx.pid_path,
                                    &ctx.instance_lock_path,
                                    Some(r.port),
                                    timeout_ms,
                                )
                                .await;
                                match rollback {
                                    Ok(true) => "rollback_stopped".to_string(),
                                    Ok(false) => "rollback_not_found".to_string(),
                                    Err(err) => format!("rollback_failed: {}", err.message),
                                }
                            };
                            let _ = store.append_log(
                                &session,
                                command_name,
                                "error",
                                Some(json!({
                                    "message": e.message,
                                    "rollback": rollback_summary
                                })),
                            );
                            exit_with_error(command_name, e);
                        }

                        let active_runtime = ActiveRuntime {
                            store: store.clone(),
                            session: session.clone(),
                            ctx: ctx.clone(),
                        };
                        finish_runtime_command(
                            &active_runtime,
                            command_name,
                            true,
                            json!({ "port": r.port, "pid": r.pid, "store": "filesystem" }),
                        );

                        let project_root = resolve_project_root_path()
                            .ok()
                            .map(|path| path.to_string_lossy().to_string());
                        let meta = build_response_meta(Some(&active_runtime), started_at);
                        print_result(&make_success_with_meta(
                            command_name,
                            json!({
                                "port": r.port,
                                "pid": r.pid,
                                "wsUrl": r.ws_url,
                                "session": {
                                    "sessionId": session.session_id,
                                    "instance": session.instance,
                                    "client": session.client
                                },
                                "viewport": {
                                    "width": session.viewport_width,
                                    "height": session.viewport_height
                                },
                                "projectRoot": project_root
                            }),
                            meta,
                        ));
                    }
                    Err(e) => {
                        let _ = store.append_log(
                            &session,
                            command_name,
                            "error",
                            Some(json!({ "message": e.message })),
                        );
                        exit_with_error(command_name, e);
                    }
                }
            }
            RuntimeCommands::Stop => {
                let started_at = Instant::now();
                let command_name = "runtime.stop";
                let mut runtime =
                    ensure_runtime_or_exit(&store, session_id_flag.clone(), command_name);

                if let Err(e) = begin_runtime_command(&mut runtime, command_name) {
                    exit_with_error(command_name, e);
                }

                let timeout_ms = timeout_ms_flag.unwrap_or(5_000);
                let stopped = match daemon::stop(
                    &runtime.ctx.pid_path,
                    &runtime.ctx.instance_lock_path,
                    port_flag,
                    timeout_ms,
                )
                .await
                {
                    Ok(stopped) => stopped,
                    Err(e) => {
                        finish_runtime_command(
                            &runtime,
                            command_name,
                            false,
                            json!({ "message": e.message }),
                        );
                        exit_with_error(command_name, e);
                    }
                };

                let mut terminate_errors: Vec<String> = Vec::new();
                runtime.session.mark_terminated();
                if let Err(e) = terminate_session(&runtime.store, &runtime.session) {
                    terminate_errors.push(e.message);
                }

                if let Err(e) = cleanup_session_state(&runtime.ctx.base_dir, &runtime.session) {
                    terminate_errors.push(e.message);
                }
                if let Err(e) = cleanup_runtime_context(&runtime.ctx) {
                    terminate_errors.push(e.message);
                }

                if !terminate_errors.is_empty() {
                    finish_runtime_command(
                        &runtime,
                        command_name,
                        false,
                        json!({ "errors": terminate_errors }),
                    );
                    exit_with_error(
                        command_name,
                        CliError::unknown(
                            "runtime stop completed with cleanup errors",
                            "Check filesystem permissions and retry",
                        ),
                    );
                }

                finish_runtime_command(
                    &runtime,
                    command_name,
                    true,
                    json!({ "daemonStopped": stopped }),
                );
                let removed_log_artifacts = cleanup_terminate_log_artifacts(&runtime);
                let meta = build_response_meta(Some(&runtime), started_at);
                print_result(&make_success_with_meta(
                    command_name,
                    json!({
                        "daemonStopped": stopped,
                        "logArtifactsRemoved": removed_log_artifacts
                    }),
                    meta,
                ));
            }
            RuntimeCommands::Status => {
                let started_at = Instant::now();
                let command_name = "runtime.status";
                let status_target = match resolve_runtime_status_target(
                    &store,
                    session_id_flag.clone(),
                    instance_flag.clone(),
                    client_flag.clone(),
                    pid_path_flag.clone(),
                    user_data_dir_flag.clone(),
                    port_flag,
                ) {
                    Ok(target) => target,
                    Err(e) => exit_with_error(command_name, e),
                };

                let (runtime, status_payload) = match status_target {
                    RuntimeStatusTarget::Runtime(mut runtime) => {
                        if let Err(e) = begin_runtime_command(&mut runtime, command_name) {
                            exit_with_error(command_name, e);
                        }

                        let port = runtime.ctx.resolve_port(port_flag);
                        let status_payload = status_payload_from_daemon_status(
                            daemon::get_status(&runtime.ctx.pid_path, port).await,
                        );
                        finish_runtime_command(
                            &runtime,
                            command_name,
                            true,
                            json!({ "status": status_payload["status"] }),
                        );
                        (Some(*runtime), status_payload)
                    }
                    RuntimeStatusTarget::Probe(probe) => {
                        let status_payload = status_payload_from_daemon_status(
                            daemon::get_status(&probe.pid_path, probe.port).await,
                        );
                        (None, status_payload)
                    }
                    RuntimeStatusTarget::Missing => (None, json!({ "status": "stopped" })),
                };
                let meta = build_response_meta(runtime.as_ref(), started_at);
                print_result(&make_success_with_meta(command_name, status_payload, meta));
            }
            RuntimeCommands::Cleanup => {
                let started_at = Instant::now();
                let command_name = "runtime.cleanup";
                let base_dir = match context::resolve_base_dir() {
                    Ok(base_dir) => base_dir,
                    Err(e) => exit_with_error(command_name, e),
                };
                let stats = match run_cleanup(&base_dir) {
                    Ok(stats) => stats,
                    Err(e) => exit_with_error(command_name, e),
                };
                let meta = build_response_meta(None, started_at);
                print_result(&make_success_with_meta(
                    command_name,
                    json!({ "cleanup": cleanup_stats_json(stats) }),
                    meta,
                ));
            }
        },

        Commands::Page { command } => {
            let command_name = page_command_label(&command);
            let mut runtime = ensure_runtime_or_exit(&store, session_id_flag.clone(), command_name);
            let ctx = runtime.ctx.clone();

            match command {
                PageCommands::Goto { url, until } => {
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "page.goto",
                        |page| async move {
                            let timeout = Duration::from_millis(timeout_ms_flag.unwrap_or(30_000));
                            let outcome = page.navigate(&url, &until, timeout).await?;
                            #[derive(Serialize)]
                            #[serde(rename_all = "camelCase")]
                            struct Out {
                                url: String,
                                #[serde(skip_serializing_if = "Option::is_none")]
                                status: Option<i64>,
                            }
                            Ok(make_success(
                                "page.goto",
                                Out {
                                    url,
                                    status: outcome.status,
                                },
                            ))
                        },
                    )
                    .await;
                }
                PageCommands::Snapshot {
                    interactive,
                    clickable,
                    scope,
                    include_iframes,
                    format,
                } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "page.snapshot",
                        move |page| {
                            let cmd_ctx = cmd_ctx.clone();
                            async move {
                                let opts = types::SnapshotOptions {
                                    interactive,
                                    clickable,
                                    scope: scope.clone(),
                                    include_iframes,
                                };
                                let snap = page.snapshot_and_persist(&cmd_ctx, opts).await?;
                                let refs: Vec<serde_json::Value> = snap
                                    .refs
                                    .iter()
                                    .map(|(id, reference)| {
                                        json!({
                                            "id": id,
                                            "role": reference.role,
                                            "name": reference.name,
                                            "locator": reference.locator
                                        })
                                    })
                                    .collect();

                                let payload = match format {
                                    SnapshotFormatArg::Text => json!({
                                        "format": "text",
                                        "url": snap.url,
                                        "snapshotId": snap.snapshot_id,
                                        "refCount": snap.refs.len(),
                                        "tree": snap.tree
                                    }),
                                    SnapshotFormatArg::Json => json!({
                                        "format": "json",
                                        "url": snap.url,
                                        "snapshotId": snap.snapshot_id,
                                        "refCount": snap.refs.len(),
                                        "tree": snap.tree,
                                        "refs": refs
                                    }),
                                    SnapshotFormatArg::Nodes => {
                                        let ax = page.accessibility_tree().await?;
                                        let nodes = snapshot_nodes::flatten_snapshot_nodes(
                                            &ax,
                                            clickable,
                                            scope.as_deref(),
                                        );
                                        json!({
                                            "format": "nodes",
                                            "url": snap.url,
                                            "snapshotId": snap.snapshot_id,
                                            "refCount": snap.refs.len(),
                                            "tree": snap.tree,
                                            "refs": refs,
                                            "nodes": nodes
                                        })
                                    }
                                };

                                Ok(make_success("page.snapshot", payload))
                            }
                        },
                    )
                    .await;
                }
                PageCommands::Screenshot {
                    full,
                    responsive,
                    quality,
                    output,
                    output_dir,
                } => {
                    let cmd_ctx = ctx.clone();
                    let artifact_mode = effective_config.artifact_mode;
                    let responsive_desktop =
                        viewport_override.unwrap_or(runtime.session.viewport());
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "page.screenshot",
                        move |page| {
                            let cmd_ctx = cmd_ctx.clone();
                            async move {
                            let quality_profile = quality.to_browser();
                            let output_file = output.clone();
                            let output_dir_arg = output_dir.clone();

                            if !responsive && output_dir_arg.is_some() {
                                return Err(CliError::bad_input(
                                    "--output-dir requires --responsive",
                                    "Add --responsive, or use --output <file> for a single screenshot",
                                ));
                            }

                            if responsive {
                                let output_dir = if let Some(p) = output_dir_arg.or(output_file.clone()) {
                                    if p.exists() {
                                        let meta = std::fs::metadata(&p).map_err(|e| {
                                            CliError::unknown(
                                                format!(
                                                    "Failed to inspect screenshot path {}: {}",
                                                    p.display(),
                                                    e
                                                ),
                                                "Check filesystem permissions",
                                            )
                                        })?;
                                        if !meta.is_dir() {
                                            return Err(CliError::bad_input(
                                                format!(
                                                    "Responsive screenshots require a directory path, got file: {}",
                                                    p.display()
                                                ),
                                                "Use --output-dir <directory> when --responsive is set",
                                            ));
                                        }
                                    } else if path_looks_like_image_file(&p) {
                                        return Err(CliError::bad_input(
                                            format!(
                                                "Responsive screenshots require a directory path, got file-like path: {}",
                                                p.display()
                                            ),
                                            "Use --output-dir <directory> when --responsive is set",
                                        ));
                                    }
                                    p
                                } else {
                                    std::env::current_dir().map_err(|e| {
                                        CliError::unknown(
                                            format!(
                                                "Failed to resolve current directory for screenshots: {}",
                                                e
                                            ),
                                            "Run the command from an accessible directory",
                                        )
                                    })?
                                };

                                tokio::fs::create_dir_all(&output_dir).await.map_err(|e| {
                                    CliError::unknown(
                                        format!(
                                            "Failed to create screenshot directory {}: {}",
                                            output_dir.display(),
                                            e
                                        ),
                                        "Check filesystem permissions",
                                    )
                                })?;

                                let mut screenshots: Vec<serde_json::Value> = Vec::new();
                                for preset in responsive_screenshot_presets(responsive_desktop) {
                                    page.set_viewport(preset.width, preset.height, preset.mobile)
                                        .await?;
                                    tokio::time::sleep(Duration::from_millis(200)).await;
                                    let shot =
                                        page.capture_screenshot(full, quality_profile).await?;
                                    let file_name =
                                        format!("screenshot-{}.{}", preset.name, shot.extension);
                                    let file_path = output_dir.join(file_name);
                                    let bytes = base64::engine::general_purpose::STANDARD
                                        .decode(&shot.data)
                                        .map_err(|e| {
                                            CliError::unknown(
                                                format!("Invalid base64 screenshot: {}", e),
                                                "This should not happen",
                                            )
                                        })?;
                                    tokio::fs::write(&file_path, bytes).await.map_err(|e| {
                                        CliError::unknown(
                                            format!(
                                                "Failed to write screenshot to {}: {}",
                                                file_path.display(),
                                                e
                                            ),
                                            "Check filesystem permissions",
                                        )
                                    })?;
                                    screenshots.push(json!({
                                        "preset": preset.name,
                                        "width": preset.width,
                                        "height": preset.height,
                                        "path": file_path.to_string_lossy(),
                                        "saved": true,
                                        "mime": shot.mime
                                    }));
                                }

                                Ok(make_success(
                                    "page.screenshot",
                                    json!({
                                        "responsive": true,
                                        "mode": "responsive",
                                        "quality": quality.as_str(),
                                        "screenshots": screenshots
                                    }),
                                ))
                            } else if let Some(p) = output_file {
                                let shot = page.capture_screenshot(full, quality_profile).await?;
                                validate_screenshot_output_path(&p, shot.extension.as_str())?;
                                let bytes = base64::engine::general_purpose::STANDARD
                                    .decode(&shot.data)
                                    .map_err(|e| {
                                        CliError::unknown(
                                            format!("Invalid base64 screenshot: {}", e),
                                            "",
                                        )
                                    })?;
                                tokio::fs::write(&p, bytes).await.map_err(|e| {
                                    CliError::unknown(
                                        format!(
                                            "Failed to write screenshot to {}: {}",
                                            p.display(),
                                            e
                                        ),
                                        "Check filesystem permissions",
                                    )
                                })?;
                                Ok(make_success(
                                    "page.screenshot",
                                    json!({
                                        "mode": "saved",
                                        "saved": true,
                                        "path": p.to_string_lossy(),
                                        "mime": shot.mime,
                                        "quality": quality.as_str()
                                    }),
                                ))
                            } else {
                                let shot = page.capture_screenshot(full, quality_profile).await?;
                                if matches!(artifact_mode, artifacts::ArtifactMode::Inline) {
                                    Ok(make_success(
                                        "page.screenshot",
                                        json!({
                                            "mode": "inline",
                                            "data": shot.data,
                                            "mime": shot.mime,
                                            "quality": quality.as_str()
                                        }),
                                    ))
                                } else {
                                    let write = artifacts::write_screenshot_artifact(
                                        &cmd_ctx.base_dir,
                                        &cmd_ctx.instance,
                                        artifact_mode,
                                        &shot.mime,
                                        &shot.extension,
                                        &shot.data,
                                        None,
                                        None,
                                    )?;
                                    let payload = match artifact_mode {
                                        artifacts::ArtifactMode::Path => json!({
                                            "mode": "path",
                                            "artifact": write.reference,
                                            "quality": quality.as_str()
                                        }),
                                        artifacts::ArtifactMode::Manifest => json!({
                                            "mode": "manifest",
                                            "manifest": artifacts::ArtifactManifest { items: vec![write.reference] },
                                            "quality": quality.as_str()
                                        }),
                                        artifacts::ArtifactMode::None => json!({
                                            "mode": "none",
                                            "quality": quality.as_str()
                                        }),
                                        artifacts::ArtifactMode::Inline => unreachable!(),
                                    };
                                    Ok(make_success("page.screenshot", payload))
                                }
                            }
                        }
                        },
                    )
                    .await;
                }
                PageCommands::Collect {
                    snapshot,
                    screenshot,
                    content,
                    interactive,
                    clickable,
                    scope,
                    include_iframes,
                    full,
                    quality,
                    output,
                } => {
                    let cmd_ctx = ctx.clone();
                    let artifact_mode = effective_config.artifact_mode;
                    let max_bytes = effective_config.max_bytes;
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "page.collect",
                        move |page| {
                            let cmd_ctx = cmd_ctx.clone();
                            let artifact_mode = artifact_mode;
                            let max_bytes = max_bytes;
                            async move {
                                if !(snapshot || screenshot || content) {
                                    return Err(CliError::bad_input(
                                        "collect requires at least one action flag",
                                        "Use one or more of: --snapshot, --screenshot, --content",
                                    ));
                                }

                                let page_for_url = page.clone();
                                let url_fut = async move { page_for_url.url().await.ok() };

                                let page_for_snapshot = page.clone();
                                let cmd_ctx_for_snapshot = cmd_ctx.clone();
                                let scope_for_snapshot = scope.clone();
                                let snapshot_fut = async move {
                                    if !snapshot {
                                        return Ok::<Option<serde_json::Value>, CliError>(None);
                                    }
                                    let opts = types::SnapshotOptions {
                                        interactive,
                                        clickable,
                                        scope: scope_for_snapshot,
                                        include_iframes,
                                    };
                                    let snap = page_for_snapshot
                                        .snapshot_and_persist(&cmd_ctx_for_snapshot, opts)
                                        .await?;
                                    Ok(Some(json!({
                                        "url": snap.url,
                                        "snapshotId": snap.snapshot_id,
                                        "refCount": snap.refs.len(),
                                        "tree": snap.tree
                                    })))
                                };

                                let page_for_screenshot = page.clone();
                                let screenshot_path_for_task = output.clone();
                                let screenshot_quality_for_task = quality.to_browser();
                                let cmd_ctx_for_screenshot = cmd_ctx.clone();
                                let screenshot_fut = async move {
                                    if !screenshot {
                                        return Ok::<Option<serde_json::Value>, CliError>(None);
                                    }
                                    let shot = page_for_screenshot
                                        .capture_screenshot(full, screenshot_quality_for_task)
                                        .await?;
                                    if let Some(p) = screenshot_path_for_task {
                                        validate_screenshot_output_path(
                                            &p,
                                            shot.extension.as_str(),
                                        )?;
                                        let bytes = base64::engine::general_purpose::STANDARD
                                            .decode(&shot.data)
                                            .map_err(|e| {
                                                CliError::unknown(
                                                    format!("Invalid base64 screenshot: {}", e),
                                                    "This should not happen",
                                                )
                                            })?;
                                        tokio::fs::write(&p, bytes).await.map_err(|e| {
                                            CliError::unknown(
                                                format!(
                                                    "Failed to write screenshot to {}: {}",
                                                    p.display(),
                                                    e
                                                ),
                                                "Check filesystem permissions",
                                            )
                                        })?;
                                        Ok(Some(json!({
                                            "saved": true,
                                            "path": p.to_string_lossy(),
                                            "mime": shot.mime,
                                            "quality": quality.as_str()
                                        })))
                                    } else if matches!(artifact_mode, artifacts::ArtifactMode::Inline) {
                                        Ok(Some(json!({
                                            "mode": "inline",
                                            "data": shot.data,
                                            "mime": shot.mime,
                                            "quality": quality.as_str()
                                        })))
                                    } else {
                                        let write = artifacts::write_screenshot_artifact(
                                            &cmd_ctx_for_screenshot.base_dir,
                                            &cmd_ctx_for_screenshot.instance,
                                            artifact_mode,
                                            &shot.mime,
                                            &shot.extension,
                                            &shot.data,
                                            None,
                                            None,
                                        )?;
                                        let payload = match artifact_mode {
                                            artifacts::ArtifactMode::Path => json!({
                                                "mode": "path",
                                                "artifact": write.reference,
                                                "quality": quality.as_str()
                                            }),
                                            artifacts::ArtifactMode::Manifest => json!({
                                                "mode": "manifest",
                                                "manifest": artifacts::ArtifactManifest { items: vec![write.reference] },
                                                "quality": quality.as_str()
                                            }),
                                            artifacts::ArtifactMode::None => json!({
                                                "mode": "none",
                                                "quality": quality.as_str()
                                            }),
                                            artifacts::ArtifactMode::Inline => unreachable!(),
                                        };
                                        Ok(Some(payload))
                                    }
                                };

                                let page_for_content = page.clone();
                                let content_fut = async move {
                                    if !content {
                                        return Ok::<Option<String>, CliError>(None);
                                    }
                                    let markdown = page_for_content.extract_markdown().await?;
                                    let (markdown, _) = truncate_to_max_bytes(&markdown, max_bytes);
                                    Ok(Some(markdown))
                                };

                                let (url_opt, snap_res, shot_res, content_res) = tokio::join!(
                                    url_fut,
                                    snapshot_fut,
                                    screenshot_fut,
                                    content_fut
                                );

                                let mut errors: Vec<serde_json::Value> = Vec::new();
                                let mut first_error: Option<CliError> = None;

                                let snapshot_out = match snap_res {
                                    Ok(v) => v,
                                    Err(e) => {
                                        if first_error.is_none() {
                                            first_error = Some(e.clone());
                                        }
                                        errors.push(json!({
                                            "action": "snapshot",
                                            "code": e.code,
                                            "message": e.message,
                                            "hint": e.hint,
                                            "recoverable": e.recoverable
                                        }));
                                        None
                                    }
                                };

                                let screenshot_out = match shot_res {
                                    Ok(v) => v,
                                    Err(e) => {
                                        if first_error.is_none() {
                                            first_error = Some(e.clone());
                                        }
                                        errors.push(json!({
                                            "action": "screenshot",
                                            "code": e.code,
                                            "message": e.message,
                                            "hint": e.hint,
                                            "recoverable": e.recoverable
                                        }));
                                        None
                                    }
                                };

                                let content_out = match content_res {
                                    Ok(v) => v,
                                    Err(e) => {
                                        if first_error.is_none() {
                                            first_error = Some(e.clone());
                                        }
                                        errors.push(json!({
                                            "action": "content",
                                            "code": e.code,
                                            "message": e.message,
                                            "hint": e.hint,
                                            "recoverable": e.recoverable
                                        }));
                                        None
                                    }
                                };

                                let any_success = snapshot_out.is_some()
                                    || screenshot_out.is_some()
                                    || content_out.is_some();

                                if !any_success {
                                    if let Some(e) = first_error {
                                        return Err(e);
                                    }
                                    return Err(CliError::unknown(
                                        "collect failed",
                                        "Try running the actions individually to isolate the failure",
                                    ));
                                }

                                #[derive(Serialize)]
                                #[serde(rename_all = "camelCase")]
                                struct Out {
                                    #[serde(skip_serializing_if = "Option::is_none")]
                                    url: Option<String>,
                                    #[serde(skip_serializing_if = "Option::is_none")]
                                    snapshot: Option<serde_json::Value>,
                                    #[serde(skip_serializing_if = "Option::is_none")]
                                    screenshot: Option<serde_json::Value>,
                                    #[serde(skip_serializing_if = "Option::is_none")]
                                    content: Option<String>,
                                    #[serde(skip_serializing_if = "Vec::is_empty", default)]
                                    errors: Vec<serde_json::Value>,
                                }

                                Ok(make_success(
                                    "page.collect",
                                    Out {
                                        url: url_opt,
                                        snapshot: snapshot_out,
                                        screenshot: screenshot_out,
                                        content: content_out,
                                        errors,
                                    },
                                ))
                            }
                        },
                    )
                    .await;
                }
                PageCommands::Markdown => {
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "page.markdown",
                        |page| async move {
                            let url = page.url().await?;
                            let markdown = page.extract_markdown().await?;
                            Ok(make_success(
                                "page.markdown",
                                json!({ "url": url, "markdown": markdown }),
                            ))
                        },
                    )
                    .await;
                }
                PageCommands::Wait {
                    for_ms,
                    text,
                    url,
                    selector,
                    state,
                    count,
                    idle,
                } => {
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "page.wait",
                        |page| async move {
                            let timeout = Duration::from_millis(timeout_ms_flag.unwrap_or(30_000));
                            if selector.is_none() && (state.is_some() || count.is_some()) {
                                return Err(CliError::bad_input(
                                    "--state/--count require --selector",
                                    "Provide --selector when using --state or --count",
                                ));
                            }
                            let mut selected = 0usize;
                            if for_ms.is_some() {
                                selected += 1;
                            }
                            if text.is_some() {
                                selected += 1;
                            }
                            if url.is_some() {
                                selected += 1;
                            }
                            if selector.is_some() {
                                selected += 1;
                            }
                            if idle {
                                selected += 1;
                            }
                            if selected != 1 {
                                return Err(CliError::bad_input(
                                    "Provide exactly one wait condition",
                                    "Use exactly one of --for-ms/--text/--url/--selector/--idle",
                                ));
                            }

                            if let Some(ms) = for_ms {
                                tokio::time::sleep(Duration::from_millis(ms)).await;
                                return Ok(make_success(
                                    "page.wait",
                                    json!({ "waited": true, "forMs": ms }),
                                ));
                            }
                            if let Some(t) = text {
                                page.wait_for_text(&t, timeout).await?;
                                return Ok(make_success(
                                    "page.wait",
                                    json!({ "waited": true, "text": t }),
                                ));
                            }
                            if let Some(u) = url {
                                page.wait_for_url(&u, timeout).await?;
                                return Ok(make_success(
                                    "page.wait",
                                    json!({ "waited": true, "url": u }),
                                ));
                            }
                            if let Some(sel) = selector {
                                let wait_state =
                                    state.unwrap_or(SelectorStateArg::Attached).to_browser();
                                let outcome = page
                                    .wait_for_selector_state(&sel, wait_state, count, timeout)
                                    .await?;
                                return Ok(make_success(
                                    "page.wait",
                                    json!({
                                        "waited": true,
                                        "selector": sel,
                                        "state": wait_state.as_str(),
                                        "count": outcome.count,
                                        "visibleCount": outcome.visible_count,
                                        "expectedCount": outcome.expected_count
                                    }),
                                ));
                            }
                            if idle {
                                page.wait_for_idle(timeout).await?;
                                return Ok(make_success(
                                    "page.wait",
                                    json!({ "waited": true, "idle": true }),
                                ));
                            }

                            Err(CliError::unknown(
                                "wait condition resolution failed",
                                "Retry command",
                            ))
                        },
                    )
                    .await;
                }
                PageCommands::Js { expression } => {
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "page.js",
                        |page| async move {
                            let value = page.eval(&expression).await?;
                            Ok(make_success("page.js", json!({ "result": value })))
                        },
                    )
                    .await;
                }
                PageCommands::Diff { from, to, format } => {
                    let command_name = "page.diff";
                    let started_at = Instant::now();
                    if let Err(e) = begin_runtime_command(&mut runtime, command_name) {
                        exit_with_error(command_name, e);
                    }

                    let state = match browser::load_ref_state(&ctx).await {
                        Ok(Some(state)) => state,
                        Ok(None) => {
                            let err = CliError::bad_input(
                                "No snapshots available",
                                "Run 'sauron page snapshot' first",
                            );
                            finish_runtime_command(
                                &runtime,
                                command_name,
                                false,
                                json!({ "message": err.message }),
                            );
                            exit_with_error(command_name, err);
                        }
                        Err(e) => {
                            finish_runtime_command(
                                &runtime,
                                command_name,
                                false,
                                json!({ "message": e.message }),
                            );
                            exit_with_error(command_name, e);
                        }
                    };

                    let to_id = to.unwrap_or(state.snapshot_id);
                    if to_id == 0 {
                        let err = CliError::bad_input(
                            "Invalid --to snapshot id",
                            "--to must be greater than zero",
                        );
                        finish_runtime_command(
                            &runtime,
                            command_name,
                            false,
                            json!({ "message": err.message }),
                        );
                        exit_with_error(command_name, err);
                    }

                    let from_id = from.unwrap_or(to_id.saturating_sub(1));
                    if from_id == 0 {
                        let err = CliError::bad_input(
                            "Need at least two snapshots to diff",
                            "Provide --from/--to, or capture another snapshot",
                        );
                        finish_runtime_command(
                            &runtime,
                            command_name,
                            false,
                            json!({ "message": err.message }),
                        );
                        exit_with_error(command_name, err);
                    }
                    if from_id == to_id {
                        let err = CliError::bad_input(
                            "--from and --to cannot be equal",
                            "Choose different snapshot ids",
                        );
                        finish_runtime_command(
                            &runtime,
                            command_name,
                            false,
                            json!({ "message": err.message }),
                        );
                        exit_with_error(command_name, err);
                    }

                    let prev = match browser::load_snapshot(&ctx, from_id).await {
                        Ok(Some(prev)) => prev,
                        Ok(None) => {
                            let err = CliError::unknown(
                                format!("Snapshot {} not found", from_id),
                                "Run 'sauron page snapshot' again",
                            );
                            finish_runtime_command(
                                &runtime,
                                command_name,
                                false,
                                json!({ "message": err.message }),
                            );
                            exit_with_error(command_name, err);
                        }
                        Err(e) => {
                            finish_runtime_command(
                                &runtime,
                                command_name,
                                false,
                                json!({ "message": e.message }),
                            );
                            exit_with_error(command_name, e);
                        }
                    };

                    let after = if to_id == state.snapshot_id {
                        state.last_snapshot
                    } else {
                        match browser::load_snapshot(&ctx, to_id).await {
                            Ok(Some(after)) => after,
                            Ok(None) => {
                                let err = CliError::unknown(
                                    format!("Snapshot {} not found", to_id),
                                    "Run 'sauron page snapshot' again",
                                );
                                finish_runtime_command(
                                    &runtime,
                                    command_name,
                                    false,
                                    json!({ "message": err.message }),
                                );
                                exit_with_error(command_name, err);
                            }
                            Err(e) => {
                                finish_runtime_command(
                                    &runtime,
                                    command_name,
                                    false,
                                    json!({ "message": e.message }),
                                );
                                exit_with_error(command_name, e);
                            }
                        }
                    };
                    let d = diff::diff_snapshots(&prev, &after);

                    if format == "json" {
                        let meta = build_response_meta(Some(&runtime), started_at);
                        print_result(&make_success_with_meta(
                            command_name,
                            json!({
                                "added": d.added,
                                "removed": d.removed,
                                "changed": d.changed,
                                "unified": d.unified,
                                "fromSnapshotId": from_id,
                                "toSnapshotId": to_id
                            }),
                            meta,
                        ));
                    } else {
                        let meta = build_response_meta(Some(&runtime), started_at);
                        print_result(&make_success_with_meta(
                            command_name,
                            json!({
                                "unified": d.unified,
                                "fromSnapshotId": from_id,
                                "toSnapshotId": to_id
                            }),
                            meta,
                        ));
                    }
                    finish_runtime_command(
                        &runtime,
                        command_name,
                        true,
                        json!({ "fromSnapshotId": from_id, "toSnapshotId": to_id }),
                    );
                }
                PageCommands::Dialog { action, text } => {
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "page.dialog",
                        |page| async move {
                            let timeout = Duration::from_millis(2_000);
                            let d = page.next_dialog(timeout).await?;
                            let Some(d) = d else {
                                return Err(CliError::new(
                                    types::ErrorCode::Timeout,
                                    "No dialog appeared within 2000ms".to_string(),
                                    "Trigger the dialog before running this command",
                                    true,
                                    1,
                                ));
                            };

                            let accept = match action.as_str() {
                                "accept" => true,
                                "dismiss" => false,
                                _ => {
                                    return Err(CliError::bad_input(
                                        "Invalid dialog action",
                                        "Use 'accept' or 'dismiss'",
                                    ))
                                }
                            };

                            page.handle_dialog(accept, text.as_deref()).await?;
                            Ok(make_success(
                                "page.dialog",
                                json!({
                                    "action": action,
                                    "type": d.dialog_type,
                                    "message": d.message,
                                    "text": text
                                }),
                            ))
                        },
                    )
                    .await;
                }
            }
        }

        Commands::Input { command } => {
            let command_name = input_command_label(&command);
            let mut runtime = ensure_runtime_or_exit(&store, session_id_flag.clone(), command_name);
            let ctx = runtime.ctx.clone();

            match command {
                InputCommands::Click { target, double } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "input.click",
                        move |page| {
                            let cmd_ctx = cmd_ctx.clone();
                            async move {
                                let (backend, target_resolution) =
                                    resolve_backend_from_target(&page, &cmd_ctx, &target).await?;
                                page.click(backend, double).await?;
                                Ok(make_success(
                                    "input.click",
                                    json!({
                                        "clicked": true,
                                        "double": double,
                                        "targetResolution": target_resolution
                                    }),
                                ))
                            }
                        },
                    )
                    .await;
                }
                InputCommands::Fill { target, value } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "input.fill",
                        move |page| {
                            let cmd_ctx = cmd_ctx.clone();
                            async move {
                                let (backend, target_resolution) =
                                    resolve_backend_from_target(&page, &cmd_ctx, &target).await?;
                                let typ = page.fill(backend, &value).await?;
                                Ok(make_success(
                                    "input.fill",
                                    json!({
                                        "filled": true,
                                        "type": typ,
                                        "targetResolution": target_resolution
                                    }),
                                ))
                            }
                        },
                    )
                    .await;
                }
                InputCommands::Hover { target } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "input.hover",
                        move |page| {
                            let cmd_ctx = cmd_ctx.clone();
                            async move {
                                let (backend, target_resolution) =
                                    resolve_backend_from_target(&page, &cmd_ctx, &target).await?;
                                page.hover(backend).await?;
                                Ok(make_success(
                                    "input.hover",
                                    json!({
                                        "hovered": true,
                                        "targetResolution": target_resolution
                                    }),
                                ))
                            }
                        },
                    )
                    .await;
                }
                InputCommands::Scroll {
                    direction,
                    amount,
                    target,
                } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "input.scroll",
                        move |page| {
                            let cmd_ctx = cmd_ctx.clone();
                            async move {
                                if let Some(direction) = direction {
                                    if !target.is_empty() {
                                        return Err(CliError::bad_input(
                                            "Do not combine --direction with --selector/--ref/--text",
                                            "Use directional scroll or target scroll, not both",
                                        ));
                                    }
                                    let dir = match direction {
                                        ScrollDirectionArg::Up => "up",
                                        ScrollDirectionArg::Down => "down",
                                        ScrollDirectionArg::Top => "top",
                                        ScrollDirectionArg::Bottom => "bottom",
                                    };
                                    let expr = match direction {
                                        ScrollDirectionArg::Top => "window.scrollTo(0, 0)".to_string(),
                                        ScrollDirectionArg::Bottom => {
                                            "window.scrollTo(0, document.body.scrollHeight)"
                                                .to_string()
                                        }
                                        ScrollDirectionArg::Up => format!("window.scrollBy(0, -{})", amount),
                                        ScrollDirectionArg::Down => format!("window.scrollBy(0, {})", amount),
                                    };
                                    let _ = page.eval(&expr).await?;
                                    Ok(make_success(
                                        "input.scroll",
                                        json!({ "direction": dir, "amount": amount }),
                                    ))
                                } else {
                                    let (backend, target_resolution) =
                                        resolve_backend_from_target(&page, &cmd_ctx, &target)
                                            .await?;
                                    page.scroll_into_view(backend).await?;
                                    Ok(make_success(
                                        "input.scroll",
                                        json!({
                                            "scrolledIntoView": true,
                                            "targetResolution": target_resolution
                                        }),
                                    ))
                                }
                            }
                        },
                    )
                    .await;
                }
                InputCommands::Press { combo } => {
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        "input.press",
                        |page| async move {
                            page.press_key(&combo).await?;
                            Ok(make_success(
                                "input.press",
                                json!({ "key": combo, "pressed": true }),
                            ))
                        },
                    )
                    .await;
                }
            }
        }

        Commands::Tab { command } => {
            let command_name = tab_command_label(&command);
            let mut runtime = ensure_runtime_or_exit(&store, session_id_flag.clone(), command_name);
            let ctx = runtime.ctx.clone();
            let cmd_ctx = ctx.clone();

            with_browser_only_command(&mut runtime, port_flag, command_name, move |browser| {
                let cmd_ctx = cmd_ctx.clone();
                async move {
                    match command {
                        TabCommands::List => {
                            let bound_target_id = browser::get_bound_target_id(&cmd_ctx)?;
                            let pages = browser.get_targets().await?;
                            let pages: Vec<_> = pages
                                .into_iter()
                                .filter(|t| t.target_type == "page")
                                .map(|t| {
                                    let bound =
                                        bound_target_id.as_deref() == Some(t.target_id.as_str());
                                    json!({
                                        "targetId": t.target_id,
                                        "url": t.url,
                                        "title": t.title,
                                        "isAttached": t.attached,
                                        "bound": bound,
                                    })
                                })
                                .collect();

                            Ok(make_success(
                                command_name,
                                json!({
                                    "action": "list",
                                    "boundTargetId": bound_target_id,
                                    "tabs": pages
                                }),
                            ))
                        }

                        TabCommands::Open { url } => {
                            let target_id = browser.create_target(&url).await?;
                            browser.activate_target(&target_id).await?;
                            browser::set_bound_target_id(&cmd_ctx, &target_id)?;

                            Ok(make_success(
                                command_name,
                                json!({
                                    "action": "open",
                                    "url": url,
                                    "targetId": target_id,
                                    "bound": true
                                }),
                            ))
                        }

                        TabCommands::Switch { index } => {
                            let pages = browser.get_targets().await?;
                            let pages: Vec<_> = pages
                                .into_iter()
                                .filter(|t| t.target_type == "page")
                                .collect();
                            if index >= pages.len() {
                                return Err(CliError::bad_input(
                                    format!(
                                        "Tab index {} out of range ({} tabs)",
                                        index,
                                        pages.len()
                                    ),
                                    "Run `sauron tab list` and choose a valid index",
                                ));
                            }
                            let target_id = pages[index].target_id.clone();
                            browser.activate_target(&target_id).await?;
                            browser::set_bound_target_id(&cmd_ctx, &target_id)?;

                            Ok(make_success(
                                command_name,
                                json!({
                                    "action": "switch",
                                    "index": index,
                                    "targetId": target_id,
                                    "bound": true
                                }),
                            ))
                        }

                        TabCommands::Close { index } => {
                            let pages = browser.get_targets().await?;
                            let pages: Vec<_> = pages
                                .into_iter()
                                .filter(|t| t.target_type == "page")
                                .collect();
                            if index >= pages.len() {
                                return Err(CliError::bad_input(
                                    format!(
                                        "Tab index {} out of range ({} tabs)",
                                        index,
                                        pages.len()
                                    ),
                                    "Run `sauron tab list` and choose a valid index",
                                ));
                            }

                            let target_id = pages[index].target_id.clone();
                            let bound_target_id = browser::get_bound_target_id(&cmd_ctx)?;
                            let was_bound = bound_target_id.as_deref() == Some(target_id.as_str());

                            browser.close_target(&target_id).await?;

                            let mut new_bound: Option<String> = None;
                            if was_bound {
                                let remaining = browser.get_targets().await?;
                                let remaining: Vec<_> = remaining
                                    .into_iter()
                                    .filter(|t| t.target_type == "page")
                                    .collect();

                                if let Some(t) = remaining
                                    .iter()
                                    .find(|t| t.url != "about:blank")
                                    .or_else(|| remaining.first())
                                {
                                    browser.activate_target(&t.target_id).await?;
                                    browser::set_bound_target_id(&cmd_ctx, &t.target_id)?;
                                    new_bound = Some(t.target_id.clone());
                                } else {
                                    let id = browser.create_target("about:blank").await?;
                                    browser.activate_target(&id).await?;
                                    browser::set_bound_target_id(&cmd_ctx, &id)?;
                                    new_bound = Some(id);
                                }
                            }

                            Ok(make_success(
                                command_name,
                                json!({
                                    "action": "close",
                                    "index": index,
                                    "closedTargetId": target_id,
                                    "wasBound": was_bound,
                                    "newBoundTargetId": new_bound
                                }),
                            ))
                        }
                    }
                }
            })
            .await;
        }

        Commands::State { command } => {
            let command_name = state_command_label(&command);
            let mut runtime = ensure_runtime_or_exit(&store, session_id_flag.clone(), command_name);
            let ctx = runtime.ctx.clone();
            let cmd_ctx = ctx.clone();

            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let cmd_ctx = cmd_ctx.clone();
                    async move {
                        match command {
                            StateCommands::Save { name } => {
                                let data = session::save_session(&cmd_ctx, &name, &page).await?;
                                Ok(make_success(
                                    command_name,
                                    json!({
                                        "action": "save",
                                        "name": data.name,
                                        "savedAt": data.saved_at,
                                        "url": data.url,
                                        "cookieCount": data.cookies.len(),
                                        "localStorageOrigins": data.local_storage.len(),
                                        "sessionStorageOrigins": data.session_storage.len()
                                    }),
                                ))
                            }
                            StateCommands::Load { name } => {
                                let data = session::load_session(&cmd_ctx, &name, &page).await?;
                                Ok(make_success(
                                    command_name,
                                    json!({
                                        "action": "load",
                                        "name": data.name,
                                        "url": data.url,
                                        "cookieCount": data.cookies.len(),
                                        "localStorageOrigins": data.local_storage.len(),
                                        "sessionStorageOrigins": data.session_storage.len()
                                    }),
                                ))
                            }
                            StateCommands::List => {
                                let list = session::list_sessions(&cmd_ctx).await?;
                                Ok(make_success(
                                    command_name,
                                    json!({ "action": "list", "sessions": list }),
                                ))
                            }
                            StateCommands::Delete { name } => {
                                let ok = session::delete_session(&cmd_ctx, &name).await?;
                                Ok(make_success(
                                    command_name,
                                    json!({ "action": "delete", "name": name, "deleted": ok }),
                                ))
                            }
                            StateCommands::Show { name } => {
                                let metadata = session::show_session(&cmd_ctx, &name).await?;
                                Ok(make_success(
                                    command_name,
                                    json!({
                                        "action": "show",
                                        "session": metadata
                                    }),
                                ))
                            }
                            StateCommands::Rename { from, to } => {
                                let renamed = session::rename_session(&cmd_ctx, &from, &to).await?;
                                Ok(make_success(
                                    command_name,
                                    json!({
                                        "action": "rename",
                                        "from": from,
                                        "to": to,
                                        "renamed": renamed
                                    }),
                                ))
                            }
                            StateCommands::Clear => {
                                let removed = session::clear_sessions(&cmd_ctx).await?;
                                Ok(make_success(
                                    command_name,
                                    json!({
                                        "action": "clear",
                                        "removed": removed
                                    }),
                                ))
                            }
                            StateCommands::Clean => {
                                let removed = session::clean_sessions(&cmd_ctx).await?;
                                Ok(make_success(
                                    command_name,
                                    json!({
                                        "action": "clean",
                                        "removed": removed
                                    }),
                                ))
                            }
                        }
                    }
                },
            )
            .await;
        }

        Commands::Ref { command } => {
            let command_name = ref_command_label(&command);
            let mut runtime = ensure_runtime_or_exit(&store, session_id_flag.clone(), command_name);
            let ctx = runtime.ctx.clone();

            match command {
                RefCommands::List => {
                    let started_at = Instant::now();
                    if let Err(e) = begin_runtime_command(&mut runtime, command_name) {
                        exit_with_error(command_name, e);
                    }
                    let state = match browser::load_ref_state(&ctx).await {
                        Ok(Some(state)) => state,
                        Ok(None) => {
                            let err = missing_ref_state_error();
                            finish_runtime_command(
                                &runtime,
                                command_name,
                                false,
                                json!({ "message": err.message }),
                            );
                            exit_with_error(command_name, err);
                        }
                        Err(e) => {
                            finish_runtime_command(
                                &runtime,
                                command_name,
                                false,
                                json!({ "message": e.message }),
                            );
                            exit_with_error(command_name, e);
                        }
                    };

                    let mut refs: Vec<_> = state.refs.into_iter().collect();
                    refs.sort_by(|a, b| a.0.cmp(&b.0));
                    let refs: Vec<_> = refs
                        .into_iter()
                        .map(|(id, value)| {
                            json!({
                                "id": id,
                                "role": value.role,
                                "name": value.name,
                                "locator": value.locator
                            })
                        })
                        .collect();
                    let ref_count = refs.len();
                    let meta = build_response_meta(Some(&runtime), started_at);

                    print_result(&make_success_with_meta(
                        command_name,
                        json!({
                            "snapshotId": state.snapshot_id,
                            "url": state.url,
                            "refCount": ref_count,
                            "refs": refs
                        }),
                        meta,
                    ));
                    finish_runtime_command(
                        &runtime,
                        command_name,
                        true,
                        json!({ "refCount": ref_count }),
                    );
                }
                RefCommands::Show { reference } => {
                    let started_at = Instant::now();
                    if let Err(e) = begin_runtime_command(&mut runtime, command_name) {
                        exit_with_error(command_name, e);
                    }
                    let reference = match normalize_ref_key(&reference) {
                        Ok(reference) => reference,
                        Err(e) => {
                            finish_runtime_command(
                                &runtime,
                                command_name,
                                false,
                                json!({ "message": e.message }),
                            );
                            exit_with_error(command_name, e);
                        }
                    };

                    let state = match browser::load_ref_state(&ctx).await {
                        Ok(Some(state)) => state,
                        Ok(None) => {
                            let err = missing_ref_state_error();
                            finish_runtime_command(
                                &runtime,
                                command_name,
                                false,
                                json!({ "message": err.message }),
                            );
                            exit_with_error(command_name, err);
                        }
                        Err(e) => {
                            finish_runtime_command(
                                &runtime,
                                command_name,
                                false,
                                json!({ "message": e.message }),
                            );
                            exit_with_error(command_name, e);
                        }
                    };

                    let entry = match state.refs.get(reference.as_str()) {
                        Some(entry) => entry,
                        None => {
                            let err = CliError::bad_input(
                                format!("Ref @{} not found", reference),
                                "Run 'sauron ref list' to inspect available refs",
                            );
                            finish_runtime_command(
                                &runtime,
                                command_name,
                                false,
                                json!({ "message": err.message }),
                            );
                            exit_with_error(command_name, err);
                        }
                    };

                    let meta = build_response_meta(Some(&runtime), started_at);
                    print_result(&make_success_with_meta(
                        command_name,
                        json!({
                            "snapshotId": state.snapshot_id,
                            "url": state.url,
                            "ref": {
                                "id": reference,
                                "role": entry.role,
                                "name": entry.name,
                                "locator": entry.locator
                            }
                        }),
                        meta,
                    ));
                    finish_runtime_command(&runtime, command_name, true, json!({}));
                }
                RefCommands::Validate { reference } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        viewport_override,
                        command_name,
                        move |page| {
                            let cmd_ctx = cmd_ctx.clone();
                            async move {
                                let state = browser::load_ref_state(&cmd_ctx)
                                    .await?
                                    .ok_or_else(missing_ref_state_error)?;

                                let refs_to_check: Vec<String> = if let Some(reference) = reference
                                {
                                    vec![normalize_ref_key(&reference)?]
                                } else {
                                    let mut ids: Vec<String> = state.refs.keys().cloned().collect();
                                    ids.sort();
                                    ids
                                };

                                if refs_to_check.is_empty() {
                                    return Err(missing_ref_state_error());
                                }

                                let mut valid: Vec<String> = Vec::new();
                                let mut invalid: Vec<serde_json::Value> = Vec::new();
                                for ref_id in refs_to_check {
                                    if !state.refs.contains_key(ref_id.as_str()) {
                                        invalid.push(json!({
                                            "id": ref_id,
                                            "code": types::ErrorCode::RefNotFound,
                                            "message": "Ref not found in current snapshot state"
                                        }));
                                        continue;
                                    }
                                    let target = format!("@{}", ref_id);
                                    match page
                                        .resolve_target_backend_node_id(&cmd_ctx, &target, None)
                                        .await
                                    {
                                        Ok(_) => valid.push(ref_id),
                                        Err(e) => invalid.push(json!({
                                            "id": target,
                                            "code": e.code,
                                            "message": e.message,
                                            "hint": e.hint,
                                            "recoverable": e.recoverable
                                        })),
                                    }
                                }

                                Ok(make_success(
                                    command_name,
                                    json!({
                                        "snapshotId": state.snapshot_id,
                                        "checked": valid.len() + invalid.len(),
                                        "validCount": valid.len(),
                                        "valid": valid,
                                        "invalidCount": invalid.len(),
                                        "invalid": invalid
                                    }),
                                ))
                            }
                        },
                    )
                    .await;
                }
            }
        }

        Commands::Logs { command } => match command {
            LogCommands::List => {
                let started_at = Instant::now();
                let command_name = "logs.list";
                let base_dir = match context::resolve_base_dir() {
                    Ok(base_dir) => base_dir,
                    Err(e) => exit_with_error(command_name, e),
                };
                let logs_dir = base_dir.join("runtime").join("logs");
                let mut logs: Vec<serde_json::Value> = Vec::new();

                match std::fs::read_dir(&logs_dir) {
                    Ok(entries) => {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.extension().and_then(|ext| ext.to_str()) != Some("ndjson") {
                                continue;
                            }
                            let Ok(metadata) = entry.metadata() else {
                                continue;
                            };
                            let session_id = path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or_default()
                                .to_string();
                            let modified = metadata
                                .modified()
                                .ok()
                                .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
                            logs.push(json!({
                                "sessionId": session_id,
                                "path": path.to_string_lossy(),
                                "sizeBytes": metadata.len(),
                                "modifiedAt": modified
                            }));
                        }
                    }
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            exit_with_error(
                                command_name,
                                CliError::unknown(
                                    format!(
                                        "Failed to read logs directory {}: {}",
                                        logs_dir.display(),
                                        e
                                    ),
                                    "Check filesystem permissions",
                                ),
                            );
                        }
                    }
                }

                logs.sort_by(|a, b| {
                    a.get("sessionId")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .cmp(
                            b.get("sessionId")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default(),
                        )
                });

                let meta = build_response_meta(None, started_at);
                print_result(&make_success_with_meta(
                    command_name,
                    json!({ "logs": logs }),
                    meta,
                ));
            }
            LogCommands::Tail { lines } => {
                let started_at = Instant::now();
                let command_name = "logs.tail";
                let mut runtime =
                    ensure_runtime_or_exit(&store, session_id_flag.clone(), command_name);

                if let Err(e) = begin_runtime_command(&mut runtime, command_name) {
                    exit_with_error(command_name, e);
                }

                let log_path = runtime
                    .ctx
                    .base_dir
                    .join("runtime")
                    .join("logs")
                    .join(format!("{}.ndjson", runtime.session.session_id));
                let text = match std::fs::read_to_string(&log_path) {
                    Ok(text) => text,
                    Err(e) => {
                        let err = if e.kind() == std::io::ErrorKind::NotFound {
                            CliError::bad_input(
                                format!(
                                    "No log file found for session {}",
                                    runtime.session.session_id
                                ),
                                "Run commands for this session first, then retry logs tail",
                            )
                        } else {
                            CliError::unknown(
                                format!("Failed to read log file {}: {}", log_path.display(), e),
                                "Check filesystem permissions",
                            )
                        };
                        finish_runtime_command(
                            &runtime,
                            command_name,
                            false,
                            json!({ "message": err.message }),
                        );
                        exit_with_error(command_name, err);
                    }
                };

                let limit = lines.max(1);
                let all_lines: Vec<&str> = text.lines().collect();
                let start = all_lines.len().saturating_sub(limit);
                let items: Vec<_> = all_lines[start..]
                    .iter()
                    .map(|line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .unwrap_or_else(|_| json!({ "raw": line }))
                    })
                    .collect();

                let meta = build_response_meta(Some(&runtime), started_at);
                print_result(&make_success_with_meta(
                    command_name,
                    json!({
                        "sessionId": runtime.session.session_id,
                        "path": log_path.to_string_lossy(),
                        "count": items.len(),
                        "items": items
                    }),
                    meta,
                ));
                finish_runtime_command(
                    &runtime,
                    command_name,
                    true,
                    json!({ "count": items.len() }),
                );
            }
        },

        Commands::Console {
            command: ConsoleCommands::Capture { duration_ms, level },
        } => {
            let command_name = "console.capture";
            let mut runtime = ensure_runtime_or_exit(&store, session_id_flag.clone(), command_name);
            let level_filter: Option<Vec<String>> = level.map(|raw| {
                raw.split(',')
                    .map(|entry| entry.trim().to_ascii_lowercase())
                    .filter(|entry| !entry.is_empty())
                    .collect()
            });
            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let level_filter = level_filter.clone();
                    async move {
                        let mut entries = page
                            .capture_console_for(Duration::from_millis(duration_ms))
                            .await?;

                        if let Some(levels) = level_filter {
                            entries.retain(|entry| {
                                levels
                                    .iter()
                                    .any(|level| level == &entry.level.to_ascii_lowercase())
                            });
                        }

                        let mut by_level: std::collections::HashMap<String, usize> =
                            std::collections::HashMap::new();
                        for entry in &entries {
                            *by_level.entry(entry.level.clone()).or_insert(0) += 1;
                        }

                        Ok(make_success(
                            command_name,
                            json!({
                                "durationMs": duration_ms,
                                "count": entries.len(),
                                "byLevel": by_level,
                                "entries": entries
                            }),
                        ))
                    }
                },
            )
            .await;
        }

        Commands::Network {
            command:
                NetworkCommands::Capture {
                    duration_ms,
                    url_glob,
                },
        } => {
            let command_name = "network.capture";
            let mut runtime = ensure_runtime_or_exit(&store, session_id_flag.clone(), command_name);
            let matcher = match url_glob {
                Some(pattern) => match wildcard_to_regex(&pattern) {
                    Ok(regex) => Some(regex),
                    Err(e) => exit_with_error(command_name, e),
                },
                None => None,
            };

            with_browser_command(
                &mut runtime,
                port_flag,
                viewport_override,
                command_name,
                move |page| {
                    let matcher = matcher.clone();
                    async move {
                        let mut entries = page
                            .capture_network_for(Duration::from_millis(duration_ms))
                            .await?;
                        if let Some(regex) = matcher {
                            entries.retain(|entry| regex.is_match(&entry.url));
                        }

                        let mut by_kind: std::collections::HashMap<String, usize> =
                            std::collections::HashMap::new();
                        for entry in &entries {
                            *by_kind.entry(entry.kind.clone()).or_insert(0) += 1;
                        }

                        Ok(make_success(
                            command_name,
                            json!({
                                "durationMs": duration_ms,
                                "count": entries.len(),
                                "byKind": by_kind,
                                "entries": entries
                            }),
                        ))
                    }
                },
            )
            .await;
        }

        Commands::Run {
            file,
            stop_on_error,
        } => {
            let started_at = Instant::now();
            let command_name = "run";
            let content = match std::fs::read_to_string(&file) {
                Ok(content) => content,
                Err(e) => {
                    exit_with_error(
                        command_name,
                        CliError::unknown(
                            format!("Failed to read workflow file {}: {}", file.display(), e),
                            "Check file path and permissions",
                        ),
                    );
                }
            };
            let workflow: WorkflowFile = match serde_json::from_str(&content) {
                Ok(workflow) => workflow,
                Err(e) => {
                    exit_with_error(
                        command_name,
                        CliError::bad_input(
                            format!("Workflow file is not valid JSON: {}", e),
                            "Expected {\"steps\":[{\"command\":[\"runtime\",\"status\"]}]}",
                        ),
                    );
                }
            };
            if workflow.steps.is_empty() {
                exit_with_error(
                    command_name,
                    CliError::bad_input(
                        "Workflow has no steps",
                        "Provide at least one step in steps[]",
                    ),
                );
            }

            let exe = match std::env::current_exe() {
                Ok(path) => path,
                Err(e) => {
                    exit_with_error(
                        command_name,
                        CliError::unknown(
                            format!("Failed to resolve current executable: {}", e),
                            "Run workflow steps manually",
                        ),
                    );
                }
            };

            let mut steps_out: Vec<serde_json::Value> = Vec::new();
            let mut failed_step: Option<usize> = None;
            for (index, step) in workflow.steps.iter().enumerate() {
                if step.command.is_empty() {
                    steps_out.push(json!({
                        "index": index,
                        "name": step.name,
                        "ok": false,
                        "error": "step command is empty"
                    }));
                    failed_step = Some(index);
                    if stop_on_error {
                        break;
                    }
                    continue;
                }

                let output = match ProcessCommand::new(&exe).args(&step.command).output() {
                    Ok(output) => output,
                    Err(e) => {
                        steps_out.push(json!({
                            "index": index,
                            "name": step.name,
                            "ok": false,
                            "argv": step.command,
                            "error": format!("failed to execute step: {}", e)
                        }));
                        failed_step = Some(index);
                        if stop_on_error {
                            break;
                        }
                        continue;
                    }
                };

                let stdout_text = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr_text = String::from_utf8_lossy(&output.stderr).to_string();
                let parsed = stdout_text
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .and_then(|line| serde_json::from_str::<serde_json::Value>(line).ok());
                let ok = parsed
                    .as_ref()
                    .and_then(|value| value.get("result"))
                    .and_then(|value| value.get("ok"))
                    .and_then(|value| value.as_bool())
                    .unwrap_or_else(|| output.status.success());

                if !ok && failed_step.is_none() {
                    failed_step = Some(index);
                }

                steps_out.push(json!({
                    "index": index,
                    "name": step.name,
                    "argv": step.command,
                    "ok": ok,
                    "exitCode": output.status.code(),
                    "result": parsed,
                    "stderr": stderr_text
                }));

                if !ok && stop_on_error {
                    break;
                }
            }

            let overall_ok = failed_step.is_none();
            let meta = build_response_meta(None, started_at);
            if overall_ok {
                print_result(&make_success_with_meta(
                    command_name,
                    json!({
                        "file": file.to_string_lossy(),
                        "stepsRun": steps_out.len(),
                        "overallOk": overall_ok,
                        "stopOnError": stop_on_error,
                        "failedStep": failed_step,
                        "steps": steps_out
                    }),
                    meta,
                ));
            } else {
                let failed_index = failed_step.unwrap_or(0);
                let failed_name = workflow
                    .steps
                    .get(failed_index)
                    .and_then(|step| step.name.as_deref())
                    .unwrap_or("unnamed");
                let err = CliError::unknown(
                    format!("Workflow failed at step {} ({})", failed_index, failed_name),
                    "Inspect workflow step stderr output and fix the failing command",
                );
                print_result(&make_error_with_meta(command_name, &err, meta));
                std::process::exit(err.exit_code);
            }
        }

        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let bin_name = cmd.get_name().to_string();
            let target_shell = match shell {
                CompletionShell::Bash => Shell::Bash,
                CompletionShell::Zsh => Shell::Zsh,
            };
            generate(target_shell, &mut cmd, bin_name, &mut std::io::stdout());
        }
    }
}

async fn with_browser_command<F, Fut, T>(
    runtime: &mut ActiveRuntime,
    port_flag: Option<u16>,
    viewport_override: Option<types::Viewport>,
    command_name: &'static str,
    f: F,
) where
    F: FnOnce(browser::PageClient) -> Fut,
    Fut: std::future::Future<Output = Result<types::ResultEnvelope<T>, CliError>>,
    T: Serialize,
{
    let started_at = Instant::now();
    if let Err(e) = begin_runtime_command(runtime, command_name) {
        let meta = build_response_meta(Some(runtime), started_at);
        let res = make_error_with_meta(command_name, &e, meta);
        print_result(&res);
        std::process::exit(e.exit_code);
    }

    let port = runtime.ctx.resolve_port(port_flag);

    let browser = match BrowserClient::connect(port).await {
        Ok(b) => b,
        Err(e) => {
            let code = serde_json::to_value(e.code).unwrap_or(json!("UNKNOWN"));
            finish_runtime_command(
                runtime,
                command_name,
                false,
                json!({ "errorCode": code, "message": e.message }),
            );
            let meta = build_response_meta(Some(runtime), started_at);
            let res = make_error_with_meta(command_name, &e, meta);
            print_result(&res);
            std::process::exit(e.exit_code);
        }
    };

    let page = match browser.get_page_for_context(&runtime.ctx).await {
        Ok(p) => p,
        Err(e) => {
            let code = serde_json::to_value(e.code).unwrap_or(json!("UNKNOWN"));
            finish_runtime_command(
                runtime,
                command_name,
                false,
                json!({ "errorCode": code, "message": e.message }),
            );
            let meta = build_response_meta(Some(runtime), started_at);
            let res = make_error_with_meta(command_name, &e, meta);
            print_result(&res);
            std::process::exit(e.exit_code);
        }
    };

    let viewport = viewport_override.unwrap_or(runtime.session.viewport());
    if let Err(e) = page
        .set_viewport(viewport.width, viewport.height, false)
        .await
    {
        let code = serde_json::to_value(e.code).unwrap_or(json!("UNKNOWN"));
        finish_runtime_command(
            runtime,
            command_name,
            false,
            json!({ "errorCode": code, "message": e.message }),
        );
        let meta = build_response_meta(Some(runtime), started_at);
        let res = make_error_with_meta(command_name, &e, meta);
        print_result(&res);
        std::process::exit(e.exit_code);
    }

    match f(page).await {
        Ok(mut res) => {
            res.meta = build_response_meta(Some(runtime), started_at);
            print_result(&res);
            finish_runtime_command(runtime, command_name, true, json!({}));
        }
        Err(e) => {
            let code = serde_json::to_value(e.code).unwrap_or(json!("UNKNOWN"));
            finish_runtime_command(
                runtime,
                command_name,
                false,
                json!({ "errorCode": code, "message": e.message }),
            );
            let meta = build_response_meta(Some(runtime), started_at);
            let res = make_error_with_meta(command_name, &e, meta);
            print_result(&res);
            std::process::exit(e.exit_code);
        }
    }
}

async fn with_browser_only_command<F, Fut, T>(
    runtime: &mut ActiveRuntime,
    port_flag: Option<u16>,
    command_name: &'static str,
    f: F,
) where
    F: FnOnce(BrowserClient) -> Fut,
    Fut: std::future::Future<Output = Result<types::ResultEnvelope<T>, CliError>>,
    T: Serialize,
{
    let started_at = Instant::now();
    if let Err(e) = begin_runtime_command(runtime, command_name) {
        let meta = build_response_meta(Some(runtime), started_at);
        let res = make_error_with_meta(command_name, &e, meta);
        print_result(&res);
        std::process::exit(e.exit_code);
    }

    let port = runtime.ctx.resolve_port(port_flag);

    let browser = match BrowserClient::connect(port).await {
        Ok(b) => b,
        Err(e) => {
            let code = serde_json::to_value(e.code).unwrap_or(json!("UNKNOWN"));
            finish_runtime_command(
                runtime,
                command_name,
                false,
                json!({ "errorCode": code, "message": e.message }),
            );
            let meta = build_response_meta(Some(runtime), started_at);
            let res = make_error_with_meta(command_name, &e, meta);
            print_result(&res);
            std::process::exit(e.exit_code);
        }
    };

    match f(browser).await {
        Ok(mut res) => {
            res.meta = build_response_meta(Some(runtime), started_at);
            print_result(&res);
            finish_runtime_command(runtime, command_name, true, json!({}));
        }
        Err(e) => {
            let code = serde_json::to_value(e.code).unwrap_or(json!("UNKNOWN"));
            finish_runtime_command(
                runtime,
                command_name,
                false,
                json!({ "errorCode": code, "message": e.message }),
            );
            let meta = build_response_meta(Some(runtime), started_at);
            let res = make_error_with_meta(command_name, &e, meta);
            print_result(&res);
            std::process::exit(e.exit_code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_test_home() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("sauron-main-test-{}", nanos))
    }

    #[test]
    fn command_labels_are_namespaced() {
        assert_eq!(
            command_label(&Commands::Runtime {
                command: RuntimeCommands::Status
            }),
            "runtime.status"
        );
        assert_eq!(
            command_label(&Commands::Page {
                command: PageCommands::Goto {
                    url: "https://example.com".to_string(),
                    until: "load".to_string(),
                }
            }),
            "page.goto"
        );
        assert_eq!(
            command_label(&Commands::Input {
                command: InputCommands::Press {
                    combo: "Enter".to_string(),
                }
            }),
            "input.press"
        );
        assert_eq!(
            command_label(&Commands::State {
                command: StateCommands::List
            }),
            "state.list"
        );
        assert_eq!(
            command_label(&Commands::Ref {
                command: RefCommands::Validate { reference: None }
            }),
            "ref.validate"
        );
        assert_eq!(
            command_label(&Commands::Logs {
                command: LogCommands::Tail { lines: 5 }
            }),
            "logs.tail"
        );
    }

    #[test]
    fn parse_viewport_accepts_valid_dimensions() {
        let viewport = parse_viewport("1440x900").expect("viewport should parse");
        assert_eq!(viewport.width, 1440);
        assert_eq!(viewport.height, 900);
    }

    #[test]
    fn parse_viewport_rejects_bad_format() {
        let err = parse_viewport("1440-900").expect_err("viewport should fail");
        assert!(matches!(err.code, types::ErrorCode::BadInput));
    }

    #[test]
    fn parse_viewport_rejects_zero_dimension() {
        let err = parse_viewport("0x900").expect_err("viewport should fail");
        assert!(matches!(err.code, types::ErrorCode::BadInput));
    }

    #[test]
    fn resolve_runtime_status_target_is_missing_when_no_session_or_probe_exists() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);

        let store = build_runtime_store().expect("store should initialize");
        let target = resolve_runtime_status_target(&store, None, None, None, None, None, None)
            .expect("status target should resolve");

        assert!(matches!(target, RuntimeStatusTarget::Missing));

        std::env::remove_var("SAURON_HOME");
    }

    #[test]
    fn cleanup_runtime_context_removes_default_runtime_artifacts() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);

        let ctx = AppContext::new("inst-clean", "client-clean", None, None)
            .expect("context should build");
        std::fs::create_dir_all(&ctx.user_data_dir).expect("user data dir should exist");
        std::fs::write(&ctx.instance_lock_path, b"").expect("instance lock should exist");
        context::write_pid_file(
            &ctx.pid_path,
            &types::PidFileData {
                pid: 12345,
                port: 9222,
                xvfb_pid: None,
                display: None,
            },
        )
        .expect("pidfile should exist");

        cleanup_runtime_context(&ctx).expect("cleanup should succeed");

        assert!(!ctx.pid_path.exists());
        assert!(!ctx.user_data_dir.exists());
        assert!(!ctx.instance_lock_path.exists());
        assert!(!ctx.instance_dir.exists());

        std::env::remove_var("SAURON_HOME");
    }
}
