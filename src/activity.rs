//! What a running review is doing right now.
//!
//! dash-p says nothing while a review runs: its stderr is silent in json
//! mode and its answer lands when the review ends. The one thing written as
//! the review goes is the session transcript Claude Code keeps, one JSON
//! line per block -- a tool call with its input, the text the reviewer
//! wrote, the thinking in between. Following that file is how a row on the
//! board can say "Bash cargo test" instead of only "reviewing 1m47s".
//!
//! Read incrementally: each poll is one stat, and only the bytes past the
//! last read are parsed. The file may not exist for the first seconds of a
//! job, and never exists for a reviewer that is not claude, so a tail knows
//! how to have nothing to say.

use crate::report::{sanitize_for_display, transcript_epoch};
use serde::Deserialize;
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How many recent events a tail keeps. The details view shows fewer; the
/// rest is slack for a burst between two draws.
pub const KEEP: usize = 8;
/// The most columns a summary of one event may take. The details line has
/// an age and a tool name to fit as well.
const WHAT_WIDTH: usize = 48;
/// How often to look for a transcript that has not appeared yet. The lookup
/// walks every Claude Code project directory, which is not a per-tick cost.
const LOOKUP_EVERY: Duration = Duration::from_secs(1);

/// One thing the reviewer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The tool called, or None for something the reviewer said.
    pub tool: Option<String>,
    /// A one-line summary: the command, the file, the first line of text.
    pub what: String,
    /// When the transcript says it happened, as an epoch second.
    pub at: Option<i64>,
}

#[derive(Debug)]
enum Source {
    /// Nothing to follow, and the reason in words the details view shows.
    None(&'static str),
    /// Claude's transcript for a session. The path is found once the file
    /// exists; until then the tail looks for it, at most once a second.
    Transcript { sid: String, path: Option<PathBuf>, looked: Option<Instant> },
    /// A plain log: every line is an event.
    Plain(PathBuf),
}

#[derive(Debug)]
pub struct Tail {
    source: Source,
    /// Start at the end of the file when it is first seen. A resumed
    /// session's transcript already holds every earlier pass, which `since`
    /// would filter out one parsed line at a time; skipping it is the
    /// difference between one stat and re-reading the history on every
    /// pass of a run that lasts all day.
    from_end: bool,
    offset: u64,
    /// The bytes after the last newline read: a line the writer had not
    /// finished when the poll happened.
    partial: String,
    /// Entries older than this are another run's. A resumed session's
    /// transcript starts with the review it is resuming.
    since: i64,
    pub events: VecDeque<Event>,
    /// Assistant lines that said or did something; thinking is not a turn.
    pub turns: u32,
    pub tool_calls: u32,
}

impl Tail {
    fn new(source: Source, since: i64) -> Tail {
        Tail {
            source,
            from_end: false,
            offset: 0,
            partial: String::new(),
            since,
            events: VecDeque::new(),
            turns: 0,
            tool_calls: 0,
        }
    }

    /// Follow from the end of the file as it is when first seen, not from
    /// its start. For a session that existed before this run: what was
    /// written before is another pass's review.
    pub fn from_end(mut self) -> Tail {
        self.from_end = true;
        self
    }

    /// A review with nothing to follow: an override reviewer, or a session
    /// claude named itself, whose transcript is only found when it ends.
    pub fn silent(why: &'static str) -> Tail {
        Tail::new(Source::None(why), 0)
    }

    /// Follow the transcript of `sid`, from `since` on.
    pub fn transcript(sid: String, since: i64) -> Tail {
        Tail::new(Source::Transcript { sid, path: None, looked: None }, since)
    }

    /// Follow a transcript already located, from `since` on.
    pub fn transcript_at(path: PathBuf, since: i64) -> Tail {
        Tail::new(Source::Transcript { sid: String::new(), path: Some(path), looked: None }, since)
    }

    /// Follow a plain log, one event per line.
    pub fn plain(path: PathBuf) -> Tail {
        Tail::new(Source::Plain(path), 0)
    }

    /// Why there is nothing to follow, when there is nothing.
    pub fn why_silent(&self) -> Option<&'static str> {
        match &self.source {
            Source::None(why) => Some(why),
            _ => None,
        }
    }

    /// The session being followed, when it is one.
    pub fn session(&self) -> Option<&str> {
        match &self.source {
            Source::Transcript { sid, .. } if !sid.is_empty() => Some(sid),
            _ => None,
        }
    }

    /// What is being followed, in the words the details view opens with.
    pub fn source_label(&self) -> String {
        match &self.source {
            Source::None(why) => (*why).to_string(),
            Source::Transcript { sid, .. } if self.found() => format!("session {sid}"),
            Source::Transcript { sid, .. } => format!("session {sid} · waiting for its transcript"),
            Source::Plain(_) => "following the reviewer's stderr".to_string(),
        }
    }

    /// True once the file has been seen at all.
    pub fn found(&self) -> bool {
        match &self.source {
            Source::None(_) => false,
            Source::Transcript { path, .. } => path.is_some() && self.offset > 0,
            Source::Plain(_) => self.offset > 0,
        }
    }

    /// The tool the reviewer is in right now: the last event, when it was a
    /// tool call. Text after a tool call means the call is over.
    pub fn current_tool(&self) -> Option<&str> {
        self.events.back().and_then(|e| e.tool.as_deref())
    }

    /// Read what was written since the last poll. Cheap when nothing was: one
    /// stat, no open.
    pub fn poll(&mut self) {
        let path = match &mut self.source {
            Source::None(_) => return,
            Source::Plain(path) => path.clone(),
            Source::Transcript { sid, path, looked } => {
                if path.is_none() {
                    let due = looked.is_none_or(|t| t.elapsed() >= LOOKUP_EVERY);
                    if !due {
                        return;
                    }
                    *looked = Some(Instant::now());
                    *path = crate::session::transcript_path(sid);
                }
                match path {
                    Some(p) => p.clone(),
                    None => return,
                }
            }
        };
        let Ok(meta) = std::fs::metadata(&path) else { return };
        let len = meta.len();
        if self.from_end {
            // Seen for the first time: everything in it so far is history.
            // A line the writer is in the middle of reads as a fragment
            // that parses as nothing, which is what a fragment should be.
            self.from_end = false;
            self.offset = len;
            return;
        }
        if len < self.offset {
            // Shorter than last time: a new file under the same name. Start
            // over rather than read from the middle of a line.
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return;
        }
        let Some(text) = read_from(&path, self.offset) else { return };
        self.offset = len;
        self.feed(&text);
    }

    /// Take a chunk of the file: whole lines become events, the rest waits
    /// for the next chunk.
    pub(crate) fn feed(&mut self, text: &str) {
        let mut buf = std::mem::take(&mut self.partial);
        buf.push_str(text);
        let mut rest = buf.as_str();
        while let Some((line, after)) = rest.split_once('\n') {
            self.take_line(line);
            rest = after;
        }
        self.partial = rest.to_string();
    }

    fn take_line(&mut self, line: &str) {
        let events = match &self.source {
            Source::Plain(_) => {
                let what = cut(&sanitize_for_display(line), WHAT_WIDTH);
                if what.trim().is_empty() {
                    return;
                }
                vec![Event { tool: None, what, at: None }]
            }
            _ => parse_transcript_line(line, self.since),
        };
        if events.is_empty() {
            return;
        }
        self.turns += 1;
        for event in events {
            if event.tool.is_some() {
                self.tool_calls += 1;
            }
            if self.events.len() == KEEP {
                self.events.pop_front();
            }
            self.events.push_back(event);
        }
    }
}

fn read_from(path: &Path, offset: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The slice of a transcript line the tail reads. Everything else on the
/// line -- cwd, uuids, the git branch -- is ignored, and a line of another
/// shape reads as nothing rather than as an error.
#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<serde_json::Value>,
}

/// The events one transcript line carries: none for anything but an
/// assistant line, and none for one written before `since`.
pub fn parse_transcript_line(line: &str, since: i64) -> Vec<Event> {
    let Ok(entry) = serde_json::from_str::<Line>(line) else {
        return Vec::new();
    };
    if entry.kind != "assistant" {
        return Vec::new();
    }
    let at = entry.timestamp.as_deref().and_then(transcript_epoch);
    if at.is_some_and(|t| t < since) {
        return Vec::new();
    }
    let Some(blocks) = entry.message.and_then(|m| m.content).and_then(|c| c.as_array().cloned()) else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| {
            let kind = block.get("type").and_then(|t| t.as_str())?;
            match kind {
                "tool_use" => {
                    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                    let input = block.get("input").cloned().unwrap_or(serde_json::Value::Null);
                    Some(Event { tool: Some(name.to_string()), what: summarize(name, &input), at })
                }
                "text" => {
                    let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    let first = text.lines().map(str::trim).find(|l| !l.is_empty())?;
                    Some(Event { tool: None, what: cut(&sanitize_for_display(first), WHAT_WIDTH), at })
                }
                _ => None,
            }
        })
        .collect()
}

/// One line about a tool call, from the part of its input a reader would
/// want: the command, the file, the pattern, the skill. Tools this does not
/// know are named and nothing more.
pub fn summarize(name: &str, input: &serde_json::Value) -> String {
    let field = |key: &str| input.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
    let raw = match name {
        // A description is what the reviewer meant; the command is what it
        // ran. The first says more in fewer columns.
        "Bash" => field("description").or_else(|| field("command").and_then(|c| c.lines().next())),
        "Read" | "Edit" | "Write" | "MultiEdit" => field("file_path").map(basename),
        "NotebookEdit" => field("notebook_path").map(basename),
        "Grep" | "Glob" => field("pattern"),
        "Skill" => field("skill"),
        "Agent" | "Task" => field("description"),
        "WebFetch" => field("url"),
        "WebSearch" | "ToolSearch" => field("query"),
        _ => None,
    };
    raw.map(|s| cut(&sanitize_for_display(s), WHAT_WIDTH)).unwrap_or_default()
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn cut(s: &str, width: usize) -> String {
    console::truncate_str(s, width, "…").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(at: &str, block: serde_json::Value) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": at,
            "cwd": "/tmp/x",
            "message": { "role": "assistant", "content": [block] }
        })
        .to_string()
    }

    fn tool(name: &str, input: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "type": "tool_use", "id": "toolu_1", "name": name, "input": input })
    }

    const AT: &str = "2026-09-01T11:00:05.123Z";

    #[test]
    fn a_tool_call_is_summarized_by_what_a_reader_wants() {
        let cases = [
            (tool("Bash", serde_json::json!({"command": "cargo test --quiet\necho done", "description": "Run the unit tests"})), "Bash", "Run the unit tests"),
            (tool("Bash", serde_json::json!({"command": "cargo test --quiet\necho done"})), "Bash", "cargo test --quiet"),
            (tool("Read", serde_json::json!({"file_path": "/repo/src/pool.rs"})), "Read", "pool.rs"),
            (tool("Edit", serde_json::json!({"file_path": "/repo/src/ui.rs", "old_string": "a", "new_string": "b"})), "Edit", "ui.rs"),
            (tool("Grep", serde_json::json!({"pattern": "fn spawn", "path": "src"})), "Grep", "fn spawn"),
            (tool("Skill", serde_json::json!({"skill": "panel-review", "args": "9"})), "Skill", "panel-review"),
            (tool("Agent", serde_json::json!({"description": "Verify the retry path", "prompt": "..."})), "Agent", "Verify the retry path"),
            (tool("mcp__slack__send", serde_json::json!({"channel": "c"})), "mcp__slack__send", ""),
        ];
        for (block, name, what) in cases {
            let events = parse_transcript_line(&assistant(AT, block), 0);
            assert_eq!(events.len(), 1, "{name}");
            assert_eq!(events[0].tool.as_deref(), Some(name));
            assert_eq!(events[0].what, what, "{name}");
            assert!(events[0].at.is_some());
        }
    }

    #[test]
    fn text_is_its_first_line_and_thinking_is_nothing() {
        let said = assistant(AT, serde_json::json!({"type": "text", "text": "\n\nLooking at the diff first.\nThen the tests."}));
        let events = parse_transcript_line(&said, 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool, None);
        assert_eq!(events[0].what, "Looking at the diff first.");
        let thought = assistant(AT, serde_json::json!({"type": "thinking", "thinking": "hmm"}));
        assert!(parse_transcript_line(&thought, 0).is_empty());
    }

    #[test]
    fn lines_of_other_shapes_read_as_nothing() {
        let user = serde_json::json!({"type": "user", "timestamp": AT, "message": {"role": "user", "content": [{"type": "tool_result", "content": "ok"}]}}).to_string();
        assert!(parse_transcript_line(&user, 0).is_empty());
        let plain = serde_json::json!({"type": "user", "message": {"role": "user", "content": "/auto-review 9"}}).to_string();
        assert!(parse_transcript_line(&plain, 0).is_empty());
        let latch = serde_json::json!({"type": "atis-latch", "atis": {}}).to_string();
        assert!(parse_transcript_line(&latch, 0).is_empty());
        assert!(parse_transcript_line("not json at all", 0).is_empty());
        assert!(parse_transcript_line("", 0).is_empty());
    }

    #[test]
    fn an_older_entry_belongs_to_another_run() {
        // 2026-09-01T11:00:05Z is 1_788_260_405; a resumed session's file
        // starts with the review being resumed, which is not this run's
        // activity.
        let line = assistant(AT, tool("Read", serde_json::json!({"file_path": "a.rs"})));
        assert_eq!(parse_transcript_line(&line, 1_788_260_405).len(), 1);
        assert!(parse_transcript_line(&line, 1_788_260_406).is_empty());
    }

    #[test]
    fn summaries_lose_their_control_bytes_and_their_length() {
        let block = tool("Bash", serde_json::json!({"command": "echo \u{1b}[31mred\u{1b}[0m"}));
        let events = parse_transcript_line(&assistant(AT, block), 0);
        assert_eq!(events[0].what, "echo [31mred[0m");
        let long = "x".repeat(200);
        let block = tool("Bash", serde_json::json!({"command": long}));
        let events = parse_transcript_line(&assistant(AT, block), 0);
        assert_eq!(console::measure_text_width(&events[0].what), WHAT_WIDTH);
    }

    #[test]
    fn a_line_split_across_two_polls_is_read_whole() {
        let mut tail = Tail::transcript_at(PathBuf::from("/nonexistent"), 0);
        let line = assistant(AT, tool("Read", serde_json::json!({"file_path": "a.rs"})));
        let (head, rest) = line.split_at(20);
        tail.feed(head);
        assert!(tail.events.is_empty(), "half a line is not an event");
        tail.feed(&format!("{rest}\n"));
        assert_eq!(tail.events.len(), 1);
        assert_eq!(tail.current_tool(), Some("Read"));
        assert_eq!((tail.turns, tail.tool_calls), (1, 1));
    }

    #[test]
    fn the_tail_keeps_the_last_events_only() {
        let mut tail = Tail::transcript_at(PathBuf::from("/nonexistent"), 0);
        for i in 0..(KEEP + 3) {
            let line = assistant(AT, tool("Bash", serde_json::json!({"command": format!("step {i}")})));
            tail.feed(&format!("{line}\n"));
        }
        assert_eq!(tail.events.len(), KEEP);
        assert_eq!(tail.events.back().unwrap().what, format!("step {}", KEEP + 2));
        assert_eq!(tail.tool_calls, (KEEP + 3) as u32);
    }

    #[test]
    fn text_after_a_tool_call_ends_it() {
        let mut tail = Tail::transcript_at(PathBuf::from("/nonexistent"), 0);
        tail.feed(&format!("{}\n", assistant(AT, tool("Bash", serde_json::json!({"command": "ls"})))));
        assert_eq!(tail.current_tool(), Some("Bash"));
        tail.feed(&format!("{}\n", assistant(AT, serde_json::json!({"type": "text", "text": "done"}))));
        assert_eq!(tail.current_tool(), None);
    }

    #[test]
    fn a_file_is_followed_from_where_the_last_poll_left_off() {
        let dir = std::env::temp_dir().join(format!("ar-activity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let mut tail = Tail::transcript_at(path.clone(), 0);
        // Not there yet: the reviewer has not started writing.
        tail.poll();
        assert!(tail.events.is_empty() && !tail.found());
        std::fs::write(&path, format!("{}\n", assistant(AT, tool("Read", serde_json::json!({"file_path": "a.rs"}))))).unwrap();
        tail.poll();
        assert_eq!(tail.events.len(), 1);
        assert!(tail.found());
        // Nothing new: nothing read.
        tail.poll();
        assert_eq!(tail.events.len(), 1);
        // Appended: only the new line is read.
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut f, format!("{}\n", assistant(AT, tool("Grep", serde_json::json!({"pattern": "x"})))).as_bytes()).unwrap();
        tail.poll();
        assert_eq!(tail.events.len(), 2);
        assert_eq!(tail.current_tool(), Some("Grep"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_resumed_transcript_is_followed_from_its_end() {
        let dir = std::env::temp_dir().join(format!("ar-activity-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        // Two passes of history, written before this run started.
        let old = assistant("2026-09-01T10:00:00.000Z", tool("Read", serde_json::json!({"file_path": "old.rs"})));
        std::fs::write(&path, format!("{old}\n{old}\n")).unwrap();
        let mut tail = Tail::transcript_at(path.clone(), 0).from_end();
        tail.poll();
        assert!(tail.events.is_empty(), "history is skipped, not parsed");
        assert!(tail.found());
        // What this run writes is read as it lands.
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut f, format!("{}\n", assistant(AT, tool("Grep", serde_json::json!({"pattern": "new"})))).as_bytes()).unwrap();
        tail.poll();
        assert_eq!(tail.events.len(), 1);
        assert_eq!(tail.current_tool(), Some("Grep"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plain_log_is_one_event_per_line() {
        let dir = std::env::temp_dir().join(format!("ar-activity-plain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pr-9.log");
        std::fs::write(&path, "checking out\n\nrunning the suite\n").unwrap();
        let mut tail = Tail::plain(path);
        tail.poll();
        let whats: Vec<&str> = tail.events.iter().map(|e| e.what.as_str()).collect();
        assert_eq!(whats, vec!["checking out", "running the suite"]);
        assert_eq!(tail.current_tool(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_silent_tail_says_why() {
        let tail = Tail::silent("the reviewer is not claude");
        assert_eq!(tail.why_silent(), Some("the reviewer is not claude"));
        assert!(Tail::plain(PathBuf::from("/x")).why_silent().is_none());
        assert_eq!(Tail::transcript("abc".into(), 0).session(), Some("abc"));
    }
}
