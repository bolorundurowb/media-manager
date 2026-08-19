//! Plan renderers (§8 human output, --json output).

use std::collections::BTreeMap;

use mm_core::plan::{Action, Plan};

/// Render a plan as a diff-style human-readable table.
pub fn render_text(plan: &Plan, verbose: bool) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "Plan for {} ({}) — {} items",
        plan.root.display(),
        plan.kind.as_str(),
        plan.items.len()
    ));
    lines.push(format!("config digest: {}", plan.config_digest));
    lines.push(String::new());

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in &plan.items {
        *counts.entry(action_label(&item.action)).or_default() += 1;
    }
    for (label, n) in counts {
        lines.push(format!("{label:12} {n}"));
    }
    lines.push(String::new());

    if verbose {
        for item in &plan.items {
            match &item.action {
                Action::Move { from, to } => {
                    lines.push(format!("MOVE   {} -> {}", from.display(), to.display()));
                }
                Action::NoOp => {
                    lines.push(format!("NOOP   {}", item.source.display()));
                }
                Action::Conflict { from, to, .. } => {
                    lines.push(format!("CONFLICT {} -> {}", from.display(), to.display()));
                }
                Action::Duplicate { from, identical_to } => {
                    lines.push(format!(
                        "DUPLICATE {} (same as {})",
                        from.display(),
                        identical_to.display()
                    ));
                }
                Action::NeedsReview { path, missing } => {
                    let fields: Vec<String> = missing.iter().map(|f| f.as_str().into()).collect();
                    lines.push(format!(
                        "REVIEW {} missing: {}",
                        path.display(),
                        fields.join(", ")
                    ));
                }
                Action::Skip { reason } => {
                    lines.push(format!("SKIP   {} ({:?})", item.source.display(), reason));
                }
            }
        }
    }

    lines.join("\n")
}

fn action_label(action: &Action) -> String {
    match action {
        Action::NoOp => "NoOp".into(),
        Action::Move { .. } => "Move".into(),
        Action::Skip { .. } => "Skip".into(),
        Action::Conflict { .. } => "Conflict".into(),
        Action::Duplicate { .. } => "Duplicate".into(),
        Action::NeedsReview { .. } => "Review".into(),
    }
}

/// Render a plan as JSON.
pub fn render_json(plan: &Plan) -> String {
    serde_json::to_string_pretty(plan).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_summary() {
        let plan = Plan::new(
            uuid::Uuid::nil(),
            std::path::PathBuf::from("/media"),
            mm_core::MediaKind::Movies,
            "abc123".into(),
            mm_core::volume::VolumeSemantics::conservative(),
        );
        let txt = render_text(&plan, false);
        assert!(txt.contains("Plan for /media"));
    }
}
