use crate::context::{remove_pid_file, write_pid_file, FileLock};
use crate::errors::CliError;
use crate::types::{DaemonStatus, PidFileData, Viewport};
use rand::Rng;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const FETCH_TIMEOUT: Duration = Duration::from_millis(2_000);

#[derive(Debug, Clone, Copy)]
pub struct ChromeLaunchOptions {
    /// Run Chrome headless (`--headless=new`).
    pub headless: bool,
    /// Disable GPU acceleration (`--disable-gpu`).
    pub disable_gpu: bool,
    /// Enable WebGL-friendly flags (SwiftShader). macOS-only.
    pub webgl: bool,
    /// Initial viewport size passed to Chrome at launch.
    pub viewport: Viewport,
}

pub async fn get_ws_url(port: u16) -> Result<String, CliError> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| CliError::unknown(format!("Failed to build HTTP client: {}", e), ""))?;

    let url = format!("http://127.0.0.1:{}/json/version", port);
    let resp = client.get(url).send().await.map_err(|e| {
        CliError::unknown(
            format!("Failed to fetch Chrome DevTools version: {}", e),
            "Is Chrome running?",
        )
    })?;

    if !resp.status().is_success() {
        return Err(CliError::unknown(
            format!("Chrome DevTools responded with {}", resp.status()),
            "Is Chrome running with --remote-debugging-port?",
        ));
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| {
        CliError::unknown(format!("Failed to parse Chrome DevTools JSON: {}", e), "")
    })?;

    let ws = json
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            CliError::unknown(
                "Chrome DevTools JSON missing webSocketDebuggerUrl".to_string(),
                "Is this port serving Chrome DevTools?",
            )
        })?;

    Ok(ws.to_string())
}

pub async fn is_running(port: u16) -> bool {
    get_ws_url(port).await.is_ok()
}

#[cfg(unix)]
pub(crate) fn is_process_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

#[cfg(unix)]
fn send_signal(pid: u32, sig: nix::sys::signal::Signal) -> Result<(), CliError> {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), Some(sig))
        .map_err(|e| CliError::unknown(format!("Failed to signal process {}: {}", pid, e), ""))
}

#[cfg(windows)]
pub(crate) fn is_process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code as *mut u32);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

#[cfg(windows)]
fn terminate_process(pid: u32) -> Result<(), CliError> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle == 0 {
            return Ok(());
        }
        let _ = TerminateProcess(handle, 0);
        CloseHandle(handle);
    }
    Ok(())
}

fn best_effort_terminate_pid(pid: u32) {
    #[cfg(unix)]
    {
        use nix::sys::signal::Signal;
        let _ = send_signal(pid, Signal::SIGTERM);
        let _ = send_signal(pid, Signal::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = terminate_process(pid);
    }
}

pub fn find_chrome_binary() -> String {
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        ];
        for c in candidates {
            if Path::new(c).exists() {
                return c.to_string();
            }
        }
        "google-chrome".to_string()
    }

    #[cfg(target_os = "linux")]
    {
        let candidates = [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium-browser",
            "/usr/bin/chromium",
            "/snap/bin/chromium",
        ];
        for c in candidates {
            if Path::new(c).exists() {
                return c.to_string();
            }
        }
        "google-chrome".to_string()
    }

    #[cfg(target_os = "windows")]
    {
        let candidates = [
            r"C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
            r"C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
            r"C:\\Program Files\\Chromium\\Application\\chrome.exe",
        ];
        for c in candidates {
            if Path::new(c).exists() {
                return c.to_string();
            }
        }
        "chrome".to_string()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "google-chrome".to_string()
    }
}

pub fn build_chrome_args(
    port: u16,
    user_data_dir: &Path,
    opts: ChromeLaunchOptions,
) -> Vec<String> {
    let mut args = Vec::new();
    args.push(format!(
        "--window-size={},{}",
        opts.viewport.width, opts.viewport.height
    ));

    if opts.headless {
        args.push("--headless=new".to_string());
    } else {
        #[cfg(target_os = "macos")]
        {
            // Launch windowless on macOS to avoid focus steals/window flashes.
            args.push("--no-startup-window".to_string());
            args.push("--silent-launch".to_string());
        }

        #[cfg(not(target_os = "macos"))]
        {
            // Best-effort: keep the window from interrupting user flow.
            args.push("--window-position=-32000,-32000".to_string());
            args.push("--start-minimized".to_string());
        }
    }

    if opts.disable_gpu {
        args.push("--disable-gpu".to_string());
    }

    #[cfg(target_os = "macos")]
    if opts.webgl {
        // WebGL in headless/server environments often needs explicit SwiftShader opt-in.
        // See: Chromium SwiftShader docs and the deprecation of automatic SwiftShader fallback.
        args.push("--enable-webgl".to_string());
        args.push("--ignore-gpu-blocklist".to_string());
        args.push("--use-angle=swiftshader".to_string());
        args.push("--enable-unsafe-swiftshader".to_string());
    }

    args.push(format!("--remote-debugging-port={}", port));
    args.push(format!("--user-data-dir={}", user_data_dir.display()));
    args.push("--no-first-run".to_string());
    args.push("--no-default-browser-check".to_string());

    // Agent-friendly / stability flags
    args.push("--disable-background-timer-throttling".to_string());
    args.push("--disable-backgrounding-occluded-windows".to_string());
    args.push("--disable-renderer-backgrounding".to_string());
    args.push("--mute-audio".to_string());

    args
}

pub async fn get_status(pid_path: &Path, port: u16) -> DaemonStatus {
    let pid_data: Option<PidFileData> = match std::fs::read_to_string(pid_path) {
        Ok(text) => serde_json::from_str(&text).ok(),
        Err(_) => None,
    };

    if let Some(data) = pid_data {
        if !is_process_alive(data.pid) {
            return DaemonStatus::Stale {
                pid: Some(data.pid),
                port: Some(data.port),
            };
        }

        match get_ws_url(data.port).await {
            Ok(ws) => {
                return DaemonStatus::Running {
                    pid: Some(data.pid),
                    port: data.port,
                    ws_url: Some(ws),
                };
            }
            Err(_) => {
                return DaemonStatus::Stale {
                    pid: Some(data.pid),
                    port: Some(data.port),
                };
            }
        }
    }

    // No PID file — check if Chrome is responding on the given port anyway.
    match get_ws_url(port).await {
        Ok(ws) => DaemonStatus::Running {
            pid: None,
            port,
            ws_url: Some(ws),
        },
        Err(_) => DaemonStatus::Stopped,
    }
}

fn port_is_bindable(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn pick_free_port(preferred: Option<u16>) -> u16 {
    if let Some(p) = preferred {
        if port_is_bindable(p) {
            return p;
        }
    }

    let mut rng = rand::thread_rng();
    for _ in 0..100 {
        let p: u16 = rng.gen_range(40_000..60_000);
        if port_is_bindable(p) {
            return p;
        }
    }

    // Fall back: OS-assigned ephemeral port
    TcpListener::bind(("127.0.0.1", 0))
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(0)
}

pub struct StartResult {
    pub pid: u32,
    pub port: u16,
    pub ws_url: String,
    #[allow(dead_code)]
    pub xvfb_pid: Option<u32>,
    #[allow(dead_code)]
    pub display: Option<String>,
}

pub async fn start(
    pid_path: PathBuf,
    user_data_dir: PathBuf,
    instance_lock_path: PathBuf,
    port_flag: Option<u16>,
    timeout_ms: u64,
    opts: ChromeLaunchOptions,
) -> Result<StartResult, CliError> {
    let _instance_lock = FileLock::acquire_exclusive(&instance_lock_path)?;

    if opts.webgl && opts.disable_gpu {
        return Err(CliError::bad_input(
            "--webgl conflicts with --disable-gpu",
            "Remove --disable-gpu, or omit --webgl",
        ));
    }

    let launch_opts = opts;

    // Choose port.
    let port = if let Some(p) = port_flag {
        p
    } else {
        // If pidfile exists, keep its port.
        if let Ok(text) = std::fs::read_to_string(&pid_path) {
            if let Ok(data) = serde_json::from_str::<PidFileData>(&text) {
                data.port
            } else {
                pick_free_port(Some(9222))
            }
        } else {
            pick_free_port(Some(9222))
        }
    };

    let status = get_status(&pid_path, port).await;
    match status {
        DaemonStatus::Running {
            pid: Some(pid),
            port: status_port,
            ws_url: Some(ws_url),
        } => {
            if status_port != port {
                return Err(CliError::unknown(
                    format!(
                        "Chrome daemon for this instance is already running on port {} (requested {}).",
                        status_port, port
                    ),
                    "Run 'sauron terminate' first, then retry with your desired --port",
                ));
            }
            return Ok(StartResult {
                pid,
                port: status_port,
                ws_url,
                xvfb_pid: None,
                display: None,
            });
        }
        DaemonStatus::Running {
            pid: None, port, ..
        } => {
            return Err(CliError::unknown(
                format!(
                    "Port {} is already in use by an unmanaged Chrome process. Stop it manually or use a different port/instance.",
                    port
                ),
                "Try: sauron start --port <free-port> or sauron start --instance <name>",
            ));
        }
        DaemonStatus::Stale { .. } => {
            // Clean up stale pidfile.
            let _ = remove_pid_file(&pid_path);

            // A stale pidfile can still point to a port now occupied by another process.
            if get_ws_url(port).await.is_ok() || !port_is_bindable(port) {
                return Err(CliError::unknown(
                    format!(
                        "Port {} is already in use by an unmanaged process. Stop it manually or use a different port/instance.",
                        port
                    ),
                    "Try: sauron start --port <free-port> or sauron start --instance <name>",
                ));
            }
        }
        DaemonStatus::Stopped => {}
        _ => {}
    }

    std::fs::create_dir_all(&user_data_dir).map_err(|e| {
        CliError::unknown(
            format!(
                "Failed to create Chrome user data dir {}: {}",
                user_data_dir.display(),
                e
            ),
            "Check filesystem permissions",
        )
    })?;

    let binary = find_chrome_binary();

    let (xvfb_pid, display): (Option<u32>, Option<String>) = (None, None);

    let args = build_chrome_args(port, &user_data_dir, launch_opts);

    let mut cmd = Command::new(binary);
    if let Some(ref disp) = display {
        cmd.env("DISPLAY", disp);
    }
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let child = cmd.spawn().map_err(|e| {
        CliError::unknown(
            format!("Failed to spawn Chrome: {}", e),
            "Ensure Chrome is installed and discoverable",
        )
    })?;

    let pid = child.id();
    // Drop child handle; daemon continues.
    drop(child);

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        match get_ws_url(port).await {
            Ok(ws_url) => {
                write_pid_file(
                    &pid_path,
                    &PidFileData {
                        pid,
                        port,
                        xvfb_pid,
                        display: display.clone(),
                    },
                )?;
                return Ok(StartResult {
                    pid,
                    port,
                    ws_url,
                    xvfb_pid,
                    display,
                });
            }
            Err(_) => {
                if !is_process_alive(pid) {
                    return Err(CliError::unknown(
                        format!(
                            "Chrome process (pid: {}) died during startup on port {}",
                            pid, port
                        ),
                        "Check Chrome logs and retry with a longer --timeout if needed",
                    ));
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }

    // Timeout — attempt to terminate.
    #[cfg(unix)]
    {
        let _ = send_signal(pid, nix::sys::signal::Signal::SIGTERM);
    }
    #[cfg(windows)]
    {
        let _ = terminate_process(pid);
    }
    Err(CliError::timeout(
        format!(
            "Chrome failed to start within {}ms on port {}",
            timeout_ms, port
        ),
        "Try increasing --timeout or using a different --port",
    ))
}

pub async fn stop(
    pid_path: &Path,
    instance_lock_path: &Path,
    port_hint: Option<u16>,
    timeout_ms: u64,
) -> Result<bool, CliError> {
    let _instance_lock = FileLock::acquire_exclusive(instance_lock_path)?;

    let pid_data: Option<PidFileData> = match std::fs::read_to_string(pid_path) {
        Ok(text) => serde_json::from_str(&text).ok(),
        Err(_) => None,
    };

    let Some(data) = pid_data else {
        return Ok(false);
    };

    let xvfb_pid = data.xvfb_pid;

    match get_status(pid_path, data.port).await {
        DaemonStatus::Running {
            pid: Some(live_pid),
            port,
            ..
        } if live_pid == data.pid && port == data.port => {}
        _ => {
            // Stale pid file
            if let Some(xpid) = xvfb_pid {
                best_effort_terminate_pid(xpid);
            }
            let _ = remove_pid_file(pid_path);
            return Ok(false);
        }
    }

    let pid = data.pid;

    // Graceful shutdown
    #[cfg(unix)]
    {
        let _ = send_signal(pid, nix::sys::signal::Signal::SIGTERM);
    }
    #[cfg(windows)]
    {
        let _ = terminate_process(pid);
    }

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        if !is_process_alive(pid) {
            if let Some(xpid) = xvfb_pid {
                best_effort_terminate_pid(xpid);
            }
            let _ = remove_pid_file(pid_path);
            return Ok(true);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Force kill
    #[cfg(unix)]
    {
        let _ = send_signal(pid, nix::sys::signal::Signal::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = terminate_process(pid);
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    if let Some(xpid) = xvfb_pid {
        best_effort_terminate_pid(xpid);
    }
    let _ = remove_pid_file(pid_path);

    // Verify port freed (best-effort)
    let port = port_hint.unwrap_or(data.port);
    if is_running(port).await {
        return Err(CliError::unknown(
            format!("Chrome process killed but port {} is still in use. There may be orphaned child processes.", port),
            "Check for leftover chrome/chromium processes and terminate them",
        ));
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch_opts(headless: bool) -> ChromeLaunchOptions {
        ChromeLaunchOptions {
            headless,
            disable_gpu: true,
            webgl: false,
            viewport: Viewport {
                width: 1440,
                height: 900,
            },
        }
    }

    #[test]
    fn build_chrome_args_headless_includes_headless_flag() {
        let args = build_chrome_args(
            9222,
            Path::new("/tmp/sauron-test-profile"),
            launch_opts(true),
        );

        assert!(args.iter().any(|arg| arg == "--headless=new"));
        assert!(args.iter().any(|arg| arg == "--window-size=1440,900"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn build_chrome_args_headed_macos_uses_windowless_startup() {
        let args = build_chrome_args(
            9222,
            Path::new("/tmp/sauron-test-profile"),
            launch_opts(false),
        );

        assert!(args.iter().any(|arg| arg == "--no-startup-window"));
        assert!(args.iter().any(|arg| arg == "--silent-launch"));
        assert!(!args.iter().any(|arg| arg == "--start-minimized"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn build_chrome_args_headed_non_macos_uses_window_management_flags() {
        let args = build_chrome_args(
            9222,
            Path::new("/tmp/sauron-test-profile"),
            launch_opts(false),
        );

        assert!(args.iter().any(|arg| arg == "--window-size=1440,900"));
        assert!(args
            .iter()
            .any(|arg| arg == "--window-position=-32000,-32000"));
        assert!(args.iter().any(|arg| arg == "--start-minimized"));
        assert!(!args.iter().any(|arg| arg == "--no-startup-window"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn build_chrome_args_non_macos_ignores_webgl_flags() {
        let mut opts = launch_opts(true);
        opts.webgl = true;
        let args = build_chrome_args(9222, Path::new("/tmp/sauron-test-profile"), opts);

        assert!(!args.iter().any(|arg| arg == "--enable-webgl"));
        assert!(!args.iter().any(|arg| arg == "--use-angle=swiftshader"));
    }
}
