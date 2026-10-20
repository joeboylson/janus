//! Live views, as in legion: redrawn every 2 seconds until q. Without a
//! terminal a view prints once and ends.

use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, queue,
    terminal::{self, ClearType},
};
use std::{io::Write, time::Duration};

pub const REDRAW_EVERY: Duration = Duration::from_secs(2);
const FOOTER: &str = "q to quit · updates every 2 seconds";

/// Whether a key quits: q, Esc, or Ctrl-C (raw mode turns it into a key).
pub fn is_quit(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc) || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// Whether q was pressed within `wait`.
fn quit_within(wait: Duration) -> Result<bool> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if !event::poll(left)? {
            return Ok(false);
        }
        if let Event::Key(key) = event::read()? {
            if is_quit(&key) {
                return Ok(true);
            }
        }
    }
}

/// The terminal handed over to a live view; given back when dropped.
pub struct Live;

impl Live {
    fn whole_screen() -> Result<Live> {
        terminal::enable_raw_mode()?;
        execute!(std::io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
        Ok(Live)
    }

    /// Draws a whole screen, then waits up to 2 seconds. False once q is pressed.
    fn show(&self, text: &str) -> Result<bool> {
        let mut out = std::io::stdout();
        queue!(out, cursor::MoveTo(0, 0), terminal::Clear(ClearType::All))?;
        write!(out, "{}\r\n\r\n{}", text.trim_end().replace('\n', "\r\n"), console::style(FOOTER).dim())?;
        out.flush()?;
        quit_within(REDRAW_EVERY).map(|is_quit| !is_quit)
    }
}

/// A whole-screen live view when `watch` is asked for and there's a terminal.
pub fn start(watch: bool, is_interactive: bool) -> Result<Option<Live>> {
    match watch && is_interactive {
        true => Live::whole_screen().map(Some),
        false => Ok(None),
    }
}

/// Shows a view: printed once without a live one, otherwise drawn and held
/// for 2 seconds. True while it should be drawn again.
pub fn present(live: Option<&Live>, text: &str) -> Result<bool> {
    match live {
        None => {
            print!("{text}");
            Ok(false)
        }
        Some(view) => view.show(text),
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_esc_and_ctrl_c_quit() {
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        assert!(is_quit(&key(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(is_quit(&key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(is_quit(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(!is_quit(&key(KeyCode::Char('c'), KeyModifiers::NONE)));
    }
}
