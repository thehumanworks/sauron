use crate::errors::CliError;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "kebab_case")]
pub enum ArtifactMode {
    Inline,
    Path,
    #[default]
    Manifest,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRef {
    pub kind: String,
    pub mime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactManifest {
    pub items: Vec<ArtifactRef>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactWriteResult {
    pub reference: ArtifactRef,
    pub bytes: Vec<u8>,
}

pub fn decode_base64(data: &str) -> Result<Vec<u8>, CliError> {
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data).map_err(|e| {
        CliError::unknown(
            format!("Invalid base64 artifact payload: {}", e),
            "Retry the command; if the issue persists, capture a new artifact",
        )
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hash = hasher.finalize();
    format!("{:x}", hash)
}

pub fn artifact_root(base_dir: &Path, session_id: &str) -> PathBuf {
    base_dir.join("runtime").join("artifacts").join(session_id)
}

pub fn write_artifact(
    root: &Path,
    kind: &str,
    extension: &str,
    bytes: &[u8],
) -> Result<PathBuf, CliError> {
    std::fs::create_dir_all(root).map_err(|e| {
        CliError::unknown(
            format!(
                "Failed to create artifact directory {}: {}",
                root.display(),
                e
            ),
            "Check filesystem permissions",
        )
    })?;

    let ts = chrono::Utc::now().timestamp_millis();
    let file_name = format!("{}-{}.{}", kind, ts, extension);
    let path = root.join(file_name);

    std::fs::write(&path, bytes).map_err(|e| {
        CliError::unknown(
            format!("Failed to write artifact {}: {}", path.display(), e),
            "Check filesystem permissions",
        )
    })?;

    Ok(path)
}

pub fn write_screenshot_artifact(
    base_dir: &Path,
    session_id: &str,
    mode: ArtifactMode,
    mime: &str,
    extension: &str,
    data_base64: &str,
    explicit_path: Option<&Path>,
    annotations: Option<Value>,
) -> Result<ArtifactWriteResult, CliError> {
    let bytes = decode_base64(data_base64)?;
    let sha = sha256_hex(&bytes);

    let path = if matches!(mode, ArtifactMode::Inline | ArtifactMode::None) {
        explicit_path.map(PathBuf::from)
    } else {
        match explicit_path {
            Some(path) => Some(path.to_path_buf()),
            None => Some(write_artifact(
                &artifact_root(base_dir, session_id),
                "screenshot",
                extension,
                &bytes,
            )?),
        }
    };

    if let Some(path) = &path {
        if !matches!(mode, ArtifactMode::Inline | ArtifactMode::None) || explicit_path.is_some() {
            std::fs::write(path, &bytes).map_err(|e| {
                CliError::unknown(
                    format!("Failed to write artifact {}: {}", path.display(), e),
                    "Check filesystem permissions",
                )
            })?;
        }
    }

    let reference = ArtifactRef {
        kind: "screenshot".to_string(),
        mime: mime.to_string(),
        path: path.as_ref().map(|p| p.to_string_lossy().to_string()),
        inline_data: if matches!(mode, ArtifactMode::Inline) {
            Some(data_base64.to_string())
        } else {
            None
        },
        bytes: Some(bytes.len() as u64),
        sha256: Some(sha),
        annotations,
    };

    Ok(ArtifactWriteResult { reference, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_is_stable() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
