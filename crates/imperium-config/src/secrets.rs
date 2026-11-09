//! Secrets live in the OS keychain (Windows Credential Manager, macOS Keychain, or the Linux
//! Secret Service). Configuration refers to them by name only. Code that needs a secret goes
//! through a [`SecretStore`] so the keychain can be replaced, such as by [`MemorySecrets`].

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;

const KEYCHAIN_SERVICE: &str = "imperium";
const MAX_NAME_LEN: usize = 64;

/// A secret's name: 1–64 characters of lowercase ASCII letters, digits, `.`, `_`, or `-`,
/// starting with a letter or digit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretName(String);

impl SecretName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for SecretName {
    type Err = SecretError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let allowed = |c: char| matches!(c, 'a'..='z' | '0'..='9' | '.' | '_' | '-');
        let starts_well = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if starts_well && name.len() <= MAX_NAME_LEN && name.chars().all(allowed) {
            Ok(SecretName(name.to_string()))
        } else {
            Err(SecretError::InvalidName(name.to_string()))
        }
    }
}

impl<'de> serde::Deserialize<'de> for SecretName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl serde::Serialize for SecretName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl fmt::Display for SecretName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error(
        "invalid secret name {0:?}: use 1-64 lowercase letters, digits, '.', '_' or '-', starting with a letter or digit"
    )]
    InvalidName(String),
    #[error("a secret value cannot be empty")]
    EmptyValue,
    #[error("keychain error: {0}")]
    Keychain(#[from] keyring::Error),
}

/// Stores or replaces a secret in the OS keychain.
pub fn store_secret(name: &SecretName, value: &str) -> Result<(), SecretError> {
    if value.is_empty() {
        return Err(SecretError::EmptyValue);
    }
    keyring::Entry::new(KEYCHAIN_SERVICE, name.as_str())?.set_password(value)?;
    Ok(())
}

/// Reads a secret. Returns `None` when no secret has that name.
pub fn load_secret(name: &SecretName) -> Result<Option<String>, SecretError> {
    match keyring::Entry::new(KEYCHAIN_SERVICE, name.as_str())?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Removes a secret. Returns whether one existed.
pub fn delete_secret(name: &SecretName) -> Result<bool, SecretError> {
    match keyring::Entry::new(KEYCHAIN_SERVICE, name.as_str())?.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Where secrets are kept. The daemon holds one `Arc<dyn SecretStore>` and hands it to every
/// part that needs a key, so the backing store can be swapped (the OS keychain normally, memory
/// in tests or headless runs).
pub trait SecretStore: Send + Sync + fmt::Debug {
    /// Reads a secret. Returns `None` when no secret has that name.
    fn load(&self, name: &SecretName) -> Result<Option<String>, SecretError>;
    /// Stores or replaces a secret; an empty value is refused.
    fn store(&self, name: &SecretName, value: &str) -> Result<(), SecretError>;
    /// Removes a secret. Returns whether one existed.
    fn delete(&self, name: &SecretName) -> Result<bool, SecretError>;
}

/// Secrets in the OS keychain: [`load_secret`], [`store_secret`], and [`delete_secret`].
#[derive(Debug, Clone, Copy, Default)]
pub struct KeychainSecrets;

impl SecretStore for KeychainSecrets {
    fn load(&self, name: &SecretName) -> Result<Option<String>, SecretError> {
        load_secret(name)
    }

    fn store(&self, name: &SecretName, value: &str) -> Result<(), SecretError> {
        store_secret(name, value)
    }

    fn delete(&self, name: &SecretName) -> Result<bool, SecretError> {
        delete_secret(name)
    }
}

/// Secrets held in memory for the life of the process, for tests and headless runs.
#[derive(Default)]
pub struct MemorySecrets {
    values: Mutex<HashMap<SecretName, String>>,
}

impl MemorySecrets {
    pub fn new() -> Self {
        MemorySecrets::default()
    }

    fn values(&self) -> std::sync::MutexGuard<'_, HashMap<SecretName, String>> {
        // The map is only read or replaced whole-entry, so a poisoned lock holds valid data.
        self.values
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl fmt::Debug for MemorySecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<String> = self.values().keys().map(ToString::to_string).collect();
        names.sort();
        f.debug_struct("MemorySecrets")
            .field("names", &names)
            .finish_non_exhaustive()
    }
}

impl SecretStore for MemorySecrets {
    fn load(&self, name: &SecretName) -> Result<Option<String>, SecretError> {
        Ok(self.values().get(name).cloned())
    }

    fn store(&self, name: &SecretName, value: &str) -> Result<(), SecretError> {
        if value.is_empty() {
            return Err(SecretError::EmptyValue);
        }
        self.values().insert(name.clone(), value.to_string());
        Ok(())
    }

    fn delete(&self, name: &SecretName) -> Result<bool, SecretError> {
        Ok(self.values().remove(name).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names_parse() {
        for name in ["openrouter", "hf.token", "my_key-2", "0day"] {
            assert_eq!(name.parse::<SecretName>().unwrap().as_str(), name);
        }
    }

    #[test]
    fn invalid_names_are_rejected() {
        let too_long = "a".repeat(MAX_NAME_LEN + 1);
        for name in [
            "",
            "OpenRouter",
            "-lead",
            "has space",
            "slash/name",
            &too_long,
        ] {
            assert!(
                matches!(name.parse::<SecretName>(), Err(SecretError::InvalidName(_))),
                "{name:?} should be invalid"
            );
        }
    }

    #[test]
    fn empty_values_are_rejected_before_touching_the_keychain() {
        let name: SecretName = "unused".parse().unwrap();
        assert!(matches!(
            store_secret(&name, ""),
            Err(SecretError::EmptyValue)
        ));
        assert!(matches!(
            KeychainSecrets.store(&name, ""),
            Err(SecretError::EmptyValue)
        ));
    }

    #[test]
    fn memory_secrets_store_load_and_delete() {
        let store = MemorySecrets::new();
        let name: SecretName = "openrouter".parse().unwrap();
        assert_eq!(store.load(&name).unwrap(), None);
        assert!(matches!(
            store.store(&name, ""),
            Err(SecretError::EmptyValue)
        ));
        store.store(&name, "sk-1").unwrap();
        store.store(&name, "sk-2").unwrap();
        assert_eq!(store.load(&name).unwrap().as_deref(), Some("sk-2"));
        let debug = format!("{store:?}");
        assert!(
            debug.contains("openrouter") && !debug.contains("sk-2"),
            "{debug}"
        );
        assert!(store.delete(&name).unwrap());
        assert!(!store.delete(&name).unwrap());
        assert_eq!(store.load(&name).unwrap(), None);
    }
}
