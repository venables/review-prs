//! The one model call that turns N independent reviews into one report.
//!
//! This is the half of a panel review that is genuinely judgment: dedup,
//! severity reconciliation, spotting a panelist that misread the change, and
//! verifying a claim against the code before it reaches the reader. It runs
//! in the repository, with read access, because a synthesizer that can only
//! see the panelists' text cannot check whether any of it is true.

use crate::panel::cli::Config;
use crate::panel::fanout::Outcome;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const INSTRUCTIONS: &str = include_str!("../../prompts/synthesis.md");

/// The synthesizer's input: what it was told to review, who answered, who did
/// not, and every section verbatim.
///
/// The roster is not decoration. "3 of 6 returned" changes what consensus
/// means, and a synthesizer that was never told a panelist died would read
/// its silence as agreement.
pub fn build_prompt(target_label: &str, outcomes: &[Outcome], focus: Option<&str>) -> String {
    let mut p = String::from(INSTRUCTIONS);

    p.push_str("\n\n## This run\n\n");
    p.push_str(&format!("- Target line to quote: {target_label}\n"));
    let answered = outcomes.iter().filter(|o| o.ok()).count();
    p.push_str(&format!(
        "- Panelists: {} of {} returned a review\n",
        answered,
        outcomes.len()
    ));
    if let Some(focus) = focus {
        p.push_str(&format!("- The reviewers were asked to focus on: {focus}\n"));
    }

    let failed: Vec<&Outcome> = outcomes.iter().filter(|o| !o.ok()).collect();
    if !failed.is_empty() {
        p.push_str("\nThese panelists contributed nothing. Do not count them toward consensus:\n\n");
        for o in failed {
            p.push_str(&format!(
                "- {} ({}): {}\n",
                o.id,
                o.model,
                o.failure.clone().unwrap_or_default()
            ));
        }
    }

    p.push_str("\n## Panelist reports\n");
    for o in outcomes {
        if o.stdout.trim().is_empty() {
            continue;
        }
        p.push_str(&format!("\n### {} / {}\n\n", o.id, o.model));
        p.push_str(o.stdout.trim_end());
        p.push('\n');
    }
    p
}

/// Run the synthesis. Read-only on purpose: verifying a finding needs Read
/// and Grep, and nothing beyond them -- the synthesizer has no business
/// editing the tree it is judging.
pub fn run(
    target_label: &str,
    outcomes: &[Outcome],
    cfg: &Config,
    dashp: &str,
    repo_root: &Path,
    out_dir: &Path,
) -> Result<String> {
    let prompt = build_prompt(target_label, outcomes, cfg.focus.as_deref());
    let prompt_path = out_dir.join("synthesis.prompt");
    File::create(&prompt_path)
        .context("creating the synthesis prompt file")?
        .write_all(prompt.as_bytes())
        .context("writing the synthesis prompt")?;

    eprintln!("panel: synthesizing with {} ...", cfg.synth_backend);
    let stdin = File::open(&prompt_path).context("opening the synthesis prompt")?;
    let mut argv = vec![
        "-H".to_string(),
        cfg.synth_backend.clone(),
        "--output-format".into(),
        "text".into(),
        "--timeout".into(),
        cfg.timeout_secs.to_string(),
        "--cwd".into(),
        repo_root.display().to_string(),
        "--perms".into(),
        "read-only".into(),
    ];
    if let Some(model) = &cfg.synth_model {
        argv.push("--model".into());
        argv.push(model.clone());
    }

    let err_path = out_dir.join("synthesis.err");
    let out = Command::new(dashp)
        .args(&argv)
        .stdin(Stdio::from(stdin))
        .stderr(Stdio::from(
            File::create(&err_path).context("creating the synthesis error file")?,
        ))
        .output()
        .context("running the synthesis")?;

    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || text.is_empty() {
        let detail = std::fs::read_to_string(&err_path).unwrap_or_default();
        let detail = detail.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
        anyhow::bail!(
            "the synthesis failed (exit {}){}{}. The panelist sections above are still the full reviews, and {} holds them.",
            out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "killed".into()),
            if detail.is_empty() { "" } else { ": " },
            detail,
            out_dir.display()
        );
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(id: &str, model: &str, stdout: &str, failure: Option<&str>) -> Outcome {
        Outcome {
            id: id.into(),
            model: model.into(),
            exit: Some(0),
            stdout: stdout.into(),
            failure: failure.map(str::to_string),
            elapsed_secs: 10,
            retried: false,
        }
    }

    #[test]
    fn the_prompt_carries_the_instructions_and_every_section() {
        let outcomes = vec![
            outcome("codex", "gpt-5", "Model: gpt-5\nGoal (clear): x\n- [HIGH] a.rs:1 — bug", None),
            outcome("claude", "opus-5", "Model: opus-5\nGoal (clear): x\nNO_FINDINGS — checked", None),
        ];
        let p = build_prompt("2 commits on feat/x vs main", &outcomes, Some("the auth path"));
        assert!(p.contains("Verify before you surface"));
        assert!(p.contains("- Target line to quote: 2 commits on feat/x vs main"));
        assert!(p.contains("- Panelists: 2 of 2 returned a review"));
        assert!(p.contains("focus on: the auth path"));
        assert!(p.contains("### codex / gpt-5"));
        assert!(p.contains("- [HIGH] a.rs:1 — bug"));
        assert!(p.contains("### claude / opus-5"));
        // Nothing failed, so no panelist is discounted. (The phrase itself
        // lives in the static instructions; the roster block is what varies.)
        assert!(!p.contains("Do not count them toward consensus"));
    }

    #[test]
    fn a_failed_panelist_is_named_and_discounted() {
        let outcomes = vec![
            outcome("codex", "gpt-5", "Model: gpt-5\n- [HIGH] a.rs:1 — bug", None),
            outcome("opencode", "glm-5.3", "", Some("timed out")),
        ];
        let p = build_prompt("uncommitted changes on main", &outcomes, None);
        assert!(p.contains("- Panelists: 1 of 2 returned a review"));
        assert!(p.contains("Do not count them toward consensus"));
        assert!(p.contains("- opencode (glm-5.3): timed out"));
        // An empty report contributes no section at all.
        assert!(!p.contains("### opencode / glm-5.3\n"));
    }
}
