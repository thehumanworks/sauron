mod browser;
mod cdp;
mod context;
mod daemon;
mod diff;
mod errors;
mod runtime;
mod session;
mod snapshot;
#[cfg(test)]
mod test_support;
mod types;

use base64::Engine as _;
use browser::BrowserClient;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use context::AppContext;
use errors::{make_error, make_success, print_result, CliError};
use runtime::{
    activate_session, cleanup_session_state, cleanup_stale_state, cleanup_stale_state_for_store,
    create_session_record, resolve_active_session, resolve_project_root_path,
    session_required_error, terminate_session, CleanupStats, RuntimeSessionRecord, RuntimeStore,
    SessionStoreKind,
};
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "sauron",
    version,
    about = "Rust-native CLI for controlling Chrome via CDP"
)]
struct Cli {
    /// Runtime session id override (otherwise resolved by process binding, project context, then SAURON_SESSION_ID)
    #[arg(long)]
    session_id: Option<String>,

    /// Optional instance id override for `start` (auto-generated when omitted)
    #[arg(long)]
    instance: Option<String>,

    /// Optional client id override for `start` (auto-generated when omitted)
    #[arg(long)]
    client: Option<String>,

    /// Runtime session storage backend
    #[arg(long, default_value = "filesystem", value_enum)]
    session_store: SessionStoreKind,

    /// Valkey URL when --session-store=valkey (or set SAURON_VALKEY_URL)
    #[arg(long)]
    valkey_url: Option<String>,

    /// Session TTL in seconds (used by Valkey backend)
    #[arg(long)]
    session_ttl_seconds: Option<u64>,

    /// Chrome DevTools debugging port (overrides pidfile)
    #[arg(long)]
    port: Option<u16>,

    /// Optional override for pidfile location
    #[arg(long)]
    pid_path: Option<PathBuf>,

    /// Optional override for Chrome user data dir
    #[arg(long)]
    user_data_dir: Option<PathBuf>,

    /// Optional timeout in milliseconds (command-specific defaults apply when omitted)
    #[arg(long)]
    timeout: Option<u64>,

    /// Sleep for N milliseconds before executing the subcommand.
    ///
    /// This is useful for agent loops that need deterministic pacing.
    #[arg(long, global = true)]
    wait: Option<u64>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start a new runtime session and Chrome daemon
    Start {
        /// Run Chrome headed (macOS uses windowless startup).
        ///
        /// Sauron will try to keep the window minimized/off-screen so it does not interrupt user flow.
        #[arg(long)]
        headed: bool,

        /// Enable WebGL-friendly rendering flags (software rendering via SwiftShader).
        ///
        /// Useful when you need WebGL/canvas-heavy pages to work in headless environments.
        /// Note: SwiftShader is less secure; use only with trusted content.
        #[arg(long)]
        webgl: bool,

        /// (Linux) Run headed Chrome inside Xvfb (virtual display) so no GUI window appears.
        ///
        /// Requires the 'Xvfb' binary to be installed and in PATH.
        #[arg(long)]
        xvfb: bool,

        /// Enable GPU acceleration (overrides the default `--disable-gpu` behaviour).
        ///
        /// Note: WebGL mode (`--webgl`) automatically enables the GPU process.
        #[arg(long)]
        enable_gpu: bool,
    },

    /// Terminate the active runtime session and clean up state
    #[command(alias = "stop")]
    Terminate {
        /// After stopping, remove all stale instances, dead sessions, and orphaned state.
        #[arg(long)]
        cleanup: bool,
    },

    /// Show Chrome daemon status
    Status,

    /// Navigate to a URL
    Navigate {
        url: String,
        #[arg(long, default_value = "load")]
        wait_until: String,
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
        iframes: bool,
    },

    /// Take a screenshot
    Screenshot {
        #[arg(long)]
        full_page: bool,
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Extract the full text content of the current page as Markdown
    #[command(alias = "markdown")]
    Content,

    /// Collect multiple artifacts in one command (runs actions in parallel)
    ///
    /// Example:
    ///   sauron collect --snapshot --screenshot --content
    Collect {
        /// Include an accessibility snapshot (same as `snapshot`)
        #[arg(long)]
        snapshot: bool,

        /// Include a screenshot (same as `screenshot`)
        #[arg(long)]
        screenshot: bool,

        /// Include Markdown content (same as `content`)
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
        iframes: bool,

        // --- Screenshot options (only used if --screenshot is set) ---
        #[arg(long)]
        full_page: bool,
        /// Optional file path to write the screenshot PNG.
        /// If omitted, screenshot data is returned as base64 in JSON.
        #[arg(long)]
        screenshot_path: Option<PathBuf>,
    },

    /// Click an element by ref (@e1) or text
    Click {
        target: String,
        #[arg(long)]
        double: bool,
        /// When targeting by text, click the nth match
        #[arg(long)]
        nth: Option<u32>,
    },

    /// Fill an input/select by ref (@e1) or text
    Fill {
        target: String,
        text: String,
        /// When targeting by text, fill the nth match
        #[arg(long)]
        nth: Option<u32>,
    },

    /// Press a key or key combination (e.g. Enter, Control+A)
    Key { combo: String },

    /// Hover an element by ref (@e1) or text
    Hover {
        target: String,
        #[arg(long)]
        nth: Option<u32>,
    },

    /// Scroll
    Scroll {
        /// Direction (up/down/top/bottom) or element ref/text
        target: String,
        /// Amount in pixels for directional scroll
        #[arg(long, default_value_t = 500)]
        amount: i64,
        #[arg(long)]
        nth: Option<u32>,
    },

    /// Wait for a condition
    Wait {
        /// Milliseconds to sleep
        duration: Option<u64>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        selector: Option<String>,
        #[arg(long)]
        idle: bool,
    },

    /// Handle the next JavaScript dialog (accept/dismiss)
    Dialog {
        action: String,
        #[arg(long)]
        text: Option<String>,
    },

    /// Run JavaScript in the page context
    #[command(alias = "js")]
    Eval { expression: String },

    /// Diff the last two snapshots
    Diff {
        #[arg(long, default_value = "unified")]
        format: String,
    },

    /// Tab management
    Tab {
        #[command(subcommand)]
        command: TabCommands,
    },

    /// Save/load/list/delete browser sessions
    Session {
        #[command(subcommand)]
        command: SessionCommands,
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
enum TabCommands {
    List,
    Open { url: String },
    Switch { index: usize },
    Close { index: usize },
}

#[derive(Subcommand, Debug)]
enum SessionCommands {
    Save { name: String },
    Load { name: String },
    List,
    Delete { name: String },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CompletionShell {
    Bash,
    Zsh,
}

#[derive(Clone)]
struct ActiveRuntime {
    store: RuntimeStore,
    session: RuntimeSessionRecord,
    ctx: AppContext,
}

fn build_runtime_store(cli: &Cli) -> Result<RuntimeStore, CliError> {
    RuntimeStore::new(
        cli.session_store,
        cli.valkey_url.clone(),
        cli.session_ttl_seconds,
    )
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
    json_output: bool,
) -> ActiveRuntime {
    match require_runtime(store, session_id) {
        Ok(runtime) => runtime,
        Err(mut e) => {
            if matches!(e.code, types::ErrorCode::SessionRequired) {
                e = session_required_error(command_name);
            }
            if json_output {
                let res = make_error(command_name, &e);
                print_result(&res);
            } else {
                eprintln!("{}", e.message);
            }
            std::process::exit(e.exit_code);
        }
    }
}

fn command_label(command: &Commands) -> &'static str {
    match command {
        Commands::Start { .. } => "start",
        Commands::Terminate { .. } => "terminate",
        Commands::Status => "status",
        Commands::Navigate { .. } => "navigate",
        Commands::Snapshot { .. } => "snapshot",
        Commands::Screenshot { .. } => "screenshot",
        Commands::Content => "content",
        Commands::Collect { .. } => "collect",
        Commands::Click { .. } => "click",
        Commands::Fill { .. } => "fill",
        Commands::Key { .. } => "key",
        Commands::Hover { .. } => "hover",
        Commands::Scroll { .. } => "scroll",
        Commands::Wait { .. } => "wait",
        Commands::Dialog { .. } => "dialog",
        Commands::Eval { .. } => "eval",
        Commands::Diff { .. } => "diff",
        Commands::Tab { .. } => "tab",
        Commands::Session { .. } => "session",
        Commands::Completions { .. } => "completions",
    }
}

fn should_fallback_to_cleanup_without_runtime(
    error_code: types::ErrorCode,
    explicit_session_id: bool,
) -> bool {
    matches!(error_code, types::ErrorCode::SessionRequired)
        || (!explicit_session_id
            && matches!(
                error_code,
                types::ErrorCode::SessionInvalid
                    | types::ErrorCode::SessionTerminated
                    | types::ErrorCode::BadInput
            ))
}

fn cleanup_stats_json(stats: CleanupStats) -> serde_json::Value {
    json!({
        "instances": stats.instances,
        "sessions": stats.sessions,
        "logs": stats.logs
    })
}

fn print_cleanup_summary(stats: CleanupStats) {
    if stats.instances == 0 && stats.sessions == 0 && stats.logs == 0 {
        eprintln!("Cleanup: nothing to clean up");
        return;
    }

    eprintln!(
        "Cleanup: removed {} stale instances, {} orphaned sessions, {} log files",
        stats.instances, stats.sessions, stats.logs
    );
}

fn run_cleanup(
    base_dir: &std::path::Path,
    store_kind: SessionStoreKind,
) -> Result<CleanupStats, CliError> {
    if store_kind == SessionStoreKind::Filesystem {
        return cleanup_stale_state(base_dir);
    }
    cleanup_stale_state_for_store(base_dir, store_kind)
}

fn cleanup_terminate_log_artifacts(runtime: &ActiveRuntime) -> usize {
    if runtime.store.kind() != SessionStoreKind::Filesystem {
        return 0;
    }

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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if let Some(ms) = cli.wait {
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

    let store = match build_runtime_store(&cli) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("{}", e.message);
            std::process::exit(e.exit_code);
        }
    };

    let command = cli.command;
    let port_flag = cli.port;
    let timeout_flag = cli.timeout;

    match command {
        Commands::Start {
            headed,
            webgl,
            xvfb,
            enable_gpu,
        } => {
            let session = match create_session_record(
                &store,
                cli.session_id.clone(),
                cli.instance.clone(),
                cli.client.clone(),
                cli.pid_path.clone(),
                cli.user_data_dir.clone(),
            ) {
                Ok(session) => session,
                Err(e) => {
                    eprintln!("{}", e.message);
                    std::process::exit(e.exit_code);
                }
            };

            let ctx = match AppContext::new(
                &session.instance,
                &session.client,
                session.pid_path.clone(),
                session.user_data_dir.clone(),
            ) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("{}", e.message);
                    std::process::exit(e.exit_code);
                }
            };

            let timeout_ms = timeout_flag.unwrap_or(10_000);
            let headless = !(headed || xvfb);
            let disable_gpu = !(enable_gpu || webgl);
            let _ = store.append_log(&session, "start", "start", None);
            match daemon::start(
                ctx.pid_path.clone(),
                ctx.user_data_dir.clone(),
                ctx.instance_lock_path.clone(),
                port_flag,
                timeout_ms,
                daemon::ChromeLaunchOptions {
                    headless,
                    disable_gpu,
                    webgl,
                    xvfb,
                },
            )
            .await
            {
                Ok(r) => {
                    if let Err(e) = activate_session(&store, &session) {
                        let rollback = daemon::stop(
                            &ctx.pid_path,
                            &ctx.instance_lock_path,
                            Some(r.port),
                            timeout_ms,
                        )
                        .await;
                        let rollback_summary = match rollback {
                            Ok(true) => "rollback_stopped".to_string(),
                            Ok(false) => "rollback_not_found".to_string(),
                            Err(err) => format!("rollback_failed: {}", err.message),
                        };
                        let _ = store.append_log(
                            &session,
                            "start",
                            "error",
                            Some(json!({
                                "message": e.message,
                                "rollback": rollback_summary
                            })),
                        );
                        eprintln!("{}", e.message);
                        eprintln!("Start rollback result: {}", rollback_summary);
                        std::process::exit(e.exit_code);
                    }
                    finish_runtime_command(
                        &ActiveRuntime {
                            store: store.clone(),
                            session: session.clone(),
                            ctx: ctx.clone(),
                        },
                        "start",
                        true,
                        json!({ "port": r.port, "pid": r.pid, "store": format!("{:?}", store.kind()) }),
                    );
                    println!("Chrome daemon started on port {} (pid: {})", r.port, r.pid);
                    println!("WebSocket: {}", r.ws_url);
                    println!("Session ID: {}", session.session_id);
                    println!("Instance ID: {}", session.instance);
                    println!("Client ID: {}", session.client);
                    if let Ok(project_root) = resolve_project_root_path() {
                        println!("Project binding: {}", project_root.display());
                    }
                    println!(
                        "Session is auto-resolved in this project (override with --session-id {}).",
                        session.session_id
                    );
                }
                Err(e) => {
                    let _ = store.append_log(
                        &session,
                        "start",
                        "error",
                        Some(json!({ "message": e.message })),
                    );
                    eprintln!("{}", e.message);
                    std::process::exit(e.exit_code);
                }
            }
        }

        Commands::Terminate { cleanup } => {
            let requested_session_id = cli.session_id.clone();
            let explicit_session_id = requested_session_id.is_some();
            let runtime = if cleanup {
                match require_runtime(&store, requested_session_id.clone()) {
                    Ok(runtime) => Some(runtime),
                    Err(e) => {
                        if should_fallback_to_cleanup_without_runtime(e.code, explicit_session_id) {
                            None
                        } else {
                            eprintln!("{}", e.message);
                            std::process::exit(e.exit_code);
                        }
                    }
                }
            } else {
                Some(ensure_runtime_or_exit(
                    &store,
                    requested_session_id,
                    "terminate",
                    false,
                ))
            };

            let Some(mut runtime) = runtime else {
                let base_dir = match context::resolve_base_dir() {
                    Ok(base_dir) => base_dir,
                    Err(e) => {
                        eprintln!("{}", e.message);
                        std::process::exit(e.exit_code);
                    }
                };
                match run_cleanup(&base_dir, store.kind()) {
                    Ok(stats) => {
                        print_cleanup_summary(stats);
                        println!("No active runtime session found. Cleanup completed.");
                    }
                    Err(e) => {
                        eprintln!("{}", e.message);
                        std::process::exit(e.exit_code);
                    }
                }
                return;
            };

            if let Err(e) = begin_runtime_command(&mut runtime, "terminate") {
                eprintln!("{}", e.message);
                std::process::exit(e.exit_code);
            }

            let timeout_ms = timeout_flag.unwrap_or(5_000);
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
                        "terminate",
                        false,
                        json!({ "message": e.message }),
                    );
                    eprintln!("{}", e.message);
                    std::process::exit(e.exit_code);
                }
            };

            let mut terminate_errors: Vec<String> = Vec::new();
            let mut terminate_exit_code = 1;

            runtime.session.mark_terminated();
            if let Err(e) = terminate_session(&runtime.store, &runtime.session) {
                terminate_errors.push(e.message);
                terminate_exit_code = e.exit_code;
            }

            if let Err(e) = cleanup_session_state(&runtime.ctx.base_dir, &runtime.session) {
                terminate_errors.push(e.message);
                terminate_exit_code = e.exit_code;
            }
            if let Some(pid_path) = &runtime.session.pid_path {
                if let Err(e) = context::remove_pid_file(pid_path) {
                    terminate_errors.push(e.message);
                    terminate_exit_code = e.exit_code;
                }
            }
            if let Some(user_data_dir) = &runtime.session.user_data_dir {
                if let Err(e) = std::fs::remove_dir_all(user_data_dir) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        let err = CliError::unknown(
                            format!(
                                "Failed to remove custom Chrome user data dir {}: {}",
                                user_data_dir.display(),
                                e
                            ),
                            "Check filesystem permissions",
                        );
                        terminate_errors.push(err.message);
                        terminate_exit_code = err.exit_code;
                    }
                }
            }

            let mut cleanup_stats = None;
            if cleanup {
                match run_cleanup(&runtime.ctx.base_dir, runtime.store.kind()) {
                    Ok(stats) => {
                        cleanup_stats = Some(stats);
                    }
                    Err(e) => {
                        terminate_errors.push(e.message);
                        terminate_exit_code = e.exit_code;
                    }
                }
            }

            if !terminate_errors.is_empty() {
                let mut details = json!({ "errors": terminate_errors.clone() });
                if let Some(stats) = cleanup_stats {
                    details["cleanup"] = cleanup_stats_json(stats);
                }
                finish_runtime_command(&runtime, "terminate", false, details);
                if let Some(mut stats) = cleanup_stats {
                    if cleanup {
                        stats.logs = stats
                            .logs
                            .saturating_add(cleanup_terminate_log_artifacts(&runtime));
                    }
                    print_cleanup_summary(stats);
                }
                eprintln!("Terminate completed with errors:");
                for message in terminate_errors {
                    eprintln!("- {}", message);
                }
                std::process::exit(terminate_exit_code);
            }

            let mut details = json!({ "daemonStopped": stopped });
            if let Some(stats) = cleanup_stats {
                details["cleanup"] = cleanup_stats_json(stats);
            }
            finish_runtime_command(&runtime, "terminate", true, details);
            if let Some(mut stats) = cleanup_stats {
                if cleanup {
                    stats.logs = stats
                        .logs
                        .saturating_add(cleanup_terminate_log_artifacts(&runtime));
                }
                print_cleanup_summary(stats);
            }
            if stopped {
                println!("Runtime session terminated and Chrome daemon stopped.");
            } else {
                println!("Runtime session terminated. No Chrome daemon was running.");
            }
        }

        Commands::Completions { shell } => {
            // Note: handled earlier before runtime store initialization, but kept here for completeness.
            let mut cmd = Cli::command();
            let bin_name = cmd.get_name().to_string();
            let target_shell = match shell {
                CompletionShell::Bash => Shell::Bash,
                CompletionShell::Zsh => Shell::Zsh,
            };
            generate(target_shell, &mut cmd, bin_name, &mut std::io::stdout());
        }

        command => {
            let requires_json_output = !matches!(command, Commands::Status);
            let mut runtime = ensure_runtime_or_exit(
                &store,
                cli.session_id.clone(),
                command_label(&command),
                requires_json_output,
            );
            let ctx = runtime.ctx.clone();

            match command {
                Commands::Start { .. } | Commands::Terminate { .. } => unreachable!(),
                Commands::Completions { .. } => unreachable!(),
                Commands::Status => {
                    if let Err(e) = begin_runtime_command(&mut runtime, "status") {
                        eprintln!("{}", e.message);
                        std::process::exit(e.exit_code);
                    }

                    let port = ctx.resolve_port(port_flag);
                    let st = daemon::get_status(&ctx.pid_path, port).await;
                    match st {
                        types::DaemonStatus::Running { pid, port, ws_url } => {
                            println!("Chrome daemon running on port {}", port);
                            if let Some(pid) = pid {
                                println!("PID: {}", pid);
                            }
                            if let Some(ws) = ws_url {
                                println!("WebSocket: {}", ws);
                            }
                        }
                        types::DaemonStatus::Stale { pid, port } => {
                            println!("Chrome daemon state is stale.");
                            if let Some(pid) = pid {
                                println!("PID: {}", pid);
                            }
                            if let Some(port) = port {
                                println!("Port: {}", port);
                            }
                            println!(
                                "Try: sauron terminate (or remove the pidfile) then sauron start"
                            );
                        }
                        types::DaemonStatus::Stopped => {
                            println!("Chrome daemon not running.");
                        }
                    }
                    finish_runtime_command(
                        &runtime,
                        "status",
                        true,
                        json!({ "status": "reported" }),
                    );
                }

                // --- Browser commands (JSON output) ---
                Commands::Navigate { url, wait_until } => {
                    with_browser_command(&mut runtime, port_flag, "navigate", |page| async move {
                        let timeout = Duration::from_millis(timeout_flag.unwrap_or(30_000));
                        let outcome = page.navigate(&url, &wait_until, timeout).await?;
                        #[derive(Serialize)]
                        #[serde(rename_all = "camelCase")]
                        struct Out {
                            url: String,
                            #[serde(skip_serializing_if = "Option::is_none")]
                            status: Option<i64>,
                        }
                        Ok(make_success(
                            "navigate",
                            Out {
                                url,
                                status: outcome.status,
                            },
                        ))
                    })
                    .await;
                }

                Commands::Snapshot {
                    interactive,
                    clickable,
                    scope,
                    iframes,
                } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(&mut runtime, port_flag, "snapshot", move |page| {
                        let cmd_ctx = cmd_ctx.clone();
                        async move {
                            let opts = types::SnapshotOptions {
                                interactive,
                                clickable,
                                scope,
                                include_iframes: iframes,
                            };
                            let snap = page.snapshot_and_persist(&cmd_ctx, opts).await?;

                            #[derive(Serialize)]
                            #[serde(rename_all = "camelCase")]
                            struct Out {
                                url: String,
                                snapshot_id: u64,
                                ref_count: usize,
                                tree: String,
                            }

                            Ok(make_success(
                                "snapshot",
                                Out {
                                    url: snap.url,
                                    snapshot_id: snap.snapshot_id,
                                    ref_count: snap.refs.len(),
                                    tree: snap.tree,
                                },
                            ))
                        }
                    })
                    .await;
                }

                Commands::Screenshot { full_page, path } => {
                    with_browser_command(
                        &mut runtime,
                        port_flag,
                        "screenshot",
                        |page| async move {
                            let data = page.capture_screenshot(full_page).await?;
                            if let Some(p) = path {
                                let bytes = base64::engine::general_purpose::STANDARD
                                    .decode(&data)
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
                                    "screenshot",
                                    json!({ "saved": true, "path": p.to_string_lossy() }),
                                ))
                            } else {
                                Ok(make_success(
                                    "screenshot",
                                    json!({ "data": data, "mime": "image/png" }),
                                ))
                            }
                        },
                    )
                    .await;
                }

                Commands::Content => {
                    with_browser_command(&mut runtime, port_flag, "content", |page| async move {
                        let url = page.url().await?;
                        let markdown = page.extract_markdown().await?;
                        Ok(make_success(
                            "content",
                            json!({ "url": url, "markdown": markdown }),
                        ))
                    })
                    .await;
                }

                Commands::Collect {
                    snapshot,
                    screenshot,
                    content,
                    interactive,
                    clickable,
                    scope,
                    iframes,
                    full_page,
                    screenshot_path,
                } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(&mut runtime, port_flag, "collect", move |page| {
                        let cmd_ctx = cmd_ctx.clone();
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
                                    include_iframes: iframes,
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
                            let screenshot_path_for_task = screenshot_path.clone();
                            let screenshot_fut = async move {
                                if !screenshot {
                                    return Ok::<Option<serde_json::Value>, CliError>(None);
                                }
                                let data =
                                    page_for_screenshot.capture_screenshot(full_page).await?;
                                if let Some(p) = screenshot_path_for_task {
                                    let bytes = base64::engine::general_purpose::STANDARD
                                        .decode(&data)
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
                                        "mime": "image/png"
                                    })))
                                } else {
                                    Ok(Some(json!({ "data": data, "mime": "image/png" })))
                                }
                            };

                            let page_for_content = page.clone();
                            let content_fut = async move {
                                if !content {
                                    return Ok::<Option<String>, CliError>(None);
                                }
                                let markdown = page_for_content.extract_markdown().await?;
                                Ok(Some(markdown))
                            };

                            let (url_opt, snap_res, shot_res, content_res) =
                                tokio::join!(url_fut, snapshot_fut, screenshot_fut, content_fut);

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
                                "collect",
                                Out {
                                    url: url_opt,
                                    snapshot: snapshot_out,
                                    screenshot: screenshot_out,
                                    content: content_out,
                                    errors,
                                },
                            ))
                        }
                    })
                    .await;
                }

                Commands::Click {
                    target,
                    double,
                    nth,
                } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(&mut runtime, port_flag, "click", move |page| {
                        let cmd_ctx = cmd_ctx.clone();
                        async move {
                            let backend = page
                                .resolve_target_backend_node_id(&cmd_ctx, &target, nth)
                                .await?;
                            page.click(backend, double).await?;
                            Ok(make_success(
                                "click",
                                json!({ "target": target, "clicked": true, "double": double }),
                            ))
                        }
                    })
                    .await;
                }

                Commands::Fill { target, text, nth } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(&mut runtime, port_flag, "fill", move |page| {
                        let cmd_ctx = cmd_ctx.clone();
                        async move {
                            let backend = page
                                .resolve_target_backend_node_id(&cmd_ctx, &target, nth)
                                .await?;
                            let typ = page.fill(backend, &text).await?;
                            Ok(make_success(
                                "fill",
                                json!({ "target": target, "filled": true, "type": typ }),
                            ))
                        }
                    })
                    .await;
                }

                Commands::Key { combo } => {
                    with_browser_command(&mut runtime, port_flag, "key", |page| async move {
                        page.press_key(&combo).await?;
                        Ok(make_success(
                            "key",
                            json!({ "key": combo, "pressed": true }),
                        ))
                    })
                    .await;
                }

                Commands::Hover { target, nth } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(&mut runtime, port_flag, "hover", move |page| {
                        let cmd_ctx = cmd_ctx.clone();
                        async move {
                            let backend = page
                                .resolve_target_backend_node_id(&cmd_ctx, &target, nth)
                                .await?;
                            page.hover(backend).await?;
                            Ok(make_success(
                                "hover",
                                json!({ "target": target, "hovered": true }),
                            ))
                        }
                    })
                    .await;
                }

                Commands::Scroll {
                    target,
                    amount,
                    nth,
                } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(&mut runtime, port_flag, "scroll", move |page| {
                        let cmd_ctx = cmd_ctx.clone();
                        async move {
                            let dir = target.to_lowercase();
                            if matches!(dir.as_str(), "up" | "down" | "top" | "bottom") {
                                let expr = match dir.as_str() {
                                    "top" => "window.scrollTo(0, 0)".to_string(),
                                    "bottom" => {
                                        "window.scrollTo(0, document.body.scrollHeight)".to_string()
                                    }
                                    "up" => format!("window.scrollBy(0, -{})", amount),
                                    "down" => format!("window.scrollBy(0, {})", amount),
                                    _ => format!("window.scrollBy(0, {})", amount),
                                };
                                let _ = page.eval(&expr).await?;
                                Ok(make_success(
                                    "scroll",
                                    json!({ "direction": dir, "amount": amount }),
                                ))
                            } else {
                                let backend = page
                                    .resolve_target_backend_node_id(&cmd_ctx, &target, nth)
                                    .await?;
                                page.scroll_into_view(backend).await?;
                                Ok(make_success(
                                    "scroll",
                                    json!({ "target": target, "scrolledIntoView": true }),
                                ))
                            }
                        }
                    })
                    .await;
                }

                Commands::Wait {
                    duration,
                    text,
                    url,
                    selector,
                    idle,
                } => {
                    with_browser_command(&mut runtime, port_flag, "wait", |page| async move {
                        let timeout = Duration::from_millis(timeout_flag.unwrap_or(30_000));

                        if let Some(ms) = duration {
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                            return Ok(make_success(
                                "wait",
                                json!({ "waited": true, "duration": ms }),
                            ));
                        }
                        if let Some(t) = text {
                            page.wait_for_text(&t, timeout).await?;
                            return Ok(make_success("wait", json!({ "waited": true, "text": t })));
                        }
                        if let Some(u) = url {
                            page.wait_for_url(&u, timeout).await?;
                            return Ok(make_success("wait", json!({ "waited": true, "url": u })));
                        }
                        if let Some(sel) = selector {
                            page.wait_for_selector(&sel, timeout).await?;
                            return Ok(make_success(
                                "wait",
                                json!({ "waited": true, "selector": sel }),
                            ));
                        }
                        if idle {
                            page.wait_for_idle(timeout).await?;
                            return Ok(make_success(
                                "wait",
                                json!({ "waited": true, "idle": true }),
                            ));
                        }

                        Err(CliError::bad_input(
                            "No wait condition provided",
                            "Provide a duration (ms) or one of --text/--url/--selector/--idle",
                        ))
                    })
                    .await;
                }

                Commands::Dialog { action, text } => {
                    with_browser_command(&mut runtime, port_flag, "dialog", |page| async move {
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
                            "dialog",
                            json!({
                                "action": action,
                                "type": d.dialog_type,
                                "message": d.message,
                                "text": text
                            }),
                        ))
                    })
                    .await;
                }

                Commands::Eval { expression } => {
                    with_browser_command(&mut runtime, port_flag, "eval", |page| async move {
                        let value = page.eval(&expression).await?;
                        Ok(make_success("eval", json!({ "result": value })))
                    })
                    .await;
                }

                Commands::Diff { format } => {
                    if let Err(e) = begin_runtime_command(&mut runtime, "diff") {
                        let res = make_error("diff", &e);
                        print_result(&res);
                        std::process::exit(e.exit_code);
                    }

                    let state = browser::load_ref_state(&ctx).await.unwrap_or(None);
                    let Some(state) = state else {
                        let err = CliError::bad_input(
                            "No snapshots available",
                            "Run 'sauron snapshot' first",
                        );
                        print_result(&make_error("diff", &err));
                        finish_runtime_command(
                            &runtime,
                            "diff",
                            false,
                            json!({ "message": err.message }),
                        );
                        std::process::exit(err.exit_code);
                    };

                    let snapshot_id = state.snapshot_id;
                    if snapshot_id < 2 {
                        let err = CliError::bad_input(
                            "Need at least 2 snapshots to diff",
                            "Run 'sauron snapshot' twice",
                        );
                        print_result(&make_error("diff", &err));
                        finish_runtime_command(
                            &runtime,
                            "diff",
                            false,
                            json!({ "message": err.message }),
                        );
                        std::process::exit(err.exit_code);
                    }

                    let prev_id = snapshot_id - 1;
                    let prev = browser::load_snapshot(&ctx, prev_id).await.unwrap_or(None);
                    let Some(prev) = prev else {
                        let err = CliError::unknown(
                            format!("Previous snapshot {} not found", prev_id),
                            "Run 'sauron snapshot' again",
                        );
                        print_result(&make_error("diff", &err));
                        finish_runtime_command(
                            &runtime,
                            "diff",
                            false,
                            json!({ "message": err.message }),
                        );
                        std::process::exit(err.exit_code);
                    };

                    let after = state.last_snapshot;
                    let d = diff::diff_snapshots(&prev, &after);

                    if format == "json" {
                        print_result(&make_success(
                            "diff",
                            json!({
                                "added": d.added,
                                "removed": d.removed,
                                "changed": d.changed,
                                "unified": d.unified,
                                "snapshotId": snapshot_id,
                                "previousId": prev_id
                            }),
                        ));
                    } else {
                        print_result(&make_success(
                            "diff",
                            json!({
                                "unified": d.unified,
                                "snapshotId": snapshot_id,
                                "previousId": prev_id
                            }),
                        ));
                    }
                    finish_runtime_command(
                        &runtime,
                        "diff",
                        true,
                        json!({ "snapshotId": snapshot_id, "previousId": prev_id }),
                    );
                }

                Commands::Tab { command } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_only_command(&mut runtime, port_flag, "tab", move |browser| {
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
                                            let bound = bound_target_id.as_deref()
                                                == Some(t.target_id.as_str());
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
                                        "tab",
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
                                        "tab",
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
                                        "tab",
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
                                    let was_bound =
                                        bound_target_id.as_deref() == Some(target_id.as_str());

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
                                        "tab",
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

                Commands::Session { command } => {
                    let cmd_ctx = ctx.clone();
                    with_browser_command(&mut runtime, port_flag, "session", move |page| {
                        let cmd_ctx = cmd_ctx.clone();
                        async move {
                            match command {
                                SessionCommands::Save { name } => {
                                    let data =
                                        session::save_session(&cmd_ctx, &name, &page).await?;
                                    Ok(make_success(
                                        "session",
                                        json!({
                                            "action": "save",
                                            "name": data.name,
                                            "savedAt": data.saved_at,
                                            "url": data.url,
                                            "cookieCount": data.cookies.len()
                                        }),
                                    ))
                                }
                                SessionCommands::Load { name } => {
                                    let data =
                                        session::load_session(&cmd_ctx, &name, &page).await?;
                                    Ok(make_success(
                                        "session",
                                        json!({
                                            "action": "load",
                                            "name": data.name,
                                            "url": data.url,
                                            "cookieCount": data.cookies.len()
                                        }),
                                    ))
                                }
                                SessionCommands::List => {
                                    let list = session::list_sessions(&cmd_ctx).await?;
                                    Ok(make_success(
                                        "session",
                                        json!({ "action": "list", "sessions": list }),
                                    ))
                                }
                                SessionCommands::Delete { name } => {
                                    let ok = session::delete_session(&cmd_ctx, &name).await?;
                                    Ok(make_success(
                                        "session",
                                        json!({ "action": "delete", "name": name, "deleted": ok }),
                                    ))
                                }
                            }
                        }
                    })
                    .await;
                }
            }
        }
    }
}

async fn with_browser_command<F, Fut, T>(
    runtime: &mut ActiveRuntime,
    port_flag: Option<u16>,
    command_name: &'static str,
    f: F,
) where
    F: FnOnce(browser::PageClient) -> Fut,
    Fut: std::future::Future<Output = Result<types::ResultEnvelope<T>, CliError>>,
    T: Serialize,
{
    if let Err(e) = begin_runtime_command(runtime, command_name) {
        let res = make_error(command_name, &e);
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
            let res = make_error(command_name, &e);
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
            let res = make_error(command_name, &e);
            print_result(&res);
            std::process::exit(e.exit_code);
        }
    };

    match f(page).await {
        Ok(res) => {
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
            let res = make_error(command_name, &e);
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
    if let Err(e) = begin_runtime_command(runtime, command_name) {
        let res = make_error(command_name, &e);
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
            let res = make_error(command_name, &e);
            print_result(&res);
            std::process::exit(e.exit_code);
        }
    };

    match f(browser).await {
        Ok(res) => {
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
            let res = make_error(command_name, &e);
            print_result(&res);
            std::process::exit(e.exit_code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_fallback_allows_missing_runtime() {
        assert!(should_fallback_to_cleanup_without_runtime(
            types::ErrorCode::SessionRequired,
            false,
        ));
    }

    #[test]
    fn cleanup_fallback_allows_implicit_bad_input() {
        assert!(should_fallback_to_cleanup_without_runtime(
            types::ErrorCode::BadInput,
            false,
        ));
    }

    #[test]
    fn cleanup_fallback_allows_implicit_invalid_session() {
        assert!(should_fallback_to_cleanup_without_runtime(
            types::ErrorCode::SessionInvalid,
            false,
        ));
    }

    #[test]
    fn cleanup_fallback_rejects_explicit_bad_input() {
        assert!(!should_fallback_to_cleanup_without_runtime(
            types::ErrorCode::BadInput,
            true,
        ));
    }
}
