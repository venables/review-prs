# A running review is followed through its transcript

Recorded: 2026-09-01
Status: accepted

## Context

A running row said `reviewing 1m47s` and nothing else for the length of a
review, because nothing else was there to say. dash-p writes nothing to
stderr in json mode and its answer lands in `pr-N.json` when the review
ends. The one file written as the review goes is the session transcript
Claude Code keeps, one JSON line per block: a tool call with its input, the
text the reviewer wrote, the thinking between them. This repo already
locates that file after a review to read the trailer and the review text.

dash-p's `stream-json` output was the other option. It was not taken: it
would replace the json answer the summary reads with a stream to parse, and
it is per message, not per token, so it says nothing the transcript does
not.

## Decision

Each running job carries a `Tail` (`src/activity.rs`) that follows one file
incrementally: one stat per tick, and only the bytes past the last read are
parsed, with a partial last line kept for the next poll.

- The built-in reviewer in a session this run named (pinned or resumed) is
  followed through that session's transcript. The file is looked for at
  most once a second until it appears.
- A session claude named itself has no known transcript until the review
  ends. Its row says so instead of following nothing.
- A command override has no transcript. Its row follows the reviewer's
  stderr, one event per line, because that is the only live channel it has.
- Only assistant lines count. A tool call becomes `Bash cargo test --quiet`,
  a text block its first line; thinking is skipped. Entries older than the
  job's start belong to the review being resumed, not this run.
- Summaries take the part of a tool's input a reader wants: a command's
  description or first line, a file's basename, a pattern, a skill's name.
  Every summary passes through the same display sanitizer as a PR title.
- A key expands a row: `space` and `enter` every running row, `1` to `9` one
  row by position, `esc` none. The block under a row is at most six lines:
  what is followed, the counts, the last four events with their ages. When
  the width allows, the row itself ends with the tool the review is in.

## Consequences

- The transcript format is not documented. The parser is lenient: a line of
  another shape reads as nothing, never as an error, and a format change
  costs an empty block rather than a broken board.
- Live detail exists only for the built-in reviewer in a session this run
  named. That is the common case for a sweep; the picker's fresh sessions
  and overrides fall back as above.
- The board grows to hold an expanded row and does not shrink again until
  the pass ends, which is the inline viewport's shape.
- The suite proves the path end to end: the fake dash-p writes a transcript
  under the sandbox session store, and the pty test presses space and reads
  the block back.
