use crate::errors::CliError;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "kebab_case")]
pub enum PolicyMode {
    Safe,
    Confirm,
    #[default]
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    Navigate,
    Read,
    Interact,
    Download,
    Dialog,
    Script,
    StateWrite,
}

impl ActionClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Navigate => "navigate",
            Self::Read => "read",
            Self::Interact => "interact",
            Self::Download => "download",
            Self::Dialog => "dialog",
            Self::Script => "script",
            Self::StateWrite => "state-write",
        }
    }

    pub fn is_mutating(self) -> bool {
        matches!(
            self,
            Self::Interact | Self::Download | Self::Dialog | Self::Script | Self::StateWrite
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecisionKind {
    Allow,
    Deny,
    Confirm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDecision {
    pub decision: PolicyDecisionKind,
    pub reason: String,
    pub matched_rules: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct PolicyFile {
    pub allow_hosts: Vec<String>,
    pub deny_hosts: Vec<String>,
    pub allow_origins: Vec<String>,
    pub deny_origins: Vec<String>,
    pub allow_actions: Vec<String>,
    pub deny_actions: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PolicyInputs {
    pub mode: PolicyMode,
    pub allow_hosts: Vec<String>,
    pub allow_origins: Vec<String>,
    pub allow_actions: Vec<String>,
    pub policy_file: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct EffectivePolicy {
    pub mode: PolicyMode,
    pub allow_hosts: HashSet<String>,
    pub deny_hosts: HashSet<String>,
    pub allow_origins: HashSet<String>,
    pub deny_origins: HashSet<String>,
    pub allow_actions: HashSet<String>,
    pub deny_actions: HashSet<String>,
}

impl EffectivePolicy {
    pub fn evaluate(&self, action: ActionClass, url: Option<&str>) -> PolicyDecision {
        let mut matched: Vec<String> = Vec::new();
        let action_name = action.as_str().to_string();

        if self.deny_actions.contains(&action_name) {
            matched.push(format!("deny_action:{}", action_name));
            return PolicyDecision {
                decision: PolicyDecisionKind::Deny,
                reason: "Action denied by policy rule".to_string(),
                matched_rules: matched,
                confirmation_id: None,
            };
        }

        let host = url.and_then(extract_host);
        let origin = url.and_then(extract_origin);

        if let Some(host) = host.as_ref() {
            if self.deny_hosts.contains(host) {
                matched.push(format!("deny_host:{}", host));
                return PolicyDecision {
                    decision: PolicyDecisionKind::Deny,
                    reason: "Host denied by policy rule".to_string(),
                    matched_rules: matched,
                    confirmation_id: None,
                };
            }
        }

        if let Some(origin) = origin.as_ref() {
            if self.deny_origins.contains(origin) {
                matched.push(format!("deny_origin:{}", origin));
                return PolicyDecision {
                    decision: PolicyDecisionKind::Deny,
                    reason: "Origin denied by policy rule".to_string(),
                    matched_rules: matched,
                    confirmation_id: None,
                };
            }
        }

        if !self.allow_actions.is_empty() && !self.allow_actions.contains(&action_name) {
            matched.push(format!("allow_action_missing:{}", action_name));
            return PolicyDecision {
                decision: PolicyDecisionKind::Deny,
                reason: "Action not allowlisted by policy".to_string(),
                matched_rules: matched,
                confirmation_id: None,
            };
        }

        if !self.allow_hosts.is_empty() {
            match host {
                Some(ref host) if self.allow_hosts.contains(host) => {
                    matched.push(format!("allow_host:{}", host));
                }
                Some(host) => {
                    matched.push(format!("allow_host_missing:{}", host));
                    return PolicyDecision {
                        decision: PolicyDecisionKind::Deny,
                        reason: "Host is not allowlisted by policy".to_string(),
                        matched_rules: matched,
                        confirmation_id: None,
                    };
                }
                None => {
                    matched.push("allow_host_missing:<unknown>".to_string());
                    return PolicyDecision {
                        decision: PolicyDecisionKind::Deny,
                        reason: "Action has no URL context for host allowlist".to_string(),
                        matched_rules: matched,
                        confirmation_id: None,
                    };
                }
            }
        }

        if !self.allow_origins.is_empty() {
            match origin {
                Some(ref origin) if self.allow_origins.contains(origin) => {
                    matched.push(format!("allow_origin:{}", origin));
                }
                Some(origin) => {
                    matched.push(format!("allow_origin_missing:{}", origin));
                    return PolicyDecision {
                        decision: PolicyDecisionKind::Deny,
                        reason: "Origin is not allowlisted by policy".to_string(),
                        matched_rules: matched,
                        confirmation_id: None,
                    };
                }
                None => {
                    matched.push("allow_origin_missing:<unknown>".to_string());
                    return PolicyDecision {
                        decision: PolicyDecisionKind::Deny,
                        reason: "Action has no URL context for origin allowlist".to_string(),
                        matched_rules: matched,
                        confirmation_id: None,
                    };
                }
            }
        }

        match self.mode {
            PolicyMode::Open => PolicyDecision {
                decision: PolicyDecisionKind::Allow,
                reason: "Policy mode open allows this action".to_string(),
                matched_rules: matched,
                confirmation_id: None,
            },
            PolicyMode::Confirm if action.is_mutating() => PolicyDecision {
                decision: PolicyDecisionKind::Confirm,
                reason: "Mutating action requires confirmation in confirm mode".to_string(),
                matched_rules: matched,
                confirmation_id: Some(format!("confirm-{}", uuid::Uuid::now_v7())),
            },
            PolicyMode::Safe if action.is_mutating() => PolicyDecision {
                decision: PolicyDecisionKind::Deny,
                reason: "Mutating action denied in safe mode unless explicitly allowlisted"
                    .to_string(),
                matched_rules: matched,
                confirmation_id: None,
            },
            PolicyMode::Safe
                if matches!(action, ActionClass::Navigate) && self.allow_hosts.is_empty() =>
            {
                PolicyDecision {
                    decision: PolicyDecisionKind::Deny,
                    reason: "Navigation requires --allow-host in safe mode".to_string(),
                    matched_rules: matched,
                    confirmation_id: None,
                }
            }
            _ => PolicyDecision {
                decision: PolicyDecisionKind::Allow,
                reason: "Action allowed by policy".to_string(),
                matched_rules: matched,
                confirmation_id: None,
            },
        }
    }
}

pub fn build_policy(inputs: PolicyInputs) -> Result<EffectivePolicy, CliError> {
    let mut effective = EffectivePolicy {
        mode: inputs.mode,
        ..EffectivePolicy::default()
    };

    if let Some(path) = inputs.policy_file {
        let file = load_policy_file(&path)?;
        merge_policy_file(&mut effective, file);
    }

    for host in inputs.allow_hosts {
        effective.allow_hosts.insert(host.to_ascii_lowercase());
    }
    for origin in inputs.allow_origins {
        effective.allow_origins.insert(origin);
    }
    for action in inputs.allow_actions {
        effective.allow_actions.insert(action.to_ascii_lowercase());
    }

    Ok(effective)
}

fn merge_policy_file(effective: &mut EffectivePolicy, file: PolicyFile) {
    for host in file.allow_hosts {
        effective.allow_hosts.insert(host.to_ascii_lowercase());
    }
    for host in file.deny_hosts {
        effective.deny_hosts.insert(host.to_ascii_lowercase());
    }
    for origin in file.allow_origins {
        effective.allow_origins.insert(origin);
    }
    for origin in file.deny_origins {
        effective.deny_origins.insert(origin);
    }
    for action in file.allow_actions {
        effective.allow_actions.insert(action.to_ascii_lowercase());
    }
    for action in file.deny_actions {
        effective.deny_actions.insert(action.to_ascii_lowercase());
    }
}

fn load_policy_file(path: &Path) -> Result<PolicyFile, CliError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        CliError::unknown(
            format!("Failed to read policy file {}: {}", path.display(), e),
            "Check filesystem permissions",
        )
    })?;

    serde_json::from_str(&text).map_err(|e| {
        CliError::bad_input(
            format!("Invalid policy file JSON {}: {}", path.display(), e),
            "Fix the JSON syntax in the policy file",
        )
    })
}

pub fn extract_host(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
}

pub fn extract_origin(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .map(|u| u.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_mode_blocks_navigate_without_allow_host() {
        let policy = build_policy(PolicyInputs {
            mode: PolicyMode::Safe,
            ..PolicyInputs::default()
        })
        .expect("policy");
        let decision = policy.evaluate(ActionClass::Navigate, Some("https://example.com"));
        assert!(matches!(decision.decision, PolicyDecisionKind::Deny));
    }

    #[test]
    fn safe_mode_allows_allowlisted_navigation() {
        let policy = build_policy(PolicyInputs {
            mode: PolicyMode::Safe,
            allow_hosts: vec!["example.com".to_string()],
            ..PolicyInputs::default()
        })
        .expect("policy");
        let decision = policy.evaluate(ActionClass::Navigate, Some("https://example.com"));
        assert!(matches!(decision.decision, PolicyDecisionKind::Allow));
    }
}
