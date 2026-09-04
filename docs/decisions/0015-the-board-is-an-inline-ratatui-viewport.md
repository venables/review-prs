# The board is an inline ratatui viewport

Recorded: 2026-09-01
Status: accepted

## Context

The first board was indicatif's `MultiProgress`. It redraws by moving the
cursor up by the number of rows the previous draw took, counted at the
width the terminal had then. A resize makes the terminal reflow those rows,
the count goes stale, the redraw lands on the wrong rows, and the old board
stays on screen. There was no resize handling to add: the design has no way
to learn where its rows went. It also had no way to read a key, which the
next step (a key that expands a running row) needs.

## Decision

The live area is a ratatui `Viewport::Inline` over crossterm, in
`src/board.rs`. `src/ui.rs` still decides what every row says and how wide
it may be; the board decides where it goes.

- Inline, not full screen. Finished rows go above the live area with
  `insert_before` and scroll away like ordinary output.
- The board rebuilds itself on a resize, before ratatui can notice one: it
  clears from the live area's first row down and opens a new viewport there,
  so everything above survives. Left to itself, ratatui clears the entire
  screen whenever the terminal gets narrower and starts the viewport again
  at the top row, which takes the header and every finished review with it.
- Which row the live area starts on is asked, not remembered. A terminal
  that rewraps its lines when it changes width moves everything below the
  rewrapped ones: widen one and the header takes fewer rows, so the area
  rides up. Clearing the row it was drawn on then leaves the real rows
  stranded above the new ones, with their clocks stopped. The cursor is
  parked on the area's first row after every draw and the terminal carries
  it along, which is what makes it answerable.
- The lower of that answer and the remembered row wins, the remembered one
  adjusted by any height the terminal lost, which is how far a screen that
  only scrolled has moved. A terminal that rewraps without carrying the
  cursor would stand its old rows behind otherwise, and that is worth a
  clipped header line to avoid.
- Raw mode is on only while a board is open. `end_pass` turns it off before
  anything else prints, and a panic hook turns it off before a panic prints.
- Events are polled on the main thread after every wake of the pass loop.
  There is no reader thread: crossterm's cursor query shares a lock with its
  event reader and waits two seconds for it, and a thread parked in `read()`
  holds that lock. The query is what re-anchors the viewport on a resize.
- The live area only grows. Growing rebuilds the viewport at the new height
  on the same top row; a row that finishes leaves a blank row under the
  footer until the pass ends.
- A terminal that does not answer the cursor query within two seconds gets
  the pass as plain lines, with a note. `script(1)` is such a terminal. That
  is decided when the board opens. A query that fails later, on a resize,
  falls back to the remembered row instead: a board that is already up is
  worth more than an exact row, and the terminal answered once to get here.
- `q` and ctrl-C both take the interrupt path. In raw mode ctrl-C is a key,
  not a signal; TERM and HUP still arrive as signals.

Ratatui's `scrolling-regions` feature is not enabled. It turns the insert
above the live area into a terminal scroll rather than a redraw, which is
faster and needs `DECSTBM` plus scroll-up and scroll-down. A terminal
missing any of those draws each finished row over the row above it instead.
A handful of inserts per pass is not worth a class of terminal bug.

## Consequences

- Nothing prints with `println!` while a board is open. Mid-pass lines go
  through `ui.note`, which inserts them above the live area.
- The suite tests the board through `tests/pty.py`, a driver that answers
  the cursor query, resizes the pty mid-pass and presses keys. Whether the
  terminal came back cooked is read from a `stty -a` run inside the session,
  because macOS revokes the pty when its session leader exits. Its answer to
  the cursor query is an estimate, so the suite pins what the board writes
  rather than where a row lands: chiefly that a run never emits a
  full-screen clear, which is what a resize used to cost.
- Board rows still carry a plain `#N`. The summary tables are the one place a
  number links.
- The startup `Status` spinner stays on indicatif: one line on stderr, no
  rows to lose. `panel`'s fan-out uses the same single line and does not have
  a board yet.
