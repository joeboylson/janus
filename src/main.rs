//! The `janus` command. Each command does one kind of thing. Whatever it
//! needs and wasn't given, it asks for in a terminal; without one, it says
//! which flag or name is missing.

mod account;
mod arguments;
mod ask;
mod auto;
mod claude;
mod live;
mod setup;
mod status;
mod store;
mod usage;

use anyhow::{bail, Result};
use arguments::{CommandLine, Top};
use ask::Asker;
use clap::{CommandFactory, Parser};

/// "0.3.0 (built 2026-10-19 14:02)": the build time tells two copies apart.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (built ", env!("JANUS_BUILT"), ")");

/// Copy the login in place back into the active account before leaving it.
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

/// Put a saved account's login in place and make it the active one; returns
/// its email, and a warning when open sessions won't follow. Callers hold the lock.
pub fn switch_to(name: &str) -> Result<(String, Option<String>)> {
    if !store::exists(name)? {
        let have = store::names()?.join(", ");
        bail!("No saved account '{name}' (have: {}).", if have.is_empty() { "none" } else { &have });
    }
    sync_live_into_current()?;
    let target = store::load(name)?;
    let Some(tokens) = target.get("claudeAiOauth") else {
        bail!("The saved account '{name}' has no login in it. Log in with it and run `janus account add {name}`.");
    };
    let account = target.get("oauthAccount").filter(|a| !a.is_null());
    let warning = claude::put(tokens, account)?;
    store::set_current(name)?;
    Ok((claude::email(account).to_string(), warning))
}

fn cmd_use(ask: &Asker, name: Option<String>) -> Result<i32> {
    let name = account::pick_account(ask, name, "Switch to which account?", "janus use <name>")?;
    let _lock = store::lock()?;
    let (email, warning) = switch_to(&name)?;
    // A switch by hand: hold it there until `janus resume`.
    auto::set_away(None)?;
    auto::set_held(Some(&name))?;
    ask.success(format!("Switched to '{name}' ({email})."));
    if let Some(warning) = warning {
        ask.warning(warning);
    }
    if auto::load().any_on() {
        ask.note("Automatic switching is paused while you're on it by hand. `janus resume` hands it back.");
    }
    Ok(0)
}

fn cmd_logs(watch: bool) -> Result<i32> {
    let log = auto::log_path();
    if watch {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new("tail").args(["-n", "40", "-F"]).arg(&log).exec();
        bail!("couldn't run tail: {err}");
    }
    match std::fs::read_to_string(&log) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            println!("{}", lines[lines.len().saturating_sub(40)..].join("\n"));
        }
        Err(_) => println!("Nothing yet: the watcher writes here when it checks or switches."),
    }
    Ok(0)
}

fn run(line: CommandLine) -> Result<i32> {
    // On its own, it lists what it can do, the same as --help.
    let Some(command) = line.command else {
        CommandLine::command().print_help()?;
        return Ok(0);
    };
    let ask = Asker::new();
    match command {
        Top::Setup => setup::run(&ask),
        Top::Status { watch } => status::cmd_status(&ask, watch),
        Top::Use { name } => cmd_use(&ask, name),
        Top::Name(words) => match words.as_slice() {
            [name] => cmd_use(&ask, Some(name.clone())),
            _ => bail!("Unknown command `{}`. `janus --help` lists them.", words.join(" ")),
        },
        Top::Resume => auto::cmd_resume(&ask),
        Top::Account { action, name, yes } => account::run(&ask, action, name, yes),
        Top::Auto { action, primary, secondary, weekly, five_hour, notify } => {
            auto::run(&ask, action, auto::AutoInputs { primary, secondary, weekly, five_hour, notify })
        }
        Top::Logs { watch } => cmd_logs(watch),
        Top::Watch { once } => auto::cmd_watch(once),
    }
}

fn main() {
    let line = CommandLine::parse();
    // Short commands stop quietly when their output is cut off (`janus status | head`).
    // The watcher keeps ignoring it, so a closed log can't stop it.
    if !matches!(line.command, Some(Top::Watch { .. })) {
        // SAFETY: restoring the default action for one signal before any threads start.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    // NO_COLOR turns color off, as in most terminal tools.
    if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
    }
    let code = match run(line) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{} {e:#}", console::style("janus:").red().bold());
            1
        }
    };
    std::process::exit(code);
}
