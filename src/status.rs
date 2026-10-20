//! `janus status`: everything on one page. Which account is in use and why,
//! how much of each limit is left and when it resets, how long each saved
//! login lasts, and what the watcher is doing.

use crate::auto;
use crate::claude;
use crate::store;
use crate::usage::{self, Window};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// With -w the page redraws this often. Usage is only fetched again once
/// it's this old, about as often as the watcher fetches it.
const REDRAW_SECS: u64 = 30;
const FETCH_AFTER_SECS: i64 = 110;

/// A login this close to expiring gets a warning.
const LOGIN_WARN_DAYS: i64 = 3;

pub fn cmd_status(watch: bool) -> Result<i32> {
    if !watch {
        return render(&mut std::io::stdout());
    }
    loop {
        // Draw off screen first, so the page doesn't flicker while usage loads.
        let mut page = Vec::new();
        render(&mut page)?;
        print!("\x1b[2J\x1b[H{}", String::from_utf8_lossy(&page));
        println!("\nRedraws every {REDRAW_SECS}s. Ctrl-C to stop.");
        std::thread::sleep(std::time::Duration::from_secs(REDRAW_SECS));
    }
}

fn render(out: &mut impl std::io::Write) -> Result<i32> {
    let now = Utc::now();
    let names = store::names()?;
    let s = auto::load();
    let active = store::current();
    writeln!(out, "janus {}", env!("CARGO_PKG_VERSION"))?;
    if names.is_empty() {
        writeln!(out, "\nNo saved accounts yet. Log in, then: janus save <name>")?;
        return Ok(0);
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
    writeln!(out)?;
    match &active {
        Some(name) => writeln!(out, "In use   {name}  {}", claude::email(live.account.as_ref()))?,
        None => writeln!(out, "In use   none picked yet (janus <name>)")?,
    }
    writeln!(out, "         {}", mode(&s, active.as_deref()))?;
    if let (Some(active), Some(owner)) = (&active, &owner) {
        if active != owner {
            writeln!(out, "         ! A session wrote '{owner}' back to disk. Run `janus {active}` again.")?;
            code = 1;
        }
    }

    // ---- each account
    for (name, (u, note)) in names.iter().zip(usages) {
        let mark = if active.as_ref() == Some(name) { "*" } else { " " };
        let role = match (&s.primary, &s.secondary) {
            (Some(p), _) if p == name => "  primary",
            (_, Some(sec)) if sec == name => "  secondary",
            _ => "",
        };
        writeln!(out, "\n{mark} {name}{role}  {}", account_line(name)?)?;
        let five_at = s.five_hour.filter(|_| s.any_on());
        let week_at = s.weekly.filter(|_| s.any_on());
        writeln!(out, "    5-hour  {}", limit_line(u.as_ref().and_then(|u| u.five_hour.as_ref()), five_at, now))?;
        writeln!(out, "    weekly  {}", limit_line(u.as_ref().and_then(|u| u.seven_day.as_ref()), week_at, now))?;
        writeln!(out, "    login   {}", login_line(name, active.as_deref(), now)?)?;
        let checked = u.and_then(|u| u.fetched_at).map(|t| match (now - t).num_minutes() {
            0 => "checked just now".to_string(),
            _ => format!("checked {} ago", usage::until(now, t)),
        });
        match (note, checked) {
            (Some(note), Some(checked)) => writeln!(out, "    usage   {checked} ({})", short(&note))?,
            (Some(note), None) => writeln!(out, "    usage   not checked yet ({})", short(&note))?,
            (None, Some(checked)) => writeln!(out, "    usage   {checked}")?,
            (None, None) => {}
        }
    }

    // ---- automatic switching
    writeln!(out, "\nAutomatic switching")?;
    if s.any_on() {
        let notify = if s.notify { "on" } else { "off" };
        writeln!(out, "  weekly {}   5-hour {}   notify {notify}", auto::on_off(s.weekly), auto::on_off(s.five_hour))?;
        let watcher = match auto::watcher_pid() {
            Some(pid) => format!("running (pid {pid}), checks every {}m", auto::EVERY_SECS / 60),
            None => "not running; see `janus logs`".into(),
        };
        writeln!(out, "  watcher {watcher}")?;
    } else {
        writeln!(out, "  off (janus auto weekly on, or janus auto 5h on)")?;
    }

    // ---- recent switches
    let log = std::fs::read_to_string(auto::log_path()).unwrap_or_default();
    let switches: Vec<&str> = log.lines().filter(|l| l.contains(" switched ")).collect();
    if !switches.is_empty() {
        writeln!(out, "\nRecent automatic switches")?;
        for line in &switches[switches.len().saturating_sub(5)..] {
            writeln!(out, "  {}", tidy_switch(line))?;
        }
    }
    Ok(code)
}

/// Why this account is the one in use.
fn mode(s: &auto::Settings, active: Option<&str>) -> String {
    if !s.any_on() {
        return "picked by hand; automatic switching is off".into();
    }
    if let Some(name) = auto::held() {
        return format!("held on '{name}' by hand: nothing switches until `janus resume`");
    }
    if auto::is_away() {
        let primary = s.primary.as_deref().unwrap_or("");
        let back = auto::back_in(s, primary).map(|t| format!(" in {t}")).unwrap_or_default();
        return format!("moved off the primary ({}); back{back}, when that resets", auto::away_reason());
    }
    if active.is_some() && active == s.primary.as_deref() {
        return "automatic: on the primary, switches when a limit runs low".into();
    }
    "automatic".into()
}

fn account_line(name: &str) -> Result<String> {
    let data = store::load(name)?;
    let plan = data.get("claudeAiOauth").and_then(|t| t.get("subscriptionType")).and_then(Value::as_str);
    Ok(format!("{}  [{}]", claude::email(data.get("oauthAccount")), plan.unwrap_or("?")))
}

/// "████████░░  82%   resets in 1h 59m   (switches at 99%)"
fn limit_line(w: Option<&Window>, switch_at: Option<f64>, now: DateTime<Utc>) -> String {
    let Some(w) = w.map(|w| w.at(now)) else { return "not checked yet".into() };
    let pct = usage::whole(w.percent);
    let resets = w.resets_at.map(|t| format!("resets in {}", usage::until(t, now))).unwrap_or_default();
    let at = match switch_at {
        Some(at) if w.percent >= at => format!("   (over the {}% switch point)", usage::whole(at)),
        Some(at) => format!("   (switches at {}%)", usage::whole(at)),
        None => String::new(),
    };
    format!("{}  {:>3}%   {resets:<18}{at}", bar(w.percent), pct)
}

fn bar(percent: f64) -> String {
    let filled = ((percent / 10.0).round() as usize).min(10);
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
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
        None => "no expiry saved".into(),
        Some(t) if t <= now => format!("expired: /login with it, then janus save {name}"),
        Some(t) if t - now < chrono::Duration::days(LOGIN_WARN_DAYS) => {
            format!("expires in {}: soon /login with it, then janus save {name}", usage::until(t, now))
        }
        Some(t) => format!("good for {}", usage::until(t, now)),
    })
}

/// A log line without the seconds and the email:
/// "2026-10-20 08:35:17 switched to 'work' (w@x.com): …" -> "2026-10-20 08:35  switched to 'work': …"
fn tidy_switch(line: &str) -> String {
    let (when, what) = (line.get(..16).unwrap_or(line), line.get(20..).unwrap_or(""));
    let what = match (what.find(" ("), what.find("):")) {
        (Some(open), Some(close)) if open < close => format!("{}{}", &what[..open], &what[close + 1..]),
        _ => what.to_string(),
    };
    format!("{when}  {what}")
}

/// Notes from the usage check, trimmed for one line.
fn short(note: &str) -> String {
    note.split(';').next().unwrap_or(note).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_fill_by_tens() {
        assert_eq!(bar(0.0), "░░░░░░░░░░");
        assert_eq!(bar(54.0), "█████░░░░░");
        assert_eq!(bar(99.4), "██████████");
        assert_eq!(bar(140.0), "██████████");
    }

    #[test]
    fn limit_lines_say_where_the_switch_point_is() {
        let now = Utc::now();
        let w = |p| Window { percent: p, resets_at: Some(now + chrono::Duration::minutes(119)) };
        assert_eq!(limit_line(Some(&w(82.0)), Some(99.0), now), "████████░░   82%   resets in 1h 59m     (switches at 99%)");
        assert!(limit_line(Some(&w(99.5)), Some(99.0), now).ends_with("(over the 99% switch point)"));
        assert_eq!(limit_line(None, None, now), "not checked yet");
    }

    #[test]
    fn a_window_past_its_reset_reads_empty() {
        let now = Utc::now();
        let w = Window { percent: 100.0, resets_at: Some(now - chrono::Duration::minutes(1)) };
        assert!(limit_line(Some(&w), None, now).starts_with("░░░░░░░░░░    0%"));
    }

    #[test]
    fn switch_lines_drop_the_seconds_and_email() {
        let line = "2026-10-20 08:35:17 switched to 'work' (w@x.com): personal 5-hour limit at 99%, back in 1h 59m";
        assert_eq!(tidy_switch(line), "2026-10-20 08:35  switched to 'work': personal 5-hour limit at 99%, back in 1h 59m");
    }
}
