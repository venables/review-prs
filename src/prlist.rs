//! Fetching, ranking and selecting the PRs to review, mirroring
//! lib/pr-list.sh (which review-prs still uses -- the two must agree on what
//! is worth reviewing).

use crate::repo::RepoContext;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::process::Command;

/// Author logins treated as bots: hidden unless --dependabot, dimmed when
/// shown. An anchored prefix match, like lib/pr-list.sh's `^dependabot`.
const BOT_LOGIN_PREFIX: &str = "dependabot";

pub fn is_bot(login: &str) -> bool {
    login.starts_with(BOT_LOGIN_PREFIX)
}

// The same query lib/pr-list.sh sends: one GraphQL call for the open PRs and
// enough activity to rank engagement.
const QUERY: &str = "
      query($owner:String!, $name:String!) {
        repository(owner:$owner, name:$name) {
          pullRequests(states:OPEN, first:50, orderBy:{field:UPDATED_AT, direction:DESC}) {
            nodes {
              number
              title
              isDraft
              updatedAt
              reviewDecision
              author { login }
              comments(last:100) { nodes { author { login } updatedAt } }
              reviews(last:100)  { nodes { author { login } submittedAt } }
              commits(last:100)  { nodes { commit { committedDate author { user { login } } } } }
            }
          }
        }
      }";

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Actor {
    pub login: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommentNode {
    pub author: Option<Actor>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ReviewNode {
    pub author: Option<Actor>,
    #[serde(rename = "submittedAt")]
    pub submitted_at: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommitAuthor {
    pub user: Option<Actor>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Commit {
    #[serde(rename = "committedDate")]
    pub committed_date: Option<String>,
    pub author: Option<CommitAuthor>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommitNode {
    pub commit: Commit,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Nodes<T> {
    #[serde(default = "Vec::new")]
    pub nodes: Vec<T>,
}

fn empty_nodes<T>() -> Nodes<T> {
    Nodes { nodes: Vec::new() }
}

#[derive(Deserialize, Debug, Clone)]
pub struct PrNode {
    pub number: u64,
    pub title: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "reviewDecision")]
    pub review_decision: Option<String>,
    pub author: Option<Actor>,
    #[serde(default = "empty_nodes")]
    pub comments: Nodes<CommentNode>,
    #[serde(default = "empty_nodes")]
    pub reviews: Nodes<ReviewNode>,
    #[serde(default = "empty_nodes")]
    pub commits: Nodes<CommitNode>,
}

impl PrNode {
    pub fn author_login(&self) -> &str {
        self.author.as_ref().and_then(|a| a.login.as_deref()).unwrap_or("")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engagement {
    New,
    Updated,
    Seen,
}

impl Engagement {
    pub fn label(self) -> &'static str {
        match self {
            Engagement::New => "NEW",
            Engagement::Updated => "UPDATED",
            Engagement::Seen => "SEEN",
        }
    }
    fn rank(self) -> u8 {
        match self {
            Engagement::New => 0,
            Engagement::Updated => 1,
            Engagement::Seen => 2,
        }
    }
}

/// One display/selection row, already ranked.
#[derive(Debug, Clone)]
pub struct Row {
    pub bot: bool,
    pub number: u64,
    pub engage: Engagement,
    pub review: &'static str,
    pub rel_time: String,
    pub author: String,
    pub title: String,
    pub updated_at: String,
    pub resumable: bool,
}

/// Fetch, filter, and say why when nothing is left. Returns None (after
/// printing) when there is nothing to review -- the caller exits 0.
pub fn fetch_prs(ctx: &RepoContext, include_approved: bool, include_dependabot: bool) -> Result<Option<Vec<PrNode>>> {
    let out = Command::new("gh")
        .args(["api", "graphql", "-F"])
        .arg(format!("owner={}", ctx.owner))
        .arg("-F")
        .arg(format!("name={}", ctx.name))
        .arg("-f")
        .arg(format!("query={QUERY}"))
        .output()
        .context("running gh api graphql")?;
    if !out.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&out.stderr).trim_end());
        bail!(crate::repo::AlreadyReported);
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("parsing gh api graphql output")?;
    let prs = extract_nodes(&parsed)?;

    let prs: Vec<PrNode> = prs
        .into_iter()
        .filter(|pr| !pr.is_draft)
        // Always hide your own PRs -- this tool is for reviewing others' work.
        .filter(|pr| pr.author_login() != ctx.me)
        .filter(|pr| include_dependabot || !is_bot(pr.author_login()))
        .filter(|pr| include_approved || pr.review_decision.as_deref() != Some("APPROVED"))
        .collect();

    if prs.is_empty() {
        let mut hint = String::new();
        if !include_approved {
            hint.push_str(" --all (approved)");
        }
        if !include_dependabot {
            hint.push_str(" --dependabot (bots)");
        }
        if !hint.is_empty() {
            println!("no matching open PRs; try:{hint}");
        } else {
            println!("no open non-draft PRs");
        }
        return Ok(None);
    }
    Ok(Some(prs))
}

/// The PR nodes out of a GraphQL response -- or a refusal. A 200 carrying an
/// `errors` array, or a response missing the data path entirely, must not
/// read as "no PRs": an unattended sweep would then exit 0 having reviewed
/// nothing, which is the lie the exit status exists to prevent.
fn extract_nodes(parsed: &serde_json::Value) -> Result<Vec<PrNode>> {
    if let Some(errors) = parsed.get("errors").and_then(|e| e.as_array())
        && !errors.is_empty()
    {
        eprintln!("error: gh api graphql returned errors:");
        for err in errors {
            match err.get("message").and_then(|m| m.as_str()) {
                Some(msg) => eprintln!("  {msg}"),
                None => eprintln!("  {err}"),
            }
        }
        bail!(crate::repo::AlreadyReported);
    }
    let Some(nodes) = parsed.pointer("/data/repository/pullRequests/nodes") else {
        eprintln!("error: gh api graphql returned no pull request data");
        bail!(crate::repo::AlreadyReported);
    };
    serde_json::from_value(nodes.clone()).context("parsing the pull request list")
}

/// Seconds since the epoch for a strict "YYYY-MM-DDTHH:MM:SSZ" timestamp --
/// the only shape GitHub emits here. Days-from-civil, no external crate.
fn parse_iso(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let (y, m, d) = (num(&ts[0..4])?, num(&ts[5..7])?, num(&ts[8..10])?);
    let (hh, mm, ss) = (num(&ts[11..13])?, num(&ts[14..16])?, num(&ts[17..19])?);
    // Howard Hinnant's days_from_civil.
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// "5h ago" shapes, floored like lib/pr-list.sh's jq rel().
pub fn rel(now_epoch: i64, ts: &str) -> String {
    let Some(t) = parse_iso(ts) else {
        return "-".into();
    };
    let s = now_epoch - t;
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86_400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}

/// NEW/UPDATED/SEEN from the PR's activity, exactly as the jq did: the latest
/// event by me vs the latest by anyone else, ISO strings compared
/// lexicographically (which is chronological for this fixed shape).
pub fn engagement(pr: &PrNode, me: &str) -> Engagement {
    let mut events: Vec<(&str, &str)> = Vec::new();
    for c in &pr.comments.nodes {
        if let Some(at) = c.updated_at.as_deref() {
            events.push((at, c.author.as_ref().and_then(|a| a.login.as_deref()).unwrap_or("")));
        }
    }
    for r in &pr.reviews.nodes {
        if let Some(at) = r.submitted_at.as_deref() {
            events.push((at, r.author.as_ref().and_then(|a| a.login.as_deref()).unwrap_or("")));
        }
    }
    for c in &pr.commits.nodes {
        if let Some(at) = c.commit.committed_date.as_deref() {
            let who = c
                .commit
                .author
                .as_ref()
                .and_then(|a| a.user.as_ref())
                .and_then(|u| u.login.as_deref())
                .unwrap_or("");
            events.push((at, who));
        }
    }
    let mine = events.iter().filter(|(_, w)| *w == me).map(|(at, _)| *at).max();
    let other = events.iter().filter(|(_, w)| *w != me).map(|(at, _)| *at).max();
    match (mine, other) {
        (None, _) => Engagement::New,
        (Some(m), Some(o)) if o > m => Engagement::Updated,
        _ => Engagement::Seen,
    }
}

pub fn build_rows(prs: &[PrNode], me: &str, now_epoch: i64) -> Vec<Row> {
    let mut rows: Vec<Row> = prs
        .iter()
        .map(|pr| {
            let engage = engagement(pr, me);
            let review = match pr.review_decision.as_deref() {
                Some("CHANGES_REQUESTED") => "CHANGES",
                Some("APPROVED") => "APPROVED",
                _ => "-",
            };
            let author = if pr.author_login().is_empty() { "ghost" } else { pr.author_login() };
            Row {
                bot: is_bot(pr.author_login()),
                number: pr.number,
                engage,
                review,
                rel_time: rel(now_epoch, &pr.updated_at),
                author: author.to_string(),
                title: pr.title.clone(),
                updated_at: pr.updated_at.clone(),
                resumable: false,
            }
        })
        .collect();
    // Most actionable first: rank ascending, then recency descending (string
    // compare on the ISO timestamps, as `sort -k2,2r` did).
    rows.sort_by(|a, b| {
        a.engage
            .rank()
            .cmp(&b.engage.rank())
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    rows
}

/// The auto sweep: every NEW/UPDATED PR. SEEN PRs are skipped on purpose --
/// nothing has changed since you last engaged, so an automated sweep has no
/// reason to re-review them. Prints the selection; None (after printing)
/// means nothing to do and the caller exits 0.
pub fn select_auto(rows: &[Row]) -> Option<Vec<u64>> {
    let numbers: Vec<u64> = rows
        .iter()
        .filter(|r| matches!(r.engage, Engagement::New | Engagement::Updated))
        .map(|r| r.number)
        .collect();
    if numbers.is_empty() {
        println!("no NEW or UPDATED PRs to auto-review");
        return None;
    }
    let list: String = numbers.iter().map(|n| format!("#{n} ")).collect();
    println!("auto-reviewing {} PR(s): {}", numbers.len(), list);
    Some(numbers)
}

/// Why a babysit loop should stop watching a PR, or None while it should keep
/// waiting. Approval is the expected end, but a PR that was closed, or merged
/// without ever collecting an approving review, is just as finished. A failed
/// lookup reads as "keep waiting" -- a transient API error must not end the
/// loop early.
pub fn pr_babysit_done(n: u64) -> Option<String> {
    let out = Command::new("gh")
        .args(["pr", "view", &n.to_string(), "--json", "state,reviewDecision"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let decision = v["reviewDecision"].as_str().unwrap_or("");
    let state = v["state"].as_str().unwrap_or("");
    if decision == "APPROVED" {
        Some("approved".into())
    } else if !state.is_empty() && state != "OPEN" {
        Some(state.to_lowercase())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The same shape tests/helpers.sh seeds (its numbering differs slightly:
    // there 5 is the approved PR and 2 the draft): 9 and 8 are NEW by others,
    // 6 is SEEN (me commented last), 5 is a draft, 4 is mine, 3 is
    // dependabot, 2 is approved.
    fn fixture() -> Vec<PrNode> {
        let json = r#"[
          {"number":9,"title":"Add retry logic","isDraft":false,
           "updatedAt":"2026-08-10T10:00:00Z","reviewDecision":null,
           "author":{"login":"alice"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},
           "commits":{"nodes":[{"commit":{"committedDate":"2026-08-10T10:00:00Z","author":{"user":{"login":"alice"}}}}]}},
          {"number":8,"title":"Fix flaky test","isDraft":false,
           "updatedAt":"2026-08-09T10:00:00Z","reviewDecision":"CHANGES_REQUESTED",
           "author":{"login":"bob"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},
           "commits":{"nodes":[{"commit":{"committedDate":"2026-08-09T10:00:00Z","author":{"user":{"login":"bob"}}}}]}},
          {"number":6,"title":"Refactor helpers","isDraft":false,
           "updatedAt":"2026-08-07T10:00:00Z","reviewDecision":null,
           "author":{"login":"carol"},
           "comments":{"nodes":[{"author":{"login":"me"},"updatedAt":"2026-08-08T10:00:00Z"}]},
           "reviews":{"nodes":[]},
           "commits":{"nodes":[{"commit":{"committedDate":"2026-08-07T10:00:00Z","author":{"user":{"login":"carol"}}}}]}},
          {"number":5,"title":"WIP draft","isDraft":true,
           "updatedAt":"2026-08-06T10:00:00Z","reviewDecision":null,
           "author":{"login":"dave"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]}},
          {"number":4,"title":"My own PR","isDraft":false,
           "updatedAt":"2026-08-05T10:00:00Z","reviewDecision":null,
           "author":{"login":"me"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]}},
          {"number":3,"title":"Bump lodash","isDraft":false,
           "updatedAt":"2026-08-04T10:00:00Z","reviewDecision":null,
           "author":{"login":"dependabot[bot]"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]}},
          {"number":2,"title":"Approved already","isDraft":false,
           "updatedAt":"2026-08-03T10:00:00Z","reviewDecision":"APPROVED",
           "author":{"login":"erin"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]}}
        ]"#;
        serde_json::from_str(json).unwrap()
    }

    fn filtered(include_approved: bool, include_dependabot: bool) -> Vec<PrNode> {
        fixture()
            .into_iter()
            .filter(|pr| !pr.is_draft)
            .filter(|pr| pr.author_login() != "me")
            .filter(|pr| include_dependabot || !is_bot(pr.author_login()))
            .filter(|pr| include_approved || pr.review_decision.as_deref() != Some("APPROVED"))
            .collect()
    }

    #[test]
    fn filters_match_the_bash_pipeline() {
        let nums: Vec<u64> = filtered(false, false).iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![9, 8, 6]);
        let nums: Vec<u64> = filtered(true, true).iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![9, 8, 6, 3, 2]);
    }

    #[test]
    fn engagement_and_ranking() {
        let prs = filtered(false, false);
        let rows = build_rows(&prs, "me", parse_iso("2026-08-10T12:00:00Z").unwrap());
        let shaped: Vec<(u64, &str)> = rows.iter().map(|r| (r.number, r.engage.label())).collect();
        assert_eq!(shaped, vec![(9, "NEW"), (8, "NEW"), (6, "SEEN")]);
    }

    #[test]
    fn select_auto_takes_new_and_updated_only() {
        let prs = filtered(false, false);
        let rows = build_rows(&prs, "me", parse_iso("2026-08-10T12:00:00Z").unwrap());
        assert_eq!(select_auto(&rows), Some(vec![9, 8]));
    }

    #[test]
    fn updated_beats_seen() {
        // me commented, then carol pushed: UPDATED.
        let json = r#"{"number":7,"title":"t","isDraft":false,
          "updatedAt":"2026-08-09T10:00:00Z","reviewDecision":null,
          "author":{"login":"carol"},
          "comments":{"nodes":[{"author":{"login":"me"},"updatedAt":"2026-08-08T10:00:00Z"}]},
          "reviews":{"nodes":[]},
          "commits":{"nodes":[{"commit":{"committedDate":"2026-08-09T09:00:00Z","author":{"user":{"login":"carol"}}}}]}}"#;
        let pr: PrNode = serde_json::from_str(json).unwrap();
        assert_eq!(engagement(&pr, "me"), Engagement::Updated);
    }

    #[test]
    fn rel_time_shapes() {
        let now = parse_iso("2026-08-10T12:00:00Z").unwrap();
        assert_eq!(rel(now, "2026-08-10T11:59:30Z"), "30s ago");
        assert_eq!(rel(now, "2026-08-10T11:30:00Z"), "30m ago");
        assert_eq!(rel(now, "2026-08-10T07:00:00Z"), "5h ago");
        assert_eq!(rel(now, "2026-08-07T12:00:00Z"), "3d ago");
    }

    #[test]
    fn graphql_errors_do_not_read_as_an_empty_repo() {
        // A 200 carrying errors, and a response missing the data path: both
        // must refuse, or an unattended sweep exits 0 having reviewed nothing.
        let with_errors: serde_json::Value =
            serde_json::from_str(r#"{"errors":[{"message":"rate limited"}],"data":null}"#).unwrap();
        assert!(extract_nodes(&with_errors).is_err());

        let missing_path: serde_json::Value = serde_json::from_str(r#"{"data":{}}"#).unwrap();
        assert!(extract_nodes(&missing_path).is_err());

        let ok: serde_json::Value = serde_json::from_str(
            r#"{"data":{"repository":{"pullRequests":{"nodes":[]}}}}"#,
        )
        .unwrap();
        assert_eq!(extract_nodes(&ok).unwrap().len(), 0);
    }

    #[test]
    fn iso_parsing_is_correct_around_epoch_math() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        // Cross-checked against `date -u -j -f ... +%s` on this machine.
        assert_eq!(parse_iso("2026-08-10T10:00:00Z"), Some(1_786_356_000));
        assert_eq!(parse_iso("not a date"), None);
    }
}
