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
  `insert_before` and scroll away like ordinary output. The `scrolling-regions`
  feature makes that a scroll instead of a redraw.
- On a resize ratatui asks the terminal where the cursor is, recomputes the
  area from the answer, and clears it before the next draw. A narrower
  terminal clears the whole screen and the board starts again at the top.
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
  the pass as plain lines, with a note. `script(1)` is such a terminal.
- `q` and ctrl-C both take the interrupt path. In raw mode ctrl-C is a key,
  not a signal; TERM and HUP still arrive as signals.

## Consequences

- Nothing prints with `println!` while a board is open. Mid-pass lines go
  through `ui.note`, which inserts them above the live area.
- The suite tests the board through `tests/pty.py`, a driver that answers
  the cursor query, resizes the pty mid-pass and presses `q`. Whether the
  terminal came back cooked is read from a `stty -a` run inside the session,
  because macOS revokes the pty when its session leader exits.
- Board rows still carry a plain `#N`. The summary tables are the one place a
  number links.
- The startup `Status` spinner stays on indicatif: one line on stderr, no
  rows to lose. `panel`'s fan-out uses the same single line and does not have
  a board yet.
