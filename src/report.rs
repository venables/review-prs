//! What a finished review concluded, from two independent sources.
//!
//! The trailer is the reviewer's own report: a system-prompt instruction asks
//! the agent to end its final reply with a fenced ```autoreview block holding
//! one JSON object -- the decision, the synthesized risk, the finding counts,
//! and every panelist with its self-reported model. It is best-effort: an
//! agent that never writes the block costs a "-" in the summary, nothing more.
//!
//! The verdict is read back from GitHub: did *my* login submit a review on
//! this PR since the job started? That is authoritative in the way the
//! trailer can never be -- an agent that believed its own report would show
//! "approved" for an approval that never landed.

use serde::Deserialize;
use std::process::Command;

/// The agent's self-reported outcome, parsed leniently: every field is
/// optional, unknown fields are ignored, and a malformed block reads as no
/// block at all.
#[derive(Deserialize, Debug, Default, Clone)]
pub struct Trailer {
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub risk: Option<String>,
    #[serde(default)]
    pub findings: Option<Findings>,
    #[serde(default)]
    pub panel: Vec<Panelist>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct Findings {
    #[serde(default)]
    pub must_fix: Option<u64>,
    #[serde(default)]
    pub should_fix: Option<u64>,
    #[serde(default)]
    pub polish: Option<u64>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct Panelist {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub ok: Option<bool>,
    #[serde(default)]
    pub findings: Option<u64>,
    #[serde(default)]
    pub top: Option<String>,
}

/// The one-line system-prompt instruction that asks for the trailer. One line
/// on purpose: it travels through dash-p as a single `=`-form token, and the
/// test suite logs each reviewer call on a single line.
pub const TRAILER_INSTRUCTION: &str = "When your reply concludes a PR review task, end it with a fenced code block tagged autoreview containing exactly one JSON object shaped like {\"decision\":\"approved|commented|changes-requested|none\",\"risk\":\"LOW|MEDIUM|HIGH|CRITICAL\",\"findings\":{\"must_fix\":0,\"should_fix\":0,\"polish\":0},\"panel\":[{\"name\":\"codex\",\"model\":\"gpt-5.5\",\"ok\":true,\"findings\":2,\"top\":\"MEDIUM\"}]}. decision is what actually happened on the PR: approved = an approving review was submitted, commented = findings were posted without approval, changes-requested = a blocking review was submitted, none = nothing landed on the PR. risk and findings come from the synthesized review. panel lists every launched panelist with its self-reported model, whether it returned a verdict (ok), its finding count, and its top severity. Use null for anything unknown. No prose inside the block.";

/// The trailer out of the reviewer's stdout envelope (dash-p's
/// {"answer": ...}). The last fenced block wins: a review that quotes an
/// earlier block is reporting the newer state.
pub fn read_trailer(stdout_path: &std::path::Path) -> Option<Trailer> {
    let raw = std::fs::read_to_string(stdout_path).ok()?;
    let envelope: serde_json::Value = serde_json::from_str(&raw).ok()?;
    parse_trailer(envelope.get("answer")?.as_str()?)
}

pub fn parse_trailer(answer: &str) -> Option<Trailer> {
    // The last complete, parseable block wins -- scanned backwards so prose
    // that merely quotes the fence tag after the real block cannot hide it.
    let mut search_end = answer.len();
    while let Some(start) = answer[..search_end].rfind("```autoreview") {
        let body = &answer[start + "```autoreview".len()..];
        if let Some(end) = body.find("```")
            && let Ok(mut trailer) = serde_json::from_str::<Trailer>(body[..end].trim())
        {
            sanitize(&mut trailer);
            return Some(trailer);
        }
        search_end = start;
    }
    None
}

/// The trailer is agent output headed for the terminal, and this module's
/// job is to distrust it: no field may flood the summary, and no panelist
/// list may scroll it off the screen.
const MAX_FIELD_CHARS: usize = 80;
const MAX_PANELISTS: usize = 16;

/// Agent-authored strings end up on the terminal, and control bytes in them
/// are the classic escape-injection vector -- dropped at the door, so no
/// display path has to remember to.
fn sanitize(trailer: &mut Trailer) {
    strip_risky(&mut trailer.decision);
    strip_risky(&mut trailer.risk);
    trailer.panel.truncate(MAX_PANELISTS);
    for p in &mut trailer.panel {
        strip_risky(&mut p.name);
        strip_risky(&mut p.model);
        strip_risky(&mut p.top);
    }
}

fn strip_risky(s: &mut Option<String>) {
    if let Some(v) = s {
        *v = sanitize_for_display(v).chars().take(MAX_FIELD_CHARS).collect();
    }
}

/// Drop what can rewrite or reorder terminal output: C0/C1 controls, plus
/// the invisible characters terminals honor -- bidi embedding, override and
/// isolate marks, and the zero-width/format family.
pub fn sanitize_for_display(s: &str) -> String {
    s.chars().filter(|c| !is_display_risky(*c)).collect()
}

/// The same filter over many lines. Model output is printed whole, and the
/// newlines in it are not the risk -- the escape sequences and the bidi
/// overrides that could repaint or reorder the report around them are.
pub fn sanitize_block(s: &str) -> String {
    s.lines().map(sanitize_for_display).collect::<Vec<_>>().join("\n")
}

fn is_display_risky(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                | '\u{200B}'..='\u{200F}'
                | '\u{061C}'
                | '\u{FEFF}'
        )
}

/// What the GitHub readback learned. Failed and Nothing differ on purpose: a
/// successful empty readback affirmatively contradicts an agent claiming its
/// review landed, while a failed call contradicts nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readback {
    /// The gh call failed or timed out; nothing was learned.
    Failed,
    /// gh answered: no review by this login since the job started.
    Nothing,
    /// gh answered: my review landed -- "approved", "commented", ...
    Landed(String),
}

/// The review my login submitted on this PR since the job started.
/// latestReviews holds each reviewer's most recent review, so a stale
/// approval from a run last week does not read as this run's verdict. The
/// grace absorbs small clock skew between this box and GitHub -- kept tight
/// on purpose, because it is also the window in which a review landed by an
/// immediately-preceding run could read as this run's.
const SKEW_GRACE_SECS: i64 = 10;

/// How long the readback may take before it is killed. It runs off the event
/// loop (on the job's monitor thread), so this bounds how long a finished job
/// can hold its pool slot, not how long the pass stalls -- the pass never
/// waits on it directly.
const GH_TIMEOUT_SECS: u64 = 15;

pub fn github_verdict(pr: u64, me: &str, since_epoch: i64) -> Readback {
    let Some(stdout) = gh_latest_reviews(pr) else {
        return Readback::Failed;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&stdout) else {
        return Readback::Failed;
    };
    match verdict_from_reviews(&v, me, since_epoch) {
        Some(word) => Readback::Landed(word),
        // Only a response that actually carried the list counts as "GitHub
        // said nothing landed"; a malformed shape teaches us nothing.
        None if v.get("latestReviews").is_some_and(|r| r.is_array()) => Readback::Nothing,
        None => Readback::Failed,
    }
}

/// Run the gh query with a hard deadline: a hung network call must not hold a
/// pool slot forever, in a tool built for cron. Stdout is drained on its own
/// thread so a child blocked writing can never deadlock against our wait.
fn gh_latest_reviews(pr: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = Command::new("gh")
        .args(["pr", "view", &pr.to_string(), "--json", "latestReviews"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(GH_TIMEOUT_SECS);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return rx.recv_timeout(std::time::Duration::from_secs(1)).ok();
            }
            Ok(Some(_)) => return None,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn verdict_from_reviews(v: &serde_json::Value, me: &str, since_epoch: i64) -> Option<String> {
    let reviews = v.get("latestReviews")?.as_array()?;
    for review in reviews {
        let login = review.pointer("/author/login").and_then(|l| l.as_str()).unwrap_or("");
        if login != me {
            continue;
        }
        let submitted = review.get("submittedAt").and_then(|s| s.as_str()).unwrap_or("");
        let Some(t) = crate::prlist::parse_iso(submitted) else {
            continue;
        };
        if t >= since_epoch - SKEW_GRACE_SECS {
            return verdict_word(review.get("state").and_then(|s| s.as_str()).unwrap_or(""));
        }
    }
    None
}

fn verdict_word(state: &str) -> Option<String> {
    match state {
        "APPROVED" => Some("approved".into()),
        "CHANGES_REQUESTED" => Some("changes requested".into()),
        "COMMENTED" => Some("commented".into()),
        _ => None,
    }
}

/// The one verdict the summary shows. GitHub's readback outranks the agent's
/// own decision -- and a successful empty readback vetoes it: an agent
/// claiming its approval landed when GitHub says nothing did is exactly the
/// self-belief the verdict column exists to prevent. "commented" survives the
/// veto because plain comments never appear in latestReviews.
pub fn resolve_verdict(gh: &Readback, trailer: Option<&Trailer>) -> Option<String> {
    if let Readback::Landed(word) = gh {
        return Some(word.clone());
    }
    let decision = trailer?.decision.as_deref()?;
    match (decision, gh) {
        ("approved" | "changes-requested", Readback::Nothing) => None,
        ("approved", _) => Some("approved".into()),
        ("changes-requested", _) => Some("changes requested".into()),
        ("commented", _) => Some("commented".into()),
        _ => None,
    }
}

/// The landing claim a successful empty readback vetoed, if any. Callers
/// surface it: silently dropping a false "approved" would hide exactly the
/// lie the readback exists to catch.
pub fn vetoed_claim<'a>(gh: &Readback, trailer: Option<&'a Trailer>) -> Option<&'a str> {
    if *gh != Readback::Nothing {
        return None;
    }
    match trailer?.decision.as_deref()? {
        claim @ ("approved" | "changes-requested") => Some(claim),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailer_parses_from_a_fenced_block() {
        let answer = "The review is done.\n\n```autoreview\n{\"decision\":\"commented\",\"risk\":\"MEDIUM\",\"findings\":{\"must_fix\":0,\"should_fix\":1,\"polish\":2},\"panel\":[{\"name\":\"codex\",\"model\":\"gpt-5.5\",\"ok\":true,\"findings\":2,\"top\":\"MEDIUM\"}]}\n```";
        let t = parse_trailer(answer).unwrap();
        assert_eq!(t.decision.as_deref(), Some("commented"));
        assert_eq!(t.risk.as_deref(), Some("MEDIUM"));
        assert_eq!(t.findings.as_ref().unwrap().should_fix, Some(1));
        assert_eq!(t.panel[0].model.as_deref(), Some("gpt-5.5"));
    }

    #[test]
    fn the_last_block_wins() {
        let answer = "```autoreview\n{\"decision\":\"none\"}\n```\ntext\n```autoreview\n{\"decision\":\"approved\"}\n```";
        assert_eq!(parse_trailer(answer).unwrap().decision.as_deref(), Some("approved"));
    }

    #[test]
    fn missing_or_malformed_blocks_read_as_none() {
        assert!(parse_trailer("no block here").is_none());
        assert!(parse_trailer("```autoreview\nnot json\n```").is_none());
        assert!(parse_trailer("```autoreview\n{\"decision\":").is_none());
    }

    #[test]
    fn a_trailing_mention_of_the_tag_does_not_hide_the_block() {
        let answer =
            "```autoreview\n{\"decision\":\"approved\"}\n```\nsee the ```autoreview block above";
        assert_eq!(parse_trailer(answer).unwrap().decision.as_deref(), Some("approved"));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let t = parse_trailer("```autoreview\n{\"decision\":\"approved\",\"surprise\":42}\n```").unwrap();
        assert_eq!(t.decision.as_deref(), Some("approved"));
    }

    #[test]
    fn verdict_resolution_prefers_github() {
        let commented = parse_trailer("```autoreview\n{\"decision\":\"commented\"}\n```");
        assert_eq!(
            resolve_verdict(&Readback::Landed("approved".into()), commented.as_ref()).as_deref(),
            Some("approved")
        );
        assert_eq!(
            resolve_verdict(&Readback::Failed, commented.as_ref()).as_deref(),
            Some("commented")
        );
        assert_eq!(resolve_verdict(&Readback::Failed, None), None);
        let hyphenated = parse_trailer("```autoreview\n{\"decision\":\"changes-requested\"}\n```");
        assert_eq!(
            resolve_verdict(&Readback::Failed, hyphenated.as_ref()).as_deref(),
            Some("changes requested")
        );
        let none = parse_trailer("```autoreview\n{\"decision\":\"none\"}\n```");
        assert_eq!(resolve_verdict(&Readback::Failed, none.as_ref()), None);
    }

    #[test]
    fn an_empty_readback_vetoes_landing_claims() {
        // GitHub answered and nothing landed: the agent's "approved" is
        // exactly the self-belief the column exists to catch. A comment,
        // though, never appears in latestReviews -- it survives.
        let approved = parse_trailer("```autoreview\n{\"decision\":\"approved\"}\n```");
        assert_eq!(resolve_verdict(&Readback::Nothing, approved.as_ref()), None);
        let blocking = parse_trailer("```autoreview\n{\"decision\":\"changes-requested\"}\n```");
        assert_eq!(resolve_verdict(&Readback::Nothing, blocking.as_ref()), None);
        let commented = parse_trailer("```autoreview\n{\"decision\":\"commented\"}\n```");
        assert_eq!(
            resolve_verdict(&Readback::Nothing, commented.as_ref()).as_deref(),
            Some("commented")
        );
        // A failed call vetoes nothing -- there is no contradiction to lean on.
        assert_eq!(
            resolve_verdict(&Readback::Failed, approved.as_ref()).as_deref(),
            Some("approved")
        );
    }

    #[test]
    fn vetoed_claims_are_named_for_the_note() {
        let approved = parse_trailer("```autoreview\n{\"decision\":\"approved\"}\n```");
        assert_eq!(vetoed_claim(&Readback::Nothing, approved.as_ref()), Some("approved"));
        // A failed readback contradicts nothing, and a comment claim is
        // never vetoed in the first place.
        assert_eq!(vetoed_claim(&Readback::Failed, approved.as_ref()), None);
        let commented = parse_trailer("```autoreview\n{\"decision\":\"commented\"}\n```");
        assert_eq!(vetoed_claim(&Readback::Nothing, commented.as_ref()), None);
        assert_eq!(vetoed_claim(&Readback::Nothing, None), None);
    }

    #[test]
    fn trailer_strings_are_stripped_of_control_bytes() {
        let t = parse_trailer(
            "```autoreview\n{\"decision\":\"approved\",\"risk\":\"L\\u001b[31mOW\",\"panel\":[{\"name\":\"co\\u001b]0;pwned\\u0007dex\",\"model\":\"gpt\\u001b[0m-5.5\"}]}\n```",
        )
        .unwrap();
        assert_eq!(t.risk.as_deref(), Some("L[31mOW"));
        assert_eq!(t.panel[0].name.as_deref(), Some("co]0;pwneddex"));
        assert_eq!(t.panel[0].model.as_deref(), Some("gpt[0m-5.5"));
    }

    #[test]
    fn github_readback_wants_my_fresh_review_only() {
        let reviews = |login: &str, state: &str, at: &str| {
            serde_json::json!({"latestReviews": [
                {"author": {"login": login}, "state": state, "submittedAt": at}
            ]})
        };
        let start = crate::prlist::parse_iso("2026-08-18T12:00:00Z").unwrap();

        // A review I submitted after the job started is this run's verdict.
        let mine = reviews("me", "APPROVED", "2026-08-18T12:05:00Z");
        assert_eq!(verdict_from_reviews(&mine, "me", start).as_deref(), Some("approved"));

        // A review from before the run is stale -- some earlier engagement.
        let stale = reviews("me", "APPROVED", "2026-08-18T11:00:00Z");
        assert_eq!(verdict_from_reviews(&stale, "me", start), None);

        // ...even one landed half a minute before: the grace is for clock
        // skew, not for adopting an immediately-preceding run's review.
        let recent = reviews("me", "APPROVED", "2026-08-18T11:59:30Z");
        assert_eq!(verdict_from_reviews(&recent, "me", start), None);

        // Small clock skew must not hide a review submitted at the boundary.
        let skewed = reviews("me", "COMMENTED", "2026-08-18T11:59:55Z");
        assert_eq!(verdict_from_reviews(&skewed, "me", start).as_deref(), Some("commented"));

        // Someone else's approval is not my verdict.
        let theirs = reviews("alice", "APPROVED", "2026-08-18T12:05:00Z");
        assert_eq!(verdict_from_reviews(&theirs, "me", start), None);

        let blocking = reviews("me", "CHANGES_REQUESTED", "2026-08-18T12:05:00Z");
        assert_eq!(
            verdict_from_reviews(&blocking, "me", start).as_deref(),
            Some("changes requested")
        );

        let empty = serde_json::json!({"latestReviews": []});
        assert_eq!(verdict_from_reviews(&empty, "me", start), None);
    }

    #[test]
    fn trailer_fields_and_panel_are_bounded() {
        let long = "x".repeat(500);
        let panel: Vec<String> =
            (0..40).map(|i| format!("{{\"name\":\"p{i}\",\"model\":\"{long}\"}}")).collect();
        let t = parse_trailer(&format!(
            "```autoreview\n{{\"risk\":\"{long}\",\"panel\":[{}]}}\n```",
            panel.join(",")
        ))
        .unwrap();
        assert_eq!(t.risk.as_deref().unwrap().len(), 80);
        assert_eq!(t.panel.len(), 16);
        assert_eq!(t.panel[0].model.as_deref().unwrap().len(), 80);
    }

    #[test]
    fn the_instruction_stays_on_one_line() {
        // It travels as a single argv token and the test fakes log one call
        // per line; a newline would split both.
        assert!(!TRAILER_INSTRUCTION.contains('\n'));
    }
}
