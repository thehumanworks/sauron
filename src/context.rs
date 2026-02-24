use crate::errors::CliError;
use crate::types::PidFileData;
use directories::BaseDirs;
use fs2::FileExt;
use regex::Regex;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_PORT: u16 = 9222;

pub fn resolve_base_dir() -> Result<PathBuf, CliError> {
    if let Ok(home) = std::env::var("SAURON_HOME") {
        return Ok(PathBuf::from(home));
    }

    let base_dirs = BaseDirs::new().ok_or_else(|| {
        CliError::unknown(
            "Could not determine home directory",
            "Set SAURON_HOME to override the state directory",
        )
    })?;
    Ok(base_dirs.home_dir().join(".sauron"))
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AppContext {
    pub instance: String,
    pub client: String,
    pub base_dir: PathBuf,
    pub instance_dir: PathBuf,
    pub client_dir: PathBuf,
    pub refs_path: PathBuf,
    pub snapshots_dir: PathBuf,
    pub sessions_dir: PathBuf,
    pub target_path: PathBuf,
    pub instance_lock_path: PathBuf,
    pub client_lock_path: PathBuf,
    pub pid_path: PathBuf,
    pub user_data_dir: PathBuf,
}

impl AppContext {
    pub fn new(
        instance: &str,
        client: &str,
        pid_path: Option<PathBuf>,
        user_data_dir: Option<PathBuf>,
    ) -> Result<Self, CliError> {
        let instance = validate_name(
            instance,
            "--instance cannot be empty",
            "Invalid instance name",
        )?;
        let client = validate_name(client, "--client cannot be empty", "Invalid client name")?;

        let base_dir = resolve_base_dir()?;

        let instance_dir = base_dir.join("instances").join(&instance);
        let client_dir = instance_dir.join("clients").join(&client);

        let refs_path = client_dir.join("refs.json");
        let snapshots_dir = client_dir.join("snapshots");
        let sessions_dir = client_dir.join("sessions");
        let target_path = client_dir.join("target.json");
        let instance_lock_path = instance_dir.join(".instance.lock");
        let client_lock_path = client_dir.join(".client.lock");

        let pid_path = pid_path.unwrap_or_else(|| instance_dir.join("chrome.pid"));
        let user_data_dir = user_data_dir.unwrap_or_else(|| instance_dir.join("chrome-data"));

        Ok(Self {
            instance,
            client,
            base_dir,
            instance_dir,
            client_dir,
            refs_path,
            snapshots_dir,
            sessions_dir,
            target_path,
            instance_lock_path,
            client_lock_path,
            pid_path,
            user_data_dir,
        })
    }

    pub fn ensure_instance_dirs(&self) -> Result<(), CliError> {
        fs::create_dir_all(&self.instance_dir).map_err(|e| {
            CliError::unknown(
                format!(
                    "Failed to create instance dir {}: {}",
                    self.instance_dir.display(),
                    e
                ),
                "Check filesystem permissions",
            )
        })?;
        fs::create_dir_all(&self.client_dir).map_err(|e| {
            CliError::unknown(
                format!(
                    "Failed to create client dir {}: {}",
                    self.client_dir.display(),
                    e
                ),
                "Check filesystem permissions",
            )
        })?;
        Ok(())
    }

    pub fn acquire_client_lock(&self) -> Result<FileLock, CliError> {
        FileLock::acquire_exclusive(&self.client_lock_path)
    }

    pub fn default_port(&self) -> u16 {
        DEFAULT_PORT
    }

    pub fn read_pid_file(&self) -> Option<PidFileData> {
        read_pid_file(&self.pid_path).ok().flatten()
    }

    /// Determine which port to use for browser commands.
    ///
    /// Priority:
    /// 1) explicit `--port`
    /// 2) pidfile port (if present)
    /// 3) default 9222
    pub fn resolve_port(&self, port_flag: Option<u16>) -> u16 {
        if let Some(p) = port_flag {
            return p;
        }
        if let Some(pid) = self.read_pid_file() {
            return pid.port;
        }
        self.default_port()
    }
}

pub fn read_pid_file(path: &Path) -> Result<Option<PidFileData>, CliError> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let parsed: Result<PidFileData, _> = serde_json::from_str(&text);
            match parsed {
                Ok(data) => {
                    if data.pid == 0 || data.port == 0 {
                        Ok(None)
                    } else {
                        Ok(Some(data))
                    }
                }
                Err(_) => Ok(None),
            }
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(CliError::unknown(
                    format!("Failed to read pidfile {}: {}", path.display(), e),
                    "Check filesystem permissions",
                ))
            }
        }
    }
}

pub fn write_pid_file(path: &Path, data: &PidFileData) -> Result<(), CliError> {
    let s = serde_json::to_string(data).map_err(|e| {
        CliError::unknown(
            format!("Failed to serialize pidfile JSON: {}", e),
            "This should not happen",
        )
    })?;
    atomic_write(path, s.as_bytes())
}

pub fn remove_pid_file(path: &Path) -> Result<(), CliError> {
    match fs::remove_file(path) {
        Ok(_) => Ok(()),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(CliError::unknown(
                    format!("Failed to remove pidfile {}: {}", path.display(), e),
                    "Check filesystem permissions",
                ))
            }
        }
    }
}

pub struct FileLock {
    file: fs::File,
}

impl FileLock {
    pub fn acquire_exclusive(path: &Path) -> Result<Self, CliError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                CliError::unknown(
                    format!("Failed to create lock dir {}: {}", parent.display(), e),
                    "Check filesystem permissions",
                )
            })?;
        }

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| {
                CliError::unknown(
                    format!("Failed to open lock file {}: {}", path.display(), e),
                    "Check filesystem permissions",
                )
            })?;

        file.lock_exclusive().map_err(|e| {
            CliError::unknown(
                format!("Failed to lock {}: {}", path.display(), e),
                "Another process may be holding the lock",
            )
        })?;

        Ok(Self { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), CliError> {
    let parent = path.parent().ok_or_else(|| {
        CliError::unknown(
            format!("Path {} has no parent directory", path.display()),
            "Use a file path under a directory",
        )
    })?;

    fs::create_dir_all(parent).map_err(|e| {
        CliError::unknown(
            format!("Failed to create directory {}: {}", parent.display(), e),
            "Check filesystem permissions",
        )
    })?;

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| CliError::unknown(format!("Invalid filename {}", path.display()), ""))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_path = parent.join(format!(
        ".{}.tmp.{}.{}",
        file_name,
        std::process::id(),
        nonce
    ));

    {
        let mut tmp = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| {
                CliError::unknown(
                    format!("Failed to create temp file {}: {}", tmp_path.display(), e),
                    "Check filesystem permissions",
                )
            })?;
        tmp.write_all(data).map_err(|e| {
            CliError::unknown(
                format!("Failed writing temp file {}: {}", tmp_path.display(), e),
                "Check filesystem permissions",
            )
        })?;
        tmp.sync_all().map_err(|e| {
            CliError::unknown(
                format!("Failed to sync temp file {}: {}", tmp_path.display(), e),
                "Check filesystem permissions",
            )
        })?;
    }

    match fs::rename(&tmp_path, path) {
        Ok(_) => {}
        Err(e) => {
            #[cfg(windows)]
            {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    let _ = fs::remove_file(path);
                    fs::rename(&tmp_path, path).map_err(|e2| {
                        CliError::unknown(
                            format!(
                                "Failed to replace {} with {}: {}",
                                path.display(),
                                tmp_path.display(),
                                e2
                            ),
                            "Check filesystem permissions",
                        )
                    })?;
                } else {
                    return Err(CliError::unknown(
                        format!(
                            "Failed to move temp file {} to {}: {}",
                            tmp_path.display(),
                            path.display(),
                            e
                        ),
                        "Check filesystem permissions",
                    ));
                }
            }
            #[cfg(not(windows))]
            {
                return Err(CliError::unknown(
                    format!(
                        "Failed to move temp file {} to {}: {}",
                        tmp_path.display(),
                        path.display(),
                        e
                    ),
                    "Check filesystem permissions",
                ));
            }
        }
    }

    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

fn validate_name(value: &str, empty_msg: &str, invalid_prefix: &str) -> Result<String, CliError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CliError::bad_input(
            empty_msg,
            "Use a non-empty value like 'default' or 'work'",
        ));
    }

    let re = Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
    if !re.is_match(trimmed) {
        return Err(CliError::bad_input(
            format!("{}: \"{}\"", invalid_prefix, trimmed),
            "Use only letters, numbers, hyphens, and underscores",
        ));
    }
    Ok(trimmed.to_string())
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
        std::env::temp_dir().join(format!("sauron-test-{}", nanos))
    }

    #[test]
    fn context_uses_client_scoped_state_paths() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);

        let ctx = AppContext::new("work", "default", None, None).expect("context should build");

        assert_eq!(
            ctx.refs_path,
            home.join("instances")
                .join("work")
                .join("clients")
                .join("default")
                .join("refs.json")
        );
        assert_eq!(
            ctx.snapshots_dir,
            home.join("instances")
                .join("work")
                .join("clients")
                .join("default")
                .join("snapshots")
        );
        assert_eq!(
            ctx.sessions_dir,
            home.join("instances")
                .join("work")
                .join("clients")
                .join("default")
                .join("sessions")
        );

        std::env::remove_var("SAURON_HOME");
    }

    #[test]
    fn atomic_write_replaces_existing_content() {
        let dir = unique_test_home();
        let path = dir.join("state.json");

        atomic_write(&path, br#"{"a":1}"#).expect("first write");
        atomic_write(&path, br#"{"a":2}"#).expect("second write");

        let text = std::fs::read_to_string(&path).expect("read output");
        assert_eq!(text, r#"{"a":2}"#);
    }
}
