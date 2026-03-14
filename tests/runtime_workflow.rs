use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct IntegrationEnv {
    home: PathBuf,
}

impl IntegrationEnv {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let home = std::env::temp_dir().join(format!(
            "sauron-int-{}-{}-{}",
            prefix,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&home).expect("integration home should be created");
        Self { home }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sauron"));
        cmd.env("SAURON_HOME", &self.home);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .output()
            .unwrap_or_else(|err| panic!("failed to run sauron {:?}: {}", args, err))
    }

    fn run_json(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "command {:?} failed\nstatus: {:?}\nstdout: {}\nstderr: {}",
            args,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
            panic!(
                "failed to parse JSON for {:?}: {}\nstdout: {}",
                args,
                err,
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }
}

impl Drop for IntegrationEnv {
    fn drop(&mut self) {
        let _ = self.command().args(["runtime", "cleanup"]).output();
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn command_exists(binary: &str) -> bool {
    #[cfg(windows)]
    {
        which::which(binary).is_ok()
    }
    #[cfg(not(windows))]
    {
        which::which(binary).is_ok()
    }
}

fn chrome_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        ]
        .iter()
        .any(|path| Path::new(path).exists())
            || command_exists("google-chrome")
    }

    #[cfg(target_os = "linux")]
    {
        [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium-browser",
            "/usr/bin/chromium",
            "/snap/bin/chromium",
        ]
        .iter()
        .any(|path| Path::new(path).exists())
            || command_exists("google-chrome")
            || command_exists("chromium")
    }

    #[cfg(target_os = "windows")]
    {
        [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files\Chromium\Application\chrome.exe",
        ]
        .iter()
        .any(|path| Path::new(path).exists())
            || command_exists("chrome")
    }
}

fn skip_if_no_chrome() -> bool {
    if chrome_available() {
        return false;
    }

    eprintln!("skipping runtime workflow integration test because no Chrome binary is available");
    true
}

#[cfg(unix)]
fn kill_process(pid: u32) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    kill(Pid::from_raw(pid as i32), Signal::SIGKILL).expect("failed to kill Chrome process");
}

#[cfg(windows)]
fn kill_process(pid: u32) {
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status()
        .expect("failed to invoke taskkill");
    assert!(status.success(), "failed to kill Chrome process {pid}");
}

fn collect_relative_files(root: &Path) -> Vec<String> {
    fn walk(root: &Path, current: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                walk(root, &path, out);
            } else if file_type.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .expect("path should stay under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(relative);
            }
        }
    }

    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn wait_for_status(env: &IntegrationEnv, args: &[&str], expected: &str) -> Value {
    let mut last = Value::Null;
    for _ in 0..30 {
        last = env.run_json(args);
        if last["result"]["data"]["status"] == expected {
            return last;
        }
        thread::sleep(Duration::from_millis(200));
    }

    panic!(
        "status did not reach {:?}; last payload: {}",
        expected, last
    );
}

#[test]
fn runtime_flow_survives_across_subprocesses_and_stops_cleanly() {
    if skip_if_no_chrome() {
        return;
    }

    let env = IntegrationEnv::new("workflow");

    let start = env.run_json(&[
        "--session-id",
        "sess_it_flow",
        "--instance",
        "inst_it_flow",
        "--client",
        "client_it_flow",
        "runtime",
        "start",
    ]);
    let pid = start["result"]["data"]["pid"]
        .as_u64()
        .expect("start pid should exist");

    let status = env.run_json(&["--session-id", "sess_it_flow", "runtime", "status"]);
    assert_eq!(status["result"]["data"]["status"], "running");
    assert_eq!(status["result"]["data"]["pid"], pid);

    let goto = env.run_json(&[
        "--session-id",
        "sess_it_flow",
        "page",
        "goto",
        "https://example.com",
    ]);
    assert_eq!(goto["result"]["data"]["status"], 200);
    assert_eq!(goto["result"]["data"]["url"], "https://example.com");

    let stop = env.run_json(&["--session-id", "sess_it_flow", "runtime", "stop"]);
    assert_eq!(stop["result"]["data"]["daemonStopped"], true);

    let stopped = env.run_json(&["runtime", "status"]);
    assert_eq!(stopped["result"]["data"]["status"], "stopped");

    let remaining_files = collect_relative_files(&env.home);
    assert_eq!(remaining_files, vec!["runtime/.store.lock"]);
}

#[test]
fn runtime_cleanup_recovers_a_stale_session_end_to_end() {
    if skip_if_no_chrome() {
        return;
    }

    let env = IntegrationEnv::new("stale");

    let start = env.run_json(&[
        "--session-id",
        "sess_it_stale",
        "--instance",
        "inst_it_stale",
        "--client",
        "client_it_stale",
        "runtime",
        "start",
    ]);
    let pid = start["result"]["data"]["pid"]
        .as_u64()
        .expect("start pid should exist") as u32;

    kill_process(pid);

    let stale = wait_for_status(
        &env,
        &["--session-id", "sess_it_stale", "runtime", "status"],
        "stale",
    );
    assert_eq!(stale["result"]["data"]["pid"], pid);

    let cleanup = env.run_json(&["runtime", "cleanup"]);
    assert_eq!(cleanup["result"]["data"]["cleanup"]["sessions"], 1);
    assert_eq!(cleanup["result"]["data"]["cleanup"]["instances"], 1);

    let stopped = env.run_json(&["--session-id", "sess_it_stale", "runtime", "status"]);
    assert_eq!(stopped["result"]["data"]["status"], "stopped");

    let remaining_files = collect_relative_files(&env.home);
    assert_eq!(remaining_files, vec!["runtime/.store.lock"]);
}
