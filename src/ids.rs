//! Random identifiers: share slugs, API keys and record IDs.

use sha2::{Digest, Sha256};

/// Length of a share slug. 26 base36 chars carry ~134 bits of entropy,
/// so slugs are unguessable and collisions are practically impossible
/// (the UNIQUE index still guarantees it).
pub const SLUG_LEN: usize = 26;

/// Every API key starts with this prefix so leaked keys are easy to grep for.
pub const API_KEY_PREFIX: &str = "rsk_";
const API_KEY_RANDOM_LEN: usize = 40;

/// Number of characters (including `API_KEY_PREFIX`) kept in clear text
/// to let users tell their keys apart.
const API_KEY_DISPLAY_LEN: usize = 12;

const BASE36: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const BASE62: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

fn random_string(alphabet: &[u8], len: usize) -> String {
    (0..len)
        .map(|_| alphabet[rand::random_range(0..alphabet.len())] as char)
        .collect()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn new_slug() -> String {
    random_string(BASE36, SLUG_LEN)
}

/// Cheap syntactic check, used to reject garbage before touching the database.
pub fn is_valid_slug(slug: &str) -> bool {
    slug.len() == SLUG_LEN && slug.bytes().all(|b| BASE36.contains(&b))
}

pub struct NewApiKey {
    /// The full secret, shown to the user exactly once.
    pub token: String,
    /// Non-secret leading part of the token, for display.
    pub prefix: String,
    /// What gets stored in the database.
    pub hash: String,
}

pub fn new_api_key() -> NewApiKey {
    let token = format!("{API_KEY_PREFIX}{}", random_string(BASE62, API_KEY_RANDOM_LEN));
    NewApiKey {
        prefix: token[..API_KEY_DISPLAY_LEN].to_string(),
        hash: hash_api_key(&token),
        token,
    }
}

/// API keys are high-entropy random strings, so a plain SHA-256 is enough
/// (no salt / slow KDF needed) and allows an indexed lookup by hash.
pub fn hash_api_key(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_valid_and_distinct() {
        let a = new_slug();
        let b = new_slug();
        assert!(is_valid_slug(&a));
        assert_ne!(a, b);
        assert!(!is_valid_slug("short"));
        assert!(!is_valid_slug(&"A".repeat(SLUG_LEN)));
    }

    #[test]
    fn api_key_hash_matches() {
        let key = new_api_key();
        assert!(key.token.starts_with(API_KEY_PREFIX));
        assert!(key.token.starts_with(&key.prefix));
        assert_eq!(hash_api_key(&key.token), key.hash);
    }
}
