//! Claude Code's login on disk, which every session reads:
//!
//!     ~/.claude/.credentials.json   "claudeAiOauth": the tokens
//!     ~/.claude.json                "oauthAccount": who you are
//!
//! Only those two keys are ever changed. MCP logins ("mcpOAuth") and every
//! other setting stay as they are, shared by all accounts.

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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
    match serde_json::from_str(&text).with_context(|| format!("{} isn't valid JSON", path.display()))? {
        Value::Object(m) => Ok(m),
        _ => anyhow::bail!("{} isn't a JSON object", path.display()),
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

/// The login on disk right now.
pub struct Live {
    pub tokens: Option<Value>,
    pub account: Option<Value>,
}

pub fn live() -> Result<Live> {
    Ok(Live {
        tokens: read_object(&credentials_path())?.remove("claudeAiOauth"),
        account: read_object(&config_path())?.remove("oauthAccount"),
    })
}

/// Put another account's login on disk.
pub fn put(tokens: &Value, account: Option<&Value>) -> Result<()> {
    let path = credentials_path();
    let mut creds = read_object(&path)?;
    creds.insert("claudeAiOauth".into(), tokens.clone());
    write_object(&path, &creds, 0o600)?;

    if let Some(account) = account {
        let path = config_path();
        let mut config = read_object(&path)?;
        config.insert("oauthAccount".into(), account.clone());
        write_object(&path, &config, mode_of(&path, 0o644))?;
    }
    Ok(())
}

pub fn refresh_token(tokens: &Value) -> Option<&str> {
    tokens.get("refreshToken").and_then(Value::as_str)
}

pub fn email(account: Option<&Value>) -> &str {
    account.and_then(|a| a.get("emailAddress")).and_then(Value::as_str).unwrap_or("?")
}
