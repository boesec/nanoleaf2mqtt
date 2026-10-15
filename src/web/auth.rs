//! Admin password, sessions, login throttling and the same-origin check.
//!
//! - The password is hashed with argon2id (default parameters) and a random salt.
//! - Sessions live in memory under a random 256-bit id; a restart logs everybody out. A session
//!   ends after [`AuthTiming::idle`] without a request or [`AuthTiming::absolute`] after login.
//! - After a failed login every attempt is refused unchecked for a while, doubling per failure up
//!   to [`AuthTiming::lockout_max`]; the failed answer itself is delayed by
//!   [`AuthTiming::failure_delay`].
//! - State-changing requests are POSTs; one whose `Origin` (or, without it, `Referer`) names
//!   another host than `Host` is refused.

use std::collections::HashMap;
use std::time::Duration;

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::http::HeaderMap;
use axum::http::header::{COOKIE, HOST, ORIGIN, REFERER};
use rand::RngCore;
use tokio::time::Instant;

use crate::secret::Secret;

/// Name of the session cookie.
pub(crate) const COOKIE_NAME: &str = "nanoleaf2mqtt_session";
/// Shortest accepted admin password, in characters.
pub(crate) const MIN_PASSWORD_LEN: usize = 8;

/// Timing of sessions and login throttling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthTiming {
    /// A session without a request for this long ends.
    pub idle: Duration,
    /// A session ends this long after login, used or not.
    pub absolute: Duration,
    /// How long the answer to a failed login is held back.
    pub failure_delay: Duration,
    /// Lockout after the first failed login; doubled per further failure.
    pub lockout_initial: Duration,
    /// Longest lockout.
    pub lockout_max: Duration,
}

impl Default for AuthTiming {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(12 * 60 * 60),
            absolute: Duration::from_secs(7 * 24 * 60 * 60),
            failure_delay: Duration::from_secs(1),
            lockout_initial: Duration::from_secs(1),
            lockout_max: Duration::from_secs(60),
        }
    }
}

/// Error hashing or checking a password. Never contains the password.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HashError {
    /// The hashing library failed.
    #[error("password hashing failed: {0}")]
    Hash(argon2::password_hash::Error),
    /// The blocking task failed.
    #[error("password hashing task failed")]
    Task,
}

/// Hashes `password` (argon2id, default parameters, random salt) into a PHC string. Runs on the
/// blocking pool: hashing takes a noticeable moment on purpose.
pub(crate) async fn hash_password(password: Secret<String>) -> Result<String, HashError> {
    tokio::task::spawn_blocking(move || {
        let mut salt = [0_u8; 16];
        rand::rng().fill_bytes(&mut salt);
        let salt = SaltString::encode_b64(&salt).map_err(HashError::Hash)?;
        Argon2::default()
            .hash_password(password.expose().as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(HashError::Hash)
    })
    .await
    .map_err(|_| HashError::Task)?
}

/// Whether `password` matches the PHC string `hash`. An unreadable hash matches nothing.
pub(crate) async fn verify_password(
    password: Secret<String>,
    hash: String,
) -> Result<bool, HashError> {
    tokio::task::spawn_blocking(move || {
        let Ok(parsed) = PasswordHash::new(&hash) else {
            tracing::error!("the stored admin password hash is unreadable");
            return false;
        };
        Argon2::default()
            .verify_password(password.expose().as_bytes(), &parsed)
            .is_ok()
    })
    .await
    .map_err(|_| HashError::Task)
}

/// A new random session id: 256 bits, hex.
fn new_session_id() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Session {
    created: Instant,
    last_seen: Instant,
}

/// The open sessions.
#[derive(Default)]
pub(crate) struct Sessions {
    open: HashMap<String, Session>,
}

impl Sessions {
    /// Opens a session and returns its id.
    pub(crate) fn create(&mut self, timing: &AuthTiming) -> String {
        let now = Instant::now();
        self.open
            .retain(|_, s| now - s.created < timing.absolute && now - s.last_seen < timing.idle);
        let id = new_session_id();
        self.open.insert(
            id.clone(),
            Session {
                created: now,
                last_seen: now,
            },
        );
        id
    }

    /// Whether `id` is an open session; marks it as used. An expired session is closed.
    pub(crate) fn touch(&mut self, id: &str, timing: &AuthTiming) -> bool {
        let now = Instant::now();
        let Some(session) = self.open.get_mut(id) else {
            return false;
        };
        if now - session.created >= timing.absolute || now - session.last_seen >= timing.idle {
            self.open.remove(id);
            return false;
        }
        session.last_seen = now;
        true
    }

    /// Closes one session.
    pub(crate) fn remove(&mut self, id: &str) {
        self.open.remove(id);
    }

    /// Closes every session (password change).
    pub(crate) fn clear(&mut self) {
        self.open.clear();
    }
}

/// Consecutive failed logins and the lockout they caused.
#[derive(Default)]
pub(crate) struct Throttle {
    failures: u32,
    blocked_until: Option<Instant>,
}

impl Throttle {
    /// `Err(remaining)` while attempts are refused.
    pub(crate) fn check(&self) -> Result<(), Duration> {
        let now = Instant::now();
        match self.blocked_until {
            Some(until) if until > now => Err(until - now),
            _ => Ok(()),
        }
    }

    /// Records a failed attempt and starts the next, longer lockout.
    pub(crate) fn failed(&mut self, timing: &AuthTiming) {
        self.failures = self.failures.saturating_add(1);
        let doublings = self.failures.saturating_sub(1).min(20);
        let lockout = timing
            .lockout_initial
            .saturating_mul(1 << doublings)
            .min(timing.lockout_max);
        self.blocked_until = Some(Instant::now() + lockout);
    }

    /// A successful login resets the count.
    pub(crate) fn succeeded(&mut self) {
        *self = Self::default();
    }
}

/// The session id from the request's cookies.
pub(crate) fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE_NAME)
        .map(|(_, value)| value.to_owned())
}

/// The `Set-Cookie` value for a new session.
pub(crate) fn session_set_cookie(id: &str) -> String {
    format!("{COOKIE_NAME}={id}; Path=/; HttpOnly; Secure; SameSite=Strict")
}

/// The `Set-Cookie` value that removes the session cookie.
pub(crate) fn session_clear_cookie() -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0")
}

/// Whether a state-changing request comes from a page of this service: its `Origin` (or,
/// without one, its `Referer`) must name the host the request was sent to. A request with
/// neither does not come from a browser page and passes; the `SameSite=Strict` cookie keeps
/// other sites' pages from sending a session along anyway.
pub(crate) fn same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(HOST).and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let source = headers
        .get(ORIGIN)
        .or_else(|| headers.get(REFERER))
        .map(|v| v.to_str().ok());
    match source {
        None => true,
        Some(None) => false,
        Some(Some(source)) => authority(source).is_some_and(|a| a.eq_ignore_ascii_case(host)),
    }
}

/// `host[:port]` of an absolute URL; `None` for anything else (`null` included).
fn authority(url: &str) -> Option<&str> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    (!authority.is_empty() && !authority.contains('@')).then_some(authority)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(axum::http::HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(name.clone(), HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn origin_checks() {
        let host = (HOST, "ui.local:8080");
        assert!(same_origin(&headers(std::slice::from_ref(&host))));
        assert!(same_origin(&headers(&[
            host.clone(),
            (ORIGIN, "https://ui.local:8080")
        ])));
        assert!(same_origin(&headers(&[
            host.clone(),
            (REFERER, "http://UI.local:8080/devices?x=1")
        ])));
        for foreign in [
            "https://evil.example",
            "https://ui.local:8081",
            "null",
            "https://ui.local:8080@evil.example",
            "file:///x",
        ] {
            assert!(
                !same_origin(&headers(&[host.clone(), (ORIGIN, foreign)])),
                "{foreign}"
            );
        }
        assert!(!same_origin(&headers(&[(ORIGIN, "https://ui.local:8080")])));
    }

    #[test]
    fn cookie_is_found_among_others() {
        let map = headers(&[(COOKIE, "a=1; nanoleaf2mqtt_session=abc; b=2")]);
        assert_eq!(session_cookie(&map).as_deref(), Some("abc"));
        assert_eq!(session_cookie(&headers(&[(COOKIE, "a=1")])), None);
        let set = session_set_cookie("abc");
        for attribute in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
            assert!(set.contains(attribute), "{set}");
        }
    }

    #[test]
    fn session_ids_are_long_and_random() {
        let a = new_session_id();
        assert_eq!(a.len(), 64);
        assert_ne!(a, new_session_id());
    }

    #[tokio::test(start_paused = true)]
    async fn lockout_doubles_up_to_the_cap() {
        let timing = AuthTiming::default();
        let mut throttle = Throttle::default();
        assert!(throttle.check().is_ok());
        let mut lockouts = Vec::new();
        for _ in 0..9 {
            throttle.failed(&timing);
            lockouts.push(throttle.check().unwrap_err().as_secs());
        }
        assert_eq!(lockouts, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        tokio::time::advance(Duration::from_secs(61)).await;
        assert!(throttle.check().is_ok());
        throttle.succeeded();
        throttle.failed(&timing);
        assert_eq!(throttle.check().unwrap_err().as_secs(), 1);
    }

    #[tokio::test]
    async fn password_hash_round_trip() {
        let hash = hash_password(Secret::new("correct horse".to_owned()))
            .await
            .unwrap();
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(!hash.contains("correct horse"));
        assert!(
            verify_password(Secret::new("correct horse".to_owned()), hash.clone())
                .await
                .unwrap()
        );
        assert!(
            !verify_password(Secret::new("wrong".to_owned()), hash)
                .await
                .unwrap()
        );
        assert!(
            !verify_password(Secret::new("x".to_owned()), "garbage".to_owned())
                .await
                .unwrap()
        );
    }
}
