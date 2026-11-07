//! Secrets live in the OS keychain (Windows Credential Manager, macOS Keychain, or the Linux
//! Secret Service). Configuration refers to them by name only.

use std::fmt;
use std::str::FromStr;

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

/// Removes a secret. Returns whether one existed.
pub fn delete_secret(name: &SecretName) -> Result<bool, SecretError> {
    match keyring::Entry::new(KEYCHAIN_SERVICE, name.as_str())?.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(error) => Err(error.into()),
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
    }
}
