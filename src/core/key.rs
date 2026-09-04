//! Auth key for wire calls.
//!
//! Every wire request (except the health probe) carries a key. It is parsed
//! from a single scalar field — either an integer or a string:
//!
//! - integer / numeric string  → [`Key::Numeric`], a simple numeric password.
//! - `pass:<secret>`            → [`Key::Pass`], a password.
//! - `did:key=<...>`            → reserved; rejected for now (crypto path).
//! - any other `did:<method>=`  → unsupported.
//!
//! A [`Key`] is what a robot *presents*. What the coordinator *stores* is a
//! [`KeyVerifier`] — `keylock`'s password hash, never the secret — so a leaked
//! state file does not hand over the fleet.
//!
//! # Forms
//!
//! | Presented | Meaning |
//! |---|---|
//! | `1234` | a numeric password |
//! | `pass:<secret>` | a password |
//! | `did:key:<multibase>` | an Ed25519 identity, parsed by `authbox` |
//!
//! Real DIDs go through `authbox`, which is what knows DID syntax. Passwords
//! deliberately do *not*: an earlier form spelled them `did:pass=<secret>`,
//! which is not a DID at all — DID syntax separates on colons, and a password
//! is not a decentralized identifier. Dressing one as a DID also restricted it
//! to the DID method-id charset (no `!`, no spaces) for no benefit, so it now
//! has a plain `pass:` prefix and may contain anything.

use serde::{Deserialize, Serialize};

/// Deliberately cheap hashing cost for tests and benchmarks.
///
/// A suite that registers thousands of robots would otherwise spend minutes
/// hashing. Never serve with this: it is microseconds, which is exactly what
/// an offline attacker wants.
pub fn insecure_test_cost() -> keylock::kdf::pwhash::Config {
    keylock::kdf::pwhash::Config {
        algorithm: keylock::kdf::pwhash::Algorithm::Argon2id,
        nb_blocks: 8,
        nb_passes: 1,
        nb_lanes: 1,
    }
}

/// What the coordinator stores for a robot: `keylock`'s password hash of its
/// key, never the key.
///
/// This is a thin newtype over a PHC string
/// (`$argon2id$v=19$m=…,t=…,p=…$salt$digest`). Salting, cost recording and
/// constant-time verification all live in `keylock::kdf::pwhash` — assembling
/// them per project is how password storage goes wrong.
///
/// Verifying costs a full derivation — milliseconds, deliberately — so it must
/// not sit on the hot path of every heartbeat and claim. Callers verify once
/// and remember the answer; see `Coordinator::validate_key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyVerifier {
    /// A shared secret, stored as `keylock`'s PHC string.
    Secret(String),
    /// An Ed25519 public key from a `did:key`. Public by nature, so it is
    /// stored as-is — there is nothing here worth hashing.
    DidKey(Vec<u8>),
}

impl KeyVerifier {
    /// Hash `key` for storage at the default cost.
    pub fn derive(key: &Key) -> Result<Self, KeyError> {
        Self::derive_with_cost(key, keylock::kdf::pwhash::Config::default())
    }

    /// As [`Self::derive`], at an explicit cost.
    pub fn derive_with_cost(
        key: &Key,
        cost: keylock::kdf::pwhash::Config,
    ) -> Result<Self, KeyError> {
        match key {
            Key::DidKey(public_key) => Ok(Self::DidKey(public_key.clone())),
            // A token is proof that a key was presented, not a key. Binding one
            // as an identity would make the identity expire with the token.
            Key::Token(_) => Err(KeyError::Malformed),
            secret => keylock::kdf::pwhash::hash_with(&secret.material(), cost)
                .map(Self::Secret)
                .map_err(|_| KeyError::Malformed),
        }
    }

    /// The Ed25519 public key this verifier accepts signatures from, if it is
    /// a `did:key` identity.
    pub fn public_key(&self) -> Option<&[u8]> {
        match self {
            Self::DidKey(public_key) => Some(public_key),
            Self::Secret(_) => None,
        }
    }

    /// Whether `presented` matches.
    ///
    /// A `did:key` identity never matches here, whatever is presented: a
    /// public key proves nothing, and the bearer token that *does* prove it is
    /// checked against the coordinator's token store, not against this.
    pub fn verify(&self, presented: &Key) -> bool {
        match self {
            Self::Secret(stored) => {
                matches!(presented, Key::Numeric(_) | Key::Pass(_))
                    && keylock::kdf::pwhash::verify(stored, &presented.material())
            }
            Self::DidKey(_) => false,
        }
    }
}

/// A robot's auth key, parsed from the wire `key` scalar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Key {
    /// Numeric password.
    Numeric(u64),
    /// `pass:<secret>` — a password. Anything after the prefix, verbatim.
    Pass(String),
    /// `did:key:<multibase>` — the Ed25519 public key it encodes.
    ///
    /// Presenting this proves nothing on its own: a public key is public. It
    /// names an identity, which is then proven by signing a challenge — see
    /// `Coordinator::issue_challenge`.
    DidKey(Vec<u8>),
    /// `tok:<token>` — a bearer token issued after a successful proof. This is
    /// what a `did:key` robot actually sends on each later call.
    Token(String),
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
    ///
    /// DID forms are handed to `authbox`, the crate that owns DID syntax, so
    /// syncbot never second-guesses what a valid DID looks like.
    pub fn parse(raw: &str) -> Result<Self, KeyError> {
        let raw = raw.trim();
        if let Ok(n) = raw.parse::<u64>() {
            return Ok(Key::Numeric(n));
        }
        if let Some(secret) = raw.strip_prefix("pass:") {
            if secret.is_empty() {
                return Err(KeyError::Malformed);
            }
            return Ok(Key::Pass(secret.to_string()));
        }
        if let Some(token) = raw.strip_prefix("tok:") {
            if token.is_empty() {
                return Err(KeyError::Malformed);
            }
            return Ok(Key::Token(token.to_string()));
        }
        if raw.starts_with("did:") {
            let did = authbox::did::did::parse(raw).map_err(|_| KeyError::Malformed)?;
            return match did.method.as_str() {
                "key" => {
                    let info =
                        authbox::did::key::parse_did_key(raw).map_err(|_| KeyError::Malformed)?;
                    match info.type_ {
                        authbox::did::key::DidKeyType::Ed25519 => Ok(Key::DidKey(info.public_key)),
                        // X25519 is a key-agreement key; it cannot sign, so it
                        // cannot answer a challenge.
                        _ => Err(KeyError::Unsupported("did:key (not Ed25519)".to_string())),
                    }
                }
                other => Err(KeyError::Unsupported(format!("did:{other}"))),
            };
        }
        Err(KeyError::Malformed)
    }

    /// The bytes this key contributes to a derivation.
    ///
    /// Tagged by variant so a numeric key and a password that happen to look
    /// alike (`1234` and `pass:1234`) never derive to the same digest —
    /// they are different keys and `matches` already treats them so.
    pub(crate) fn material(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Key::Numeric(n) => {
                out.push(b'n');
                out.extend_from_slice(&n.to_le_bytes());
            }
            Key::Pass(p) => {
                out.push(b'p');
                out.extend_from_slice(p.as_bytes());
            }
            Key::DidKey(pk) => {
                out.push(b'k');
                out.extend_from_slice(pk);
            }
            Key::Token(t) => {
                out.push(b't');
                out.extend_from_slice(t.as_bytes());
            }
        }
        out
    }

    /// Whether two keys authenticate as the same. Plaintext compare for now,
    /// but done in constant time to avoid a timing side channel: the secret
    /// bytes are XOR-accumulated so the comparison does not short-circuit on the
    /// first differing byte. Values of different variants or different lengths
    /// never match. Storage hashing lives in [`KeyVerifier`].
    pub fn matches(&self, other: &Key) -> bool {
        match (self, other) {
            (Key::Numeric(a), Key::Numeric(b)) => ct_eq_bytes(&a.to_le_bytes(), &b.to_le_bytes()),
            (Key::Pass(a), Key::Pass(b)) => ct_eq_bytes(a.as_bytes(), b.as_bytes()),
            (Key::DidKey(a), Key::DidKey(b)) => ct_eq_bytes(a, b),
            (Key::Token(a), Key::Token(b)) => ct_eq_bytes(a.as_bytes(), b.as_bytes()),
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
            Key::Pass(p) => write!(f, "pass:{p}"),
            Key::DidKey(pk) => write!(f, "did:key:<{} bytes>", pk.len()),
            Key::Token(_) => write!(f, "tok:<redacted>"),
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
    fn parses_a_password() {
        assert_eq!(Key::parse("pass:secret"), Ok(Key::Pass("secret".into())));
        // A password is not a DID, so nothing restricts what it may contain.
        for secret in ["a=b", "with space", "sym!bol", "colon:s", "did:pass=legacy"] {
            assert_eq!(
                Key::parse(&format!("pass:{secret}")),
                Ok(Key::Pass(secret.into())),
                "pass: must carry {secret:?} verbatim"
            );
        }
        assert_eq!(Key::parse("pass:"), Err(KeyError::Malformed));
    }

    /// A password used to be spelled `did:pass=<secret>`, which is not a DID:
    /// DID syntax separates on colons, and authbox rejects it outright.
    #[test]
    fn the_old_did_pass_form_is_gone() {
        assert_eq!(Key::parse("did:pass=secret"), Err(KeyError::Malformed));
    }

    #[test]
    fn parses_did_key_and_rejects_unknown_methods() {
        // Real DID syntax, parsed by authbox down to the Ed25519 public key.
        let parsed = Key::parse("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK");
        match parsed {
            Ok(Key::DidKey(public_key)) => assert_eq!(public_key.len(), 32),
            other => panic!("expected an Ed25519 identity, got {other:?}"),
        }
        assert_eq!(
            Key::parse("did:whatever:x"),
            Err(KeyError::Unsupported("did:whatever".into()))
        );
        // Malformed DIDs are malformed, not "unsupported method".
        assert_eq!(Key::parse("did:key=z6Mk"), Err(KeyError::Malformed));
        assert_eq!(Key::parse("did:"), Err(KeyError::Malformed));
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(Key::parse("hunter2"), Err(KeyError::Malformed));
        assert_eq!(Key::parse("did:pass"), Err(KeyError::Malformed));
        assert_eq!(Key::parse("pass"), Err(KeyError::Malformed));
        assert_eq!(Key::parse(""), Err(KeyError::Malformed));
    }

    #[test]
    fn matches_is_exact() {
        assert!(Key::Numeric(1).matches(&Key::Numeric(1)));
        assert!(!Key::Numeric(1).matches(&Key::Numeric(2)));
        assert!(Key::Pass("x".into()).matches(&Key::Pass("x".into())));
        assert_eq!(Key::Pass("x".into()).to_string(), "pass:x");
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
