//! `run`: start a Claude Code session that stays on one account. Every other
//! session reads the one login janus switches; this one is handed its own,
//! through CLAUDE_CODE_OAUTH_TOKEN, which Claude Code keeps over the shared
//! login. So `janus work`, `janus resume` and the watcher never move it.
//!
//! The login it's handed is the long-lived one `claude setup-token` makes
//! (good for a year), saved once per account. The account's everyday tokens
//! won't do: they renew, and the renewed ones would stop the copy janus
//! switches in from working.

use crate::account;
use crate::ask::Asker;
use crate::claude;
use crate::store;
use anyhow::{anyhow, bail, Result};
use std::os::unix::process::CommandExt;
use std::process::Command;

/// `claude setup-token` makes logins good for a year; warn a month before.
const WARN_AFTER_SECS: u64 = 335 * 24 * 60 * 60;

pub fn cmd_run(ask: &Asker, name: Option<String>, new_token: bool, claude_args: Vec<String>) -> Result<i32> {
    let name = account::pick_account(ask, name, "Start a session on which account?", "janus run <name>")?;
    let email = claude::email(store::load(&name)?.get("oauthAccount")).to_string();
    let token = match store::long_token(&name)? {
        Some((token, saved_at)) if !new_token => {
            if store::now().saturating_sub(saved_at) > WARN_AFTER_SECS {
                ask.warning(format!("The long-lived login for '{name}' is almost a year old and stops working soon. `janus run --new-token {name}` gets a new one."));
            }
            token
        }
        _ => new_long_token(ask, &name, &email)?,
    };

    let err = Command::new("claude").args(&claude_args).env("CLAUDE_CODE_OAUTH_TOKEN", token).exec();
    bail!("couldn't run claude: {err}");
}

/// Have `claude setup-token` make a long-lived login, take it as pasted, and save it.
fn new_long_token(ask: &Asker, name: &str, email: &str) -> Result<String> {
    if !ask.is_interactive() {
        bail!("'{name}' has no long-lived login yet. Run `janus run {name}` once in a terminal to set it up.");
    }
    ask.intro("janus run");
    ask.note(format!(
        "A session that stays on '{name}' needs a long-lived login for it, made once. \
         `claude setup-token` opens your browser: sign in there as {email}, then paste the token it prints."
    ));
    if ask.yes_no("Run `claude setup-token` now?", true)? {
        let status = Command::new("claude").arg("setup-token").status().map_err(|e| anyhow!("couldn't run claude: {e}"))?;
        if !status.success() {
            bail!("`claude setup-token` didn't finish, so nothing was saved.");
        }
    }
    let token: String = cliclack::password(format!("Paste the token for {email}"))
        .validate(|text: &String| check_token(text.trim()))
        .interact()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::Interrupted => anyhow!("Cancelled."),
            _ => anyhow!(e),
        })?;
    let token = token.trim().to_string();
    store::save_long_token(name, &token)?;
    ask.success(format!("Saved. `janus run {name}` starts on {email} from now on, with no questions."));
    Ok(token)
}

/// What `claude setup-token` prints looks like sk-ant-oat01-...
fn check_token(token: &str) -> Result<(), String> {
    if !token.starts_with("sk-ant-") || token.contains(char::is_whitespace) {
        return Err("That isn't a token: it starts with sk-ant- and has no spaces.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_token_is_taken() {
        assert!(check_token("sk-ant-oat01-abc").is_ok());
        assert!(check_token("").is_err());
        assert!(check_token("hello").is_err());
        assert!(check_token("sk-ant-oat01 abc").is_err());
    }
}
