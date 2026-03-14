use crate::snapshot::{is_interactive_role, AxNode, AxState};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotNode {
    pub id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stable_selector: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
}

pub fn flatten_snapshot_nodes(
    root: &AxNode,
    clickable_only: bool,
    scope: Option<&str>,
) -> Vec<SnapshotNode> {
    let mut out = Vec::new();
    let mut counter: u64 = 0;
    let normalized_scope = scope
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase());
    flatten_node(
        root,
        clickable_only,
        normalized_scope.as_deref(),
        &mut counter,
        &mut out,
    );
    out
}

fn flatten_node(
    node: &AxNode,
    clickable_only: bool,
    scope: Option<&str>,
    counter: &mut u64,
    out: &mut Vec<SnapshotNode>,
) {
    let interactive = is_interactive_role(&node.role);
    let in_scope = scope
        .map(|needle| {
            node.name
                .as_deref()
                .map(|value| value.to_ascii_lowercase().contains(needle))
                .unwrap_or(false)
                || node.role.to_ascii_lowercase().contains(needle)
        })
        .unwrap_or(true);

    if (!clickable_only || interactive) && in_scope {
        *counter += 1;
        let id = format!("n{}", counter);
        out.push(SnapshotNode {
            id,
            role: node.role.clone(),
            name: node.name.clone(),
            value: node.value.clone(),
            states: collect_states(node),
            frame_id: None,
            bounds: None,
            stable_selector: node
                .name
                .as_deref()
                .filter(|name| !name.is_empty())
                .map(|name| format!("{}[name=\"{}\"]", node.role, name)),
            actions: infer_actions(node),
        });
    }

    for child in &node.children {
        flatten_node(child, clickable_only, scope, counter, out);
    }
}

fn collect_states(node: &AxNode) -> Vec<String> {
    let mut states = Vec::new();
    if node.disabled {
        states.push("disabled".to_string());
    }
    if node.selected {
        states.push("selected".to_string());
    }
    if node.required {
        states.push("required".to_string());
    }
    if node.focused {
        states.push("focused".to_string());
    }
    if let Some(expanded) = node.expanded {
        states.push(format!("expanded={}", expanded));
    }
    if let Some(checked) = &node.checked {
        match checked {
            AxState::True => states.push("checked=true".to_string()),
            AxState::Mixed => states.push("checked=mixed".to_string()),
        }
    }
    states
}

fn infer_actions(node: &AxNode) -> Vec<String> {
    match node.role.as_str() {
        "button" | "link" => vec!["click".to_string()],
        "textbox" | "searchbox" => vec!["fill".to_string(), "focus".to_string()],
        "checkbox" | "radio" => vec!["check".to_string(), "uncheck".to_string()],
        "combobox" | "listbox" => vec!["select".to_string()],
        _ if is_interactive_role(&node.role) => vec!["interact".to_string()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_snapshot_nodes_applies_clickable_filter() {
        let root = AxNode {
            role: "RootWebArea".to_string(),
            name: None,
            children: vec![
                AxNode {
                    role: "button".to_string(),
                    name: Some("Submit".to_string()),
                    children: vec![],
                    level: None,
                    disabled: false,
                    expanded: None,
                    checked: None,
                    selected: false,
                    required: false,
                    focused: false,
                    pressed: None,
                    value: None,
                    url: None,
                    backend_dom_node_id: Some(1),
                },
                AxNode {
                    role: "heading".to_string(),
                    name: Some("Title".to_string()),
                    children: vec![],
                    level: Some(1),
                    disabled: false,
                    expanded: None,
                    checked: None,
                    selected: false,
                    required: false,
                    focused: false,
                    pressed: None,
                    value: None,
                    url: None,
                    backend_dom_node_id: Some(2),
                },
            ],
            level: None,
            disabled: false,
            expanded: None,
            checked: None,
            selected: false,
            required: false,
            focused: false,
            pressed: None,
            value: None,
            url: None,
            backend_dom_node_id: None,
        };

        let nodes = flatten_snapshot_nodes(&root, true, None);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].role, "button");
    }
}
