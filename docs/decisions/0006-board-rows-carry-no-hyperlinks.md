# Board rows carry no hyperlinks

Recorded: 2026-09-01
Status: accepted

## Context

Every `#N` in the summary is an OSC 8 hyperlink, so a terminal that supports
them opens the PR on cmd-click. The board rows carried the same link.

indicatif measures each row it redraws with `console::measure_text_width`,
which strips SGR colour but not OSC 8. A linked `#1711` measures 54 columns
where it draws 5. Every linked row was believed to wrap, the move-up count on
redraw was wrong, and the board climbed the screen overwriting scrollback.

## Decision

Board rows go through `board_label`, which returns plain `#N`. The summary
tables are a plain `println!` that indicatif never measures, so they link
freely. A unit test asserts that no board row contains an OSC 8 sequence.

## Consequences

- Any new board call site goes through `board_label`, so the links cannot
  come back one site at a time.
- Rows are sized so the fixed parts (number, verb, clock) always fit and only
  the title shrinks. A row too narrow for a title drops it rather than
  shaving it.
- The same class of bug, a stale line count on redraw, is what a terminal
  resize triggered. Decision 0015 replaced the renderer for that reason. The
  rule here stays: the board says the number plain, and the summary is the
  one place it links.
