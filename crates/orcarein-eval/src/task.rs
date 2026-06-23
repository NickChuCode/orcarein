//! Task abstraction for the eval harness — pure data + two closures.
//!
//! An [`EvalCase`] knows how to set up its workspace, what to ask the agent,
//! and how to grade the result. Graders are **deterministic** (check a file's
//! contents / a condition) — Week 1 deliberately avoids running a real test
//! harness; the real SWE-bench grader arrives in Week 2.

use std::path::Path;

/// Whether a graded run passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Pass => write!(f, "pass"),
            Verdict::Fail => write!(f, "fail"),
        }
    }
}

/// One evaluation case.
pub struct EvalCase {
    /// Stable id, used in CSV rows and trace filenames.
    pub id: String,
    /// The user message handed to the agent.
    pub prompt: String,
    /// Lays down initial files in the (already-created, empty) workspace dir.
    pub setup: fn(&Path) -> std::io::Result<()>,
    /// Deterministic pass/fail check against the workspace after the run.
    pub grader: fn(&Path) -> Verdict,
}

/// No-op setup for cases that start from an empty workspace.
fn empty_setup(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// The Week 1 smoke suite — small, deterministic, no real test runner.
pub fn toy_suite() -> Vec<EvalCase> {
    vec![
        EvalCase {
            id: "create-hello".into(),
            prompt: "Create a file named hello.txt whose contents are exactly: hello world".into(),
            setup: empty_setup,
            grader: |dir| match std::fs::read_to_string(dir.join("hello.txt")) {
                Ok(s) if s.contains("hello world") => Verdict::Pass,
                _ => Verdict::Fail,
            },
        },
        EvalCase {
            id: "append-line".into(),
            prompt: "There is a file notes.txt. Append a new line containing the word DONE to it."
                .into(),
            setup: |dir| std::fs::write(dir.join("notes.txt"), "first line\n"),
            grader: |dir| match std::fs::read_to_string(dir.join("notes.txt")) {
                Ok(s) if s.contains("first line") && s.contains("DONE") => Verdict::Pass,
                _ => Verdict::Fail,
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grader_passes_when_file_has_expected_content() {
        let dir = tempfile::tempdir().unwrap();
        let suite = toy_suite();
        let case = suite.iter().find(|c| c.id == "create-hello").unwrap();
        // Simulate the agent having done its job.
        std::fs::write(dir.path().join("hello.txt"), "hello world\n").unwrap();
        assert_eq!((case.grader)(dir.path()), Verdict::Pass);
    }

    #[test]
    fn grader_fails_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let case = &toy_suite()[0];
        assert_eq!((case.grader)(dir.path()), Verdict::Fail);
    }

    #[test]
    fn toy_suite_is_nonempty_and_ids_unique() {
        let suite = toy_suite();
        assert!(suite.len() >= 2);
        let mut ids: Vec<&str> = suite.iter().map(|c| c.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), suite.len(), "case ids must be unique");
    }
}
