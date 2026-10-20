//! Switching on its own: `janus watch` checks both accounts' usage every
//! couple of minutes, moves off the primary when a limit you turned on is
//! nearly used up, and moves back as soon as it resets.
//!
//! Settings are in ~/.janus/.auto.json:
//!
//!     primary      the account to use whenever it has room
//!     secondary    where to go when the primary runs out
//!     weekly       switch at this % of the weekly limit, or null for off
//!     five_hour    switch at this % of the 5-hour limit, or null for off
//!     notify       show a notification when it switches (default true)
//!
//! .away records that the watcher moved off the primary, so it only moves
//! back from a switch it made. .held names an account you picked by hand:
//! while it's there the watcher switches nothing, until `janus resume`.

use crate::claude::{self, read_object, write_object};
use crate::store;
use crate::usage::{self, Usage};
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const LABEL: &str = "dev.janus.watch";
pub const EVERY_SECS: u64 = 120;
pub const DEFAULT_PERCENT: f64 = 99.0;

#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub primary: Option<String>,
    pub secondary: Option<String>,
    pub weekly: Option<f64>,
    pub five_hour: Option<f64>,
    pub notify: bool,
}

impl Settings {
    pub fn any_on(&self) -> bool {
        self.weekly.is_some() || self.five_hour.is_some()
    }
}

fn settings_path() -> PathBuf {
    store::home().join(".auto.json")
}
fn away_path() -> PathBuf {
    store::home().join(".away")
}
fn pid_path() -> PathBuf {
    store::home().join(".watch.pid")
}
pub fn log_path() -> PathBuf {
    store::home().join("watch.log")
}
fn plist_path() -> PathBuf {
    claude::user_home().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

pub fn load() -> Settings {
    let saved = read_object(&settings_path()).unwrap_or_default();
    let text = |k: &str| saved.get(k).and_then(Value::as_str).map(str::to_string);
    let number = |k: &str| saved.get(k).and_then(Value::as_f64);
    Settings {
        primary: text("primary"),
        secondary: text("secondary"),
        weekly: number("weekly"),
        five_hour: number("five_hour"),
        notify: saved.get("notify").and_then(Value::as_bool).unwrap_or(true),
    }
}

fn save(s: &Settings) -> Result<()> {
    let mut m = Map::new();
    m.insert("primary".into(), s.primary.clone().into());
    m.insert("secondary".into(), s.secondary.clone().into());
    m.insert("weekly".into(), s.weekly.into());
    m.insert("five_hour".into(), s.five_hour.into());
    m.insert("notify".into(), s.notify.into());
    write_object(&settings_path(), &m, 0o600)
}

fn held_path() -> PathBuf {
    store::home().join(".held")
}

/// The account you picked by hand, while automatic switching is paused for it.
pub fn held() -> Option<String> {
    let name = fs::read_to_string(held_path()).ok()?;
    Some(name.trim().to_string()).filter(|n| !n.is_empty())
}

pub fn set_held(name: Option<&str>) -> Result<()> {
    match name {
        Some(name) => fs::write(held_path(), format!("{name}\n"))?,
        None => {
            let _ = fs::remove_file(held_path());
        }
    }
    Ok(())
}

/// Why the watcher moved off the primary, as it recorded then.
pub fn away_reason() -> String {
    let text = fs::read_to_string(away_path()).unwrap_or_default();
    text.split_once(' ').map(|(_, why)| why.trim().to_string()).unwrap_or_default()
}

pub fn is_away() -> bool {
    away_path().exists()
}

/// Called on every switch: a switch by hand clears it.
pub fn set_away(away: Option<&str>) -> Result<()> {
    match away {
        Some(why) => fs::write(away_path(), format!("{} {why}\n", Utc::now().to_rfc3339()))?,
        None => {
            let _ = fs::remove_file(away_path());
        }
    }
    Ok(())
}

// ---- deciding ---------------------------------------------------------------

/// Why an account can't be used right now under the limits that are on, and
/// until when. The window that frees up last decides.
pub fn blocked(s: &Settings, usage: Option<&Usage>, now: DateTime<Utc>) -> Option<(String, Option<DateTime<Utc>>)> {
    let usage = usage?;
    let mut worst: Option<(String, Option<DateTime<Utc>>)> = None;
    for (at, window, label) in [(s.five_hour, &usage.five_hour, "5-hour"), (s.weekly, &usage.seven_day, "weekly")] {
        let (Some(at), Some(w)) = (at, window.as_ref().map(|w| w.at(now))) else { continue };
        if w.percent < at {
            continue;
        }
        let later = match (&worst, w.resets_at) {
            (None, _) => true,
            (Some((_, Some(prev))), Some(t)) => t > *prev,
            (Some((_, Some(_))), None) => true,
            (Some((_, None)), _) => false,
        };
        if later {
            worst = Some((format!("{label} limit at {}%", usage::whole(w.percent)), w.resets_at));
        }
    }
    worst
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Stay,
    /// Switch to this account; `Some(reason)` when leaving the primary.
    Switch(String, Option<String>),
}

pub fn decide(s: &Settings, current: Option<&str>, away: bool, primary: Option<&Usage>, secondary: Option<&Usage>, now: DateTime<Utc>) -> Action {
    let (Some(p), Some(sec)) = (s.primary.as_deref(), s.secondary.as_deref()) else { return Action::Stay };
    if !s.any_on() {
        return Action::Stay;
    }
    if current == Some(p) {
        if let Some((why, _)) = blocked(s, primary, now) {
            if blocked(s, secondary, now).is_none() {
                return Action::Switch(sec.into(), Some(why));
            }
        }
    } else if current == Some(sec) && away && blocked(s, primary, now).is_none() {
        return Action::Switch(p.into(), None);
    }
    Action::Stay
}

/// "1h 59m": how long until the primary frees up, from its last numbers.
pub fn back_in(s: &Settings, primary: &str) -> Option<String> {
    let now = Utc::now();
    match blocked(s, usage::cached(primary).as_ref(), now) {
        Some((_, Some(t))) => Some(usage::until(t, now)),
        _ => None,
    }
}

// ---- watching ---------------------------------------------------------------

/// A desktop notification, best effort: a switch still counts if it can't show.
fn notify(title: &str, body: &str) {
    let title = format!("janus: {title}");
    if cfg!(target_os = "macos") {
        // Passed as arguments, so nothing in the text needs escaping.
        let script = ["on run argv", "display notification (item 2 of argv) with title (item 1 of argv)", "end run"];
        let mut cmd = Command::new("osascript");
        for line in script {
            cmd.args(["-e", line]);
        }
        let _ = cmd.args([&title, body]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    } else {
        let _ = Command::new("notify-send").args([&title, body]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

/// A line in the watch log, wherever the check ran from (the watcher, or
/// `janus resume`); also shown when someone's watching in a terminal.
fn log(line: &str) {
    use std::io::Write;
    let line = format!("{} {line}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = f.write_all(line.as_bytes());
    }
    // SAFETY: isatty only inspects the descriptor.
    if unsafe { libc::isatty(1) } == 1 {
        print!("{line}");
    }
}

/// One look at both accounts, and a switch if one is due.
pub fn tick() -> Result<()> {
    let s = load();
    let (Some(p), Some(sec)) = (s.primary.clone(), s.secondary.clone()) else { return Ok(()) };
    let _lock = store::lock()?;
    let (pu, pnote) = usage::check(&p);
    let (su, snote) = usage::check(&sec);
    for (name, note) in [(&p, pnote), (&sec, snote)] {
        if let Some(note) = note {
            log(&format!("{name}: {note}"));
        }
    }
    // Picked by hand: keep the numbers fresh for `janus auto`, but switch nothing.
    if held().is_some() {
        return Ok(());
    }
    let current = store::current();
    match decide(&s, current.as_deref(), is_away(), pu.as_ref(), su.as_ref(), Utc::now()) {
        Action::Stay => {}
        Action::Switch(to, reason) => {
            let email = crate::switch_to(&to)?;
            set_away(reason.as_deref())?;
            let (title, body) = match reason {
                Some(why) => {
                    let back = back_in(&s, &p);
                    log(&format!("switched to '{to}' ({email}): {p} {why}{}", back.as_ref().map(|t| format!(", back in {t}")).unwrap_or_default()));
                    let back = back.map(|t| format!(" Back on {p} in {t}.")).unwrap_or_default();
                    (format!("Switched to {to}"), format!("{p}: {why}.{back}"))
                }
                None => {
                    log(&format!("switched back to '{to}' ({email}): its limits have room again"));
                    (format!("Back on {to}"), "Its limits have room again.".to_string())
                }
            };
            if s.notify {
                notify(&title, &body);
            }
        }
    }
    Ok(())
}

pub fn cmd_watch(once: bool) -> Result<i32> {
    if once {
        tick()?;
        return Ok(0);
    }
    fs::write(pid_path(), format!("{}\n", std::process::id()))?;
    log("watching");
    loop {
        if !load().any_on() {
            log("both options are off; stopping");
            let _ = fs::remove_file(pid_path());
            return Ok(0);
        }
        if let Err(e) = tick() {
            log(&format!("check failed: {e:#}"));
        }
        std::thread::sleep(std::time::Duration::from_secs(EVERY_SECS));
    }
}

pub fn watcher_pid() -> Option<i32> {
    let pid: i32 = fs::read_to_string(pid_path()).ok()?.trim().parse().ok()?;
    // SAFETY: signal 0 only checks the process exists.
    (unsafe { libc::kill(pid, 0) } == 0).then_some(pid)
}

// ---- the background watcher (a LaunchAgent) ---------------------------------

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions.
    unsafe { libc::getuid() }
}

fn run_quiet(program: &str, args: &[&str]) {
    let _ = Command::new(program).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn plist() -> String {
    let exe = std::env::current_exe().and_then(fs::canonicalize).unwrap_or_else(|_| "janus".into());
    let log = log_path().display().to_string();
    let home = store::home().display().to_string();
    let user_home = claude::user_home().display().to_string();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>{exe}</string><string>watch</string></array>
  <key>EnvironmentVariables</key>
  <dict><key>HOME</key><string>{user_home}</string><key>JANUS_HOME</key><string>{home}</string></dict>
  <key>RunAtLoad</key><true/>
  <!-- Restart after a crash, but not after it stops because both options are off. -->
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        exe = exe.display()
    )
}

/// Run the watcher now and at every login, or stop it, to match the settings.
fn sync_watcher(s: &Settings) -> Result<()> {
    if !cfg!(target_os = "macos") {
        if s.any_on() {
            println!("Run `janus watch` in the background yourself; starting it at login is macOS only.");
        }
        return Ok(());
    }
    let target = format!("gui/{}/{LABEL}", uid());
    run_quiet("launchctl", &["bootout", &target]);
    if s.any_on() {
        fs::create_dir_all(plist_path().parent().unwrap())?;
        fs::write(plist_path(), plist())?;
        run_quiet("launchctl", &["bootstrap", &format!("gui/{}", uid()), &plist_path().to_string_lossy()]);
        for _ in 0..20 {
            if watcher_pid().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    } else {
        let _ = fs::remove_file(plist_path());
        let _ = fs::remove_file(pid_path());
    }
    Ok(())
}

// ---- `janus auto ...` ----------------------------------------------------

pub fn on_off(v: Option<f64>) -> String {
    v.map(|p| format!("on, at {p:.0}%")).unwrap_or_else(|| "off".into())
}

fn show() -> Result<i32> {
    let s = load();
    let now = Utc::now();
    let current = store::current();
    println!("Automatic switching");
    for (role, name) in [("primary", &s.primary), ("secondary", &s.secondary)] {
        match name {
            Some(n) => {
                let u = usage::cached(n);
                let here = if current.as_ref() == Some(n) { "  <- in use" } else { "" };
                println!(
                    "  {role:<10} {n}: 5-hour {}, weekly {}{here}",
                    usage::describe(u.as_ref().and_then(|u| u.five_hour.as_ref()), now),
                    usage::describe(u.as_ref().and_then(|u| u.seven_day.as_ref()), now),
                );
            }
            None => println!("  {role:<10} not set (janus auto {role} <name>)"),
        }
    }
    println!("  weekly     {}", on_off(s.weekly));
    println!("  5-hour     {}", on_off(s.five_hour));
    println!("  notify     {}", if s.notify { "on" } else { "off" });
    let watcher = match watcher_pid() {
        Some(pid) => format!("running (pid {pid}), checks every {}m", EVERY_SECS / 60),
        None if s.any_on() => "not running; see `janus logs`".into(),
        None => "off".into(),
    };
    println!("  watcher    {watcher}");
    if let Some(name) = held().filter(|_| s.any_on()) {
        println!("  Held on '{name}', picked by hand: nothing switches until `janus resume`.");
    } else if is_away() && s.any_on() {
        let why = away_reason();
        let primary = s.primary.as_deref().unwrap_or("");
        let back = back_in(&s, primary).map(|t| format!(" in {t}")).unwrap_or_default();
        println!("  Moved off the primary: {why}. It moves back{back}, when that resets.");
    }
    Ok(0)
}

/// Hand the choice back to the watcher, and let it act now rather than at
/// its next check.
pub fn cmd_resume() -> Result<i32> {
    let s = load();
    let was = held();
    set_held(None)?;
    if !s.any_on() {
        println!("Nothing was paused: automatic switching is off (janus auto weekly on, or janus auto 5h on).");
        return Ok(0);
    }
    // On the secondary, treat it as the watcher's own move, so it goes back
    // to the primary once the primary has room.
    if store::current().is_some() && store::current() == s.secondary {
        let primary = s.primary.as_deref().unwrap_or("");
        let why = blocked(&s, usage::cached(primary).as_ref(), Utc::now()).map(|(why, _)| why);
        set_away(Some(why.as_deref().unwrap_or("handed back with janus resume")))?;
    }
    match was {
        Some(name) => println!("No longer held on '{name}'. Checking usage…"),
        None => println!("Nothing was held. Checking usage…"),
    }
    if let Err(e) = tick() {
        println!("The check failed ({e:#}); the watcher tries again within {}m.", EVERY_SECS / 60);
    }
    show()
}

fn percent(v: &str) -> Result<Option<f64>> {
    match v {
        "on" => Ok(Some(DEFAULT_PERCENT)),
        "off" => Ok(None),
        n => match n.trim_end_matches('%').parse::<f64>() {
            Ok(p) if p > 0.0 && p <= 100.0 => Ok(Some(p)),
            _ => bail!("Use on, off, or a percent from 1 to 100 (got `{v}`)."),
        },
    }
}

pub fn cmd_auto(args: &[&str]) -> Result<i32> {
    let mut s = load();
    match args {
        [] => return show(),
        [role @ ("primary" | "secondary"), name] => {
            if !store::exists(name)? {
                bail!("No saved account '{name}'.");
            }
            let other = if *role == "primary" { &s.secondary } else { &s.primary };
            if other.as_deref() == Some(*name) {
                bail!("'{name}' can't be both primary and secondary.");
            }
            let slot = if *role == "primary" { &mut s.primary } else { &mut s.secondary };
            *slot = Some(name.to_string());
        }
        [limit @ ("weekly" | "5h" | "5-hour"), value] => {
            let v = percent(value)?;
            if v.is_some() && (s.primary.is_none() || s.secondary.is_none()) {
                bail!("Set both accounts first: janus auto primary <name>, then janus auto secondary <name>.");
            }
            if *limit == "weekly" {
                s.weekly = v;
            } else {
                s.five_hour = v;
            }
        }
        ["notify", value @ ("on" | "off")] => s.notify = *value == "on",
        ["notify", "test"] => {
            notify("test", "Notifications from janus show up like this.");
            println!("Sent a test notification.");
            return Ok(0);
        }
        _ => bail!("Usage: janus auto [primary|secondary <name>] [weekly|5h on|off|<percent>] [notify on|off|test]"),
    }
    save(&s)?;
    sync_watcher(&s)?;
    show()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Window;
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-20T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn settings(weekly: Option<f64>, five_hour: Option<f64>) -> Settings {
        Settings { primary: Some("p".into()), secondary: Some("s".into()), weekly, five_hour, notify: false }
    }

    fn used(five_hour: f64, five_resets_h: i64, weekly: f64, weekly_resets_h: i64) -> Usage {
        let w = |percent, h| Some(Window { percent, resets_at: Some(now() + Duration::hours(h)) });
        Usage { fetched_at: Some(now()), five_hour: w(five_hour, five_resets_h), seven_day: w(weekly, weekly_resets_h) }
    }

    #[test]
    fn stays_on_the_primary_while_it_has_room() {
        let s = settings(Some(99.0), Some(99.0));
        assert_eq!(decide(&s, Some("p"), false, Some(&used(98.0, 2, 50.0, 48)), None, now()), Action::Stay);
    }

    #[test]
    fn leaves_the_primary_at_the_weekly_threshold() {
        let s = settings(Some(99.0), None);
        let action = decide(&s, Some("p"), false, Some(&used(10.0, 2, 99.0, 48)), Some(&used(0.0, 5, 0.0, 100)), now());
        assert_eq!(action, Action::Switch("s".into(), Some("weekly limit at 99%".into())));
    }

    #[test]
    fn ignores_a_limit_that_is_turned_off() {
        let s = settings(Some(99.0), None);
        assert_eq!(decide(&s, Some("p"), false, Some(&used(100.0, 2, 10.0, 48)), None, now()), Action::Stay);
    }

    #[test]
    fn does_not_move_to_a_secondary_that_is_also_out() {
        let s = settings(None, Some(99.0));
        let action = decide(&s, Some("p"), false, Some(&used(100.0, 2, 10.0, 48)), Some(&used(100.0, 1, 10.0, 48)), now());
        assert_eq!(action, Action::Stay);
    }

    #[test]
    fn goes_back_once_the_primary_resets() {
        let s = settings(None, Some(99.0));
        // Last seen at 100%, but that window reset an hour ago.
        let primary = used(100.0, -1, 10.0, 48);
        assert_eq!(decide(&s, Some("s"), true, Some(&primary), None, now()), Action::Switch("p".into(), None));
    }

    #[test]
    fn waits_for_the_window_that_resets_last() {
        let s = settings(Some(99.0), Some(99.0));
        // The 5-hour window has reset but the weekly one hasn't.
        let primary = used(100.0, -1, 99.5, 30);
        assert_eq!(decide(&s, Some("s"), true, Some(&primary), None, now()), Action::Stay);
        assert_eq!(blocked(&s, Some(&used(100.0, 3, 99.5, 30)), now()).unwrap().1, Some(now() + Duration::hours(30)));
    }

    #[test]
    fn leaves_a_switch_made_by_hand_alone() {
        let s = settings(Some(99.0), Some(99.0));
        assert_eq!(decide(&s, Some("s"), false, Some(&used(1.0, 2, 1.0, 48)), None, now()), Action::Stay);
    }

    #[test]
    fn does_nothing_when_both_options_are_off() {
        let s = settings(None, None);
        assert_eq!(decide(&s, Some("p"), false, Some(&used(100.0, 2, 100.0, 48)), None, now()), Action::Stay);
    }
}
