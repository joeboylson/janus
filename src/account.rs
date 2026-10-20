//! `account`: save a login as an account, forget one, or list them. Adding
//! can also walk you through logging in to another account first: janus
//! waits, and notices when the new login lands.

use crate::arguments::{AccountAction, COMMAND_WORDS};
use crate::ask::{item, Asker};
use crate::claude::{self, Live};
use crate::{auto, store};
use anyhow::{bail, Result};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Attribute, Cell, Color, ContentArrangement, Table};
use serde_json::Value;
use std::time::Duration;

const POLL_EVERY: Duration = Duration::from_millis(1500);

pub fn run(ask: &Asker, action: Option<AccountAction>, name: Option<String>, yes: bool) -> Result<i32> {
    let action = match action {
        Some(action) => action,
        None if store::names()?.is_empty() => AccountAction::Add,
        None => ask.action(None, "janus account")?,
    };
    match action {
        AccountAction::Add => {
            ask.intro("janus account add");
            let name = add(ask, name)?;
            ask.outro(&format!("Saved. Switch to it any time with `janus {name}`."));
            Ok(0)
        }
        AccountAction::Remove => remove(ask, name, yes),
        AccountAction::List => {
            println!("{}", list()?);
            Ok(0)
        }
    }
}

// ---- telling accounts apart --------------------------------------------------

/// Who a login belongs to: Claude Code's account id, or failing that the
/// email. Tokens don't do: open sessions renew them.
fn identity(account: Option<&Value>) -> Option<String> {
    let field = |k: &str| account.and_then(|a| a.get(k)).and_then(Value::as_str).filter(|v| !v.is_empty());
    field("accountUuid").or_else(|| field("emailAddress")).map(str::to_string)
}

/// The saved account this login is, if any.
pub fn saved_as(live: &Live) -> Result<Option<String>> {
    if let Some(id) = identity(live.account.as_ref()) {
        for name in store::names()? {
            if identity(store::load(&name)?.get("oauthAccount")) == Some(id.clone()) {
                return Ok(Some(name));
            }
        }
    }
    match &live.tokens {
        Some(tokens) => store::owner_of(tokens),
        None => Ok(None),
    }
}

/// Wait, with a spinner, until Claude Code has a login that isn't one of the
/// saved accounts: what `/login` with another account leaves behind.
pub fn wait_for_new_login(ask: &Asker) -> Result<Live> {
    if !ask.is_interactive() {
        bail!("Log in with the other account first (in Claude Code: /login), then run janus account add <name>.");
    }
    let before = claude::live()?.tokens.and_then(|t| claude::refresh_token(&t).map(str::to_string));
    let spinner = ask.spinner("Waiting for you to log in with the other account (Ctrl-C to stop)");
    loop {
        std::thread::sleep(POLL_EVERY);
        let live = claude::live()?;
        let renewed = live.tokens.as_ref().and_then(claude::refresh_token).map(str::to_string) != before;
        if live.tokens.is_some() && renewed && saved_as(&live)?.is_none() {
            if let Some(spinner) = spinner {
                spinner.stop(format!("Logged in as {}", claude::email(live.account.as_ref())));
            }
            return Ok(live);
        }
    }
}

/// Wait for any login at all, for when Claude Code has none yet.
pub fn wait_for_login(ask: &Asker) -> Result<Live> {
    if !ask.is_interactive() {
        bail!("Claude Code isn't logged in. Run `claude`, then /login, then janus account add <name>.");
    }
    ask.note("Claude Code isn't logged in yet. Start it (`claude`) and run /login; janus waits here.");
    let spinner = ask.spinner("Waiting for a login (Ctrl-C to stop)");
    loop {
        std::thread::sleep(POLL_EVERY);
        let live = claude::live()?;
        if live.tokens.is_some() {
            if let Some(spinner) = spinner {
                spinner.stop(format!("Logged in as {}", claude::email(live.account.as_ref())));
            }
            return Ok(live);
        }
    }
}

// ---- add ---------------------------------------------------------------------

/// A name for a new account: free, and not a command word.
pub fn check_new_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Give it a name, such as work.".into());
    }
    if name.contains('/') || name.starts_with('.') || name.contains(char::is_whitespace) {
        return Err("Use letters, numbers, dots and dashes, with no spaces.".into());
    }
    if COMMAND_WORDS.contains(&name) {
        return Err(format!("`{name}` is a janus command, so it can't be an account name."));
    }
    if store::exists(name).unwrap_or(false) {
        return Err(format!("'{name}' is taken."));
    }
    Ok(())
}

/// "joe.boylson@gmail.com" -> "joe.boylson", made free if it's taken.
fn suggest_name(email: &str) -> String {
    let base: String = email.split('@').next().unwrap_or("").to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c)).collect();
    let base = if base.is_empty() || base.starts_with('.') || COMMAND_WORDS.contains(&base.as_str()) { "main".to_string() } else { base };
    (1..).map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") }).find(|n| check_new_name(n).is_ok()).unwrap_or(base)
}

#[derive(Clone, PartialEq, Eq)]
enum Source {
    Current,
    Another,
}

/// Save a login as an account and return its name: the login Claude Code
/// has now, or, picked in the menu, another one you log in to first.
pub fn add(ask: &Asker, name: Option<String>) -> Result<String> {
    let mut live = claude::live()?;
    if live.tokens.is_none() {
        live = wait_for_login(ask)?;
    } else if name.is_none() && ask.is_interactive() {
        let email = claude::email(live.account.as_ref()).to_string();
        let current = match saved_as(&live)? {
            Some(saved) => item(Source::Current, format!("Update '{saved}' with the login Claude Code has now"), email),
            None => item(Source::Current, "The login Claude Code has now", email),
        };
        let another = item(Source::Another, "Another account", "you log in to it with /login; janus waits");
        if ask.pick(None, "Which login?", vec![current, another], "", "")? == Source::Another {
            live = wait_for_new_login(ask)?;
        }
    }
    save_login(ask, &live, name)
}

/// Save this login: under its saved name if it's one already, else under
/// `name`, asked for when left out. Returns the name.
pub fn save_login(ask: &Asker, live: &Live, name: Option<String>) -> Result<String> {
    let Some(tokens) = live.tokens.clone() else { bail!("Claude Code isn't logged in.") };
    let email = claude::email(live.account.as_ref()).to_string();

    // A login that's already saved is refreshed under its own name.
    if let Some(saved) = saved_as(live)? {
        if name.as_ref().is_none_or(|n| *n == saved) {
            store::save(&saved, &tokens, live.account.as_ref())?;
            store::set_current(&saved)?;
            ask.success(format!("'{saved}' is {email}; its saved login is up to date."));
            return Ok(saved);
        }
        bail!("That login is saved already, as '{saved}'.");
    }
    if let Some(n) = &name {
        if store::exists(n)? {
            let theirs = claude::email(store::load(n)?.get("oauthAccount")).to_string();
            bail!("'{n}' is {theirs} already. Pick another name, or `janus account remove {n}` first.");
        }
    }
    let name = ask.text_or_default(name, &format!("What should janus call {email}?"), &suggest_name(&email), check_new_name)?;
    store::save(&name, &tokens, live.account.as_ref())?;
    // It's the login in place, so it's the one in use.
    store::set_current(&name)?;
    ask.success(format!("Saved {email} as '{name}'."));
    Ok(name)
}

// ---- remove ------------------------------------------------------------------

fn account_items() -> Result<Vec<crate::ask::Item<String>>> {
    let current = store::current();
    store::names()?
        .into_iter()
        .map(|n| {
            let email = claude::email(store::load(&n)?.get("oauthAccount")).to_string();
            let hint = if current.as_ref() == Some(&n) { format!("{email}, in use") } else { email };
            Ok(item(n.clone(), n, hint))
        })
        .collect()
}

pub fn pick_account(ask: &Asker, given: Option<String>, prompt: &str, usage: &str) -> Result<String> {
    if let Some(n) = &given {
        if !store::exists(n)? {
            let have = store::names()?.join(", ");
            bail!("No saved account '{n}' (have: {}).", if have.is_empty() { "none" } else { &have });
        }
    }
    ask.pick(given, prompt, account_items()?, usage, "No saved accounts yet. Run `janus setup`.")
}

fn remove(ask: &Asker, name: Option<String>, yes: bool) -> Result<i32> {
    let name = pick_account(ask, name, "Which account should janus forget?", "janus account remove <name>")?;
    let email = claude::email(store::load(&name)?.get("oauthAccount")).to_string();
    if !ask.confirm(yes, &format!("Forget '{name}' ({email})? Its saved login is deleted; Claude Code stays logged in as it is."))? {
        return Ok(0);
    }
    let _lock = store::lock()?;
    store::remove(&name)?;
    if auto::held().as_deref() == Some(name.as_str()) {
        auto::set_held(None)?;
    }
    if auto::forget_account(&name)? {
        ask.warning(format!("'{name}' was one of the two accounts automatic switching uses, so it's off. `janus auto on` sets it up again."));
    }
    ask.success(format!("Forgot '{name}'."));
    Ok(0)
}

// ---- list --------------------------------------------------------------------

pub fn list() -> Result<String> {
    let names = store::names()?;
    if names.is_empty() {
        return Ok("No saved accounts yet. Run `janus setup`.".into());
    }
    let s = auto::load();
    let current = store::current();
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(["", "Account", "Email", "Plan", "Role"].map(|h| Cell::new(h).add_attribute(Attribute::Bold)));
    for name in names {
        let data = store::load(&name)?;
        let plan = data.get("claudeAiOauth").and_then(|t| t.get("subscriptionType")).and_then(Value::as_str).unwrap_or("?").to_string();
        let role = match (&s.primary, &s.secondary) {
            (Some(p), _) if *p == name => "primary",
            (_, Some(sec)) if *sec == name => "secondary",
            _ => "",
        };
        let in_use = if current.as_ref() == Some(&name) { Cell::new("in use").fg(Color::Green) } else { Cell::new("") };
        table.add_row(vec![
            in_use,
            Cell::new(&name).add_attribute(Attribute::Bold),
            Cell::new(claude::email(data.get("oauthAccount"))),
            Cell::new(plan),
            Cell::new(role).fg(Color::DarkGrey),
        ]);
    }
    Ok(table.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_account_is_known_by_its_id_then_its_email() {
        assert_eq!(identity(Some(&json!({"accountUuid": "u1", "emailAddress": "a@x.com"}))), Some("u1".into()));
        assert_eq!(identity(Some(&json!({"emailAddress": "a@x.com"}))), Some("a@x.com".into()));
        assert_eq!(identity(Some(&json!({"accountUuid": ""}))), None);
        assert_eq!(identity(None), None);
    }

    #[test]
    fn command_words_and_odd_names_are_refused() {
        assert!(check_new_name("status").is_err());
        assert!(check_new_name("a/b").is_err());
        assert!(check_new_name(".x").is_err());
        assert!(check_new_name("my work").is_err());
        assert!(check_new_name("").is_err());
    }

    #[test]
    fn a_suggested_name_comes_from_the_email() {
        assert_eq!(suggest_name("Joe.Boylson+cc@gmail.com"), "joe.boylsoncc");
        assert_eq!(suggest_name("status@x.com"), "main");
        assert_eq!(suggest_name("@x.com"), "main");
    }
}
