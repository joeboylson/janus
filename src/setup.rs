//! `setup`: everything janus needs, one question at a time. It saves the
//! login Claude Code has now, waits while you log in to the other account,
//! then sets up switching on its own. Run again, it picks up where things
//! are and lets you change them.

use crate::account;
use crate::ask::Asker;
use crate::auto::{self, AutoInputs};
use crate::claude;
use crate::store;
use anyhow::{bail, Result};

pub fn run(ask: &Asker) -> Result<i32> {
    if !ask.is_interactive() {
        bail!(
            "janus setup asks questions, so it needs a terminal. Without one: janus account add <name> for each \
             login, then janus auto on --primary <name> --secondary <name>."
        );
    }
    ask.intro("janus setup");
    if cfg!(target_os = "macos") {
        ask.note("janus keeps your logins in ~/.janus and switches them in the Keychain, where Claude Code reads them.");
    }

    // 1. The login Claude Code has now.
    let mut live = claude::live()?;
    if live.tokens.is_none() {
        live = account::wait_for_login(ask)?;
    }
    match account::saved_as(&live)? {
        Some(name) => ask.success(format!("Claude Code is logged in as {}, saved as '{name}'.", claude::email(live.account.as_ref()))),
        None => {
            ask.note(format!("Claude Code is logged in as {}. First, a name for it.", claude::email(live.account.as_ref())));
            account::save_login(ask, &live, None)?;
        }
    }

    // 2. The other account.
    while store::names()?.len() < 2 {
        ask.note(
            "Now your other account. In any Claude Code session, run /login and sign in with it \
             (or start one with `claude`). janus notices when it's done.",
        );
        let live = account::wait_for_new_login(ask)?;
        account::save_login(ask, &live, None)?;
    }
    let names = store::names()?;
    if names.len() > 2 {
        ask.note(format!("Saved accounts: {}.", names.join(", ")));
    }
    while ask.yes_no("Save another account too?", false)? {
        ask.note("Run /login in Claude Code with it; janus notices when it's done.");
        let live = account::wait_for_new_login(ask)?;
        account::save_login(ask, &live, None)?;
    }

    // 3. Switching on its own.
    let s = match ask.yes_no("Switch accounts on its own when one runs low?", true)? {
        true => Some(auto::configure(ask, AutoInputs::default())?),
        false => None,
    };

    // 4. Start on the primary.
    if let Some(primary) = s.as_ref().filter(|s| s.any_on()).and_then(|s| s.primary.clone()) {
        if store::current().as_deref() != Some(primary.as_str()) && ask.yes_no(&format!("Switch to {primary} now?"), true)? {
            let _lock = store::lock()?;
            let (email, warning) = crate::switch_to(&primary)?;
            auto::set_away(None)?;
            auto::set_held(None)?;
            ask.success(format!("Switched to '{primary}' ({email})."));
            if let Some(warning) = warning {
                ask.warning(warning);
            }
        }
    }

    let next = match s.filter(|s| s.any_on()) {
        Some(s) => format!(
            "Ready. janus uses {} and moves to {} when it runs low. `janus status` shows what's going on.",
            s.primary.unwrap_or_default(),
            s.secondary.unwrap_or_default()
        ),
        None => "Ready. Switch with `janus <name>`; `janus status` shows what's going on.".to_string(),
    };
    ask.outro(&next);
    Ok(0)
}
