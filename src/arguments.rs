//! The janus command's arguments. Each command does one kind of thing; the
//! word after it picks what to do (it asks when left out), and whatever
//! isn't given is asked for, in a terminal.

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "janus",
    version = crate::VERSION,
    about = "Switch Claude Code between accounts without closing sessions. Each command asks for what you leave out.",
    after_help = "New here? Run `janus setup`. `janus <name>` is short for `janus use <name>`."
)]
pub struct CommandLine {
    /// What to do; every command is listed without one.
    #[command(subcommand)]
    pub command: Option<Top>,
}

#[derive(Subcommand)]
pub enum Top {
    /// Walk through setting janus up: save both accounts, pick the primary, choose when it switches on its own.
    Setup,
    /// Everything on one page: the account in use, usage and when it resets, logins, the watcher.
    Status {
        /// Keep it on screen, updated every 2 seconds; q quits.
        #[arg(long, short)]
        watch: bool,
    },
    /// Switch every open session to an account, and hold it there.
    Use {
        /// The saved account, such as work.
        name: Option<String>,
    },
    /// After picking an account by hand, let janus switch on its own again.
    Resume,
    /// Add, remove or list saved accounts.
    Account {
        action: Option<AccountAction>,
        /// The account's name, such as work.
        name: Option<String>,
        #[arg(long, short)]
        yes: bool,
    },
    /// Turn switching on its own on or off, show its settings, or send a test notification.
    Auto {
        action: Option<AutoAction>,
        /// The account to use whenever it has room.
        #[arg(long)]
        primary: Option<String>,
        /// Where to go when the primary runs low.
        #[arg(long)]
        secondary: Option<String>,
        /// Switch at this % of the weekly limit, or off.
        #[arg(long)]
        weekly: Option<String>,
        /// Switch at this % of the 5-hour limit, or off.
        #[arg(long = "five-hour", visible_alias = "5h")]
        five_hour: Option<String>,
        /// A notification when it switches: on or off.
        #[arg(long)]
        notify: Option<String>,
    },
    /// What the watcher has done.
    Logs {
        /// Keep showing new lines as they land.
        #[arg(long, short)]
        watch: bool,
    },
    /// Check usage and switch when due, every 2 minutes; the background service runs this.
    #[command(hide = true)]
    Watch {
        /// Check once and stop.
        #[arg(long)]
        once: bool,
    },
    /// `janus <name>`: the same as `janus use <name>`.
    #[command(external_subcommand)]
    Name(Vec<String>),
}

/// Command words, which can't be account names: `janus <name>` would never reach them.
pub const COMMAND_WORDS: [&str; 10] = ["setup", "status", "use", "resume", "account", "auto", "logs", "watch", "help", "version"];

/// An action word: its name on the command line, and how the menu says it.
pub trait Choice: ValueEnum + Clone + Eq {
    fn label(&self) -> &'static str;
    fn every() -> Vec<Self> {
        Self::value_variants().to_vec()
    }
    fn word(&self) -> String {
        self.to_possible_value().map(|value| value.get_name().to_string()).unwrap_or_default()
    }
}

macro_rules! choices {
    ($name:ident { $($variant:ident => $label:expr),+ $(,)? }) => {
        #[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($variant),+ }
        impl Choice for $name {
            fn label(&self) -> &'static str {
                match self { $($name::$variant => $label),+ }
            }
        }
    };
}

choices!(AccountAction { Add => "Save a login as an account", Remove => "Forget a saved account", List => "List the saved accounts" });
choices!(AutoAction {
    On => "Turn it on, or change its settings",
    Off => "Turn it off",
    Show => "Show its settings",
    Test => "Send a test notification",
});

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_arguments_hang_together() {
        CommandLine::command().debug_assert();
    }

    #[test]
    fn a_bare_name_is_a_switch() {
        let line = CommandLine::try_parse_from(["janus", "work"]).unwrap();
        assert!(matches!(line.command, Some(Top::Name(words)) if words == ["work"]));
        let line = CommandLine::try_parse_from(["janus", "auto", "on", "--5h", "95"]).unwrap();
        assert!(matches!(line.command, Some(Top::Auto { action: Some(AutoAction::On), five_hour: Some(p), .. }) if p == "95"));
    }

    #[test]
    fn every_command_word_is_reserved() {
        for sub in CommandLine::command().get_subcommands() {
            assert!(COMMAND_WORDS.contains(&sub.get_name()), "{} isn't in COMMAND_WORDS", sub.get_name());
        }
    }
}
