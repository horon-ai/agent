//! `/plan review`: a read-only review of the plan waiting for approval.
//!
//! The review is deterministic and local. It reads the submitted plan, sends no
//! model request and changes no state: the plan stays pending until the user (or
//! the configured approval policy) answers it. Every review names the revision
//! of the plan it looked at, so a verdict can be tied to exactly that text.
use sha2::{Digest, Sha256};

use crate::plan_mode::SubmittedPlan;

/// A plan longer than this is flagged because it is hard to review in one pass.
const LONG_PLAN_CHARS: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Something a reader should fix or confirm before approving.
    Concern,
    /// A heuristic observation that may not apply.
    Advisory,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub code: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlanReview {
    /// Hash prefix of the plan text and its file list: the same plan always
    /// gets the same revision, and any edit changes it.
    pub revision: String,
    pub findings: Vec<Finding>,
}

/// The revision label of a submitted plan.
pub fn revision(plan: &SubmittedPlan) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plan.text.as_bytes());
    for file in &plan.files {
        hasher.update([0u8]);
        hasher.update(file.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn review(plan: &SubmittedPlan) -> PlanReview {
    let mut findings = Vec::new();
    let mut add = |severity, code, detail: String| findings.push(Finding { severity, code, detail });

    if plan.files.is_empty() {
        add(
            Severity::Concern,
            "no_files",
            "the plan lists no files it intends to touch".to_owned(),
        );
    }
    let mut seen = std::collections::BTreeSet::new();
    for file in &plan.files {
        if file.starts_with('/') || file.split(['/', '\\']).any(|part| part == "..") {
            add(
                Severity::Concern,
                "path_outside_workspace",
                format!("{file} is absolute or climbs out of the workspace"),
            );
        }
        if !seen.insert(file.as_str()) {
            add(
                Severity::Advisory,
                "duplicate_file",
                format!("{file} is listed more than once"),
            );
        }
    }
    let lower = plan.text.to_ascii_lowercase();
    if !["test", "verify", "check", "build", "lint"]
        .iter()
        .any(|word| lower.contains(word))
    {
        add(
            Severity::Advisory,
            "no_verification",
            "the text names no test, check or build step that would show the work is done".to_owned(),
        );
    }
    if plan.text.chars().count() > LONG_PLAN_CHARS {
        add(
            Severity::Advisory,
            "long_plan",
            format!("the plan is over {LONG_PLAN_CHARS} characters, which is hard to review at once"),
        );
    }
    PlanReview {
        revision: revision(plan),
        findings,
    }
}

pub fn render(review: &PlanReview) -> String {
    let mut text = format!("Review of plan revision {}.\n", review.revision);
    if review.findings.is_empty() {
        text.push_str("No findings.\n");
    }
    for finding in &review.findings {
        let label = match finding.severity {
            Severity::Concern => "concern ",
            Severity::Advisory => "advisory",
        };
        text.push_str(&format!("- [{label}] {}: {}\n", finding.code, finding.detail));
    }
    text.push_str(
        "\nThis review decided nothing. The plan is still waiting: use `/plan approve` or `/plan reject` once you have read it.",
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(text: &str, files: &[&str]) -> SubmittedPlan {
        SubmittedPlan {
            request_id: "req-1".to_owned(),
            text: text.to_owned(),
            files: files.iter().map(|file| (*file).to_owned()).collect(),
        }
    }

    #[test]
    fn a_review_is_tied_to_the_revision_and_any_edit_changes_it() {
        let first = plan("Add a retry. Run the tests.", &["src/a.rs"]);
        assert_eq!(review(&first).revision, review(&first.clone()).revision);
        assert_ne!(
            review(&first).revision,
            review(&plan("Add a retry. Run the tests!", &["src/a.rs"])).revision
        );
        assert_ne!(
            review(&first).revision,
            review(&plan("Add a retry. Run the tests.", &["src/b.rs"])).revision
        );
        assert_eq!(review(&first).revision.len(), 12);
    }

    #[test]
    fn a_sound_plan_has_no_findings_and_a_flawed_one_names_each_problem() {
        assert!(
            review(&plan("Edit the parser and run the tests.", &["src/parser.rs"]))
                .findings
                .is_empty()
        );
        let codes: Vec<_> = review(&plan(
            "Rewrite it.",
            &["../outside.rs", "/etc/x", "src/a.rs", "src/a.rs"],
        ))
        .findings
        .iter()
        .map(|finding| finding.code)
        .collect();
        assert_eq!(
            codes,
            [
                "path_outside_workspace",
                "path_outside_workspace",
                "duplicate_file",
                "no_verification"
            ]
        );
        let none: Vec<_> = review(&plan("Run the tests.", &[]))
            .findings
            .iter()
            .map(|f| f.code)
            .collect();
        assert_eq!(none, ["no_files"]);
    }
}
