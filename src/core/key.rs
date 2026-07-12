//! Auth key for wire calls.
//!
//! Every wire request (except the health probe) carries a key. It is parsed
//! from a single scalar field — either an integer or a string:
//!
//! - integer / numeric string  → [`Key::Numeric`], a simple numeric password.
//! - `did:pass=<pass>`          → [`Key::Pass`], a password in DID clothing.
//! - `did:key=<...>`            → reserved; rejected for now (crypto path).
//! - any other `did:<method>=`  → unsupported.
//!
//! Comparison is a direct (plaintext) match for now. The hardening path is the
//! sibling `keylock` crate: Argon2 for `did:pass`, Ed25519/X25519 for
//! `did:key`. See `PLAN.md`.

use serde::{Deserialize, Serialize};

/// A robot's auth key, parsed from the wire `key` scalar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Key {
    /// Numeric password.
    Numeric(u64),
    /// `did:pass=<pass>` — a password carried in DID form.
    Pass(String),
}

/// Why a raw key string could not become a [`Key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// Not an integer and not a recognised `did:<method>=<value>` form.
    Malformed,
    /// A `did:` scheme we do not implement yet (e.g. `did:key`).
    Unsupported(String),
}

impl Key {
    /// Parse a raw wire scalar into a [`Key`].
    pub fn parse(raw: &str) -> Result<Self, KeyError> {
        let raw = raw.trim();
        if let Ok(n) = raw.parse::<u64>() {
            return Ok(Key::Numeric(n));
        }
        if let Some(rest) = raw.strip_prefix("did:") {
            let (method, value) = rest.split_once('=').ok_or(KeyError::Malformed)?;
            return match method {
                "pass" => Ok(Key::Pass(value.to_string())),
                // did:key is the crypto path — deferred (see PLAN.md / keylock).
                "key" => Err(KeyError::Unsupported("did:key".to_string())),
                other => Err(KeyError::Unsupported(format!("did:{other}"))),
            };
        }
        Err(KeyError::Malformed)
    }

    /// Whether two keys authenticate as the same. Plaintext compare for now,
    /// but done in constant time to avoid a timing side channel: the secret
    /// bytes are XOR-accumulated so the comparison does not short-circuit on the
    /// first differing byte. Values of different variants or different lengths
    /// never match. (Hashing — Argon2 for `did:pass` — is deferred to keylock;
    /// see PLAN.md.)
    pub fn matches(&self, other: &Key) -> bool {
        match (self, other) {
            (Key::Numeric(a), Key::Numeric(b)) => ct_eq_bytes(&a.to_le_bytes(), &b.to_le_bytes()),
            (Key::Pass(a), Key::Pass(b)) => ct_eq_bytes(a.as_bytes(), b.as_bytes()),
            _ => false,
        }
    }
}

/// Constant-time byte-slice equality. Returns `false` immediately for
/// different lengths (length is not itself secret), otherwise XOR-accumulates
/// every byte so timing does not reveal the position of the first mismatch.
fn ct_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Numeric(n) => write!(f, "{n}"),
            Key::Pass(p) => write!(f, "did:pass={p}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numeric() {
        assert_eq!(Key::parse("1234"), Ok(Key::Numeric(1234)));
        assert_eq!(Key::parse("  42 "), Ok(Key::Numeric(42)));
    }

    #[test]
    fn parses_did_pass() {
        assert_eq!(
            Key::parse("did:pass=secret"),
            Ok(Key::Pass("secret".into()))
        );
        // value may contain '=' and other chars
        assert_eq!(Key::parse("did:pass=a=b"), Ok(Key::Pass("a=b".into())));
    }

    #[test]
    fn rejects_did_key_and_unknown_methods() {
        assert_eq!(
            Key::parse("did:key=z6Mk..."),
            Err(KeyError::Unsupported("did:key".into()))
        );
        assert_eq!(
            Key::parse("did:whatever=x"),
            Err(KeyError::Unsupported("did:whatever".into()))
        );
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(Key::parse("hunter2"), Err(KeyError::Malformed));
        assert_eq!(Key::parse("did:pass"), Err(KeyError::Malformed));
        assert_eq!(Key::parse(""), Err(KeyError::Malformed));
    }

    #[test]
    fn matches_is_exact() {
        assert!(Key::Numeric(1).matches(&Key::Numeric(1)));
        assert!(!Key::Numeric(1).matches(&Key::Numeric(2)));
        assert!(Key::Pass("x".into()).matches(&Key::Pass("x".into())));
        assert!(!Key::Numeric(1).matches(&Key::Pass("1".into())));
    }

    #[test]
    fn constant_time_matches_correct_for_all_cases() {
        // Equal passwords match.
        assert!(Key::Pass("s3cret".into()).matches(&Key::Pass("s3cret".into())));
        // Same-length mismatch (differs only in last byte) still rejected.
        assert!(!Key::Pass("s3cret".into()).matches(&Key::Pass("s3creX".into())));
        // Different-length passwords never match.
        assert!(!Key::Pass("short".into()).matches(&Key::Pass("longer-pass".into())));
        assert!(!Key::Pass("".into()).matches(&Key::Pass("x".into())));
        // Numeric equality/inequality via the constant-time path.
        assert!(Key::Numeric(u64::MAX).matches(&Key::Numeric(u64::MAX)));
        assert!(!Key::Numeric(0).matches(&Key::Numeric(u64::MAX)));
    }
}
