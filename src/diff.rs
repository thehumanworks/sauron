use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct DiffResult {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: usize,
    pub unified: String,
}

/// Line-by-line diff of two snapshot texts.
///
/// This intentionally mirrors the simple set-based diff used by the TS version.
pub fn diff_snapshots(before: &str, after: &str) -> DiffResult {
    let before_lines: Vec<&str> = if before.is_empty() {
        vec![]
    } else {
        before.split('\n').collect()
    };
    let after_lines: Vec<&str> = if after.is_empty() {
        vec![]
    } else {
        after.split('\n').collect()
    };

    let before_set: std::collections::HashSet<&str> = before_lines.iter().copied().collect();
    let after_set: std::collections::HashSet<&str> = after_lines.iter().copied().collect();

    let mut removed: Vec<String> = Vec::new();
    let mut added: Vec<String> = Vec::new();

    for line in &before_lines {
        if !after_set.contains(line) {
            removed.push(format!("- {}", line));
        }
    }

    for line in &after_lines {
        if !before_set.contains(line) {
            added.push(format!("+ {}", line));
        }
    }

    let changed = removed.len() + added.len();

    // Build unified format
    let mut unified_lines: Vec<String> = Vec::new();
    let mut bi: usize = 0;
    let mut ai: usize = 0;

    while bi < before_lines.len() || ai < after_lines.len() {
        let b_line = before_lines.get(bi).copied();
        let a_line = after_lines.get(ai).copied();

        if let Some(b) = b_line {
            if !after_set.contains(b) {
                unified_lines.push(format!("- {}", b));
                bi += 1;
                continue;
            }
        }

        if let Some(a) = a_line {
            if !before_set.contains(a) {
                unified_lines.push(format!("+ {}", a));
                ai += 1;
                continue;
            }
        }

        // Same line in both
        if let Some(b) = b_line {
            unified_lines.push(format!("  {}", b));
            bi += 1;
            ai += 1;
        } else {
            break;
        }
    }

    DiffResult {
        added,
        removed,
        changed,
        unified: unified_lines.join("\n"),
    }
}
