use crate::context::{atomic_write, resolve_base_dir, FileLock};
use crate::errors::CliError;
use crate::types::ErrorCode;
use clap::ValueEnum;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const DEFAULT_TTL_SECONDS: u64 = 86_400;
const KEY_PREFIX: &str = "sauron:runtime";

const BINDING_KIND_PARENT: &str = "ppid";
const BINDING_KIND_PROJECT: &str = "project";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SessionStoreKind {
    Filesystem,
    Valkey,
}

#[derive(Debug, Clone)]
pub struct RuntimeStore {
    kind: SessionStoreKind,
    base_dir: PathBuf,
    valkey_url: Option<String>,
    ttl_seconds: u64,
}

#[derive(Debug, Clone)]
enum BindingScope {
    ParentProcess(u32),
    Project(String),
}

impl RuntimeStore {
    pub fn new(
        kind: SessionStoreKind,
        valkey_url: Option<String>,
        ttl_seconds: Option<u64>,
    ) -> Result<Self, CliError> {
        let base_dir = resolve_base_dir()?;
        let ttl = ttl_seconds.unwrap_or(DEFAULT_TTL_SECONDS).max(30);
        Ok(Self {
            kind,
            base_dir,
            valkey_url,
            ttl_seconds: ttl,
        })
    }

    pub fn kind(&self) -> SessionStoreKind {
        self.kind
    }

    pub fn save_session(&self, session: &RuntimeSessionRecord) -> Result<(), CliError> {
        match self.kind {
            SessionStoreKind::Filesystem => self.save_session_fs(session),
            SessionStoreKind::Valkey => self.save_session_valkey(session),
        }
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
        match self.kind {
            SessionStoreKind::Filesystem => self.mark_session_command_fs(session_id, command),
            SessionStoreKind::Valkey => self.mark_session_command_valkey(session_id, command),
        }
    }

    pub fn create_session_if_absent(&self, session: &RuntimeSessionRecord) -> Result<(), CliError> {
        match self.kind {
            SessionStoreKind::Filesystem => self.create_session_if_absent_fs(session),
            SessionStoreKind::Valkey => self.create_session_if_absent_valkey(session),
        }
    }

    pub fn load_session(&self, session_id: &str) -> Result<Option<RuntimeSessionRecord>, CliError> {
        validate_identifier(session_id, "session id")?;
        match self.kind {
            SessionStoreKind::Filesystem => self.load_session_fs(session_id),
            SessionStoreKind::Valkey => self.load_session_valkey(session_id),
        }
    }

    pub fn delete_session(&self, session_id: &str) -> Result<(), CliError> {
        validate_identifier(session_id, "session id")?;
        match self.kind {
            SessionStoreKind::Filesystem => self.delete_session_fs(session_id),
            SessionStoreKind::Valkey => self.delete_session_valkey(session_id),
        }
    }

    pub fn bind_parent_process(&self, parent_pid: u32, session_id: &str) -> Result<(), CliError> {
        validate_identifier(session_id, "session id")?;
        match self.kind {
            SessionStoreKind::Filesystem => {
                self.bind_scope_fs(BindingScope::ParentProcess(parent_pid), session_id)
            }
            SessionStoreKind::Valkey => {
                self.bind_scope_valkey(BindingScope::ParentProcess(parent_pid), session_id)
            }
        }
    }

    pub fn bind_project(&self, project_key: &str, session_id: &str) -> Result<(), CliError> {
        validate_identifier(project_key, "project key")?;
        validate_identifier(session_id, "session id")?;
        match self.kind {
            SessionStoreKind::Filesystem => {
                self.bind_scope_fs(BindingScope::Project(project_key.to_string()), session_id)
            }
            SessionStoreKind::Valkey => {
                self.bind_scope_valkey(BindingScope::Project(project_key.to_string()), session_id)
            }
        }
    }

    pub fn resolve_bound_session(&self, parent_pid: u32) -> Result<Option<String>, CliError> {
        match self.kind {
            SessionStoreKind::Filesystem => {
                self.resolve_bound_scope_fs(BindingScope::ParentProcess(parent_pid))
            }
            SessionStoreKind::Valkey => {
                self.resolve_bound_scope_valkey(BindingScope::ParentProcess(parent_pid))
            }
        }
    }

    pub fn resolve_project_binding(&self, project_key: &str) -> Result<Option<String>, CliError> {
        validate_identifier(project_key, "project key")?;
        match self.kind {
            SessionStoreKind::Filesystem => {
                self.resolve_bound_scope_fs(BindingScope::Project(project_key.to_string()))
            }
            SessionStoreKind::Valkey => {
                self.resolve_bound_scope_valkey(BindingScope::Project(project_key.to_string()))
            }
        }
    }

    pub fn unbind_parent_process_if_matches(
        &self,
        parent_pid: u32,
        session_id: &str,
    ) -> Result<(), CliError> {
        validate_identifier(session_id, "session id")?;
        match self.kind {
            SessionStoreKind::Filesystem => {
                self.unbind_scope_if_matches_fs(BindingScope::ParentProcess(parent_pid), session_id)
            }
            SessionStoreKind::Valkey => self.unbind_scope_if_matches_valkey(
                BindingScope::ParentProcess(parent_pid),
                session_id,
            ),
        }
    }

    pub fn unbind_project_if_matches(
        &self,
        project_key: &str,
        session_id: &str,
    ) -> Result<(), CliError> {
        validate_identifier(project_key, "project key")?;
        validate_identifier(session_id, "session id")?;
        match self.kind {
            SessionStoreKind::Filesystem => self.unbind_scope_if_matches_fs(
                BindingScope::Project(project_key.to_string()),
                session_id,
            ),
            SessionStoreKind::Valkey => self.unbind_scope_if_matches_valkey(
                BindingScope::Project(project_key.to_string()),
                session_id,
            ),
        }
    }

    pub fn remove_bindings_for_session(&self, session_id: &str) -> Result<(), CliError> {
        validate_identifier(session_id, "session id")?;
        match self.kind {
            SessionStoreKind::Filesystem => self.remove_bindings_for_session_fs(session_id),
            SessionStoreKind::Valkey => self.remove_bindings_for_session_valkey(session_id),
        }
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

    fn valkey_url(&self) -> Result<String, CliError> {
        if let Some(v) = &self.valkey_url {
            if !v.trim().is_empty() {
                return Ok(v.clone());
            }
        }
        if let Ok(v) = std::env::var("SAURON_VALKEY_URL") {
            if !v.trim().is_empty() {
                return Ok(v);
            }
        }
        Err(CliError::bad_input(
            "Valkey backend selected but no URL was provided",
            "Set --valkey-url or SAURON_VALKEY_URL (for example redis://127.0.0.1:6379/)",
        ))
    }

    fn with_valkey_connection<T, F>(&self, f: F) -> Result<T, CliError>
    where
        F: FnOnce(&mut redis::Connection) -> Result<T, redis::RedisError>,
    {
        let url = self.valkey_url()?;
        let client = redis::Client::open(url.clone()).map_err(|e| {
            CliError::unknown(
                format!("Failed to create Valkey client for {}: {}", url, e),
                "Check the Valkey URL format and availability",
            )
        })?;
        let mut conn = client.get_connection().map_err(|e| {
            CliError::unknown(
                format!("Failed to connect to Valkey at {}: {}", url, e),
                "Ensure the Valkey server is reachable",
            )
        })?;
        f(&mut conn).map_err(|e| {
            CliError::unknown(
                format!("Valkey operation failed: {}", e),
                "Ensure Valkey is running and accessible",
            )
        })
    }

    fn key_session(session_id: &str) -> String {
        format!("{}:session:{}", KEY_PREFIX, session_id)
    }

    fn key_parent(parent_pid: u32) -> String {
        format!("{}:ppid:{}", KEY_PREFIX, parent_pid)
    }

    fn key_project(project_key: &str) -> String {
        format!(
            "{}:bind:{}:{}",
            KEY_PREFIX, BINDING_KIND_PROJECT, project_key
        )
    }

    fn key_session_binding_index(session_id: &str) -> String {
        format!("{}:idx:session:{}:bindings", KEY_PREFIX, session_id)
    }

    fn key_session_binding_index_prefix() -> String {
        format!("{}:idx:session:", KEY_PREFIX)
    }

    fn valkey_binding_key(scope: &BindingScope) -> String {
        match scope {
            BindingScope::ParentProcess(pid) => Self::key_parent(*pid),
            BindingScope::Project(project_key) => Self::key_project(project_key),
        }
    }

    fn save_session_valkey(&self, session: &RuntimeSessionRecord) -> Result<(), CliError> {
        let session_key = Self::key_session(&session.session_id);
        let payload = serde_json::to_string(session).map_err(|e| {
            CliError::unknown(
                format!("Failed to serialize runtime session for Valkey: {}", e),
                "This should not happen",
            )
        })?;
        let ttl = self.ttl_seconds;
        self.with_valkey_connection(|conn| {
            let set_result: Option<String> = redis::cmd("SET")
                .arg(&session_key)
                .arg(payload)
                .arg("XX")
                .arg("EX")
                .arg(ttl)
                .query(conn)?;
            if set_result.is_none() {
                return Err(redis::RedisError::from((
                    redis::ErrorKind::TypeError,
                    "session missing",
                )));
            }
            Ok(())
        })
        .map_err(|e| {
            if e.message.contains("session missing") {
                CliError::session_invalid(
                    format!("Session '{}' was not found", session.session_id),
                    "Run 'sauron start' to create a fresh session",
                )
            } else {
                e
            }
        })
    }

    fn mark_session_command_valkey(
        &self,
        session_id: &str,
        command: &str,
    ) -> Result<RuntimeSessionRecord, CliError> {
        let session_key = Self::key_session(session_id);
        let updated_at = chrono::Utc::now().to_rfc3339();
        let ttl = self.ttl_seconds;
        let mark_script = redis::Script::new(
            r#"
                local raw = redis.call('GET', KEYS[1])
                if not raw then
                    return redis.error_reply('SESSION_MISSING')
                end
                local session = cjson.decode(raw)
                if session['state'] ~= 'active' then
                    return redis.error_reply('SESSION_TERMINATED')
                end
                local count = tonumber(session['commandCount']) or 0
                session['commandCount'] = count + 1
                session['lastCommand'] = ARGV[1]
                session['updatedAt'] = ARGV[2]
                local payload = cjson.encode(session)
                redis.call('SET', KEYS[1], payload, 'EX', ARGV[3])
                return payload
            "#,
        );
        self.with_valkey_connection(|conn| {
            mark_script
                .key(&session_key)
                .arg(command)
                .arg(&updated_at)
                .arg(ttl as i64)
                .invoke::<String>(conn)
        })
        .map_err(|e| {
            if e.message.contains("SESSION_MISSING") {
                CliError::session_invalid(
                    format!("Session '{}' was not found", session_id),
                    "Run 'sauron start' to create a fresh session",
                )
            } else if e.message.contains("SESSION_TERMINATED") {
                CliError::session_terminated(
                    format!("Session '{}' is terminated", session_id),
                    "Run 'sauron start' to create a new active session",
                )
            } else {
                e
            }
        })
        .and_then(|payload| {
            serde_json::from_str::<RuntimeSessionRecord>(&payload).map_err(|e| {
                CliError::unknown(
                    format!("Failed to parse updated Valkey session record: {}", e),
                    "The runtime session data may be corrupted",
                )
            })
        })
    }

    fn create_session_if_absent_valkey(
        &self,
        session: &RuntimeSessionRecord,
    ) -> Result<(), CliError> {
        let session_key = Self::key_session(&session.session_id);
        let payload = serde_json::to_string(session).map_err(|e| {
            CliError::unknown(
                format!("Failed to serialize runtime session for Valkey: {}", e),
                "This should not happen",
            )
        })?;
        let ttl = self.ttl_seconds;
        self.with_valkey_connection(|conn| {
            let set_result: Option<String> = redis::cmd("SET")
                .arg(&session_key)
                .arg(payload)
                .arg("NX")
                .arg("EX")
                .arg(ttl)
                .query(conn)?;
            if set_result.is_none() {
                return Err(redis::RedisError::from((
                    redis::ErrorKind::BusyLoadingError,
                    "session exists",
                )));
            }
            Ok(())
        })
        .map_err(|e| {
            if e.message.contains("session exists") {
                CliError::new(
                    ErrorCode::SessionConflict,
                    format!("Session '{}' already exists", session.session_id),
                    "Use a different --session-id or omit it to auto-generate one",
                    false,
                    6,
                )
            } else {
                e
            }
        })
    }

    fn load_session_valkey(
        &self,
        session_id: &str,
    ) -> Result<Option<RuntimeSessionRecord>, CliError> {
        let session_key = Self::key_session(session_id);
        self.with_valkey_connection(|conn| {
            let raw: Option<String> = redis::cmd("GET").arg(&session_key).query(conn)?;
            match raw {
                Some(text) => {
                    let parsed =
                        serde_json::from_str::<RuntimeSessionRecord>(&text).map_err(|_| {
                            redis::RedisError::from((redis::ErrorKind::TypeError, "invalid json"))
                        })?;
                    Ok(Some(parsed))
                }
                None => Ok(None),
            }
        })
    }

    fn delete_session_valkey(&self, session_id: &str) -> Result<(), CliError> {
        let session_key = Self::key_session(session_id);
        self.with_valkey_connection(|conn| {
            redis::cmd("DEL").arg(session_key).query::<i64>(conn)?;
            Ok(())
        })
    }

    fn bind_scope_valkey(&self, scope: BindingScope, session_id: &str) -> Result<(), CliError> {
        let session_key = Self::key_session(session_id);
        let binding_key = Self::valkey_binding_key(&scope);
        let index_key = Self::key_session_binding_index(session_id);
        let index_prefix = Self::key_session_binding_index_prefix();
        let ttl = self.ttl_seconds;
        let bind_script = redis::Script::new(
            r#"
                if redis.call('EXISTS', KEYS[1]) == 0 then
                    return redis.error_reply('SESSION_MISSING')
                end
                local old_sid = redis.call('GET', KEYS[2])
                if old_sid and old_sid ~= ARGV[1] then
                    redis.call('SREM', ARGV[3] .. old_sid .. ':bindings', KEYS[2])
                end
                redis.call('SET', KEYS[2], ARGV[1], 'EX', ARGV[2])
                redis.call('SADD', KEYS[3], KEYS[2])
                redis.call('EXPIRE', KEYS[3], ARGV[2])
                return 1
            "#,
        );

        self.with_valkey_connection(|conn| {
            bind_script
                .key(session_key)
                .key(binding_key)
                .key(index_key)
                .arg(session_id)
                .arg(ttl as i64)
                .arg(index_prefix)
                .invoke::<i64>(conn)?;
            Ok(())
        })
        .map_err(|e| {
            if e.message.contains("SESSION_MISSING") {
                CliError::session_invalid(
                    format!("Session '{}' was not found", session_id),
                    "Run 'sauron start' to create a fresh session",
                )
            } else {
                e
            }
        })
    }

    fn resolve_bound_scope_valkey(&self, scope: BindingScope) -> Result<Option<String>, CliError> {
        let binding_key = Self::valkey_binding_key(&scope);
        self.with_valkey_connection(|conn| {
            let raw: Option<String> = redis::cmd("GET").arg(binding_key).query(conn)?;
            Ok(raw)
        })
    }

    fn unbind_scope_if_matches_valkey(
        &self,
        scope: BindingScope,
        session_id: &str,
    ) -> Result<(), CliError> {
        let binding_key = Self::valkey_binding_key(&scope);
        let index_key = Self::key_session_binding_index(session_id);
        let unbind_script = redis::Script::new(
            r#"
                if redis.call('GET', KEYS[1]) == ARGV[1] then
                    redis.call('DEL', KEYS[1])
                end
                redis.call('SREM', KEYS[2], KEYS[1])
                if redis.call('SCARD', KEYS[2]) == 0 then
                    redis.call('DEL', KEYS[2])
                end
                return 1
            "#,
        );
        self.with_valkey_connection(|conn| {
            unbind_script
                .key(binding_key)
                .key(index_key)
                .arg(session_id)
                .invoke::<i64>(conn)?;
            Ok(())
        })
    }

    fn remove_bindings_for_session_valkey(&self, session_id: &str) -> Result<(), CliError> {
        let index_key = Self::key_session_binding_index(session_id);
        let cleanup_script = redis::Script::new(
            r#"
                local keys = redis.call('SMEMBERS', KEYS[1])
                local removed = 0
                for _, binding_key in ipairs(keys) do
                    if redis.call('GET', binding_key) == ARGV[1] then
                        redis.call('DEL', binding_key)
                        removed = removed + 1
                    end
                    redis.call('SREM', KEYS[1], binding_key)
                end
                redis.call('DEL', KEYS[1])
                return removed
            "#,
        );
        self.with_valkey_connection(|conn| {
            cleanup_script
                .key(index_key)
                .arg(session_id)
                .invoke::<i64>(conn)?;
            Ok(())
        })
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
    pub command_count: u64,
    pub last_command: Option<String>,
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

        let store = RuntimeStore::new(SessionStoreKind::Filesystem, None, Some(60))
            .expect("store should initialize");
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

        let store = RuntimeStore::new(SessionStoreKind::Filesystem, None, Some(60))
            .expect("store should initialize");
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

        let store = RuntimeStore::new(SessionStoreKind::Filesystem, None, Some(60))
            .expect("store should initialize");
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

        let store = RuntimeStore::new(SessionStoreKind::Filesystem, None, Some(60))
            .expect("store should initialize");
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

        let store = RuntimeStore::new(SessionStoreKind::Filesystem, None, Some(60))
            .expect("store should initialize");
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
}
