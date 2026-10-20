//! Getting each input the same way everywhere, as legion does: from its
//! flag when given; otherwise from a prompt when there's a terminal;
//! otherwise an error that names the flag. Lists with one choice pick it
//! without asking.

use crate::arguments::Choice;
use anyhow::{anyhow, bail, Result};
use std::io::IsTerminal;

pub struct Asker {
    is_interactive: bool,
}

/// A prompt was cancelled (Esc or Ctrl-C), or failed.
fn asked<T>(answer: std::io::Result<T>) -> Result<T> {
    answer.map_err(|error| match error.kind() {
        std::io::ErrorKind::Interrupted => anyhow!("Cancelled."),
        _ => anyhow!(error),
    })
}

/// One thing to pick: its value, what the list says, and a hint beside it.
pub struct Item<T> {
    pub value: T,
    pub label: String,
    pub hint: String,
}

pub fn item<T>(value: T, label: impl ToString, hint: impl ToString) -> Item<T> {
    Item { value, label: label.to_string(), hint: hint.to_string() }
}

impl Asker {
    pub fn new() -> Asker {
        Asker { is_interactive: std::io::stdin().is_terminal() && std::io::stdout().is_terminal() }
    }

    pub fn is_interactive(&self) -> bool {
        self.is_interactive
    }

    fn missing(&self, flag: &str) -> anyhow::Error {
        anyhow!("Say it with {flag}: there's no terminal to ask in.")
    }

    pub fn note(&self, text: impl std::fmt::Display) {
        match self.is_interactive {
            true => {
                let _ = cliclack::log::info(text);
            }
            false => eprintln!("{text}"),
        }
    }

    pub fn warning(&self, text: impl std::fmt::Display) {
        match self.is_interactive {
            true => {
                let _ = cliclack::log::warning(text);
            }
            false => eprintln!("{text}"),
        }
    }

    pub fn success(&self, text: impl std::fmt::Display) {
        match self.is_interactive {
            true => {
                let _ = cliclack::log::success(text);
            }
            false => println!("{text}"),
        }
    }

    /// What to do, when the command was given without its action word.
    pub fn action<A: Choice>(&self, given: Option<A>, command: &str) -> Result<A> {
        if let Some(action) = given {
            return Ok(action);
        }
        if !self.is_interactive {
            let words: Vec<String> = A::every().iter().map(Choice::word).collect();
            bail!("Say what to do: {command} {}", words.join("|"));
        }
        let mut menu = cliclack::select("What do you want to do?");
        for action in A::every() {
            let label = action.label();
            menu = menu.item(action, label, "");
        }
        asked(menu.interact())
    }

    /// A short text with a suggestion kept with Enter, checked by `valid`.
    pub fn text_or_default(&self, given: Option<String>, prompt: &str, default: &str, valid: impl Fn(&str) -> Result<(), String> + 'static) -> Result<String> {
        match given {
            Some(text) => valid(&text).map(|_| text).map_err(|e| anyhow!(e)),
            None if self.is_interactive => {
                let check = move |text: &String| valid(text.trim());
                let text: String = asked(cliclack::input(prompt).default_input(default).validate(check).interact())?;
                Ok(text.trim().to_string())
            }
            None => valid(default).map(|_| default.to_string()).map_err(|e| anyhow!(e)),
        }
    }

    /// One of `items`. Given, it's taken as is; one item is picked without
    /// asking; none is `empty`.
    pub fn pick<T: Clone + Eq>(&self, given: Option<T>, prompt: &str, items: Vec<Item<T>>, flag: &str, empty: &str) -> Result<T> {
        if let Some(value) = given {
            return Ok(value);
        }
        match items.as_slice() {
            [] => bail!("{empty}"),
            [only] => Ok(only.value.clone()),
            _ if !self.is_interactive => Err(self.missing(flag)),
            _ => {
                let mut menu = cliclack::select(prompt);
                for choice in items {
                    menu = menu.item(choice.value, choice.label, choice.hint);
                }
                asked(menu.interact())
            }
        }
    }

    /// Some of `items`, starting with `ticked` ticked.
    pub fn pick_many<T: Clone + Eq>(&self, prompt: &str, items: Vec<Item<T>>, ticked: Vec<T>) -> Result<Vec<T>> {
        if !self.is_interactive {
            return Ok(ticked);
        }
        let mut menu = cliclack::multiselect(format!("{prompt} (space to tick, Enter when done)")).initial_values(ticked).required(false);
        for choice in items {
            menu = menu.item(choice.value, choice.label, choice.hint);
        }
        asked(menu.interact())
    }

    /// Whether to go ahead with something that can't be undone.
    pub fn confirm(&self, is_given: bool, prompt: &str) -> Result<bool> {
        match is_given {
            true => Ok(true),
            false if self.is_interactive => asked(cliclack::confirm(prompt).initial_value(false).interact()),
            false => Err(self.missing("--yes")),
        }
    }

    pub fn yes_no(&self, prompt: &str, default: bool) -> Result<bool> {
        match self.is_interactive {
            true => asked(cliclack::confirm(prompt).initial_value(default).interact()),
            false => Ok(default),
        }
    }

    pub fn spinner(&self, message: &str) -> Option<cliclack::ProgressBar> {
        self.is_interactive.then(|| {
            let spinner = cliclack::spinner();
            spinner.start(message);
            spinner
        })
    }

    pub fn intro(&self, title: &str) {
        if self.is_interactive {
            let _ = cliclack::intro(console::style(format!(" {title} ")).on_cyan().black());
        }
    }

    pub fn outro(&self, text: &str) {
        match self.is_interactive {
            true => {
                let _ = cliclack::outro(text);
            }
            false => println!("{text}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet() -> Asker {
        Asker { is_interactive: false }
    }

    #[test]
    fn a_missing_input_names_its_flag() {
        let two = vec![item("a".to_string(), "a", ""), item("b".to_string(), "b", "")];
        assert!(quiet().pick(None, "Which?", two, "--primary", "").unwrap_err().to_string().contains("--primary"));
        assert!(quiet().confirm(false, "Sure?").unwrap_err().to_string().contains("--yes"));
        assert!(quiet().confirm(true, "Sure?").unwrap());
    }

    #[test]
    fn one_choice_is_picked_and_none_says_why() {
        let one = vec![item("work".to_string(), "work", "")];
        assert_eq!(quiet().pick(None, "Which?", one, "--to", "none").unwrap(), "work");
        assert_eq!(quiet().pick::<String>(None, "Which?", vec![], "--to", "No accounts yet.").unwrap_err().to_string(), "No accounts yet.");
    }

    #[test]
    fn a_default_is_checked_too() {
        let positive = |t: &str| t.parse::<u32>().map(|_| ()).map_err(|_| "a number".to_string());
        assert_eq!(quiet().text_or_default(None, "?", "99", positive).unwrap(), "99");
        assert!(quiet().text_or_default(Some("x".into()), "?", "99", positive).is_err());
    }
}
