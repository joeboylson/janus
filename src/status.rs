//! `janus status`: everything on one page. Which account is in use and why,
//! how much of each limit is left and when it resets, how long each saved
//! login lasts, and what the watcher is doing.

use crate::ask::Asker;
use crate::auto;
use crate::claude::{self, Store};
use crate::live::{present, start};
use crate::store;
use crate::usage::{self, Window};
use anyhow::Result;
use chrono::{DateTime, Utc};
use console::{style, Color, Style};
use serde_json::Value;
use std::fmt::Write;

/// Usage is only fetched again once it's this old, about as often as the
/// watcher fetches it; --watch redraws from what's saved in between.
const FETCH_AFTER_SECS: i64 = 110;

/// A login this close to expiring gets a warning.
const LOGIN_WARN_DAYS: i64 = 3;

pub fn cmd_status(ask: &Asker, watch: bool) -> Result<i32> {
    let live = start(watch, ask.is_interactive())?;
    loop {
        let (page, code) = render()?;
        if !present(live.as_ref(), &page)? {
            return Ok(code);
        }
    }
}

/// The page, and its exit code: 1 when a session wrote another account back.
fn render() -> Result<(String, i32)> {
    let mut out = String::new();
    let now = Utc::now();
    let names = store::names()?;
    let s = auto::load();
    let active = store::current();
    writeln!(out, "{} {}", style("janus").bold(), style(env!("CARGO_PKG_VERSION")).dim())?;
    if names.is_empty() {
        writeln!(out, "\nNo saved accounts yet. Run `janus setup`.")?;
        return Ok((out, 0));
    }

    // Fresh numbers first: the lines about the account in use depend on them.
    let usages: Vec<_> = names.iter().map(|n| usage::recent(n, FETCH_AFTER_SECS)).collect();

    // ---- in use
    let live = claude::live()?;
    let owner = match &live.tokens {
        Some(t) => store::owner_of(t)?,
        None => None,
    };
    let mut code = 0;
    let label = |text: &str| style(format!("{text:<9}")).dim().to_string();
    writeln!(out)?;
    match &active {
        Some(name) => writeln!(out, "{}{}  {}", label("In use"), style(name).green().bold(), claude::email(live.account.as_ref()))?,
        None => writeln!(out, "{}{}", label("In use"), style("none picked yet (janus use)").yellow())?,
    }
    writeln!(out, "{:9}{}", "", mode(&s, active.as_deref()))?;
    let kept = match live.store {
        Store::Keychain => style(format!("login kept in {}", live.store.describe())).dim(),
        Store::File => style(format!("login kept in {}", live.store.describe())).yellow(),
    };
    writeln!(out, "{:9}{kept}", "")?;
    if let (Some(active), Some(owner)) = (&active, &owner) {
        if active != owner {
            writeln!(out, "{:9}{} A session wrote '{owner}' back. Run `janus {active}` again.", "", style("!").red().bold())?;
            code = 1;
        }
    }

    // ---- each account
    let dot = if utf8_terminal() { "●" } else { "*" };
    for (name, (u, note)) in names.iter().zip(usages) {
        let mark = if active.as_ref() == Some(name) { style(dot).green().to_string() } else { " ".to_string() };
        let role = match (&s.primary, &s.secondary) {
            (Some(p), _) if p == name => format!("  {}", style("primary").cyan()),
            (_, Some(sec)) if sec == name => format!("  {}", style("secondary").cyan()),
            _ => String::new(),
        };
        writeln!(out, "\n{mark} {}{role}  {}", style(name).bold(), account_line(name)?)?;
        let five_at = s.five_hour.filter(|_| s.any_on());
        let week_at = s.weekly.filter(|_| s.any_on());
        writeln!(out, "    {}  {}", style("5-hour").dim(), limit_line(u.as_ref().and_then(|u| u.five_hour.as_ref()), five_at, now))?;
        writeln!(out, "    {}  {}", style("weekly").dim(), limit_line(u.as_ref().and_then(|u| u.seven_day.as_ref()), week_at, now))?;
        writeln!(out, "    {}   {}", style("login").dim(), login_line(name, active.as_deref(), now)?)?;
        let checked = u.and_then(|u| u.fetched_at).map(|t| match (now - t).num_minutes() {
            0 => "checked just now".to_string(),
            _ => format!("checked {} ago", usage::until(now, t)),
        });
        let line = match (note, checked) {
            (Some(note), Some(checked)) => Some(style(format!("{checked} ({})", short(&note))).dim()),
            (Some(note), None) => Some(style(format!("not checked yet ({})", short(&note))).yellow()),
            (None, Some(checked)) => Some(style(checked).dim()),
            (None, None) => None,
        };
        if let Some(line) = line {
            writeln!(out, "    {}   {line}", style("usage").dim())?;
        }
    }

    // ---- automatic switching
    writeln!(out)?;
    if s.any_on() {
        let notify = if s.notify { "on" } else { "off" };
        writeln!(
            out,
            "{} {}   weekly {}   5-hour {}   notify {notify}",
            style("Automatic switching").bold(),
            style("on").green().bold(),
            auto::on_off(s.weekly),
            auto::on_off(s.five_hour)
        )?;
        let watcher = match auto::watcher_pid() {
            Some(pid) => style(format!("watcher running (pid {pid}), checks every {}m", auto::EVERY_SECS / 60)).green(),
            None => style("watcher not running; see `janus logs`".to_string()).red(),
        };
        writeln!(out, "  {watcher}")?;
    } else {
        writeln!(out, "{} {}", style("Automatic switching").bold(), style("off (janus auto on)").dim())?;
    }

    // ---- recent switches
    let log = std::fs::read_to_string(auto::log_path()).unwrap_or_default();
    let switches: Vec<&str> = log.lines().filter(|l| l.contains(" switched ")).collect();
    if !switches.is_empty() {
        writeln!(out, "\n{}", style("Recent automatic switches").bold())?;
        for line in &switches[switches.len().saturating_sub(5)..] {
            let (when, what) = tidy_switch(line);
            writeln!(out, "  {}  {what}", style(when).dim())?;
        }
    }
    Ok((out, code))
}

/// Why this account is the one in use.
fn mode(s: &auto::Settings, active: Option<&str>) -> String {
    if !s.any_on() {
        return style("picked by hand; automatic switching is off").dim().to_string();
    }
    if let Some(name) = auto::held() {
        return style(format!("held on '{name}' by hand: nothing switches until `janus resume`")).yellow().to_string();
    }
    if auto::is_away() {
        let primary = s.primary.as_deref().unwrap_or("");
        let back = auto::back_in(s, primary).map(|t| format!(" in {t}")).unwrap_or_default();
        return style(format!("moved off the primary ({}); back{back}, when that resets", auto::away_reason())).yellow().to_string();
    }
    if active.is_some() && active == s.primary.as_deref() {
        return style("automatic: on the primary, switches when a limit runs low").green().to_string();
    }
    style("automatic").green().to_string()
}

fn account_line(name: &str) -> Result<String> {
    let data = store::load(name)?;
    let plan = data.get("claudeAiOauth").and_then(|t| t.get("subscriptionType")).and_then(Value::as_str);
    Ok(format!("{}  {}", claude::email(data.get("oauthAccount")), style(format!("[{}]", plan.unwrap_or("?"))).dim()))
}

/// How full a limit looks: green with room, yellow from 70%, red from 90%
/// or once it's past where janus switches.
fn level(percent: f64, switch_at: Option<f64>) -> Color {
    match percent {
        p if p >= 90.0 || switch_at.is_some_and(|at| p >= at) => Color::Red,
        p if p >= 70.0 => Color::Yellow,
        _ => Color::Green,
    }
}

/// "████████░░   82%   resets in 1h 59m     (switches at 99%)", colored by level.
fn limit_line(w: Option<&Window>, switch_at: Option<f64>, now: DateTime<Utc>) -> String {
    let Some(w) = w.map(|w| w.at(now)) else { return style("not checked yet").dim().to_string() };
    let color = Style::new().fg(level(w.percent, switch_at));
    let pct = format!("{:>3}%", usage::whole(w.percent));
    // Padded before it's styled: color codes would count toward the width.
    let resets = format!("{:<18}", w.resets_at.map(|t| format!("resets in {}", usage::until(t, now))).unwrap_or_default());
    let at = match switch_at {
        Some(at) if w.percent >= at => style(format!("   (over the {}% switch point)", usage::whole(at))).red().to_string(),
        Some(at) => style(format!("   (switches at {}%)", usage::whole(at))).dim().to_string(),
        None => String::new(),
    };
    format!("{}  {}   {}{at}", color.apply_to(bar(w.percent)), color.apply_to(pct).bold(), style(resets).dim())
}

fn bar(percent: f64) -> String {
    draw_bar(percent, utf8_terminal())
}

fn draw_bar(percent: f64, utf8: bool) -> String {
    let filled = ((percent / 10.0).round() as usize).min(10);
    let (full, empty) = if utf8 { ("█", "░") } else { ("#", "-") };
    format!("{}{}", full.repeat(filled), empty.repeat(10 - filled))
}

/// Whether the terminal says it shows UTF-8; one that doesn't (often over
/// SSH with no locale set) turns block characters into garbage.
fn utf8_terminal() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .next()
        .is_some_and(|v| v.to_uppercase().replace('-', "").contains("UTF8"))
}

/// How long until this account has to log in again. The active one's copy
/// is the one on disk, which open sessions keep renewing.
fn login_line(name: &str, active: Option<&str>, now: DateTime<Utc>) -> Result<String> {
    let tokens = if active == Some(name) {
        claude::live()?.tokens
    } else {
        store::load(name)?.remove("claudeAiOauth")
    };
    let expires = tokens
        .as_ref()
        .and_then(|t| t.get("refreshTokenExpiresAt"))
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis);
    Ok(match expires {
        None => style("no expiry saved".to_string()).dim().to_string(),
        Some(t) if t <= now => style(format!("expired: /login with it, then janus account add {name}")).red().to_string(),
        Some(t) if t - now < chrono::Duration::days(LOGIN_WARN_DAYS) => {
            style(format!("expires in {}: soon /login with it, then janus account add {name}", usage::until(t, now))).yellow().to_string()
        }
        Some(t) => format!("good for {}", usage::until(t, now)),
    })
}

/// A log line's time without the seconds, and what happened without the email:
/// "2026-10-20 08:35:17 switched to 'work' (w@x.com): …" -> ("2026-10-20 08:35", "switched to 'work': …")
fn tidy_switch(line: &str) -> (String, String) {
    let (when, what) = (line.get(..16).unwrap_or(line), line.get(20..).unwrap_or(""));
    let what = match (what.find(" ("), what.find("):")) {
        (Some(open), Some(close)) if open < close => format!("{}{}", &what[..open], &what[close + 1..]),
        _ => what.to_string(),
    };
    (when.to_string(), what)
}

/// Notes from the usage check, trimmed for one line.
fn short(note: &str) -> String {
    note.split(';').next().unwrap_or(note).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() {
        console::set_colors_enabled(false);
    }

    #[test]
    fn bars_fill_by_tens() {
        assert_eq!(draw_bar(0.0, true), "░░░░░░░░░░");
        assert_eq!(draw_bar(54.0, true), "█████░░░░░");
        assert_eq!(draw_bar(99.4, true), "██████████");
        assert_eq!(draw_bar(140.0, true), "██████████");
        assert_eq!(draw_bar(54.0, false), "#####-----");
    }

    #[test]
    fn limit_lines_say_where_the_switch_point_is() {
        plain();
        let now = Utc::now();
        let w = |p| Window { percent: p, resets_at: Some(now + chrono::Duration::minutes(119)) };
        assert_eq!(limit_line(Some(&w(82.0)), Some(99.0), now), format!("{}   82%   resets in 1h 59m     (switches at 99%)", bar(82.0)));
        assert!(limit_line(Some(&w(99.5)), Some(99.0), now).ends_with("(over the 99% switch point)"));
        assert_eq!(limit_line(None, None, now), "not checked yet");
    }

    #[test]
    fn a_window_past_its_reset_reads_empty() {
        plain();
        let now = Utc::now();
        let w = Window { percent: 100.0, resets_at: Some(now - chrono::Duration::minutes(1)) };
        assert!(limit_line(Some(&w), None, now).starts_with(&format!("{}    0%", bar(0.0))));
    }

    #[test]
    fn switch_lines_drop_the_seconds_and_email() {
        let line = "2026-10-20 08:35:17 switched to 'work' (w@x.com): personal 5-hour limit at 99%, back in 1h 59m";
        let (when, what) = tidy_switch(line);
        assert_eq!((when.as_str(), what.as_str()), ("2026-10-20 08:35", "switched to 'work': personal 5-hour limit at 99%, back in 1h 59m"));
    }

    #[test]
    fn limits_go_green_yellow_red() {
        assert_eq!(level(10.0, None), Color::Green);
        assert_eq!(level(75.0, None), Color::Yellow);
        assert_eq!(level(92.0, None), Color::Red);
        assert_eq!(level(85.0, Some(80.0)), Color::Red);
        assert_eq!(level(60.0, Some(80.0)), Color::Green);
    }
}
