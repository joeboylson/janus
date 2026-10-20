//! Claude Code's login, which every session reads:
//!
//!     "claudeAiOauth": the tokens, in the secure store below
//!     ~/.claude.json "oauthAccount": who you are
//!
//! The secure store holds one JSON document: the tokens and the MCP logins
//! ("mcpOAuth"). On a Mac it's the Keychain item "Claude Code-credentials",
//! with ~/.claude/.credentials.json as the fallback Claude Code uses only
//! while that item doesn't exist (say it logged in while the Keychain was
//! locked). Elsewhere it's the file.
//!
//! Where it lives matters for open sessions: they read the Keychain again
//! every 30 seconds, but only look at the file at start or once their token
//! stops working. So on a Mac every save goes to the Keychain.
//!
//! Only "claudeAiOauth" and "oauthAccount" are ever changed; everything else
//! stays as it is, shared by all accounts.

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn user_home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

pub fn credentials_path() -> PathBuf {
    user_home().join(".claude").join(".credentials.json")
}

pub fn config_path() -> PathBuf {
    user_home().join(".claude.json")
}

/// A JSON object from a file; a missing file is an empty object.
pub fn read_object(path: &Path) -> Result<Map<String, Value>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(e).with_context(|| format!("couldn't read {}", path.display())),
    };
    parse_object(&text).with_context(|| format!("{} isn't a JSON object", path.display()))
}

fn parse_object(text: &str) -> Result<Map<String, Value>> {
    match serde_json::from_str(text)? {
        Value::Object(m) => Ok(m),
        _ => bail!("not a JSON object"),
    }
}

/// Write a file whole or not at all. Sessions read these files at any
/// moment, so they must never see one half written.
pub fn write_object(path: &Path, data: &Map<String, Value>, mode: u32) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        fs::write(&tmp, serde_json::to_string_pretty(data)? + "\n")?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| format!("couldn't write {}", path.display()))
}

/// The file's current permissions, so writing it back doesn't change them.
fn mode_of(path: &Path, default: u32) -> u32 {
    fs::metadata(path).map(|m| m.permissions().mode() & 0o777).unwrap_or(default)
}

// ---- the Keychain (macOS) ---------------------------------------------------

/// `security` exit codes.
const NOT_FOUND: i32 = 44;
const LOCKED: i32 = 36;

/// Claude Code's item name. CLAUDE_CONFIG_DIR would add a suffix; janus
/// works with the default ~/.claude only. JANUS_KEYCHAIN_SERVICE is for tests.
fn keychain_service() -> String {
    std::env::var("JANUS_KEYCHAIN_SERVICE").unwrap_or_else(|_| "Claude Code-credentials".into())
}

/// The item's account: your username, as Claude Code picks it.
fn keychain_account() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    let plain = !user.is_empty() && user.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if plain { user } else { "claude-code-user".into() }
}

fn use_keychain() -> bool {
    cfg!(target_os = "macos")
}

/// Longer than this and `security` is waiting on a prompt on the Mac's
/// screen (a locked Keychain, say), which may be nobody's to answer.
const SECURITY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Your real home folder, from the user database rather than $HOME: the
/// Keychain belongs to the account, and with another $HOME `security` finds
/// no Keychain and asks on screen to make one.
fn account_home() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static storage or null; it's
    // read at once, before anything else could call it.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned())
    }
}

/// Run `security`, giving it `input` on stdin, and stop it if it takes too
/// long. Returns its exit code and output.
fn security(args: &[&str], input: Option<&str>) -> Result<(Option<i32>, Vec<u8>)> {
    let mut cmd = Command::new("security");
    cmd.args(args).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(home) = account_home() {
        cmd.env("HOME", home);
    }
    let mut child = cmd.spawn().context("couldn't run `security` for the Keychain")?;
    if let Some(input) = input {
        // Dropped at the end of the statement, which closes it so `security -i` finishes.
        child.stdin.take().context("no stdin for security")?.write_all(input.as_bytes())?;
    }
    let mut stdout = child.stdout.take().context("no stdout for security")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut out);
        out
    });
    let deadline = std::time::Instant::now() + SECURITY_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "The Keychain didn't answer within {}s. It's probably asking for your password on the Mac's screen; \
                 unlock it there or with `security unlock-keychain`, then try again.",
                SECURITY_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    Ok((status.code(), reader.join().unwrap_or_default()))
}

/// The Keychain item's document, or None if there's no item.
fn read_keychain() -> Result<Option<Map<String, Value>>> {
    let (code, stdout) = security(&["find-generic-password", "-a", &keychain_account(), "-w", "-s", &keychain_service()], None)?;
    match code {
        Some(0) => {}
        Some(NOT_FOUND) => return Ok(None),
        Some(LOCKED) => bail!("The Keychain is locked. Unlock it (security unlock-keychain), then try again."),
        code => bail!("Reading the Keychain failed (security exited {code:?})."),
    }
    let text = String::from_utf8_lossy(&stdout).trim().to_string();
    // `security -w` prints the value as text, or as hex when it isn't plain text.
    let parsed = parse_object(&text).or_else(|_| parse_object(&String::from_utf8_lossy(&unhex(&text)?)));
    parsed.map(Some).context("The Keychain item Claude Code-credentials isn't the JSON Claude Code saves.")
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        bail!("odd length");
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into)).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Save the document as the Keychain item, creating or replacing it. Returns
/// false if the Keychain is locked.
fn write_keychain(doc: &Map<String, Value>) -> Result<bool> {
    let value = hex(serde_json::to_string(doc)?.as_bytes());
    let (account, service) = (keychain_account(), keychain_service());
    // Like Claude Code: through stdin when it fits, so the tokens don't show
    // up in `ps`; `security -i` reads lines of at most about 4 KB.
    let line = format!("add-generic-password -U -a \"{account}\" -s \"{service}\" -X \"{value}\"\n");
    let (code, _) = if line.len() <= 4032 {
        security(&["-i"], Some(&line))?
    } else {
        security(&["add-generic-password", "-U", "-a", &account, "-s", &service, "-X", &value], None)?
    };
    match code {
        Some(0) => {}
        Some(LOCKED) => return Ok(false),
        code => bail!("Saving to the Keychain failed (security exited {code:?})."),
    }
    // `security -i` exits 0 even when its command fails, so read it back.
    if read_keychain()?.as_ref() != Some(doc) {
        bail!("Saving to the Keychain didn't take; the item reads back different.");
    }
    Ok(true)
}

// ---- the login --------------------------------------------------------------

/// Where Claude Code is reading the tokens from right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Store {
    Keychain,
    File,
}

impl Store {
    /// For `janus status`.
    pub fn describe(self) -> &'static str {
        match self {
            Store::Keychain => "the Keychain (open sessions follow a switch within 30 seconds)",
            Store::File => "~/.claude/.credentials.json (open sessions follow a switch only once they restart or renew their login)",
        }
    }
}

/// The secure store's whole document, and where it came from: the Keychain
/// item if there is one, else the file, the order Claude Code reads them in.
fn secure_store() -> Result<(Map<String, Value>, Store)> {
    if use_keychain() {
        if let Some(doc) = read_keychain()? {
            return Ok((doc, Store::Keychain));
        }
    }
    Ok((read_object(&credentials_path())?, Store::File))
}

/// The login Claude Code is using right now.
pub struct Live {
    pub tokens: Option<Value>,
    pub account: Option<Value>,
    pub store: Store,
}

pub fn live() -> Result<Live> {
    let (mut doc, store) = secure_store()?;
    Ok(Live { tokens: doc.remove("claudeAiOauth"), account: read_object(&config_path())?.remove("oauthAccount"), store })
}

/// Put another account's login in place. On a Mac it goes to the Keychain,
/// which open sessions keep checking; with no item yet, the item is made
/// from the whole file so MCP logins come along. Returns a warning to show
/// when the switch won't reach open sessions.
pub fn put(tokens: &Value, account: Option<&Value>) -> Result<Option<String>> {
    let (mut doc, store) = secure_store()?;
    doc.insert("claudeAiOauth".into(), tokens.clone());
    let mut warning = None;
    if use_keychain() {
        if !write_keychain(&doc)? {
            if store == Store::Keychain {
                bail!("The Keychain is locked. Unlock it (security unlock-keychain), then switch again.");
            }
            write_object(&credentials_path(), &doc, 0o600)?;
            warning = Some(
                "The Keychain is locked, so this went to ~/.claude/.credentials.json. New sessions use it; \
                 open ones keep the old account until they restart. Unlock it (security unlock-keychain) \
                 and switch again to reach them."
                    .to_string(),
            );
        } else if credentials_path().exists() {
            // Keep the fallback file in step, in case the item goes away.
            let mut file = read_object(&credentials_path())?;
            file.insert("claudeAiOauth".into(), tokens.clone());
            write_object(&credentials_path(), &file, 0o600)?;
        }
    } else {
        write_object(&credentials_path(), &doc, 0o600)?;
        warning = Some("Open sessions keep the old account until they restart or renew their login; new ones use this one.".to_string());
    }

    if let Some(account) = account {
        let path = config_path();
        let mut config = read_object(&path)?;
        config.insert("oauthAccount".into(), account.clone());
        write_object(&path, &config, mode_of(&path, 0o644))?;
    }
    Ok(warning)
}

pub fn refresh_token(tokens: &Value) -> Option<&str> {
    tokens.get("refreshToken").and_then(Value::as_str)
}

pub fn email(account: Option<&Value>) -> &str {
    account.and_then(|a| a.get("emailAddress")).and_then(Value::as_str).unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let text = r#"{"claudeAiOauth":{"accessToken":"x"},"name":"Zoë"}"#;
        assert_eq!(unhex(&hex(text.as_bytes())).unwrap(), text.as_bytes());
        assert!(unhex("abc").is_err());
    }
}
