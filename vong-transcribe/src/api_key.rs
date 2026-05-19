//! API key handling — OS Keychain backed + zeroize on drop.
//!
//! Plan v2 Section 15.3 — API key NEVER logged, never `Debug`-printed,
//! stored in OS Keychain via `keyring` crate, zeroized on drop via `secrecy::SecretString`.

use crate::error::SttError;
use keyring::Entry;
use secrecy::{ExposeSecret, SecretString};

/// Service identifier for keyring entries (Windows Credential Manager /
/// macOS Keychain "service" name). Stable across versions — changing this
/// string orphans existing stored keys.
const KEYRING_SERVICE: &str = "com.vong.ai-recorder";

/// Opaque wrapper around an STT provider API key.
///
/// `Debug` is intentionally NOT derived. Display is NOT implemented.
/// The only way to access the raw key string is `expose()` (intentionally
/// verbose name to discourage casual use).
pub struct ApiKey(SecretString);

impl ApiKey {
    /// Load API key from OS Keychain for the given provider account.
    ///
    /// E.g., `ApiKey::load("soniox")` reads from
    /// `keychain service=com.vong.ai-recorder account=soniox`.
    pub fn load(provider: &str) -> Result<Self, SttError> {
        let entry = Entry::new(KEYRING_SERVICE, provider)
            .map_err(|e| SttError::Credential(format!("entry init: {e}")))?;
        let password = entry
            .get_password()
            .map_err(|e| SttError::Credential(format!("get_password: {e}")))?;
        Ok(Self(SecretString::from(password)))
    }

    /// Store API key in OS Keychain (replaces existing entry).
    pub fn store(provider: &str, key: &str) -> Result<(), SttError> {
        let entry = Entry::new(KEYRING_SERVICE, provider)
            .map_err(|e| SttError::Credential(format!("entry init: {e}")))?;
        entry
            .set_password(key)
            .map_err(|e| SttError::Credential(format!("set_password: {e}")))?;
        Ok(())
    }

    /// Delete API key from OS Keychain.
    pub fn delete(provider: &str) -> Result<(), SttError> {
        let entry = Entry::new(KEYRING_SERVICE, provider)
            .map_err(|e| SttError::Credential(format!("entry init: {e}")))?;
        entry
            .delete_credential()
            .map_err(|e| SttError::Credential(format!("delete_credential: {e}")))?;
        Ok(())
    }

    /// Construct from a raw string (e.g., during onboarding before storing).
    ///
    /// Caller is responsible for calling `store()` afterward if persistence needed.
    pub fn from_raw(key: String) -> Self {
        Self(SecretString::from(key))
    }

    /// Expose the underlying string for sending to provider API.
    ///
    /// ⚠️ Only use at the network boundary (e.g., WebSocket auth header).
    /// NEVER log, format, or pass the result to anything that might persist.
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

// IMPORTANT: NO Debug, NO Display, NO Clone — minimize key leak surface.
// `secrecy::SecretString` already zeroizes on drop.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_roundtrip() {
        let k = ApiKey::from_raw("sk-test-12345".into());
        assert_eq!(k.expose(), "sk-test-12345");
    }

    #[test]
    fn keyring_service_constant() {
        // Sanity — ensures we don't accidentally change service name
        // (would orphan stored keys across versions).
        assert_eq!(KEYRING_SERVICE, "com.vong.ai-recorder");
    }
}
