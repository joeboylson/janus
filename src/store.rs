//! Saved accounts, one file each in ~/.janus/<name>.json, plus
//! .current naming the active one. JANUS_HOME moves the folder.

use crate::claude::{self, read_object, refresh_token, write_object};
use anyhow::{bail, Result};
use serde_json::{Map, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

pub fn home() -> PathBuf {
    match std::env::var("JANUS_HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h),
        _ => claude::user_home().join(".janus"),
    }
}

fn current_path() -> PathBuf {
    home().join(".current")
}

pub fn path(name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        bail!("Bad account name `{name}`.");
    }
    Ok(home().join(format!("{name}.json")))
}

pub fn names() -> Result<Vec<String>> {
    let dir = home();
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let mut names: Vec<String> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_string))
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    Ok(names)
}

pub fn exists(name: &str) -> Result<bool> {
    Ok(path(name)?.exists())
}

pub fn load(name: &str) -> Result<Map<String, Value>> {
    read_object(&path(name)?)
}

pub fn save(name: &str, tokens: &Value, account: Option<&Value>) -> Result<()> {
    let mut data = load(name)?;
    data.insert("claudeAiOauth".into(), tokens.clone());
    if let Some(account) = account {
        data.insert("oauthAccount".into(), account.clone());
    }
    data.insert("savedAt".into(), now().into());
    write_object(&path(name)?, &data, 0o600)
}

/// The account's long-lived login for `janus run`, from `claude setup-token`,
/// and when it was saved. Kept apart from "claudeAiOauth": it never renews,
/// so sessions that use it can't wear out the login janus switches in.
pub fn long_token(name: &str) -> Result<Option<(String, u64)>> {
    let data = load(name)?;
    let Some(saved) = data.get("longToken") else { return Ok(None) };
    let token = saved.get("token").and_then(Value::as_str).filter(|t| !t.is_empty());
    Ok(token.map(|t| (t.to_string(), saved.get("savedAt").and_then(Value::as_u64).unwrap_or(0))))
}

pub fn save_long_token(name: &str, token: &str) -> Result<()> {
    let mut data = load(name)?;
    data.insert("longToken".into(), serde_json::json!({ "token": token, "savedAt": now() }));
    write_object(&path(name)?, &data, 0o600)
}

pub fn remove(name: &str) -> Result<()> {
    fs::remove_file(path(name)?)?;
    if current().as_deref() == Some(name) {
        let _ = fs::remove_file(current_path());
    }
    Ok(())
}

pub fn current() -> Option<String> {
    let name = fs::read_to_string(current_path()).ok()?;
    Some(name.trim().to_string()).filter(|n| !n.is_empty())
}

pub fn set_current(name: &str) -> Result<()> {
    fs::write(current_path(), format!("{name}\n"))?;
    Ok(())
}

/// Which saved account these tokens belong to, matched by refresh token.
pub fn owner_of(tokens: &Value) -> Result<Option<String>> {
    let Some(wanted) = refresh_token(tokens) else { return Ok(None) };
    for name in names()? {
        let saved = load(&name)?;
        if saved.get("claudeAiOauth").and_then(refresh_token) == Some(wanted) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Held while switching, so the watcher and a switch by hand can't interleave.
/// Released when the returned file is dropped.
pub fn lock() -> Result<fs::File> {
    use std::os::unix::io::AsRawFd;
    fs::create_dir_all(home())?;
    let file = fs::OpenOptions::new().create(true).truncate(false).write(true).open(home().join(".lock"))?;
    // SAFETY: flock only acts on the descriptor, which `file` keeps open.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        bail!("couldn't lock {}: {}", home().display(), std::io::Error::last_os_error());
    }
    Ok(file)
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
