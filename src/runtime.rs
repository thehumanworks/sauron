use crate::context::{atomic_write, resolve_base_dir, FileLock};
use crate::daemon;
use crate::errors::CliError;
use crate::types::{ErrorCode, PidFileData};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const BINDING_KIND_PARENT: &str = "ppid";
const BINDING_KIND_PROJECT: &str = "project";

#[derive(Debug, Clone)]
pub struct RuntimeStore {
    base_dir: PathBuf,
}

#[derive(Debug, Clone)]
enum BindingScope {
    ParentProcess(u32),
    Project(String),
}

impl RuntimeStore {
    pub fn new() -> Result<Self, CliError> {
        let base_dir = resolve_base_dir()?;
        Ok(Self { base_dir })
    }

    pub fn save_session(&self, session: &RuntimeSessionRecord) -> Result<(), CliError> {
        self.save_session_fs(session)
    }

    /// Atomically record that a command is being executed against a session.
    ///
    /// Why this exists:
    /// - Multiple `sauron` processes can run concurrently against the same session.
    /// - A naive read/modify/write on the session record can lose updates (command_count, last_command).
    ///
    /// This method performs a concurrency-safe update and returns the updated session record.
    pub fn mark_session_command(
        &self,
        session_id: &str,
        command: &str,
    ) -> Result<RuntimeSessionRecord, CliError> {
        validate_identifier(session_id, "session id")?;
        if command.trim().is_empty() {
            return Err(CliError::bad_input(
                "command name cannot be empty",
                "Provide a non-empty command name",
            ));
        }
        self.mark_session_command_fs(session_id, command)
    }

    pub fn create_session_if_absent(&self, session: &RuntimeSessionRecord) -> Result<(), CliError> {
        self.create_session_if_absent_fs(session)
    }

    pub fn load_session(&self, session_id: &str) -> Result<Option<RuntimeSessionRecord>, CliError> {
        validate_identifier(session_id, "session id")?;
        self.load_session_fs(session_id)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<(), CliError> {
        validate_identifier(session_id, "session id")?;
        self.delete_session_fs(session_id)
    }

    pub fn bind_parent_process(&self, parent_pid: u32, session_id: &str) -> Result<(), CliError> {
        validate_identifier(session_id, "session id")?;
        self.bind_scope_fs(BindingScope::ParentProcess(parent_pid), session_id)
    }

    pub fn bind_project(&self, project_key: &str, session_id: &str) -> Result<(), CliError> {
        validate_identifier(project_key, "project key")?;
        validate_identifier(session_id, "session id")?;
        self.bind_scope_fs(BindingScope::Project(project_key.to_string()), session_id)
    }

    pub fn resolve_bound_session(&self, parent_pid: u32) -> Result<Option<String>, CliError> {
        self.resolve_bound_scope_fs(BindingScope::ParentProcess(parent_pid))
    }

    pub fn resolve_project_binding(&self, project_key: &str) -> Result<Option<String>, CliError> {
        validate_identifier(project_key, "project key")?;
        self.resolve_bound_scope_fs(BindingScope::Project(project_key.to_string()))
    }

    pub fn unbind_parent_process_if_matches(
        &self,
        parent_pid: u32,
        session_id: &str,
    ) -> Result<(), CliError> {
        validate_identifier(session_id, "session id")?;
        self.unbind_scope_if_matches_fs(BindingScope::ParentProcess(parent_pid), session_id)
    }

    pub fn unbind_project_if_matches(
        &self,
        project_key: &str,
        session_id: &str,
    ) -> Result<(), CliError> {
        validate_identifier(project_key, "project key")?;
        validate_identifier(session_id, "session id")?;
        self.unbind_scope_if_matches_fs(BindingScope::Project(project_key.to_string()), session_id)
    }

    pub fn remove_bindings_for_session(&self, session_id: &str) -> Result<(), CliError> {
        validate_identifier(session_id, "session id")?;
        self.remove_bindings_for_session_fs(session_id)
    }

    pub fn append_log(
        &self,
        session: &RuntimeSessionRecord,
        command: &str,
        status: &str,
        details: Option<Value>,
    ) -> Result<(), CliError> {
        let path = self
            .base_dir
            .join("runtime")
            .join("logs")
            .join(format!("{}.ndjson", session.session_id));
        let lock_path = self
            .base_dir
            .join("runtime")
            .join("logs")
            .join(format!("{}.lock", session.session_id));

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                CliError::unknown(
                    format!("Failed to create log directory {}: {}", parent.display(), e),
                    "Check filesystem permissions",
                )
            })?;
        }

        let _lock = FileLock::acquire_exclusive(&lock_path)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| {
                CliError::unknown(
                    format!("Failed to open log file {}: {}", path.display(), e),
                    "Check filesystem permissions",
                )
            })?;

        let payload = json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "sessionId": session.session_id,
            "instance": session.instance,
            "client": session.client,
            "pid": std::process::id(),
            "ppid": parent_process_id(),
            "command": command,
            "status": status,
            "details": details.unwrap_or(Value::Null)
        });

        let mut line = serde_json::to_vec(&payload).map_err(|e| {
            CliError::unknown(
                format!("Failed to serialize log line: {}", e),
                "This should not happen",
            )
        })?;
        line.push(b'\n');
        file.write_all(&line).map_err(|e| {
            CliError::unknown(
                format!("Failed to append session log {}: {}", path.display(), e),
                "Check filesystem permissions",
            )
        })?;
        file.sync_all().map_err(|e| {
            CliError::unknown(
                format!("Failed to sync session log {}: {}", path.display(), e),
                "Check filesystem permissions",
            )
        })?;
        Ok(())
    }

    fn runtime_dir(&self) -> PathBuf {
        self.base_dir.join("runtime")
    }

    fn sessions_dir(&self) -> PathBuf {
        self.runtime_dir().join("sessions")
    }

    fn parent_dir(&self) -> PathBuf {
        self.runtime_dir().join("ppid")
    }

    fn project_dir(&self) -> PathBuf {
        self.runtime_dir().join("project")
    }

    fn session_binding_index_dir(&self) -> PathBuf {
        self.runtime_dir().join("session-bindings")
    }

    fn metadata_lock_path(&self) -> PathBuf {
        self.runtime_dir().join(".store.lock")
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{}.json", session_id))
    }

    fn parent_path(&self, parent_pid: u32) -> PathBuf {
        self.parent_dir().join(format!("{}.txt", parent_pid))
    }

    fn project_path(&self, project_key: &str) -> PathBuf {
        self.project_dir().join(format!("{}.txt", project_key))
    }

    fn session_binding_index_path(&self, session_id: &str) -> PathBuf {
        self.session_binding_index_dir()
            .join(format!("{}.json", session_id))
    }

    fn binding_scope_key(scope: &BindingScope) -> String {
        match scope {
            BindingScope::ParentProcess(pid) => format!("{}:{}", BINDING_KIND_PARENT, pid),
            BindingScope::Project(project_key) => {
                format!("{}:{}", BINDING_KIND_PROJECT, project_key)
            }
        }
    }

    fn binding_scope_from_key(key: &str) -> Option<BindingScope> {
        let (kind, value) = key.split_once(':')?;
        match kind {
            BINDING_KIND_PARENT => value.parse::<u32>().ok().map(BindingScope::ParentProcess),
            BINDING_KIND_PROJECT => Some(BindingScope::Project(value.to_string())),
            _ => None,
        }
    }

    fn binding_path(&self, scope: &BindingScope) -> PathBuf {
        match scope {
            BindingScope::ParentProcess(pid) => self.parent_path(*pid),
            BindingScope::Project(project_key) => self.project_path(project_key),
        }
    }

    fn write_session_fs_locked(
        &self,
        session: &RuntimeSessionRecord,
        require_existing: bool,
    ) -> Result<(), CliError> {
        let text = serde_json::to_string_pretty(session).map_err(|e| {
            CliError::unknown(
                format!("Failed to serialize runtime session record: {}", e),
                "This should not happen",
            )
        })?;
        let path = self.session_path(&session.session_id);
        if require_existing && !path.exists() {
            return Err(CliError::session_invalid(
                format!("Session '{}' was not found", session.session_id),
                "Run 'sauron start' to create a fresh session",
            ));
        }
        atomic_write(&path, text.as_bytes())
    }

    fn save_session_fs(&self, session: &RuntimeSessionRecord) -> Result<(), CliError> {
        let _lock = FileLock::acquire_exclusive(&self.metadata_lock_path())?;
        self.write_session_fs_locked(session, true)
    }

    fn mark_session_command_fs(
        &self,
        session_id: &str,
        command: &str,
    ) -> Result<RuntimeSessionRecord, CliError> {
        let _lock = FileLock::acquire_exclusive(&self.metadata_lock_path())?;
        let mut session = self.load_session_fs(session_id)?.ok_or_else(|| {
            CliError::session_invalid(
                format!("Session '{}' was not found", session_id),
                "Run 'sauron start' to create a fresh session",
            )
        })?;
        if session.state != RuntimeSessionState::Active {
            return Err(CliError::session_terminated(
                format!("Session '{}' is terminated", session_id),
                "Run 'sauron start' to create a new active session",
            ));
        }
        session.mark_command(command);
        self.write_session_fs_locked(&session, true)?;
        Ok(session)
    }

    fn create_session_if_absent_fs(&self, session: &RuntimeSessionRecord) -> Result<(), CliError> {
        let _lock = FileLock::acquire_exclusive(&self.metadata_lock_path())?;
        let path = self.session_path(&session.session_id);
        if path.exists() {
            return Err(CliError::new(
                ErrorCode::SessionConflict,
                format!("Session '{}' already exists", session.session_id),
                "Use a different --session-id or omit it to auto-generate one",
                false,
                6,
            ));
        }
        self.write_session_fs_locked(session, false)
    }

    fn load_session_fs(&self, session_id: &str) -> Result<Option<RuntimeSessionRecord>, CliError> {
        let path = self.session_path(session_id);
        match fs::read_to_string(&path) {
            Ok(text) => {
                let parsed = serde_json::from_str::<RuntimeSessionRecord>(&text).map_err(|e| {
                    CliError::unknown(
                        format!("Failed to parse session record {}: {}", path.display(), e),
                        "The session record may be corrupted",
                    )
                })?;
                Ok(Some(parsed))
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(CliError::unknown(
                        format!("Failed to read session record {}: {}", path.display(), e),
                        "Check filesystem permissions",
                    ))
                }
            }
        }
    }

    fn delete_session_fs(&self, session_id: &str) -> Result<(), CliError> {
        let _lock = FileLock::acquire_exclusive(&self.metadata_lock_path())?;
        let path = self.session_path(session_id);
        match fs::remove_file(path) {
            Ok(_) => Ok(()),
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(CliError::unknown(
                        format!("Failed to remove session record: {}", e),
                        "Check filesystem permissions",
                    ))
                }
            }
        }
    }

    fn read_bound_scope_fs_locked(&self, scope: &BindingScope) -> Result<Option<String>, CliError> {
        let path = self.binding_path(scope);
        match fs::read_to_string(&path) {
            Ok(text) => {
                let session_id = text.trim().to_string();
                if session_id.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(session_id))
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(CliError::unknown(
                        format!("Failed to read process binding {}: {}", path.display(), e),
                        "Check filesystem permissions",
                    ))
                }
            }
        }
    }

    fn resolve_bound_scope_fs(&self, scope: BindingScope) -> Result<Option<String>, CliError> {
        self.read_bound_scope_fs_locked(&scope)
    }

    fn load_binding_index_fs_locked(&self, session_id: &str) -> Result<HashSet<String>, CliError> {
        let path = self.session_binding_index_path(session_id);
        match fs::read_to_string(&path) {
            Ok(text) => {
                let raw = serde_json::from_str::<Vec<String>>(&text).map_err(|e| {
                    CliError::unknown(
                        format!("Failed to parse binding index {}: {}", path.display(), e),
                        "The runtime state may be corrupted",
                    )
                })?;
                Ok(raw.into_iter().collect())
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(HashSet::new())
                } else {
                    Err(CliError::unknown(
                        format!("Failed to read binding index {}: {}", path.display(), e),
                        "Check filesystem permissions",
                    ))
                }
            }
        }
    }

    fn save_binding_index_fs_locked(
        &self,
        session_id: &str,
        keys: &HashSet<String>,
    ) -> Result<(), CliError> {
        let path = self.session_binding_index_path(session_id);
        if keys.is_empty() {
            match fs::remove_file(&path) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        return Ok(());
                    }
                    return Err(CliError::unknown(
                        format!("Failed to remove binding index {}: {}", path.display(), e),
                        "Check filesystem permissions",
                    ));
                }
            }
        }

        let mut values: Vec<String> = keys.iter().cloned().collect();
        values.sort_unstable();
        let text = serde_json::to_string_pretty(&values).map_err(|e| {
            CliError::unknown(
                format!(
                    "Failed to serialize binding index {}: {}",
                    path.display(),
                    e
                ),
                "This should not happen",
            )
        })?;
        atomic_write(&path, text.as_bytes())
    }

    fn bind_scope_fs(&self, scope: BindingScope, session_id: &str) -> Result<(), CliError> {
        let _lock = FileLock::acquire_exclusive(&self.metadata_lock_path())?;
        self.bind_scope_fs_locked(scope, session_id)
    }

    fn bind_scope_fs_locked(&self, scope: BindingScope, session_id: &str) -> Result<(), CliError> {
        let session_path = self.session_path(session_id);
        if !session_path.exists() {
            return Err(CliError::session_invalid(
                format!("Session '{}' was not found", session_id),
                "Run 'sauron start' to create a fresh session",
            ));
        }

        let binding_key = Self::binding_scope_key(&scope);
        let binding_path = self.binding_path(&scope);
        let previous_session = self.read_bound_scope_fs_locked(&scope)?;
        if let Some(prev_session_id) = previous_session {
            if prev_session_id != session_id {
                let mut prev_index = self.load_binding_index_fs_locked(&prev_session_id)?;
                prev_index.remove(&binding_key);
                self.save_binding_index_fs_locked(&prev_session_id, &prev_index)?;
            }
        }

        atomic_write(&binding_path, session_id.as_bytes())?;
        let mut next_index = self.load_binding_index_fs_locked(session_id)?;
        next_index.insert(binding_key);
        self.save_binding_index_fs_locked(session_id, &next_index)?;
        Ok(())
    }

    fn unbind_scope_if_matches_fs(
        &self,
        scope: BindingScope,
        session_id: &str,
    ) -> Result<(), CliError> {
        let _lock = FileLock::acquire_exclusive(&self.metadata_lock_path())?;
        self.unbind_scope_if_matches_fs_locked(scope, session_id)
    }

    fn unbind_scope_if_matches_fs_locked(
        &self,
        scope: BindingScope,
        session_id: &str,
    ) -> Result<(), CliError> {
        let binding_key = Self::binding_scope_key(&scope);
        let binding_path = self.binding_path(&scope);
        if self.read_bound_scope_fs_locked(&scope)?.as_deref() == Some(session_id) {
            match fs::remove_file(&binding_path) {
                Ok(_) => {}
                Err(e) => {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        return Err(CliError::unknown(
                            format!(
                                "Failed to remove process binding {}: {}",
                                binding_path.display(),
                                e
                            ),
                            "Check filesystem permissions",
                        ));
                    }
                }
            }
        }

        let mut index = self.load_binding_index_fs_locked(session_id)?;
        index.remove(&binding_key);
        self.save_binding_index_fs_locked(session_id, &index)?;
        Ok(())
    }

    fn remove_matching_bindings_in_dir_fs_locked(
        &self,
        dir: &Path,
        session_id: &str,
    ) -> Result<(), CliError> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Ok(());
                }
                return Err(CliError::unknown(
                    format!("Failed to read binding dir {}: {}", dir.display(), e),
                    "Check filesystem permissions",
                ));
            }
        };

        for entry in entries {
            let entry = entry.map_err(|e| {
                CliError::unknown(
                    format!("Failed to read process binding entry: {}", e),
                    "Check filesystem permissions",
                )
            })?;
            let path = entry.path();
            let bound = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        continue;
                    }
                    return Err(CliError::unknown(
                        format!("Failed to read process binding {}: {}", path.display(), e),
                        "Check filesystem permissions",
                    ));
                }
            };
            if bound.trim() == session_id {
                if let Err(e) = fs::remove_file(&path) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        return Err(CliError::unknown(
                            format!("Failed to remove process binding {}: {}", path.display(), e),
                            "Check filesystem permissions",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn remove_bindings_for_session_fs(&self, session_id: &str) -> Result<(), CliError> {
        let _lock = FileLock::acquire_exclusive(&self.metadata_lock_path())?;
        let bound_keys = self.load_binding_index_fs_locked(session_id)?;
        if bound_keys.is_empty() {
            self.remove_matching_bindings_in_dir_fs_locked(&self.parent_dir(), session_id)?;
            self.remove_matching_bindings_in_dir_fs_locked(&self.project_dir(), session_id)?;
            return Ok(());
        }

        for bound_key in &bound_keys {
            if let Some(scope) = Self::binding_scope_from_key(bound_key) {
                self.unbind_scope_if_matches_fs_locked(scope, session_id)?;
            }
        }

        self.save_binding_index_fs_locked(session_id, &HashSet::new())?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSessionState {
    Active,
    Terminated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSessionRecord {
    pub version: u32,
    pub session_id: String,
    pub instance: String,
    pub client: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data_dir: Option<PathBuf>,
    pub state: RuntimeSessionState,
    pub created_at: String,
    pub updated_at: String,
    pub started_by_pid: u32,
    pub started_by_ppid: u32,
    #[serde(default = "default_viewport_width")]
    pub viewport_width: u32,
    #[serde(default = "default_viewport_height")]
    pub viewport_height: u32,
    pub command_count: u64,
    pub last_command: Option<String>,
}

fn default_viewport_width() -> u32 {
    crate::types::DEFAULT_VIEWPORT_WIDTH
}

fn default_viewport_height() -> u32 {
    crate::types::DEFAULT_VIEWPORT_HEIGHT
}

impl RuntimeSessionRecord {
    pub fn new(
        session_id: String,
        instance: String,
        client: String,
        pid_path: Option<PathBuf>,
        user_data_dir: Option<PathBuf>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            version: 1,
            session_id,
            instance,
            client,
            pid_path,
            user_data_dir,
            state: RuntimeSessionState::Active,
            created_at: now.clone(),
            updated_at: now,
            started_by_pid: std::process::id(),
            started_by_ppid: parent_process_id(),
            viewport_width: default_viewport_width(),
            viewport_height: default_viewport_height(),
            command_count: 0,
            last_command: None,
        }
    }

    pub fn mark_command(&mut self, command: &str) {
        self.command_count = self.command_count.saturating_add(1);
        self.last_command = Some(command.to_string());
        self.updated_at = chrono::Utc::now().to_rfc3339();
    }

    pub fn mark_terminated(&mut self) {
        self.state = RuntimeSessionState::Terminated;
        self.updated_at = chrono::Utc::now().to_rfc3339();
    }

    pub fn viewport(&self) -> crate::types::Viewport {
        crate::types::Viewport {
            width: self.viewport_width,
            height: self.viewport_height,
        }
    }
}

pub fn generate_session_id() -> String {
    format!("sess-{}", Uuid::now_v7())
}

pub fn generate_instance_id() -> String {
    format!("inst-{}", Uuid::now_v7())
}

pub fn generate_client_id() -> String {
    format!("client-{}", Uuid::now_v7())
}

pub fn validate_identifier(value: &str, label: &str) -> Result<String, CliError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CliError::bad_input(
            format!("{} cannot be empty", label),
            "Provide a non-empty value",
        ));
    }
    let re = Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
    if !re.is_match(trimmed) {
        return Err(CliError::bad_input(
            format!("Invalid {}: \"{}\"", label, trimmed),
            "Use only letters, numbers, hyphens, and underscores",
        ));
    }
    Ok(trimmed.to_string())
}

fn resolve_env_session_id() -> Option<String> {
    std::env::var("SAURON_SESSION_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn parent_process_id() -> u32 {
    #[cfg(unix)]
    {
        nix::unistd::getppid().as_raw() as u32
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };

        let current_pid = std::process::id();
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return current_pid;
            }

            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

            let mut parent_pid = current_pid;
            if Process32FirstW(snapshot, &mut entry) != 0 {
                loop {
                    if entry.th32ProcessID == current_pid {
                        parent_pid = entry.th32ParentProcessID;
                        break;
                    }
                    if Process32NextW(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }

            CloseHandle(snapshot);
            if parent_pid == 0 {
                current_pid
            } else {
                parent_pid
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::process::id()
    }
}

pub fn resolve_active_session(
    store: &RuntimeStore,
    requested_session_id: Option<String>,
) -> Result<RuntimeSessionRecord, CliError> {
    if let Some(requested) = requested_session_id {
        let session_id = validate_identifier(&requested, "session id")?;
        let mut session = load_active_session_strict(store, &session_id)?;
        refresh_session_bindings(store, &mut session)?;
        return Ok(session);
    }

    let ppid = parent_process_id();
    if let Some(bound_session_id) = store.resolve_bound_session(ppid)? {
        if let Some(mut session) = load_active_session_if_present(store, &bound_session_id)? {
            refresh_session_bindings(store, &mut session)?;
            return Ok(session);
        }
        let _ = store.unbind_parent_process_if_matches(ppid, &bound_session_id);
    }

    if let Ok(project_key) = resolve_project_binding_key() {
        if let Some(bound_session_id) = store.resolve_project_binding(&project_key)? {
            if let Some(mut session) = load_active_session_if_present(store, &bound_session_id)? {
                refresh_session_bindings(store, &mut session)?;
                return Ok(session);
            }
            let _ = store.unbind_project_if_matches(&project_key, &bound_session_id);
        }
    }

    if let Some(env_id) = resolve_env_session_id() {
        let session_id = validate_identifier(&env_id, "session id")?;
        let mut session = load_active_session_strict(store, &session_id)?;
        refresh_session_bindings(store, &mut session)?;
        return Ok(session);
    }

    Err(CliError::session_required(
        "No active runtime session found for this project".to_string(),
        "Run 'sauron start' in this project, then rerun the command",
    ))
}

fn load_active_session_if_present(
    store: &RuntimeStore,
    session_id: &str,
) -> Result<Option<RuntimeSessionRecord>, CliError> {
    let Some(session) = store.load_session(session_id)? else {
        return Ok(None);
    };
    if session.state != RuntimeSessionState::Active {
        return Ok(None);
    }
    Ok(Some(session))
}

fn load_active_session_strict(
    store: &RuntimeStore,
    session_id: &str,
) -> Result<RuntimeSessionRecord, CliError> {
    let Some(session) = store.load_session(session_id)? else {
        return Err(CliError::session_invalid(
            format!("Session '{}' was not found", session_id),
            "Run 'sauron start' to create a fresh session",
        ));
    };

    if session.state != RuntimeSessionState::Active {
        return Err(CliError::session_terminated(
            format!("Session '{}' is terminated", session_id),
            "Run 'sauron start' to create a new active session",
        ));
    }

    Ok(session)
}

fn refresh_session_bindings(
    store: &RuntimeStore,
    session: &mut RuntimeSessionRecord,
) -> Result<(), CliError> {
    store.bind_parent_process(parent_process_id(), &session.session_id)?;
    if let Ok(project_key) = resolve_project_binding_key() {
        store.bind_project(&project_key, &session.session_id)?;
    }
    session.updated_at = chrono::Utc::now().to_rfc3339();
    store.save_session(session)?;
    Ok(())
}

pub fn resolve_project_root_path() -> Result<PathBuf, CliError> {
    let cwd = std::env::current_dir().map_err(|e| {
        CliError::unknown(
            format!("Failed to determine current directory: {}", e),
            "Run the command from an accessible working directory",
        )
    })?;
    let canonical = cwd.canonicalize().unwrap_or(cwd);
    let mut cursor = Some(canonical.as_path());
    while let Some(path) = cursor {
        if path.join(".git").exists() {
            return Ok(path.to_path_buf());
        }
        cursor = path.parent();
    }
    Ok(canonical)
}

pub fn project_binding_key_for_path(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let mut key = String::with_capacity(5 + (digest.len() * 2));
    key.push_str("proj-");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut key, "{:02x}", byte);
    }
    key
}

pub fn resolve_project_binding_key() -> Result<String, CliError> {
    let root = resolve_project_root_path()?;
    Ok(project_binding_key_for_path(&root))
}

pub fn create_session_record(
    _store: &RuntimeStore,
    requested_session_id: Option<String>,
    requested_instance: Option<String>,
    requested_client: Option<String>,
    pid_path: Option<PathBuf>,
    user_data_dir: Option<PathBuf>,
) -> Result<RuntimeSessionRecord, CliError> {
    let session_id = if let Some(id) = requested_session_id {
        validate_identifier(&id, "session id")?
    } else {
        generate_session_id()
    };

    let instance = if let Some(i) = requested_instance {
        validate_identifier(&i, "instance id")?
    } else {
        generate_instance_id()
    };
    let client = if let Some(c) = requested_client {
        validate_identifier(&c, "client id")?
    } else {
        generate_client_id()
    };

    Ok(RuntimeSessionRecord::new(
        session_id,
        instance,
        client,
        pid_path,
        user_data_dir,
    ))
}

pub fn activate_session(
    store: &RuntimeStore,
    session: &RuntimeSessionRecord,
) -> Result<(), CliError> {
    store.create_session_if_absent(session)?;
    store.bind_parent_process(parent_process_id(), &session.session_id)?;
    if let Ok(project_key) = resolve_project_binding_key() {
        store.bind_project(&project_key, &session.session_id)?;
    }
    Ok(())
}

pub fn terminate_session(
    store: &RuntimeStore,
    session: &RuntimeSessionRecord,
) -> Result<(), CliError> {
    store.delete_session(&session.session_id)?;
    store.remove_bindings_for_session(&session.session_id)?;
    Ok(())
}

pub fn session_required_error(command_name: &str) -> CliError {
    CliError::new(
        ErrorCode::SessionRequired,
        format!(
            "Command '{}' requires an active runtime session, but none was found",
            command_name
        ),
        "Run 'sauron start' in this project, then rerun the command",
        false,
        6,
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupStats {
    pub instances: usize,
    pub sessions: usize,
    pub logs: usize,
}

struct StaleSessionCandidate {
    path: PathBuf,
    session_id: Option<String>,
}

struct SessionCleanupPlan {
    active_instances: HashSet<String>,
    stale_sessions: Vec<StaleSessionCandidate>,
}

fn cleanup_warning(message: String) {
    eprintln!("Cleanup warning: {}", message);
}

fn session_pid_path_for_cleanup(base_dir: &Path, session: &RuntimeSessionRecord) -> PathBuf {
    session.pid_path.clone().unwrap_or_else(|| {
        base_dir
            .join("instances")
            .join(&session.instance)
            .join("chrome.pid")
    })
}

fn session_has_live_pid_for_cleanup(base_dir: &Path, session: &RuntimeSessionRecord) -> bool {
    let pid_path = session_pid_path_for_cleanup(base_dir, session);
    let text = match fs::read_to_string(&pid_path) {
        Ok(text) => text,
        Err(_) => return false,
    };
    let pid_data = match serde_json::from_str::<PidFileData>(&text) {
        Ok(pid_data) => pid_data,
        Err(_) => return false,
    };
    daemon::is_process_alive(pid_data.pid)
}

fn is_stale_session_for_cleanup(base_dir: &Path, session: &RuntimeSessionRecord) -> bool {
    if session.state != RuntimeSessionState::Active {
        return true;
    }

    let instance_dir = base_dir.join("instances").join(&session.instance);
    if !instance_dir.exists() {
        return true;
    }

    !session_has_live_pid_for_cleanup(base_dir, session)
}

fn derive_session_id_from_file(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string())
}

fn build_session_cleanup_plan(base_dir: &Path) -> SessionCleanupPlan {
    let sessions_dir = base_dir.join("runtime").join("sessions");
    let entries = match fs::read_dir(&sessions_dir) {
        Ok(entries) => entries,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                cleanup_warning(format!(
                    "Failed to read session directory {}: {}",
                    sessions_dir.display(),
                    e
                ));
            }
            return SessionCleanupPlan {
                active_instances: HashSet::new(),
                stale_sessions: Vec::new(),
            };
        }
    };

    let mut active_instances = HashSet::new();
    let mut stale_sessions = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                cleanup_warning(format!("Failed to read session entry: {}", e));
                continue;
            }
        };
        let path = entry.path();

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(e) => {
                cleanup_warning(format!(
                    "Failed to inspect session entry {}: {}",
                    path.display(),
                    e
                ));
                continue;
            }
        };
        if !file_type.is_file() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                cleanup_warning(format!(
                    "Failed to read session file {}: {}",
                    path.display(),
                    e
                ));
                continue;
            }
        };

        let session = match serde_json::from_str::<RuntimeSessionRecord>(&text) {
            Ok(session) => session,
            Err(e) => {
                let session_id = derive_session_id_from_file(&path);
                cleanup_warning(format!(
                    "Failed to parse session file {}: {}; removing as stale",
                    path.display(),
                    e
                ));
                stale_sessions.push(StaleSessionCandidate { path, session_id });
                continue;
            }
        };

        if is_stale_session_for_cleanup(base_dir, &session) {
            stale_sessions.push(StaleSessionCandidate {
                path,
                session_id: Some(session.session_id),
            });
            continue;
        }

        active_instances.insert(session.instance);
    }

    SessionCleanupPlan {
        active_instances,
        stale_sessions,
    }
}

fn remove_file_if_exists(path: &Path, what: &str) -> bool {
    match fs::remove_file(path) {
        Ok(_) => true,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                cleanup_warning(format!(
                    "Failed to remove {} {}: {}",
                    what,
                    path.display(),
                    e
                ));
            }
            false
        }
    }
}

fn cleanup_orphaned_instances(
    base_dir: &Path,
    active_instances: &HashSet<String>,
    remove_orphaned_dirs: bool,
) -> usize {
    let instances_dir = base_dir.join("instances");
    let entries = match fs::read_dir(&instances_dir) {
        Ok(entries) => entries,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                cleanup_warning(format!(
                    "Failed to read instances directory {}: {}",
                    instances_dir.display(),
                    e
                ));
            }
            return 0;
        }
    };

    let mut removed = 0usize;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                cleanup_warning(format!("Failed to read instance entry: {}", e));
                continue;
            }
        };

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(e) => {
                cleanup_warning(format!(
                    "Failed to inspect instance entry {}: {}",
                    entry.path().display(),
                    e
                ));
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }

        let instance_dir = entry.path();
        let instance_id = entry.file_name().to_string_lossy().to_string();
        let pid_path = instance_dir.join("chrome.pid");
        let mut has_live_process = false;
        let mut remove_pid_file = false;

        match fs::read_to_string(&pid_path) {
            Ok(text) => match serde_json::from_str::<PidFileData>(&text) {
                Ok(pid_data) => {
                    if daemon::is_process_alive(pid_data.pid) {
                        has_live_process = true;
                    } else {
                        remove_pid_file = true;
                    }
                }
                Err(_) => {
                    remove_pid_file = true;
                }
            },
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    cleanup_warning(format!(
                        "Failed to read pid file {}: {}",
                        pid_path.display(),
                        e
                    ));
                }
            }
        }

        if remove_pid_file {
            let _ = remove_file_if_exists(&pid_path, "stale pid file");
        }

        if has_live_process || active_instances.contains(instance_id.as_str()) {
            continue;
        }
        if !remove_orphaned_dirs {
            continue;
        }

        match fs::remove_dir_all(&instance_dir) {
            Ok(_) => removed = removed.saturating_add(1),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    cleanup_warning(format!(
                        "Failed to remove orphaned instance directory {}: {}",
                        instance_dir.display(),
                        e
                    ));
                }
            }
        }
    }

    removed
}

fn remove_matching_bindings(dir: &Path, session_id: &str) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                cleanup_warning(format!(
                    "Failed to read bindings directory {}: {}",
                    dir.display(),
                    e
                ));
            }
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                cleanup_warning(format!("Failed to read binding entry: {}", e));
                continue;
            }
        };
        let path = entry.path();

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(e) => {
                cleanup_warning(format!(
                    "Failed to inspect binding entry {}: {}",
                    path.display(),
                    e
                ));
                continue;
            }
        };
        if !file_type.is_file() {
            continue;
        }

        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    cleanup_warning(format!(
                        "Failed to read binding file {}: {}",
                        path.display(),
                        e
                    ));
                }
                continue;
            }
        };

        if text.trim() == session_id {
            let _ = remove_file_if_exists(&path, "binding file");
        }
    }
}

fn remove_session_bindings(base_dir: &Path, session_id: &str) {
    let runtime_dir = base_dir.join("runtime");
    let binding_index_path = runtime_dir
        .join("session-bindings")
        .join(format!("{}.json", session_id));
    let _ = remove_file_if_exists(&binding_index_path, "session binding index");

    remove_matching_bindings(&runtime_dir.join("ppid"), session_id);
    remove_matching_bindings(&runtime_dir.join("project"), session_id);
}

fn remove_stale_sessions(base_dir: &Path, stale_sessions: Vec<StaleSessionCandidate>) -> usize {
    let mut removed = 0usize;

    for stale in stale_sessions {
        let removed_file = remove_file_if_exists(&stale.path, "stale session file");
        if let Some(session_id) = stale.session_id.as_deref() {
            remove_session_bindings(base_dir, session_id);
        }
        if removed_file {
            removed = removed.saturating_add(1);
        }
    }

    removed
}

fn load_existing_session_ids(base_dir: &Path) -> Option<HashSet<String>> {
    let sessions_dir = base_dir.join("runtime").join("sessions");
    let entries = match fs::read_dir(&sessions_dir) {
        Ok(entries) => entries,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return Some(HashSet::new());
            }
            cleanup_warning(format!(
                "Failed to read sessions directory {}: {}",
                sessions_dir.display(),
                e
            ));
            return None;
        }
    };

    let mut session_ids = HashSet::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                cleanup_warning(format!("Failed to read session entry: {}", e));
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Some(session_id) = derive_session_id_from_file(&path) {
            session_ids.insert(session_id);
        }
    }
    Some(session_ids)
}

fn cleanup_orphaned_logs(base_dir: &Path) -> usize {
    let Some(session_ids) = load_existing_session_ids(base_dir) else {
        return 0;
    };

    let logs_dir = base_dir.join("runtime").join("logs");
    let entries = match fs::read_dir(&logs_dir) {
        Ok(entries) => entries,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                cleanup_warning(format!(
                    "Failed to read log directory {}: {}",
                    logs_dir.display(),
                    e
                ));
            }
            return 0;
        }
    };

    let mut removed = 0usize;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                cleanup_warning(format!("Failed to read log entry: {}", e));
                continue;
            }
        };
        let path = entry.path();

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(e) => {
                cleanup_warning(format!(
                    "Failed to inspect log entry {}: {}",
                    path.display(),
                    e
                ));
                continue;
            }
        };
        if !file_type.is_file() {
            continue;
        }

        let extension = path.extension().and_then(|ext| ext.to_str());
        if extension != Some("ndjson") && extension != Some("lock") {
            continue;
        }

        let Some(session_id) = derive_session_id_from_file(&path) else {
            continue;
        };
        if session_ids.contains(session_id.as_str()) {
            continue;
        }

        if remove_file_if_exists(&path, "orphaned log file") {
            removed = removed.saturating_add(1);
        }
    }

    removed
}

pub fn cleanup_stale_state(base_dir: &Path) -> Result<CleanupStats, CliError> {
    let mut stats = CleanupStats::default();
    let session_plan = build_session_cleanup_plan(base_dir);

    stats.instances = cleanup_orphaned_instances(base_dir, &session_plan.active_instances, true);
    stats.sessions = remove_stale_sessions(base_dir, session_plan.stale_sessions);
    stats.logs = cleanup_orphaned_logs(base_dir);

    Ok(stats)
}

pub fn cleanup_session_state(
    base_dir: &Path,
    session: &RuntimeSessionRecord,
) -> Result<(), CliError> {
    let instance_dir = base_dir.join("instances").join(&session.instance);
    let client_dir = instance_dir.join("clients").join(&session.client);

    if client_dir.exists() {
        fs::remove_dir_all(&client_dir).map_err(|e| {
            CliError::unknown(
                format!(
                    "Failed to remove client state {}: {}",
                    client_dir.display(),
                    e
                ),
                "Check filesystem permissions",
            )
        })?;
    }

    let clients_dir = instance_dir.join("clients");
    match fs::read_dir(&clients_dir) {
        Ok(mut it) => {
            if it.next().is_none() {
                match fs::remove_dir(&clients_dir) {
                    Ok(_) => {}
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            return Err(CliError::unknown(
                                format!(
                                    "Failed to remove clients dir {}: {}",
                                    clients_dir.display(),
                                    e
                                ),
                                "Check filesystem permissions",
                            ));
                        }
                    }
                }
            }
        }
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(CliError::unknown(
                    format!(
                        "Failed to inspect clients dir {}: {}",
                        clients_dir.display(),
                        e
                    ),
                    "Check filesystem permissions",
                ));
            }
        }
    }

    if let Err(e) = fs::remove_dir(&instance_dir) {
        if e.kind() != std::io::ErrorKind::NotFound
            && e.kind() != std::io::ErrorKind::DirectoryNotEmpty
        {
            return Err(CliError::unknown(
                format!(
                    "Failed to remove instance dir {}: {}",
                    instance_dir.display(),
                    e
                ),
                "Check filesystem permissions",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct CurrentDirGuard {
        previous: PathBuf,
    }

    impl CurrentDirGuard {
        fn change_to(path: &Path) -> Self {
            let previous = std::env::current_dir().expect("cwd should exist");
            std::env::set_current_dir(path).expect("failed to set cwd");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    fn unique_test_home() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("sauron-runtime-test-{}", nanos))
    }

    fn seed_session(store: &RuntimeStore, session_id: &str) -> RuntimeSessionRecord {
        let session = RuntimeSessionRecord::new(
            session_id.to_string(),
            format!("inst-{}", session_id),
            format!("client-{}", session_id),
            None,
            None,
        );
        store
            .create_session_if_absent(&session)
            .expect("session should be created");
        session
    }

    fn write_json<T: serde::Serialize>(path: &Path, value: &T) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent directory should be created");
        }
        let data = serde_json::to_vec(value).expect("json serialization should succeed");
        std::fs::write(path, data).expect("file write should succeed");
    }

    fn write_text(path: &Path, value: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent directory should be created");
        }
        std::fs::write(path, value).expect("file write should succeed");
    }

    fn exited_child_pid() -> u32 {
        #[cfg(unix)]
        let mut child = Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("child process should spawn");
        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/C", "exit 0"])
            .spawn()
            .expect("child process should spawn");

        let pid = child.id();
        child.wait().expect("child process should exit");
        assert!(
            !crate::daemon::is_process_alive(pid),
            "expected exited child pid {} to be dead",
            pid
        );
        pid
    }

    #[test]
    fn generated_ids_use_expected_prefixes() {
        let session = generate_session_id();
        let instance = generate_instance_id();
        let client = generate_client_id();
        assert!(session.starts_with("sess-"));
        assert!(instance.starts_with("inst-"));
        assert!(client.starts_with("client-"));
    }

    #[test]
    fn filesystem_store_round_trips_session_and_binding() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);

        let store = RuntimeStore::new().expect("store should initialize");
        let session = RuntimeSessionRecord::new(
            "sess-test".to_string(),
            "inst-test".to_string(),
            "client-test".to_string(),
            None,
            None,
        );
        store
            .create_session_if_absent(&session)
            .expect("session should save");
        store
            .bind_parent_process(123456, &session.session_id)
            .expect("binding should save");

        let loaded = store
            .load_session("sess-test")
            .expect("load should succeed")
            .expect("session should exist");
        assert_eq!(loaded.instance, "inst-test");
        assert_eq!(
            store.resolve_bound_session(123456).expect("binding lookup"),
            Some("sess-test".to_string())
        );

        store
            .remove_bindings_for_session("sess-test")
            .expect("binding cleanup");
        assert_eq!(
            store.resolve_bound_session(123456).expect("binding lookup"),
            None
        );

        std::env::remove_var("SAURON_HOME");
    }

    #[test]
    fn resolve_active_session_precedence_is_parent_then_project_then_env() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);
        std::env::remove_var("SAURON_SESSION_ID");

        let project_root = home.join("project");
        let nested = project_root.join("nested");
        std::fs::create_dir_all(project_root.join(".git")).expect("failed to create .git");
        std::fs::create_dir_all(&nested).expect("failed to create nested dir");
        let _cwd_guard = CurrentDirGuard::change_to(&nested);

        let store = RuntimeStore::new().expect("store should initialize");
        let ppid = parent_process_id();
        let project_key = resolve_project_binding_key().expect("project key should resolve");

        seed_session(&store, "sess-project");
        seed_session(&store, "sess-parent");
        seed_session(&store, "sess-env");

        store
            .bind_project(&project_key, "sess-project")
            .expect("project binding should save");
        store
            .bind_parent_process(ppid, "sess-parent")
            .expect("parent binding should save");
        std::env::set_var("SAURON_SESSION_ID", "sess-env");

        let parent_selected =
            resolve_active_session(&store, None).expect("parent binding should resolve first");
        assert_eq!(parent_selected.session_id, "sess-parent");

        store
            .bind_project(&project_key, "sess-project")
            .expect("project binding should be reset");
        store
            .bind_parent_process(ppid, "sess-parent")
            .expect("parent binding should be reset");
        store
            .unbind_parent_process_if_matches(ppid, "sess-parent")
            .expect("parent unbind should succeed");
        let project_selected =
            resolve_active_session(&store, None).expect("project binding should resolve second");
        assert_eq!(project_selected.session_id, "sess-project");

        store
            .unbind_project_if_matches(&project_key, "sess-project")
            .expect("project unbind should succeed");
        store
            .unbind_parent_process_if_matches(ppid, "sess-project")
            .expect("parent unbind should succeed");
        let env_selected =
            resolve_active_session(&store, None).expect("env fallback should resolve last");
        assert_eq!(env_selected.session_id, "sess-env");

        std::env::remove_var("SAURON_SESSION_ID");
        std::env::remove_var("SAURON_HOME");
    }

    #[test]
    fn resolve_active_session_recovers_from_stale_project_binding() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);
        std::env::set_var("SAURON_SESSION_ID", "sess-env");

        let project_root = home.join("project");
        std::fs::create_dir_all(project_root.join(".git")).expect("failed to create .git");
        let _cwd_guard = CurrentDirGuard::change_to(&project_root);

        let store = RuntimeStore::new().expect("store should initialize");
        let ppid = parent_process_id();
        let project_key = resolve_project_binding_key().expect("project key should resolve");
        seed_session(&store, "sess-env");

        seed_session(&store, "sess-stale");
        store
            .bind_project(&project_key, "sess-stale")
            .expect("stale project binding should save");
        store
            .delete_session("sess-stale")
            .expect("stale session record should be removed");

        store
            .unbind_parent_process_if_matches(ppid, "sess-stale")
            .expect("stale parent binding cleanup should be safe");

        let resolved = resolve_active_session(&store, None).expect("resolver should recover");
        assert_eq!(resolved.session_id, "sess-env");
        assert_eq!(
            store
                .resolve_project_binding(&project_key)
                .expect("project binding lookup should succeed"),
            Some("sess-env".to_string())
        );

        std::env::remove_var("SAURON_SESSION_ID");
        std::env::remove_var("SAURON_HOME");
    }

    #[test]
    fn save_session_requires_existing_record() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);

        let store = RuntimeStore::new().expect("store should initialize");
        let mut session = seed_session(&store, "sess-existing");
        store
            .delete_session("sess-existing")
            .expect("session should be deleted");

        session.mark_command("status");
        let err = store
            .save_session(&session)
            .expect_err("save should fail when session no longer exists");
        assert!(matches!(err.code, ErrorCode::SessionInvalid));

        std::env::remove_var("SAURON_HOME");
    }

    #[test]
    fn terminate_session_removes_project_and_parent_bindings() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);

        let project_root = home.join("project");
        std::fs::create_dir_all(project_root.join(".git")).expect("failed to create .git");
        let _cwd_guard = CurrentDirGuard::change_to(&project_root);

        let store = RuntimeStore::new().expect("store should initialize");
        let ppid = parent_process_id();
        let project_key = resolve_project_binding_key().expect("project key should resolve");
        let session = seed_session(&store, "sess-cleanup");

        store
            .bind_project(&project_key, &session.session_id)
            .expect("project binding should save");
        store
            .bind_parent_process(ppid, &session.session_id)
            .expect("parent binding should save");

        terminate_session(&store, &session).expect("terminate should clean runtime state");

        assert!(store
            .load_session(&session.session_id)
            .expect("load should succeed")
            .is_none());
        assert_eq!(
            store.resolve_bound_session(ppid).expect("binding lookup"),
            None
        );
        assert_eq!(
            store
                .resolve_project_binding(&project_key)
                .expect("project binding lookup"),
            None
        );

        std::env::remove_var("SAURON_HOME");
    }

    #[test]
    fn cleanup_stale_state_removes_orphaned_instance_with_dead_pid() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);

        let instance_dir = home.join("instances").join("inst-stale");
        std::fs::create_dir_all(instance_dir.join("clients").join("client-a"))
            .expect("client directory should exist");
        std::fs::create_dir_all(instance_dir.join("chrome-data"))
            .expect("chrome-data directory should exist");
        write_text(
            instance_dir
                .join("clients")
                .join("client-a")
                .join("refs.json")
                .as_path(),
            "{}",
        );
        write_json(
            &instance_dir.join("chrome.pid"),
            &PidFileData {
                pid: exited_child_pid(),
                port: 9222,
                xvfb_pid: None,
                display: None,
            },
        );

        let stats = cleanup_stale_state(&home).expect("cleanup should succeed");
        assert_eq!(stats.instances, 1);
        assert!(!instance_dir.exists());

        std::env::remove_var("SAURON_HOME");
    }

    #[test]
    fn cleanup_stale_state_preserves_instance_with_live_pid() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);

        let instance_dir = home.join("instances").join("inst-live");
        std::fs::create_dir_all(instance_dir.join("clients").join("client-a"))
            .expect("client directory should exist");
        write_json(
            &instance_dir.join("chrome.pid"),
            &PidFileData {
                pid: std::process::id(),
                port: 9222,
                xvfb_pid: None,
                display: None,
            },
        );

        let stats = cleanup_stale_state(&home).expect("cleanup should succeed");
        assert_eq!(stats.instances, 0);
        assert!(instance_dir.exists());
        assert!(instance_dir.join("chrome.pid").exists());

        std::env::remove_var("SAURON_HOME");
    }

    #[test]
    fn cleanup_stale_state_removes_orphaned_session_and_bindings() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);

        let runtime_dir = home.join("runtime");
        let mut session = RuntimeSessionRecord::new(
            "sess-orphan".to_string(),
            "inst-orphan".to_string(),
            "client-orphan".to_string(),
            None,
            None,
        );
        session.mark_terminated();

        write_json(
            &runtime_dir.join("sessions").join("sess-orphan.json"),
            &session,
        );
        write_json(
            &runtime_dir
                .join("session-bindings")
                .join("sess-orphan.json"),
            &vec!["project:proj-123".to_string()],
        );
        write_text(
            &runtime_dir.join("project").join("proj-123.txt"),
            "sess-orphan",
        );
        write_text(&runtime_dir.join("ppid").join("5555.txt"), "sess-orphan");

        let stats = cleanup_stale_state(&home).expect("cleanup should succeed");
        assert_eq!(stats.sessions, 1);
        assert!(!runtime_dir
            .join("sessions")
            .join("sess-orphan.json")
            .exists());
        assert!(!runtime_dir
            .join("session-bindings")
            .join("sess-orphan.json")
            .exists());
        assert!(!runtime_dir.join("project").join("proj-123.txt").exists());
        assert!(!runtime_dir.join("ppid").join("5555.txt").exists());

        std::env::remove_var("SAURON_HOME");
    }

    #[test]
    fn cleanup_stale_state_removes_malformed_session_file() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);

        let bad_session = home.join("runtime").join("sessions").join("sess-bad.json");
        write_text(&bad_session, "{not-json");

        let stats = cleanup_stale_state(&home).expect("cleanup should succeed");
        assert_eq!(stats.sessions, 1);
        assert!(!bad_session.exists());

        std::env::remove_var("SAURON_HOME");
    }

    #[test]
    fn cleanup_stale_state_removes_logs_for_missing_sessions() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let home = unique_test_home();
        std::env::set_var("SAURON_HOME", &home);

        let runtime_dir = home.join("runtime");
        let live_instance = home.join("instances").join("inst-live");
        std::fs::create_dir_all(&live_instance).expect("live instance should exist");
        write_json(
            &live_instance.join("chrome.pid"),
            &PidFileData {
                pid: std::process::id(),
                port: 9333,
                xvfb_pid: None,
                display: None,
            },
        );

        let live_session = RuntimeSessionRecord::new(
            "sess-live".to_string(),
            "inst-live".to_string(),
            "client-live".to_string(),
            None,
            None,
        );
        write_json(
            &runtime_dir.join("sessions").join("sess-live.json"),
            &live_session,
        );

        write_text(&runtime_dir.join("logs").join("sess-live.ndjson"), "{}\n");
        write_text(&runtime_dir.join("logs").join("sess-gone.ndjson"), "{}\n");

        let stats = cleanup_stale_state(&home).expect("cleanup should succeed");
        assert_eq!(stats.logs, 1);
        assert!(runtime_dir.join("logs").join("sess-live.ndjson").exists());
        assert!(!runtime_dir.join("logs").join("sess-gone.ndjson").exists());

        std::env::remove_var("SAURON_HOME");
    }
}
