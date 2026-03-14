use crate::types::{SnapshotOptions, SnapshotRef, SnapshotResult};
use std::collections::HashMap;

// --- AX node model (subset) ---

#[derive(Debug, Clone)]
pub enum AxState {
    True,
    Mixed,
}

#[derive(Debug, Clone)]
pub struct AxNode {
    pub role: String,
    pub name: Option<String>,
    pub children: Vec<AxNode>,

    // Optional properties used in snapshot rendering
    pub level: Option<i64>,
    pub disabled: bool,
    pub expanded: Option<bool>,
    pub checked: Option<AxState>,
    pub selected: bool,
    pub required: bool,
    pub focused: bool,
    pub pressed: Option<AxState>,
    pub value: Option<String>,
    pub url: Option<String>,

    // Used for later ref resolution
    pub backend_dom_node_id: Option<u64>,
}

// --- Role classifications ---

pub fn is_interactive_role(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "textbox"
            | "searchbox"
            | "checkbox"
            | "radio"
            | "combobox"
            | "listbox"
            | "option"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "switch"
            | "tab"
            | "treeitem"
            | "slider"
            | "spinbutton"
    )
}

fn is_named_content_role(role: &str) -> bool {
    matches!(
        role,
        "heading"
            | "cell"
            | "gridcell"
            | "columnheader"
            | "rowheader"
            | "listitem"
            | "article"
            | "region"
            | "img"
            | "alert"
            | "status"
            | "progressbar"
            | "meter"
    )
}

fn is_structural_role(role: &str) -> bool {
    matches!(
        role,
        "generic"
            | "group"
            | "list"
            | "table"
            | "row"
            | "rowgroup"
            | "document"
            | "RootWebArea"
            | "WebArea"
            | "none"
            | "presentation"
            | "directory"
            | "toolbar"
            | "tablist"
            | "menu"
            | "menubar"
            | "tree"
            | "grid"
            | "treegrid"
            | "application"
    )
}

// --- Ref assignment ---

pub fn should_assign_ref(role: &str, name: Option<&str>) -> bool {
    if is_structural_role(role) {
        return false;
    }
    if is_interactive_role(role) {
        return true;
    }
    if is_named_content_role(role) {
        if let Some(n) = name {
            if !n.is_empty() {
                return true;
            }
        }
    }
    false
}

// --- Locator construction ---

fn escape_aria_name(name: &str) -> String {
    // Match TS escaping rules: \\, ", [, ], (, )
    name.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

pub fn build_locator(role: &str, name: Option<&str>, nth_index: u32, total_count: u32) -> String {
    let base = if let Some(n) = name {
        if !n.is_empty() {
            let escaped = escape_aria_name(n);
            format!("::-p-aria({}[role=\"{}\"])", escaped, role)
        } else {
            format!("::-p-aria([role=\"{}\"])", role)
        }
    } else {
        format!("::-p-aria([role=\"{}\"])", role)
    };

    if total_count > 1 {
        format!("{} >> nth={}", base, nth_index)
    } else {
        base
    }
}

fn role_name_key(role: &str, name: Option<&str>) -> String {
    format!("{}::{}", role, name.unwrap_or(""))
}

fn count_role_names(node: &AxNode, counts: &mut HashMap<String, u32>) {
    if should_assign_ref(&node.role, node.name.as_deref()) {
        let key = role_name_key(&node.role, node.name.as_deref());
        *counts.entry(key).or_insert(0) += 1;
    }
    for child in &node.children {
        count_role_names(child, counts);
    }
}

fn serialize_props(node: &AxNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(level) = node.level {
        parts.push(format!("level={}", level));
    }
    if node.disabled {
        parts.push("disabled".to_string());
    }
    if let Some(expanded) = node.expanded {
        parts.push(format!("expanded={}", expanded));
    }
    if let Some(checked) = &node.checked {
        match checked {
            AxState::Mixed => parts.push("checked=mixed".to_string()),
            AxState::True => parts.push("checked".to_string()),
        }
    }
    if node.selected {
        parts.push("selected".to_string());
    }
    if node.required {
        parts.push("required".to_string());
    }
    if node.focused {
        parts.push("focused".to_string());
    }
    if let Some(pressed) = &node.pressed {
        match pressed {
            AxState::Mixed => parts.push("pressed=mixed".to_string()),
            AxState::True => parts.push("pressed".to_string()),
        }
    }
    if let Some(v) = &node.value {
        parts.push(format!("value=\"{}\"", v));
    }
    if let Some(url) = &node.url {
        if node.name.as_deref() != Some(url.as_str()) {
            parts.push(format!("url={}", url));
        }
    }

    if parts.is_empty() {
        "".to_string()
    } else {
        format!(" [{}]", parts.join(", "))
    }
}

#[allow(clippy::too_many_arguments)]
fn serialize_node(
    node: &AxNode,
    depth: usize,
    opts: &SnapshotOptions,
    refs: &mut HashMap<String, SnapshotRef>,
    lines: &mut Vec<String>,
    counter: &mut u64,
    role_counts: &HashMap<String, u32>,
    role_indices: &mut HashMap<String, u32>,
) {
    let is_interactive = is_interactive_role(&node.role);
    let role_lower = node.role.to_ascii_lowercase();
    let scoped_match = opts
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|scope| {
            let scope_lower = scope.to_ascii_lowercase();
            role_lower.contains(&scope_lower)
                || node
                    .name
                    .as_deref()
                    .map(|name| name.to_ascii_lowercase().contains(&scope_lower))
                    .unwrap_or(false)
        })
        .unwrap_or(true);

    // interactive-only mode: skip non-interactive nodes but recurse into children at the same depth
    if opts.interactive && !is_interactive {
        for child in &node.children {
            serialize_node(
                child,
                depth,
                opts,
                refs,
                lines,
                counter,
                role_counts,
                role_indices,
            );
        }
        return;
    }
    // clickable mode narrows output to actionable nodes while preserving traversal.
    if opts.clickable && !is_interactive {
        for child in &node.children {
            serialize_node(
                child,
                depth,
                opts,
                refs,
                lines,
                counter,
                role_counts,
                role_indices,
            );
        }
        return;
    }
    if !opts.include_iframes && role_lower.contains("iframe") {
        return;
    }
    if !scoped_match {
        for child in &node.children {
            serialize_node(
                child,
                depth,
                opts,
                refs,
                lines,
                counter,
                role_counts,
                role_indices,
            );
        }
        return;
    }

    // Assign ref if warranted
    let mut ref_id: Option<String> = None;
    if should_assign_ref(&node.role, node.name.as_deref()) {
        *counter += 1;
        let id = format!("e{}", counter);
        ref_id = Some(id.clone());

        let key = role_name_key(&node.role, node.name.as_deref());
        let total = *role_counts.get(&key).unwrap_or(&1);
        let nth = role_indices.get(&key).copied().unwrap_or(0);
        role_indices.insert(key, nth + 1);

        let locator = build_locator(&node.role, node.name.as_deref(), nth, total);
        refs.insert(
            id,
            SnapshotRef {
                role: node.role.clone(),
                name: node.name.clone(),
                locator,
            },
        );
    }

    let indent = "  ".repeat(depth);
    let name_part = node
        .name
        .as_deref()
        .filter(|n| !n.is_empty())
        .map(|n| format!(" \"{}\"", n))
        .unwrap_or_default();
    let ref_part = ref_id
        .as_deref()
        .map(|id| format!(" @{}", id))
        .unwrap_or_default();

    let prop_part = serialize_props(node);

    lines.push(format!(
        "{}- {}{}{}{}",
        indent, node.role, name_part, ref_part, prop_part
    ));

    for child in &node.children {
        serialize_node(
            child,
            depth + 1,
            opts,
            refs,
            lines,
            counter,
            role_counts,
            role_indices,
        );
    }
}

pub fn serialize_tree(
    root: &AxNode,
    opts: SnapshotOptions,
    snapshot_id: u64,
    url: String,
) -> SnapshotResult {
    let mut refs: HashMap<String, SnapshotRef> = HashMap::new();
    let mut lines: Vec<String> = Vec::new();
    let mut counter: u64 = 0;

    let mut role_counts: HashMap<String, u32> = HashMap::new();
    count_role_names(root, &mut role_counts);

    let mut role_indices: HashMap<String, u32> = HashMap::new();

    serialize_node(
        root,
        0,
        &opts,
        &mut refs,
        &mut lines,
        &mut counter,
        &role_counts,
        &mut role_indices,
    );

    SnapshotResult {
        tree: lines.join("\n"),
        refs,
        url,
        snapshot_id,
    }
}
