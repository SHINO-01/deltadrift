//! Secret handling (NFR-3, phase 1).
//!
//! * Secrets come from the process environment, optionally populated from a
//!   git-ignored `.env` file. Real environment variables win over `.env`.
//! * They are held in [`Secret`], which never prints its contents (`Debug`
//!   and `Display` are redacted) and zeroes its memory on drop.
//! * Nothing in the repository contains a secret; `.env.example` lists names only.
//!
//! Phase 2 (Sprint 7) adds an AES-256-GCM encrypted vault behind the same API.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::Path;

use zeroize::Zeroizing;

/// A secret string. Redacted in all formatting; zeroed on drop.
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(s: String) -> Self {
        Self(Zeroizing::new(s))
    }

    /// Borrow the plaintext. Keep the borrow short and never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug)]
pub enum SecretError {
    Missing(String),
    Dotenv(String),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(k) => write!(f, "missing secret `{k}`: set it in the environment or in .env (see .env.example)"),
            Self::Dotenv(e) => write!(f, "failed to read .env: {e}"),
        }
    }
}

impl std::error::Error for SecretError {}

/// Loads `.env` from `path` (or searches upward from the CWD if `None`) into the
/// process environment without overriding variables that are already set.
/// A missing file is not an error. Returns whether a file was loaded.
///
/// Call once at startup, before spawning threads (mutating the environment
/// while other threads read it is unsound on some platforms).
pub fn load_dotenv(path: Option<&Path>) -> Result<bool, SecretError> {
    let r = match path {
        Some(p) => dotenvy::from_path(p),
        None => dotenvy::dotenv().map(|_| ()),
    };
    match r {
        Ok(()) => Ok(true),
        Err(e) if e.not_found() => Ok(false),
        Err(e) => Err(SecretError::Dotenv(e.to_string())),
    }
}

/// Reads a required secret. Empty values count as missing.
pub fn require(name: &str) -> Result<Secret, SecretError> {
    optional(name).ok_or_else(|| SecretError::Missing(name.to_string()))
}

/// Reads an optional secret. Empty values count as absent.
pub fn optional(name: &str) -> Option<Secret> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty()).map(Secret::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_formats_plaintext() {
        let s = Secret::new("hunter2".into());
        assert!(!format!("{s:?} {s} {s:#?}").contains("hunter2"));
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn missing_is_reported_by_name() {
        let e = require("DD_TEST_SURELY_UNSET_VAR").unwrap_err();
        assert!(e.to_string().contains("DD_TEST_SURELY_UNSET_VAR"));
    }
}
