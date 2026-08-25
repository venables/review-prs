//! Who is on the panel, and how each one is launched.
//!
//! Every panelist is one `dash-p` subprocess over a backend CLI. The backend
//! is the only thing that varies -- dash-p maps the same generic flags onto
//! each CLI's native argv, so this module never learns what codex or opencode
//! want on their command lines.

use crate::repo::command_exists;
use std::path::Path;

/// Probed in this order, and this is also the order they appear in the
/// report. Extend it as more review-capable CLIs appear.
pub const BACKENDS: [&str; 3] = ["codex", "claude", "opencode"];

/// A panelist as asked for on the command line: a backend, and optionally the
/// model to pin it to.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub backend: String,
    pub model: Option<String>,
}

/// A panelist as run: a spec plus the unique id that names its files, its
/// worktree and its section heading.
#[derive(Debug, Clone, PartialEq)]
pub struct Panelist {
    pub id: String,
    pub backend: String,
    pub model: Option<String>,
}

/// `backend` or `backend:model`.
pub fn parse_spec(raw: &str) -> Result<Spec, String> {
    let (backend, model) = match raw.split_once(':') {
        Some((b, m)) => (b, Some(m)),
        None => (raw, None),
    };
    if !BACKENDS.contains(&backend) {
        return Err(format!(
            "error: unknown panelist backend \"{backend}\" (expected one of: {})",
            BACKENDS.join(", ")
        ));
    }
    if model.is_some_and(str::is_empty) {
        return Err(format!("error: panelist \"{raw}\" names no model after the colon"));
    }
    Ok(Spec {
        backend: backend.to_string(),
        model: model.map(str::to_string),
    })
}

/// Anything outside this set becomes a dash: the id goes into file paths and
/// `git worktree add`, so a model like `zai/glm-5.3` or `../etc` must not
/// carry its punctuation there.
fn sanitize(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' { c } else { '-' })
        .collect()
}

/// Every backend CLI on PATH, each on its own default model. What you get
/// when you name no panelists.
pub fn autodetect() -> Vec<Spec> {
    BACKENDS
        .iter()
        .filter(|b| command_exists(b))
        .map(|b| Spec { backend: b.to_string(), model: None })
        .collect()
}

/// Give each spec an id, keeping two reviewers on one backend apart. The
/// model is part of the id when there is one, and a numeric suffix settles
/// the rest -- two panelists that collided on a filename would overwrite each
/// other's output.
pub fn resolve(specs: &[Spec]) -> Vec<Panelist> {
    let mut used: Vec<String> = Vec::new();
    specs
        .iter()
        .map(|spec| {
            let base = match &spec.model {
                Some(m) => format!("{}-{}", spec.backend, sanitize(m)),
                None => spec.backend.clone(),
            };
            let mut id = base.clone();
            let mut n = 2;
            while used.contains(&id) {
                id = format!("{base}-{n}");
                n += 1;
            }
            used.push(id.clone());
            Panelist {
                id,
                backend: spec.backend.clone(),
                model: spec.model.clone(),
            }
        })
        .collect()
}

/// The dash-p argv for one panelist. No positional prompt: it arrives on
/// stdin instead, because a large embedded diff can push a single argument
/// past Linux's 128KB per-argument cap and fail the exec with E2BIG. macOS
/// has no such cap, which is exactly how that bug reaches a Linux user
/// unnoticed.
///
/// `--output-format text` so stdout is the panelist's final message and
/// nothing else: its first line is the `Model:` line the report reads back.
pub fn dashp_args(p: &Panelist, cwd: &Path, timeout_secs: u64, isolated: bool) -> Vec<String> {
    let mut argv = vec![
        "-H".to_string(),
        p.backend.clone(),
        "--output-format".into(),
        "text".into(),
        "--timeout".into(),
        timeout_secs.to_string(),
        "--cwd".into(),
        cwd.display().to_string(),
    ];
    if let Some(model) = &p.model {
        argv.push("--model".into());
        argv.push(model.clone());
    }
    if isolated {
        // A throwaway worktree: the panelist may run the test suite and edit
        // files to investigate, and nothing it does outlives the run.
        argv.push("--dangerously-skip-permissions".into());
    } else {
        // The user's actual working tree. Read, grep, reason -- nothing else.
        argv.push("--perms".into());
        argv.push("read-only".into());
    }
    argv
}

/// The model a panelist says it is running. Every prompt mandates `Model:
/// <id>` as the first line, which beats introspecting each CLI's own
/// default-model config. Only the first line is considered: a `Model:` deeper
/// in the findings is the panelist quoting something, not reporting itself.
pub fn extract_model(stdout: &str) -> Option<String> {
    let first = stdout.lines().next()?.trim();
    let rest = first.strip_prefix("Model:")?.trim();
    if rest.is_empty() { None } else { Some(rest.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_with_and_without_a_model() {
        assert_eq!(
            parse_spec("claude").unwrap(),
            Spec { backend: "claude".into(), model: None }
        );
        assert_eq!(
            parse_spec("opencode:zai/glm-5.3").unwrap(),
            Spec { backend: "opencode".into(), model: Some("zai/glm-5.3".into()) }
        );
        assert!(parse_spec("gpt4").unwrap_err().contains("unknown panelist backend"));
        assert!(parse_spec("claude:").unwrap_err().contains("names no model"));
    }

    #[test]
    fn ids_stay_unique_and_path_safe() {
        let specs = vec![
            Spec { backend: "claude".into(), model: None },
            Spec { backend: "opencode".into(), model: Some("zai/glm-5.3".into()) },
            Spec { backend: "claude".into(), model: None },
            Spec { backend: "claude".into(), model: Some("opus-4.8".into()) },
        ];
        let ids: Vec<String> = resolve(&specs).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, vec!["claude", "opencode-zai-glm-5.3", "claude-2", "claude-opus-4.8"]);
        // A model that tried to climb out of the output directory cannot:
        // the separators are gone, so the whole thing is one directory name.
        let escape = vec![Spec { backend: "codex".into(), model: Some("../../etc".into()) }];
        let id = &resolve(&escape)[0].id;
        assert_eq!(id, "codex-..-..-etc");
        assert!(!id.contains('/'), "an id becomes a path component: {id}");
    }

    #[test]
    fn the_prompt_never_rides_in_argv() {
        let p = Panelist { id: "claude".into(), backend: "claude".into(), model: None };
        let argv = dashp_args(&p, Path::new("/repo"), 600, false);
        assert_eq!(argv.join(" "), "-H claude --output-format text --timeout 600 --cwd /repo --perms read-only");
    }

    #[test]
    fn an_isolated_panelist_may_run_things() {
        let p = Panelist { id: "codex".into(), backend: "codex".into(), model: Some("gpt-5".into()) };
        let argv = dashp_args(&p, Path::new("/wt"), 900, true);
        assert!(argv.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(!argv.contains(&"read-only".to_string()));
        assert!(argv.windows(2).any(|w| w == ["--model", "gpt-5"]));
    }

    #[test]
    fn the_model_line_is_read_from_the_first_line_only() {
        assert_eq!(extract_model("Model: gpt-5.6\nGoal (clear): ...").as_deref(), Some("gpt-5.6"));
        assert_eq!(extract_model("Model:   claude-opus-5  \n").as_deref(), Some("claude-opus-5"));
        assert_eq!(extract_model("Goal: ...\nModel: quoted-later"), None);
        assert_eq!(extract_model("Model:\n"), None);
        assert_eq!(extract_model(""), None);
    }
}
