use crate::browser::{PageClient, SemanticLocatorKind};
use crate::context::AppContext;
use crate::errors::CliError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetSpec {
    Ref(String),
    Css(String),
    Text {
        text: String,
        exact: bool,
        nth: Option<u32>,
    },
    Role {
        role: String,
        name: Option<String>,
        nth: Option<u32>,
    },
    Label {
        text: String,
        nth: Option<u32>,
    },
    Placeholder {
        text: String,
        nth: Option<u32>,
    },
    AltText {
        text: String,
        nth: Option<u32>,
    },
    Title {
        text: String,
        nth: Option<u32>,
    },
    TestId {
        value: String,
        nth: Option<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

impl ResolvedTarget {
    pub fn from_strategy(strategy: &str, backend_node_id: u64, candidate_count: u32) -> Self {
        Self {
            strategy: strategy.to_string(),
            confidence: if candidate_count <= 1 { 1.0 } else { 0.6 },
            frame_id: None,
            backend_node_id: Some(backend_node_id),
            ref_id: None,
            summary_role: None,
            summary_name: None,
            candidate_count,
        }
    }
}

pub async fn resolve_target(
    page: &PageClient,
    ctx: &AppContext,
    spec: &TargetSpec,
) -> Result<(u64, ResolvedTarget), CliError> {
    match spec {
        TargetSpec::Ref(reference) => {
            let normalized = reference.trim_start_matches('@');
            let backend = page
                .resolve_target_backend_node_id(ctx, &format!("@{}", normalized), None)
                .await?;
            let mut resolved = ResolvedTarget::from_strategy("ref", backend, 1);
            resolved.ref_id = Some(format!("@{}", normalized));
            Ok((backend, resolved))
        }
        TargetSpec::Css(selector) => {
            let backend = page
                .resolve_selector_backend_node_id(selector, None)
                .await?;
            Ok((
                backend,
                ResolvedTarget::from_strategy("selector", backend, 1),
            ))
        }
        TargetSpec::Text { text, nth, .. } => {
            let backend = page.resolve_target_backend_node_id(ctx, text, *nth).await?;
            let candidate_count = page
                .count_semantic_matches(SemanticLocatorKind::Text, text, None)
                .await
                .unwrap_or(1);
            Ok((
                backend,
                ResolvedTarget::from_strategy("text", backend, candidate_count),
            ))
        }
        TargetSpec::Role { role, name, nth } => {
            let (backend, candidate_count) = page
                .resolve_semantic_backend_node_id(
                    SemanticLocatorKind::Role,
                    role,
                    name.as_deref(),
                    *nth,
                )
                .await?;
            Ok((
                backend,
                ResolvedTarget::from_strategy("role", backend, candidate_count),
            ))
        }
        TargetSpec::Label { text, nth } => {
            let (backend, candidate_count) = page
                .resolve_semantic_backend_node_id(SemanticLocatorKind::Label, text, None, *nth)
                .await?;
            Ok((
                backend,
                ResolvedTarget::from_strategy("label", backend, candidate_count),
            ))
        }
        TargetSpec::Placeholder { text, nth } => {
            let (backend, candidate_count) = page
                .resolve_semantic_backend_node_id(
                    SemanticLocatorKind::Placeholder,
                    text,
                    None,
                    *nth,
                )
                .await?;
            Ok((
                backend,
                ResolvedTarget::from_strategy("placeholder", backend, candidate_count),
            ))
        }
        TargetSpec::AltText { text, nth } => {
            let (backend, candidate_count) = page
                .resolve_semantic_backend_node_id(SemanticLocatorKind::AltText, text, None, *nth)
                .await?;
            Ok((
                backend,
                ResolvedTarget::from_strategy("altText", backend, candidate_count),
            ))
        }
        TargetSpec::Title { text, nth } => {
            let (backend, candidate_count) = page
                .resolve_semantic_backend_node_id(SemanticLocatorKind::Title, text, None, *nth)
                .await?;
            Ok((
                backend,
                ResolvedTarget::from_strategy("title", backend, candidate_count),
            ))
        }
        TargetSpec::TestId { value, nth } => {
            let (backend, candidate_count) = page
                .resolve_semantic_backend_node_id(SemanticLocatorKind::TestId, value, None, *nth)
                .await?;
            Ok((
                backend,
                ResolvedTarget::from_strategy("testId", backend, candidate_count),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_target_confidence_is_lower_for_ambiguous_matches() {
        let stable = ResolvedTarget::from_strategy("text", 1, 1);
        assert!((stable.confidence - 1.0).abs() < f32::EPSILON);

        let ambiguous = ResolvedTarget::from_strategy("text", 1, 3);
        assert!(ambiguous.confidence < 1.0);
    }
}
