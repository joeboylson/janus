//! How much of each account's 5-hour and weekly limits is used, from the same
//! endpoint Claude Code's /usage reads. Each answer is kept in
//! ~/.janus/.usage.json, so an account whose token has expired
//! still has its last numbers and, more usefully, when its limits reset.

use crate::claude::{self, read_object, write_object};
use crate::store;
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::time::Duration;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

impl Window {
    /// The window as of `now`: once its reset time has passed it's empty again.
    pub fn at(&self, now: DateTime<Utc>) -> Window {
        match self.resets_at {
            Some(t) if t <= now => Window { percent: 0.0, resets_at: None },
            _ => self.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub fetched_at: Option<DateTime<Utc>>,
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
}

fn window(v: &Value) -> Option<Window> {
    Some(Window {
        percent: v.get("utilization")?.as_f64()?,
        resets_at: v.get("resets_at").and_then(Value::as_str).and_then(parse_time),
    })
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc))
}

pub fn parse(v: &Value) -> Usage {
    Usage {
        fetched_at: v.get("fetched_at").and_then(Value::as_str).and_then(parse_time),
        five_hour: v.get("five_hour").and_then(window),
        seven_day: v.get("seven_day").and_then(window),
    }
}

fn cache_path() -> std::path::PathBuf {
    store::home().join(".usage.json")
}

pub fn cached(name: &str) -> Option<Usage> {
    read_object(&cache_path()).ok()?.get(name).map(parse)
}

fn remember(name: &str, answer: &Value) -> Result<()> {
    let keep = |w: &Value| json!({ "utilization": w.get("utilization"), "resets_at": w.get("resets_at") });
    let mut cache = read_object(&cache_path())?;
    cache.insert(
        name.into(),
        json!({
            "fetched_at": Utc::now().to_rfc3339(),
            "five_hour": answer.get("five_hour").map(keep),
            "seven_day": answer.get("seven_day").map(keep),
        }),
    );
    write_object(&cache_path(), &cache, 0o600)
}

fn fetch(access_token: &str) -> Result<Value> {
    let url = std::env::var("JANUS_USAGE_URL").unwrap_or_else(|_| USAGE_URL.into());
    let response = ureq::get(&url)
        .timeout(Duration::from_secs(15))
        .set("Authorization", &format!("Bearer {access_token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .call();
    match response {
        Ok(r) => Ok(serde_json::from_str(&r.into_string()?)?),
        Err(ureq::Error::Status(code, _)) => bail!("usage check answered {code}"),
        Err(e) => bail!("couldn't reach Anthropic: {e}"),
    }
}

/// The account's tokens, if they can be used right now. The active account's
/// are the ones on disk, which open sessions keep renewed; an inactive one's
/// are its saved copy, which stops working a few hours after it was last used.
fn usable_token(name: &str) -> Result<Option<String>> {
    let tokens = if store::current().as_deref() == Some(name) {
        let live = claude::live()?;
        match live.tokens {
            // Tokens matching another saved account aren't this one's.
            Some(t) if store::owner_of(&t)?.is_none_or(|owner| owner == name) => t,
            _ => return Ok(None),
        }
    } else {
        match store::load(name)?.remove("claudeAiOauth") {
            Some(t) => t,
            None => return Ok(None),
        }
    };
    let expires_ms = tokens.get("expiresAt").and_then(Value::as_i64).unwrap_or(0);
    if expires_ms < Utc::now().timestamp_millis() + 60_000 {
        return Ok(None);
    }
    Ok(tokens.get("accessToken").and_then(Value::as_str).map(str::to_string))
}

/// Fresh numbers when the account's token works, otherwise the last ones seen.
/// The error says why fresh numbers weren't possible, for the log.
pub fn check(name: &str) -> (Option<Usage>, Option<String>) {
    let fresh = (|| -> Result<Option<()>> {
        let Some(token) = usable_token(name)? else { return Ok(None) };
        remember(name, &fetch(&token)?)?;
        Ok(Some(()))
    })();
    let note = match fresh {
        Ok(Some(())) => None,
        Ok(None) => Some("token expired; using the last numbers seen".to_string()),
        Err(e) => Some(format!("{e:#}; using the last numbers seen")),
    };
    (cached(name), note)
}

/// "6% (resets in 2h 10m)"
pub fn describe(w: Option<&Window>, now: DateTime<Utc>) -> String {
    let Some(w) = w.map(|w| w.at(now)) else { return "?".into() };
    match w.resets_at {
        Some(t) => format!("{}% (resets in {})", whole(w.percent), until(t, now)),
        None => format!("{}%", whole(w.percent)),
    }
}

/// Rounded down, so 99.5% doesn't show as a full 100%.
pub fn whole(percent: f64) -> f64 {
    percent.floor()
}

pub fn until(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mins = (t - now).num_minutes().max(0);
    match (mins / 1440, mins % 1440 / 60, mins % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}
