//! Which terminal to open tabs in, and how to open one.
//!
//! Three targets, detected from the environment: herdr (preferred, driven over
//! its socket API), cmux, and Ghostty on macOS (driven through AppleScript,
//! which is why it is macOS-only). A spawn failure is reported and returned --
//! one bad tab must cost one review, not the whole sweep.

use crate::repo::command_exists;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spawner {
    Herdr,
    Cmux,
    Ghostty,
}

impl Spawner {
    pub fn name(self) -> &'static str {
        match self {
            Spawner::Herdr => "herdr",
            Spawner::Cmux => "cmux",
            Spawner::Ghostty => "ghostty",
        }
    }
}

/// The environment bits detection reads, gathered so the choice itself is a
/// pure function the tests can drive.
pub struct Detect {
    pub herdr_env: Option<String>,
    pub cmux_surface: Option<String>,
    pub term_program: Option<String>,
    pub is_macos: bool,
}

impl Detect {
    pub fn from_env() -> Detect {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Detect {
            herdr_env: var("HERDR_ENV"),
            cmux_surface: var("CMUX_SURFACE_ID"),
            term_program: var("TERM_PROGRAM"),
            is_macos: cfg!(target_os = "macos"),
        }
    }
}

fn shown(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or("<unset>")
}

/// Pick a terminal, or say what was looked for and what was found. A tool
/// whose whole job is opening tabs has to be specific about why it cannot.
pub fn choose(env: &Detect, available: &dyn Fn(&str) -> bool) -> Result<Spawner, String> {
    if env.herdr_env.as_deref() == Some("1") && available("herdr") {
        return Ok(Spawner::Herdr);
    }
    if env.cmux_surface.is_some() && available("cmux") {
        return Ok(Spawner::Cmux);
    }
    if env.term_program.as_deref() == Some("ghostty") {
        if !env.is_macos {
            return Err("error: Ghostty spawn requires macOS (AppleScript)".into());
        }
        return Ok(Spawner::Ghostty);
    }
    Err(format!(
        "error: no supported terminal detected\n  \
         expected HERDR_ENV=1, CMUX_SURFACE_ID, or TERM_PROGRAM=ghostty\n  \
         HERDR_ENV={} CMUX_SURFACE_ID={} TERM_PROGRAM={}\n  \
         (for a headless fan-out that needs no terminal, use: autoreview)",
        shown(&env.herdr_env),
        shown(&env.cmux_surface),
        shown(&env.term_program)
    ))
}

pub fn detect() -> Result<Spawner, String> {
    choose(&Detect::from_env(), &command_exists)
}

/// stdout only, never merged with stderr: a terminal's chatter on stderr would
/// corrupt the JSON these outputs are parsed as. Its stderr is inherited, so
/// whatever it says still reaches the user.
fn capture(cmd: &str, args: &[String]) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

fn run_quiet(cmd: &str, args: &[String]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_silent(cmd: &str, args: &[String]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// `herdr tab create` prints JSON; the new pane's id is at
/// .result.root_pane.pane_id.
pub fn parse_pane_id(out: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(out).ok()?;
    let id = v.pointer("/result/root_pane/pane_id")?;
    match id {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `cmux new-surface` prints a line carrying a "surface:N" token, which is
/// also the form --surface expects back.
pub fn parse_surface(out: &str) -> Option<String> {
    let start = out.find("surface:")?;
    let digits: String = out[start + "surface:".len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    Some(format!("surface:{digits}"))
}

fn spawn_in_herdr(cmd: &str, label: &str) -> Result<(), String> {
    // --workspace pins the tab to the workspace we renamed; without it herdr
    // may place tabs in whichever workspace the UI happens to have focused.
    let mut args = strings(&["tab", "create"]);
    if let Ok(ws) = std::env::var("HERDR_WORKSPACE_ID")
        && !ws.is_empty()
    {
        args.push("--workspace".into());
        args.push(ws);
    }
    args.extend(strings(&["--no-focus", "--label", label]));

    let Some(out) = capture("herdr", &args) else {
        return Err(format!(
            "error: herdr tab create failed for '{label}' (see herdr output above)"
        ));
    };
    let Some(pane) = parse_pane_id(&out) else {
        return Err(format!(
            "error: could not parse herdr pane id from output: {}",
            out.trim_end()
        ));
    };
    // Report the orphan explicitly: the tab exists and is labelled, so a bare
    // "skipped" would understate what got left behind.
    if !run_quiet("herdr", &strings(&["pane", "run", &pane, cmd])) {
        return Err(format!(
            "error: herdr pane run failed for '{label}'; tab was created but is empty"
        ));
    }
    Ok(())
}

fn spawn_in_cmux(cmd: &str, label: &str) -> Result<(), String> {
    let Some(out) = capture("cmux", &strings(&["new-surface"])) else {
        return Err(format!(
            "error: cmux new-surface failed for '{label}' (see cmux output above)"
        ));
    };
    let Some(surface) = parse_surface(&out) else {
        return Err(format!(
            "error: could not parse cmux surface from output: {}",
            out.trim_end()
        ));
    };
    // Best-effort, like the workspace rename: a naming failure must not abort
    // the fan-out. Renaming before the command runs means the tab is labelled
    // from the first frame rather than flashing an untitled tab.
    run_silent("cmux", &strings(&["rename-tab", "--surface", &surface, label]));
    if !run_quiet("cmux", &strings(&["send", "--surface", &surface, cmd])) {
        return Err(format!("error: cmux send failed for '{label}'"));
    }
    if !run_quiet("cmux", &strings(&["send-key", "--surface", &surface, "Return"])) {
        return Err(format!("error: cmux send-key failed for '{label}'"));
    }
    Ok(())
}

/// Ghostty has no tab-naming API, so its tabs are left unnamed: any title we
/// set would be overwritten by the review command within seconds.
fn spawn_in_ghostty(cmd: &str) -> Result<(), String> {
    let script = [
        "on run argv",
        "  set theCmd to item 1 of argv",
        "  tell application \"Ghostty\" to activate",
        "  tell application \"System Events\" to keystroke \"t\" using command down",
        "  delay 0.4",
        "  tell application \"Ghostty\"",
        "    set newTab to selected tab of front window",
        "    set newTerminal to focused terminal of newTab",
        "    input text (theCmd & return) to newTerminal",
        "  end tell",
        "end run",
    ];
    let mut args: Vec<String> = Vec::new();
    for line in script {
        args.push("-e".into());
        args.push(line.into());
    }
    args.push("--".into());
    args.push(cmd.into());

    if !run_quiet("osascript", &args) {
        return Err("error: osascript failed to open a Ghostty tab".into());
    }
    // The AppleScript returns before the new tab has taken the keystroke;
    // pacing the spawns keeps a fan-out from racing itself.
    std::thread::sleep(std::time::Duration::from_millis(300));
    Ok(())
}

pub fn spawn(spawner: Spawner, cmd: &str, label: &str) -> Result<(), String> {
    match spawner {
        Spawner::Herdr => spawn_in_herdr(cmd, label),
        Spawner::Cmux => spawn_in_cmux(cmd, label),
        Spawner::Ghostty => spawn_in_ghostty(cmd),
    }
}

/// Rename the enclosing workspace so review sessions stand out in the tab
/// switcher. Best-effort: a rename failure must never abort the fan-out, and
/// Ghostty has no workspace concept, so it is skipped.
pub fn rename_workspace(spawner: Spawner, label: &str) {
    match spawner {
        Spawner::Herdr => {
            if let Ok(ws) = std::env::var("HERDR_WORKSPACE_ID")
                && !ws.is_empty()
            {
                run_silent("herdr", &strings(&["workspace", "rename", &ws, label]));
            }
        }
        Spawner::Cmux => {
            run_silent(
                "cmux",
                &strings(&["workspace-action", "--action", "rename", "--title", label]),
            );
        }
        Spawner::Ghostty => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(herdr: Option<&str>, cmux: Option<&str>, term: Option<&str>, macos: bool) -> Detect {
        Detect {
            herdr_env: herdr.map(String::from),
            cmux_surface: cmux.map(String::from),
            term_program: term.map(String::from),
            is_macos: macos,
        }
    }

    fn all(_: &str) -> bool {
        true
    }

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn herdr_wins_when_it_is_available() {
        let e = env(Some("1"), Some("surface:1"), Some("ghostty"), true);
        assert_eq!(choose(&e, &all), Ok(Spawner::Herdr));
        // HERDR_ENV set but the CLI missing: fall through to the next target.
        assert_eq!(choose(&e, &|c: &str| c != "herdr"), Ok(Spawner::Cmux));
        // Neither CLI installed: Ghostty is still detectable from TERM_PROGRAM.
        assert_eq!(choose(&e, &none), Ok(Spawner::Ghostty));
    }

    #[test]
    fn herdr_env_must_be_exactly_one() {
        let e = env(Some("0"), None, None, true);
        assert!(choose(&e, &all).is_err());
    }

    #[test]
    fn ghostty_is_macos_only() {
        let e = env(None, None, Some("ghostty"), false);
        assert_eq!(
            choose(&e, &all).unwrap_err(),
            "error: Ghostty spawn requires macOS (AppleScript)"
        );
    }

    #[test]
    fn no_terminal_says_what_it_looked_for_and_found() {
        let msg = choose(&env(None, None, Some("Apple_Terminal"), true), &all).unwrap_err();
        assert!(msg.starts_with("error: no supported terminal detected"));
        assert!(msg.contains("expected HERDR_ENV=1, CMUX_SURFACE_ID, or TERM_PROGRAM=ghostty"));
        assert!(msg.contains("HERDR_ENV=<unset> CMUX_SURFACE_ID=<unset> TERM_PROGRAM=Apple_Terminal"));
        // The headless sibling is the answer to "I have no terminal".
        assert!(msg.contains("use: autoreview"));
    }

    #[test]
    fn herdr_pane_ids_are_read_out_of_the_json() {
        assert_eq!(
            parse_pane_id(r#"{"result":{"root_pane":{"pane_id":"pane-7"}}}"#).as_deref(),
            Some("pane-7")
        );
        // A numeric id is still an id.
        assert_eq!(
            parse_pane_id(r#"{"result":{"root_pane":{"pane_id":7}}}"#).as_deref(),
            Some("7")
        );
        assert_eq!(parse_pane_id(r#"{"result":{}}"#), None);
        assert_eq!(parse_pane_id(r#"{"result":{"root_pane":{"pane_id":""}}}"#), None);
        // Chatter that is not JSON at all must not read as a pane id.
        assert_eq!(parse_pane_id("herdr: something went wrong"), None);
    }

    #[test]
    fn cmux_surfaces_are_read_out_of_the_line() {
        assert_eq!(parse_surface("surface:1\n").as_deref(), Some("surface:1"));
        assert_eq!(
            parse_surface("created surface:42 in workspace 3").as_deref(),
            Some("surface:42")
        );
        assert_eq!(parse_surface("no surface here"), None);
        assert_eq!(parse_surface("surface:"), None);
    }
}
