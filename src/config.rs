use crate::context::resolve_base_dir;
use crate::errors::CliError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileConfig {
    pub session: Option<String>,
    pub session_id: Option<String>,
    pub instance: Option<String>,
    pub client: Option<String>,
    pub port: Option<u16>,
    pub pid_path: Option<PathBuf>,
    pub profile: Option<PathBuf>,
    pub user_data_dir: Option<PathBuf>,
    pub timeout_ms: Option<u64>,
    pub viewport: Option<String>,
    pub ensure_runtime: Option<String>,
    pub json: Option<bool>,
    pub policy: Option<String>,
    pub allow_host: Option<Vec<String>>,
    pub allow_origin: Option<Vec<String>>,
    pub allow_action: Option<Vec<String>>,
    pub artifact_mode: Option<String>,
    pub max_bytes: Option<u64>,
    pub redact: Option<bool>,
    pub content_boundaries: Option<bool>,
}

impl FileConfig {
    pub fn merged(self, override_layer: FileConfig) -> Self {
        Self {
            session: override_layer.session.or(self.session),
            session_id: override_layer.session_id.or(self.session_id),
            instance: override_layer.instance.or(self.instance),
            client: override_layer.client.or(self.client),
            port: override_layer.port.or(self.port),
            pid_path: override_layer.pid_path.or(self.pid_path),
            profile: override_layer.profile.or(self.profile),
            user_data_dir: override_layer.user_data_dir.or(self.user_data_dir),
            timeout_ms: override_layer.timeout_ms.or(self.timeout_ms),
            viewport: override_layer.viewport.or(self.viewport),
            ensure_runtime: override_layer.ensure_runtime.or(self.ensure_runtime),
            json: override_layer.json.or(self.json),
            policy: override_layer.policy.or(self.policy),
            allow_host: override_layer.allow_host.or(self.allow_host),
            allow_origin: override_layer.allow_origin.or(self.allow_origin),
            allow_action: override_layer.allow_action.or(self.allow_action),
            artifact_mode: override_layer.artifact_mode.or(self.artifact_mode),
            max_bytes: override_layer.max_bytes.or(self.max_bytes),
            redact: override_layer.redact.or(self.redact),
            content_boundaries: override_layer
                .content_boundaries
                .or(self.content_boundaries),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigLayers {
    pub user_path: PathBuf,
    pub project_path: PathBuf,
    pub user: Option<FileConfig>,
    pub project: Option<FileConfig>,
}

impl ConfigLayers {
    pub fn merged_file_config(&self) -> FileConfig {
        let base = self.user.clone().unwrap_or_default();
        base.merged(self.project.clone().unwrap_or_default())
    }
}

pub fn user_config_path() -> Result<PathBuf, CliError> {
    Ok(resolve_base_dir()?.join("config.json"))
}

pub fn project_config_path() -> Result<PathBuf, CliError> {
    let cwd = std::env::current_dir().map_err(|e| {
        CliError::unknown(
            format!("Failed to resolve current directory: {}", e),
            "Run the command from an accessible directory",
        )
    })?;
    Ok(cwd.join("sauron.json"))
}

pub fn load_config_layers() -> Result<ConfigLayers, CliError> {
    let user_path = user_config_path()?;
    let project_path = project_config_path()?;
    let user = load_optional_config_file(&user_path)?;
    let project = load_optional_config_file(&project_path)?;
    Ok(ConfigLayers {
        user_path,
        project_path,
        user,
        project,
    })
}

fn load_optional_config_file(path: &Path) -> Result<Option<FileConfig>, CliError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(CliError::unknown(
                format!("Failed to read config file {}: {}", path.display(), e),
                "Check filesystem permissions",
            ));
        }
    };

    let parsed = serde_json::from_str::<FileConfig>(&text).map_err(|e| {
        CliError::bad_input(
            format!("Invalid JSON in config file {}: {}", path.display(), e),
            "Fix the JSON syntax or remove the invalid config file",
        )
    })?;
    Ok(Some(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;

    #[test]
    fn merged_file_config_prefers_project_over_user() {
        let layers = ConfigLayers {
            user_path: PathBuf::from("/tmp/user.json"),
            project_path: PathBuf::from("/tmp/project.json"),
            user: Some(FileConfig {
                session: Some("user".to_string()),
                timeout_ms: Some(1000),
                ..FileConfig::default()
            }),
            project: Some(FileConfig {
                session: Some("project".to_string()),
                timeout_ms: None,
                ..FileConfig::default()
            }),
        };

        let merged = layers.merged_file_config();
        assert_eq!(merged.session.as_deref(), Some("project"));
        assert_eq!(merged.timeout_ms, Some(1000));
    }

    #[test]
    fn load_config_layers_reads_user_and_project_files() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let base = std::env::temp_dir().join(format!("sauron-config-test-{}", nanos));
        let home = base.join("home");
        let repo = base.join("repo");
        std::fs::create_dir_all(&home).expect("home dir");
        std::fs::create_dir_all(&repo).expect("repo dir");

        let old_cwd = std::env::current_dir().expect("cwd");
        std::env::set_var("SAURON_HOME", &home);
        std::env::set_current_dir(&repo).expect("set cwd");

        std::fs::write(home.join("config.json"), r#"{"session":"home-session"}"#)
            .expect("write home config");
        std::fs::write(repo.join("sauron.json"), r#"{"session":"project-session"}"#)
            .expect("write project config");

        let layers = load_config_layers().expect("load layers");
        assert_eq!(
            layers.user.as_ref().and_then(|cfg| cfg.session.as_deref()),
            Some("home-session")
        );
        assert_eq!(
            layers
                .project
                .as_ref()
                .and_then(|cfg| cfg.session.as_deref()),
            Some("project-session")
        );

        std::env::set_current_dir(old_cwd).expect("restore cwd");
        std::env::remove_var("SAURON_HOME");
        let _ = std::fs::remove_dir_all(base);
    }
}
