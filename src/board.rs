//! The live area of a pass on a terminal: the rows that change, drawn in
//! place, with the terminal in raw mode for as long as they are up.
//!
//! An inline viewport, not a full screen. Finished rows are inserted above
//! the live area and scroll away like ordinary output; the live area is the
//! last few rows and nothing else. On a resize ratatui asks the terminal
//! where the cursor is, recomputes the area from that, and clears it -- which
//! is the one thing a line-counting redraw cannot do, since the count it
//! remembers was taken at a width the terminal no longer has.
//!
//! Events are read on the caller's thread, never on a reader thread of their
//! own. crossterm's cursor query shares a lock with its event reader and
//! waits at most two seconds for it; a thread parked in `read()` holds that
//! lock, and the query is what re-anchors the viewport on every resize. A
//! reader thread would break the path this module exists for.

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{cursor, execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget;
use ratatui::{Terminal, TerminalOptions, Viewport};
use std::io::{self, Stdout};
use std::sync::Once;
use std::time::Duration;

/// The width to assume when the terminal will not say. Matches what console
/// falls back to, so the two never disagree.
pub const ASSUMED_WIDTH: usize = 80;

/// What a key asks the pass to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Stop the reviews and print the summary, as ctrl-C did before raw
    /// mode turned it into a key.
    Stop,
    /// Show every running row's details, or hide them all if any are shown.
    ToggleAll,
    /// Show or hide one row's details, by its position on the board from 1.
    Toggle(usize),
    /// Hide every row's details.
    Collapse,
}

/// The keys the board answers to. A pure function, so the table is testable
/// without a terminal.
pub fn key_to_action(key: KeyEvent) -> Option<Action> {
    // Release and repeat events only arrive from terminals that report them;
    // a key held down must not stop the pass twice.
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(Action::Stop);
    }
    if !key.modifiers.is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Char('q') => Some(Action::Stop),
        KeyCode::Char(' ') | KeyCode::Enter => Some(Action::ToggleAll),
        KeyCode::Char(d @ '1'..='9') => Some(Action::Toggle(d as usize - '0' as usize)),
        KeyCode::Esc => Some(Action::Collapse),
        _ => None,
    }
}

/// Put the terminal back the way a shell expects it. Safe to call when raw
/// mode was never turned on: crossterm only restores a mode it saved.
fn restore_terminal() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), cursor::Show);
}

/// A panic while the board is up would print its message in raw mode, over
/// the rows, and leave the shell without echo. The hook restores the terminal
/// first and then lets the default hook say what happened. Installed once per
/// process and left in place: it is harmless when no board is open.
fn install_panic_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            previous(info);
        }));
    });
}

fn open_terminal(height: u16) -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions { viewport: Viewport::Inline(height) },
    )
}

pub struct Board {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    height: u16,
}

impl Board {
    /// Take the terminal: raw mode on, cursor hidden, a viewport of `height`
    /// rows anchored at the cursor. Ratatui scrolls the screen up if the
    /// cursor is too close to the bottom for that many rows.
    pub fn open(height: u16) -> io::Result<Board> {
        install_panic_hook();
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), cursor::Hide)?;
        let terminal = match open_terminal(height.max(1)) {
            Ok(t) => t,
            Err(e) => {
                restore_terminal();
                return Err(e);
            }
        };
        Ok(Board { terminal, height: height.max(1) })
    }

    /// The terminal's width, or what to assume when it will not say.
    pub fn width(&self) -> usize {
        self.terminal.size().map_or(ASSUMED_WIDTH, |s| s.width as usize)
    }

    /// Grow the live area to `height` rows. It never shrinks: a row that
    /// finishes leaves a blank row under the footer until the pass ends,
    /// which costs nothing to look at, where rebuilding the viewport on every
    /// finish would cost a cursor query and a clear each time.
    ///
    /// Growing rebuilds the terminal. The inline height is fixed when the
    /// viewport is made, so the old area is cleared, the cursor put back on
    /// its top row, and a new viewport opened there at the new height.
    pub fn ensure_height(&mut self, height: u16) -> io::Result<()> {
        if height <= self.height {
            return Ok(());
        }
        let top = self.terminal.get_frame().area().y;
        self.terminal.clear()?;
        execute!(io::stdout(), cursor::MoveTo(0, top))?;
        self.terminal = open_terminal(height)?;
        self.height = height;
        Ok(())
    }

    /// A permanent line above the live area. It scrolls away with the rest
    /// of the output, which is what a finished review's result line is for.
    pub fn println(&mut self, line: Line<'static>) -> io::Result<()> {
        self.terminal.insert_before(1, |buf| line.render(buf.area, buf))
    }

    /// Draw the live area: one line per row, top to bottom, the rest blank.
    /// Ratatui diffs against the last frame, so a tick that changed one
    /// spinner cell writes one spinner cell.
    pub fn draw(&mut self, lines: &[Line<'static>]) -> io::Result<()> {
        self.terminal.draw(|frame| {
            let area = frame.area();
            for (i, line) in lines.iter().enumerate().take(area.height as usize) {
                let row = Rect { x: area.x, y: area.y + i as u16, width: area.width, height: 1 };
                frame.render_widget(line, row);
            }
        })?;
        Ok(())
    }

    /// Every key and resize that arrived since the last call. Read here, on
    /// the caller's thread, for the reason in the module comment.
    pub fn events(&self) -> Vec<Event> {
        let mut out = Vec::new();
        while event::poll(Duration::ZERO).unwrap_or(false) {
            match event::read() {
                Ok(e) => out.push(e),
                Err(_) => break,
            }
        }
        out
    }

    /// Give the terminal back, with the cursor on the row the live area
    /// started on, so whatever prints next lands where the board was.
    pub fn close(self) {
        drop(self);
    }
}

/// Every path out restores the terminal: `close`, a `?` that drops the `Ui`
/// holding this, and the interrupt path that tears the board down before it
/// prints. A panic is the panic hook's job.
impl Drop for Board {
    fn drop(&mut self) {
        let top = self.terminal.get_frame().area().y;
        let _ = self.terminal.clear();
        let _ = execute!(io::stdout(), cursor::MoveTo(0, top));
        restore_terminal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn the_keys_that_stop_a_pass() {
        assert_eq!(key_to_action(press(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(Action::Stop));
        assert_eq!(key_to_action(press(KeyCode::Char('q'), KeyModifiers::NONE)), Some(Action::Stop));
        // A plain c is not an interrupt, and a shifted q is not a quit.
        assert_eq!(key_to_action(press(KeyCode::Char('c'), KeyModifiers::NONE)), None);
        assert_eq!(key_to_action(press(KeyCode::Char('q'), KeyModifiers::SHIFT)), None);
    }

    #[test]
    fn the_keys_that_show_details() {
        assert_eq!(key_to_action(press(KeyCode::Char(' '), KeyModifiers::NONE)), Some(Action::ToggleAll));
        assert_eq!(key_to_action(press(KeyCode::Enter, KeyModifiers::NONE)), Some(Action::ToggleAll));
        assert_eq!(key_to_action(press(KeyCode::Char('1'), KeyModifiers::NONE)), Some(Action::Toggle(1)));
        assert_eq!(key_to_action(press(KeyCode::Char('9'), KeyModifiers::NONE)), Some(Action::Toggle(9)));
        assert_eq!(key_to_action(press(KeyCode::Char('0'), KeyModifiers::NONE)), None);
        assert_eq!(key_to_action(press(KeyCode::Esc, KeyModifiers::NONE)), Some(Action::Collapse));
        // A modifier on a letter is some other binding, not this one.
        assert_eq!(key_to_action(press(KeyCode::Char(' '), KeyModifiers::ALT)), None);
        assert_eq!(key_to_action(press(KeyCode::Char('x'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn a_key_release_does_nothing() {
        let mut release = press(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(key_to_action(release), None);
    }
}
