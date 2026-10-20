//! The `janus` command.

mod auto;
mod claude;
mod store;
mod usage;

use anyhow::{bail, Result};
use serde_json::Value;

const USAGE: &str = "\
janus: switch the logged-in Claude Code account without closing sessions.

Get going
  janus save <name>     save the account you're logged into now as <name>
                        (then /login with the other one and save it too)

Every day
  janus                 list saved accounts; * marks the active one
  janus <name>          switch to <name> and stay there (also: janus use <name>)
  janus usage           each account's 5-hour and weekly usage
  janus status          check the account on disk is the active one

Switch on its own
  janus auto                     show the settings and what the watcher is doing
  janus auto primary <name>      use this account whenever it has room
  janus auto secondary <name>    and this one when the primary runs out
  janus auto weekly on|off|<%>   switch when the weekly limit hits 99% (or <%>)
  janus auto 5h on|off|<%>       the same for the 5-hour limit
  janus auto notify on|off|test  a notification when it switches (on)
  janus resume                   after picking one by hand, let it switch again
  It moves back to the primary as soon as its limit resets. Picking an
  account by hand (janus <name>) holds it there until janus resume.

Look after it
  janus logs [-f]       what the watcher has done
  janus rm <name>       forget a saved account
  janus --version       which build this is

Update: ./install.sh in the repo.
";

/// Words that are commands, so they can't be account names.
const COMMANDS: [&str; 12] = ["save", "use", "rm", "ls", "list", "status", "usage", "auto", "resume", "watch", "logs", "help"];

/// \"0.1.0 (built 2026-10-19 14:02)\": the build time tells two copies apart.
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (built ", env!("JANUS_BUILT"), ")");

fn cmd_save(name: &str) -> Result<i32> {
    if COMMANDS.contains(&name) {
        bail!("`{name}` is a command, so it can't be an account name.");
    }
    let _lock = store::lock()?;
    let live = claude::live()?;
    let Some(tokens) = &live.tokens else {
        bail!("No Claude Code login found. Run `claude` and /login first.");
    };
    store::save(name, tokens, live.account.as_ref())?;
    store::set_current(name)?;
    println!("Saved {} as '{name}'", claude::email(live.account.as_ref()));
    Ok(0)
}

/// Copy the login on disk back into the active account before leaving it.
/// Sessions renew their tokens on their own and the old ones stop working,
/// so the saved copy goes stale unless it picks up the newest one here.
fn sync_live_into_current() -> Result<()> {
    let Some(name) = store::current() else { return Ok(()) };
    let live = claude::live()?;
    let Some(tokens) = &live.tokens else { return Ok(()) };
    if !store::exists(&name)? {
        return Ok(());
    }
    // Tokens that belong to another saved account must not be saved under this name.
    if store::owner_of(tokens)?.is_some_and(|owner| owner != name) {
        return Ok(());
    }
    store::save(&name, tokens, live.account.as_ref())
}

/// Put a saved account's login on disk and make it the active one; returns
/// its email. Callers hold the lock.
pub fn switch_to(name: &str) -> Result<String> {
    if !store::exists(name)? {
        let have = store::names()?.join(", ");
        bail!("No saved account '{name}' (have: {}).", if have.is_empty() { "none" } else { &have });
    }
    sync_live_into_current()?;
    let target = store::load(name)?;
    let Some(tokens) = target.get("claudeAiOauth") else {
        bail!("The saved account '{name}' has no login in it. Log in and `janus save {name}` again.");
    };
    let account = target.get("oauthAccount").filter(|a| !a.is_null());
    claude::put(tokens, account)?;
    store::set_current(name)?;
    Ok(claude::email(account).to_string())
}

fn cmd_use(name: &str) -> Result<i32> {
    let _lock = store::lock()?;
    let email = switch_to(name)?;
    // A switch by hand: hold it there until `janus resume`.
    auto::set_away(None)?;
    auto::set_held(Some(name))?;
    println!("Switched to '{name}' ({email})");
    if auto::load().any_on() {
        println!("Automatic switching is paused while you're on it by hand. `janus resume` hands it back.");
    }
    Ok(0)
}

fn cmd_usage() -> Result<i32> {
    let names = store::names()?;
    if names.is_empty() {
        println!("No saved accounts yet. Log in, then: janus save <name>");
        return Ok(0);
    }
    let now = chrono::Utc::now();
    let active = store::current();
    for name in &names {
        let (u, note) = usage::check(name);
        let mark = if active.as_ref() == Some(name) { "*" } else { " " };
        println!(
            "{mark} {name:<12} 5-hour {:<24} weekly {}",
            usage::describe(u.as_ref().and_then(|u| u.five_hour.as_ref()), now),
            usage::describe(u.as_ref().and_then(|u| u.seven_day.as_ref()), now),
        );
        if let Some(note) = note {
            let seen = u.and_then(|u| u.fetched_at).map(|t| format!(" ({} ago)", usage::until(now, t)));
            println!("  {:<12} {note}{}", "", seen.unwrap_or_default());
        }
    }
    Ok(0)
}

fn cmd_logs(follow: bool) -> Result<i32> {
    let log = auto::log_path();
    if follow {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new("tail").arg("-f").arg(&log).exec();
        bail!("couldn't run tail: {err}");
    }
    match std::fs::read_to_string(&log) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            println!("{}", lines[lines.len().saturating_sub(40)..].join("\n"));
        }
        Err(_) => println!("No logs yet."),
    }
    Ok(0)
}

fn describe(name: &str) -> Result<String> {
    let data = store::load(name)?;
    let account = data.get("oauthAccount");
    let field = |v: Option<&Value>, key: &str| v.and_then(|v| v.get(key)).and_then(Value::as_str).unwrap_or("").to_string();
    let plan = field(data.get("claudeAiOauth"), "subscriptionType");
    Ok(format!(
        "{}  [{}]  {}",
        claude::email(account),
        if plan.is_empty() { "?" } else { &plan },
        field(account, "organizationName")
    ))
}

fn cmd_list() -> Result<i32> {
    let names = store::names()?;
    if names.is_empty() {
        println!("No saved accounts yet. Log in, then: janus save <name>");
        return Ok(0);
    }
    let active = store::current();
    let held = auto::held().filter(|_| auto::load().any_on());
    for name in &names {
        let mark = if active.as_ref() == Some(name) { "*" } else { " " };
        let note = if held.as_ref() == Some(name) { "  (held by hand; janus resume)" } else { "" };
        println!("{mark} {name:<12} {}{note}", describe(name)?);
    }
    Ok(0)
}

fn cmd_status() -> Result<i32> {
    let active = store::current();
    let live = claude::live()?;
    let owner = match &live.tokens {
        Some(t) => store::owner_of(t)?,
        None => None,
    };
    println!("Active:   {}", active.as_deref().unwrap_or("none"));
    println!(
        "On disk:  {}  (tokens match: {})",
        claude::email(live.account.as_ref()),
        owner.as_deref().unwrap_or("no saved account")
    );
    match (&active, &owner) {
        (Some(active), Some(owner)) if active != owner => {
            println!("A session wrote '{owner}' back to disk. Run `janus {active}` again.");
            Ok(1)
        }
        (_, None) => {
            println!("The tokens were renewed since the last save; that's normal.");
            Ok(0)
        }
        _ => Ok(0),
    }
}

fn cmd_rm(name: &str) -> Result<i32> {
    if !store::exists(name)? {
        bail!("No saved account '{name}'.");
    }
    store::remove(name)?;
    println!("Removed '{name}'");
    Ok(0)
}

fn dispatch(argv: &[String]) -> Result<i32> {
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["ls" | "list"] => cmd_list(),
        ["-V" | "--version" | "version"] => {
            println!("janus {VERSION}");
            Ok(0)
        }
        ["-h" | "--help" | "help"] => {
            println!("{USAGE}");
            Ok(0)
        }
        ["save", name] => cmd_save(name),
        ["use", name] => cmd_use(name),
        ["status"] => cmd_status(),
        ["usage"] => cmd_usage(),
        ["auto", rest @ ..] => auto::cmd_auto(rest),
        ["resume"] => auto::cmd_resume(),
        ["watch"] => auto::cmd_watch(false),
        ["watch", "--once"] => auto::cmd_watch(true),
        ["logs"] => cmd_logs(false),
        ["logs", "-f" | "--follow"] => cmd_logs(true),
        ["rm", name] => cmd_rm(name),
        [name] if !name.starts_with('-') && !COMMANDS.contains(name) => cmd_use(name),
        _ => {
            eprintln!("janus: unknown command `{}`.\n\n{USAGE}", args.join(" "));
            Ok(2)
        }
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Short commands stop quietly when their output is cut off (`janus usage | head`).
    // The watcher keeps ignoring it, so a closed log can't stop it.
    if argv.first().map(String::as_str) != Some("watch") {
        // SAFETY: restoring the default action for one signal before any threads start.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    let code = match dispatch(&argv) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("janus: {e:#}");
            1
        }
    };
    std::process::exit(code);
}
