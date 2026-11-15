//! The fallback secret store for machines with no OS keychain, such as headless Linux without a
//! Secret Service. Every secret lives in one file, encrypted at rest with XChaCha20-Poly1305;
//! nothing is ever written in plaintext.
//!
//! The key comes from one of two places:
//!
//! - **A key file** (the default): 32 random bytes in `secrets.key`, created on first use with
//!   mode `0600` on Unix. It needs no prompt, so an unattended daemon can start. It protects the
//!   secrets file when only that file leaks (a backup, a synced folder, a pasted listing), but
//!   anyone who can read both files as the same user can decrypt everything.
//! - **A passphrase**, from `IMPERIUM_SECRETS_PASSPHRASE`, stretched with Argon2id. Without the
//!   passphrase nothing can be decrypted, so it is the stronger choice; it needs the variable in
//!   the daemon's and the CLI's environment.
//!
//! Either way this is weaker than an OS keychain, which also guards against other processes of
//! the same user; [`AutoSecrets`] says so when it falls back.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, OsRng, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

use crate::paths::Paths;
use crate::secrets::{KeychainSecrets, SecretError, SecretName, SecretStore};

/// The environment variable that selects passphrase mode.
pub const PASSPHRASE_ENV: &str = "IMPERIUM_SECRETS_PASSPHRASE";

const FORMAT_VERSION: u32 = 1;
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const SALT_LEN: usize = 16;
/// Argon2id cost for new passphrase files (the cost of an existing file is read from it).
const DEFAULT_KDF: KdfCost = KdfCost {
    memory_kib: 64 * 1024,
    iterations: 3,
    lanes: 1,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct KdfCost {
    memory_kib: u32,
    iterations: u32,
    lanes: u32,
}

/// How the file's key is derived; stored in the file and authenticated with the contents.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum KdfHeader {
    Keyfile,
    Argon2id {
        salt: String,
        #[serde(flatten)]
        cost: KdfCost,
    },
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Envelope {
    version: u32,
    kdf: KdfHeader,
    nonce: String,
    ciphertext: String,
}

impl Envelope {
    /// The bytes bound to the ciphertext, so a tampered version or KDF header fails to decrypt.
    fn associated_data(version: u32, kdf: &KdfHeader) -> Vec<u8> {
        let kdf = serde_json::to_string(kdf).unwrap_or_default();
        format!("imperium-secrets|{version}|{kdf}").into_bytes()
    }
}

/// Where the file store's key comes from.
#[derive(Clone)]
pub enum KeySource {
    /// A 32-byte key file, created next to the secrets on first use.
    KeyFile(PathBuf),
    /// A passphrase, stretched with Argon2id.
    Passphrase(Arc<Zeroizing<String>>),
}

impl fmt::Debug for KeySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeySource::KeyFile(path) => f.debug_tuple("KeyFile").field(path).finish(),
            KeySource::Passphrase(_) => f.write_str("Passphrase(..)"),
        }
    }
}

/// A derived key and the serialized header it was derived for.
type DerivedKey = (Vec<u8>, Zeroizing<[u8; KEY_LEN]>);

/// Secrets in one encrypted file. Safe to use from several processes: each change takes an
/// exclusive lock, rereads the file, and replaces it atomically.
pub struct FileSecrets {
    path: PathBuf,
    source: KeySource,
    cost: KdfCost,
    /// The last derived key and the header it was derived for, so a passphrase is stretched
    /// once per file rather than once per call.
    cache: Mutex<Option<DerivedKey>>,
}

impl fmt::Debug for FileSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSecrets")
            .field("path", &self.path)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl FileSecrets {
    /// The store for these paths: `secrets.enc` and `secrets.key` in the data directory, in
    /// passphrase mode when [`PASSPHRASE_ENV`] is set and not empty.
    pub fn for_paths(paths: &Paths) -> Self {
        let source = match std::env::var(PASSPHRASE_ENV) {
            Ok(passphrase) if !passphrase.is_empty() => {
                KeySource::Passphrase(Arc::new(Zeroizing::new(passphrase)))
            }
            _ => KeySource::KeyFile(paths.data_dir().join("secrets.key")),
        };
        FileSecrets::new(paths.data_dir().join("secrets.enc"), source)
    }

    pub fn new(path: PathBuf, source: KeySource) -> Self {
        FileSecrets {
            path,
            source,
            cost: DEFAULT_KDF,
            cache: Mutex::new(None),
        }
    }

    /// Cheap Argon2id settings so tests do not spend 64 MiB per call.
    #[cfg(test)]
    fn with_test_cost(mut self) -> Self {
        self.cost = KdfCost {
            memory_kib: 8,
            iterations: 1,
            lanes: 1,
        };
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the encrypted file exists (it may still be empty or unreadable).
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// The names of the stored secrets.
    pub fn names(&self) -> Result<Vec<SecretName>, SecretError> {
        Ok(self
            .read()?
            .into_keys()
            .filter_map(|name| name.parse().ok())
            .collect())
    }

    /// Moves every secret into `target` and removes them from this file; stops at the first
    /// failure, leaving the unmoved ones here. Returns how many moved.
    pub fn drain_into(&self, target: &dyn SecretStore) -> Result<usize, SecretError> {
        let _guard = self.lock()?;
        let mut map = self.read()?;
        let mut moved = 0;
        let mut failure = None;
        for (name, value) in map.clone() {
            let Ok(secret_name) = name.parse::<SecretName>() else {
                continue;
            };
            match target.store(&secret_name, &value) {
                Ok(()) => {
                    map.remove(&name);
                    moved += 1;
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if moved > 0 {
            self.commit(&map)?;
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(moved),
        }
    }

    fn lock(&self) -> Result<fs::File, SecretError> {
        if let Some(parent) = self.path.parent() {
            create_private_dir(parent)?;
        }
        let lock_path = self.path.with_extension("lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|error| io_error("lock", &lock_path, &error))?;
        file.lock()
            .map_err(|error| io_error("lock", &lock_path, &error))?;
        Ok(file)
    }

    fn read(&self) -> Result<BTreeMap<String, String>, SecretError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => return Err(io_error("read", &self.path, &error)),
        };
        let envelope: Envelope = serde_json::from_str(&text).map_err(|error| {
            file_error(format!(
                "{} is not a valid secret file: {error}",
                self.path.display()
            ))
        })?;
        if envelope.version != FORMAT_VERSION {
            return Err(file_error(format!(
                "{} has format version {}, which this build cannot read",
                self.path.display(),
                envelope.version
            )));
        }
        let nonce = decode(&envelope.nonce, "nonce")?;
        let ciphertext = decode(&envelope.ciphertext, "ciphertext")?;
        if nonce.len() != NONCE_LEN {
            return Err(file_error("the secret file's nonce is the wrong length"));
        }
        let key = self.key_for(&envelope.kdf, false)?;
        let aad = Envelope::associated_data(envelope.version, &envelope.kdf);
        let plain = XChaCha20Poly1305::new(Key::from_slice(&key[..]))
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| {
                file_error(match self.source {
                    KeySource::KeyFile(_) => {
                        "could not decrypt the secret file: the key file does not match it, or the file was modified"
                    }
                    KeySource::Passphrase(_) => {
                        "could not decrypt the secret file: wrong passphrase, or the file was modified"
                    }
                })
            })?;
        serde_json::from_slice(&plain)
            .map_err(|error| file_error(format!("the decrypted secrets are malformed: {error}")))
    }

    fn write(&self, map: &BTreeMap<String, String>) -> Result<(), SecretError> {
        // Keep an existing file's KDF header (and so its salt); make a fresh one otherwise.
        let existing = self.existing_kdf()?;
        let may_create = existing.is_none();
        let kdf = match existing {
            Some(kdf) => kdf,
            None => self.new_kdf()?,
        };
        let key = self.key_for(&kdf, may_create)?;
        let plain = Zeroizing::new(
            serde_json::to_vec(map)
                .map_err(|error| file_error(format!("could not encode the secrets: {error}")))?,
        );
        // A fresh random nonce per write; 192 bits make a repeat negligible.
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let aad = Envelope::associated_data(FORMAT_VERSION, &kdf);
        let ciphertext = XChaCha20Poly1305::new(Key::from_slice(&key[..]))
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: &aad,
                },
            )
            .map_err(|_| file_error("could not encrypt the secrets"))?;
        let envelope = Envelope {
            version: FORMAT_VERSION,
            kdf,
            nonce: BASE64.encode(nonce),
            ciphertext: BASE64.encode(ciphertext),
        };
        let text = serde_json::to_vec_pretty(&envelope)
            .map_err(|error| file_error(format!("could not encode the secret file: {error}")))?;
        write_private_atomically(&self.path, &text)
    }

    fn existing_kdf(&self) -> Result<Option<KdfHeader>, SecretError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error("read", &self.path, &error)),
        };
        let envelope: Envelope = serde_json::from_str(&text)
            .map_err(|error| file_error(format!("the secret file is not valid: {error}")))?;
        Ok(Some(envelope.kdf))
    }

    fn new_kdf(&self) -> Result<KdfHeader, SecretError> {
        Ok(match self.source {
            KeySource::KeyFile(_) => KdfHeader::Keyfile,
            KeySource::Passphrase(_) => {
                let mut salt = [0u8; SALT_LEN];
                OsRng.fill_bytes(&mut salt);
                KdfHeader::Argon2id {
                    salt: BASE64.encode(salt),
                    cost: self.cost,
                }
            }
        })
    }

    /// The 32-byte key for a file with this header. `may_create` lets a key file be made, which
    /// is only safe while no secrets file exists: a fresh key could never open an old file, so
    /// a missing key file next to existing secrets is an error, not something to quietly replace.
    fn key_for(
        &self,
        kdf: &KdfHeader,
        may_create: bool,
    ) -> Result<Zeroizing<[u8; KEY_LEN]>, SecretError> {
        match (&self.source, kdf) {
            (KeySource::KeyFile(path), KdfHeader::Keyfile) => read_key_file(path, may_create),
            (KeySource::Passphrase(passphrase), KdfHeader::Argon2id { salt, cost }) => {
                let id = serde_json::to_vec(kdf).unwrap_or_default();
                let mut cache = self
                    .cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some((cached_id, key)) = cache.as_ref()
                    && *cached_id == id
                {
                    return Ok(key.clone());
                }
                let salt = decode(salt, "salt")?;
                let key = derive_key(passphrase.as_bytes(), &salt, *cost)?;
                *cache = Some((id, key.clone()));
                Ok(key)
            }
            (KeySource::KeyFile(_), KdfHeader::Argon2id { .. }) => Err(file_error(format!(
                "the secret file is encrypted with a passphrase; set {PASSPHRASE_ENV} to open it"
            ))),
            (KeySource::Passphrase(_), KdfHeader::Keyfile) => Err(file_error(format!(
                "the secret file is encrypted with a key file; unset {PASSPHRASE_ENV} to open it"
            ))),
        }
    }

    /// Writes the map, or removes the file when nothing is left in it.
    fn commit(&self, map: &BTreeMap<String, String>) -> Result<(), SecretError> {
        if map.is_empty() {
            match fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(io_error("remove", &self.path, &error)),
            }
        } else {
            self.write(map)
        }
    }
}

fn derive_key(
    passphrase: &[u8],
    salt: &[u8],
    cost: KdfCost,
) -> Result<Zeroizing<[u8; KEY_LEN]>, SecretError> {
    let params = Params::new(cost.memory_kib, cost.iterations, cost.lanes, Some(KEY_LEN)).map_err(
        |error| {
            file_error(format!(
                "the secret file's key settings are invalid: {error}"
            ))
        },
    )?;
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase, salt, &mut key[..])
        .map_err(|error| file_error(format!("could not derive the key: {error}")))?;
    Ok(key)
}

fn read_key_file(path: &Path, may_create: bool) -> Result<Zeroizing<[u8; KEY_LEN]>, SecretError> {
    match fs::read(path).map(Zeroizing::new) {
        Ok(bytes) => {
            check_private(path)?;
            let key: [u8; KEY_LEN] = bytes[..].try_into().map_err(|_| {
                file_error(format!(
                    "{} is not a valid key file (expected {KEY_LEN} bytes)",
                    path.display()
                ))
            })?;
            Ok(Zeroizing::new(key))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !may_create {
                return Err(file_error(format!(
                    "the key file {} is missing, so the existing secrets cannot be decrypted; restore it from a backup",
                    path.display()
                )));
            }
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            OsRng.fill_bytes(&mut key[..]);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| io_error("create", parent, &e))?;
            }
            match open_private(path, true) {
                Ok(mut file) => {
                    file.write_all(&key[..])
                        .and_then(|()| file.sync_all())
                        .map_err(|e| io_error("write", path, &e))?;
                    Ok(key)
                }
                // Another process made it first; use theirs.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    read_key_file(path, false)
                }
                Err(error) => Err(io_error("create", path, &error)),
            }
        }
        Err(error) => Err(io_error("read", path, &error)),
    }
}

/// Refuses a key file other users can read (like ssh does for private keys). Windows has no
/// mode bits; the key file inherits the per-user profile folder's access list.
#[cfg(unix)]
fn check_private(path: &Path) -> Result<(), SecretError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)
        .map_err(|error| io_error("inspect", path, &error))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(file_error(format!(
            "the key file {} has mode {:o}, so other users could read it; run `chmod 600` on it",
            path.display(),
            mode & 0o777
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private(_path: &Path) -> Result<(), SecretError> {
    Ok(())
}

/// Opens a file only its owner can read (mode `0600` on Unix).
fn open_private(path: &Path, must_be_new: bool) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if must_be_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_private_atomically(path: &Path, bytes: &[u8]) -> Result<(), SecretError> {
    let temp = path.with_extension("tmp");
    let write = || -> std::io::Result<()> {
        let mut file = open_private(&temp, false)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    };
    write().map_err(|error| {
        let _ = fs::remove_file(&temp);
        io_error("write", path, &error)
    })
}

fn create_private_dir(path: &Path) -> Result<(), SecretError> {
    fs::create_dir_all(path).map_err(|error| io_error("create", path, &error))
}

fn decode(text: &str, what: &str) -> Result<Vec<u8>, SecretError> {
    BASE64
        .decode(text)
        .map_err(|_| file_error(format!("the secret file's {what} is not valid base64")))
}

fn file_error(message: impl Into<String>) -> SecretError {
    SecretError::File(message.into())
}

fn io_error(action: &str, path: &Path, error: &std::io::Error) -> SecretError {
    file_error(format!("could not {action} {}: {error}", path.display()))
}

impl SecretStore for FileSecrets {
    fn load(&self, name: &SecretName) -> Result<Option<String>, SecretError> {
        Ok(self.read()?.get(name.as_str()).cloned())
    }

    fn store(&self, name: &SecretName, value: &str) -> Result<(), SecretError> {
        if value.is_empty() {
            return Err(SecretError::EmptyValue);
        }
        let _guard = self.lock()?;
        let mut map = self.read()?;
        map.insert(name.as_str().to_string(), value.to_string());
        self.write(&map)
    }

    fn delete(&self, name: &SecretName) -> Result<bool, SecretError> {
        let _guard = self.lock()?;
        let mut map = self.read()?;
        let existed = map.remove(name.as_str()).is_some();
        if existed {
            self.commit(&map)?;
        }
        Ok(existed)
    }
}

/// Which store is holding secrets right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretBackend {
    /// The OS keychain.
    Keychain,
    /// The encrypted file: no keychain is available. Weaker than a keychain.
    EncryptedFile,
}

/// What [`AutoSecrets::migrate`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MigrationReport {
    /// Secrets moved from the encrypted file into the keychain.
    pub moved: usize,
}

/// The secret store the daemon and the CLI use: the OS keychain when there is one, otherwise the
/// encrypted file. When the keychain appears later, secrets in the file move into it, one at a
/// time as they are read and all at once at [`AutoSecrets::migrate`].
pub struct AutoSecrets {
    keychain: Arc<dyn SecretStore>,
    file: FileSecrets,
}

impl fmt::Debug for AutoSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AutoSecrets")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

impl AutoSecrets {
    /// The OS keychain with the encrypted file for these paths as the fallback.
    pub fn open(paths: &Paths) -> Self {
        AutoSecrets::with_parts(Arc::new(KeychainSecrets), FileSecrets::for_paths(paths))
    }

    pub fn with_parts(keychain: Arc<dyn SecretStore>, file: FileSecrets) -> Self {
        AutoSecrets { keychain, file }
    }

    /// The encrypted file's location.
    pub fn file_path(&self) -> &Path {
        self.file.path()
    }

    /// Whether the keychain works, found by reading a name no one writes.
    pub fn backend(&self) -> SecretBackend {
        match self.keychain.load(&probe_name()) {
            Err(error) if error.is_keychain_unavailable() => SecretBackend::EncryptedFile,
            _ => SecretBackend::Keychain,
        }
    }

    /// Moves every secret in the encrypted file into the keychain, when the keychain is there.
    pub fn migrate(&self) -> Result<MigrationReport, SecretError> {
        if !self.file.exists() || self.backend() == SecretBackend::EncryptedFile {
            return Ok(MigrationReport::default());
        }
        let moved = self.file.drain_into(self.keychain.as_ref())?;
        Ok(MigrationReport { moved })
    }

    /// The warning to show wherever the fallback is in use.
    pub fn fallback_warning(&self) -> String {
        let how = if std::env::var_os(PASSPHRASE_ENV).is_some_and(|value| !value.is_empty()) {
            format!("under a key derived from ${PASSPHRASE_ENV} (Argon2id)")
        } else {
            "under a key file next to it, which anyone who can read your files can use".to_string()
        };
        format!(
            "no OS keychain is available, so secrets are kept in {} encrypted {how}; this is weaker than a keychain. Secrets move into the keychain by themselves once one is available",
            self.file.path().display()
        )
    }
}

fn probe_name() -> SecretName {
    SecretName::from_valid("imperium.keychain-probe")
}

impl SecretStore for AutoSecrets {
    fn load(&self, name: &SecretName) -> Result<Option<String>, SecretError> {
        match self.keychain.load(name) {
            Ok(Some(value)) => Ok(Some(value)),
            Ok(None) if self.file.exists() => {
                // The keychain works but this secret is still in the file: move it across.
                let Some(value) = self.file.load(name)? else {
                    return Ok(None);
                };
                if self.keychain.store(name, &value).is_ok() {
                    let _ = self.file.delete(name);
                }
                Ok(Some(value))
            }
            Ok(None) => Ok(None),
            Err(error) if error.is_keychain_unavailable() => self.file.load(name),
            Err(error) => Err(error),
        }
    }

    fn store(&self, name: &SecretName, value: &str) -> Result<(), SecretError> {
        match self.keychain.store(name, value) {
            Ok(()) => {
                // Drop an older copy in the file so it cannot come back after a delete.
                if self.file.exists() {
                    let _ = self.file.delete(name);
                }
                Ok(())
            }
            Err(error) if error.is_keychain_unavailable() => self.file.store(name, value),
            Err(error) => Err(error),
        }
    }

    fn delete(&self, name: &SecretName) -> Result<bool, SecretError> {
        let in_keychain = match self.keychain.delete(name) {
            Ok(existed) => existed,
            Err(error) if error.is_keychain_unavailable() => false,
            Err(error) => return Err(error),
        };
        let in_file = if self.file.exists() {
            self.file.delete(name)?
        } else {
            false
        };
        Ok(in_keychain || in_file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecrets;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn name(text: &str) -> SecretName {
        text.parse().unwrap()
    }

    fn key_file_store(dir: &Path) -> FileSecrets {
        FileSecrets::new(
            dir.join("secrets.enc"),
            KeySource::KeyFile(dir.join("secrets.key")),
        )
    }

    fn passphrase_store(dir: &Path, passphrase: &str) -> FileSecrets {
        FileSecrets::new(
            dir.join("secrets.enc"),
            KeySource::Passphrase(Arc::new(Zeroizing::new(passphrase.to_string()))),
        )
        .with_test_cost()
    }

    fn unavailable() -> SecretError {
        SecretError::Keychain(keyring::Error::NoStorageAccess(Box::new(
            std::io::Error::other("no Secret Service"),
        )))
    }

    /// A keychain that is absent until switched on, as headless Linux gaining a Secret Service.
    #[derive(Debug, Default)]
    struct FakeKeychain {
        on: AtomicBool,
        inner: MemorySecrets,
    }

    impl FakeKeychain {
        fn check(&self) -> Result<(), SecretError> {
            if self.on.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(unavailable())
            }
        }
    }

    #[derive(Debug, Clone)]
    struct SharedKeychain(Arc<FakeKeychain>);

    impl SecretStore for SharedKeychain {
        fn load(&self, name: &SecretName) -> Result<Option<String>, SecretError> {
            self.0.check()?;
            self.0.inner.load(name)
        }
        fn store(&self, name: &SecretName, value: &str) -> Result<(), SecretError> {
            self.0.check()?;
            self.0.inner.store(name, value)
        }
        fn delete(&self, name: &SecretName) -> Result<bool, SecretError> {
            self.0.check()?;
            self.0.inner.delete(name)
        }
    }

    fn fake(dir: &Path) -> (Arc<FakeKeychain>, AutoSecrets) {
        let keychain = Arc::new(FakeKeychain::default());
        let auto = AutoSecrets::with_parts(
            Arc::new(SharedKeychain(keychain.clone())),
            key_file_store(dir),
        );
        (keychain, auto)
    }

    #[test]
    fn a_key_file_store_round_trips_and_never_holds_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let store = key_file_store(dir.path());
        assert_eq!(store.load(&name("openrouter")).unwrap(), None);
        store.store(&name("openrouter"), "sk-very-secret").unwrap();
        store.store(&name("hf.token"), "hf_other").unwrap();
        assert_eq!(
            store.load(&name("openrouter")).unwrap().as_deref(),
            Some("sk-very-secret")
        );
        let on_disk = fs::read_to_string(dir.path().join("secrets.enc")).unwrap();
        for leaked in ["sk-very-secret", "hf_other", "openrouter", "hf.token"] {
            assert!(
                !on_disk.contains(leaked),
                "{leaked} is on disk in the clear"
            );
        }
        // A new instance (another process) reads the same secrets.
        let again = key_file_store(dir.path());
        assert_eq!(
            again.load(&name("hf.token")).unwrap().as_deref(),
            Some("hf_other")
        );
        assert!(again.delete(&name("hf.token")).unwrap());
        assert!(!again.delete(&name("hf.token")).unwrap());
        assert_eq!(again.names().unwrap(), vec![name("openrouter")]);
        assert!(matches!(
            again.store(&name("x"), ""),
            Err(SecretError::EmptyValue)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn the_key_and_secret_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        key_file_store(dir.path())
            .store(&name("a"), "value")
            .unwrap();
        for file in ["secrets.key", "secrets.enc"] {
            let mode = fs::metadata(dir.path().join(file))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{file}");
        }
        fs::set_permissions(
            dir.path().join("secrets.key"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let error = key_file_store(dir.path())
            .load(&name("a"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("chmod 600"), "{error}");
    }

    #[test]
    fn a_missing_key_file_is_an_error_not_a_fresh_key() {
        let dir = tempfile::tempdir().unwrap();
        key_file_store(dir.path())
            .store(&name("a"), "value")
            .unwrap();
        fs::remove_file(dir.path().join("secrets.key")).unwrap();
        let store = key_file_store(dir.path());
        let error = store.load(&name("a")).unwrap_err().to_string();
        assert!(error.contains("missing"), "{error}");
        // Writing must not replace the key and so orphan the secrets.
        assert!(store.store(&name("b"), "v").is_err());
        assert!(!dir.path().join("secrets.key").exists());
    }

    #[test]
    fn a_passphrase_opens_the_file_and_a_wrong_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        passphrase_store(dir.path(), "correct horse")
            .store(&name("a"), "value")
            .unwrap();
        let on_disk = fs::read_to_string(dir.path().join("secrets.enc")).unwrap();
        assert!(on_disk.contains("argon2id") && !on_disk.contains("value"));
        assert!(!dir.path().join("secrets.key").exists());
        assert_eq!(
            passphrase_store(dir.path(), "correct horse")
                .load(&name("a"))
                .unwrap()
                .as_deref(),
            Some("value")
        );
        let error = passphrase_store(dir.path(), "wrong")
            .load(&name("a"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("wrong passphrase"), "{error}");
        // Several writes in a row reuse the derived key.
        let store = passphrase_store(dir.path(), "correct horse");
        store.store(&name("b"), "two").unwrap();
        store.store(&name("c"), "three").unwrap();
        assert_eq!(store.names().unwrap().len(), 3);
    }

    #[test]
    fn the_wrong_kind_of_key_is_explained() {
        let dir = tempfile::tempdir().unwrap();
        passphrase_store(dir.path(), "pw")
            .store(&name("a"), "v")
            .unwrap();
        let error = key_file_store(dir.path())
            .load(&name("a"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(PASSPHRASE_ENV), "{error}");
        let other = tempfile::tempdir().unwrap();
        key_file_store(other.path()).store(&name("a"), "v").unwrap();
        let error = passphrase_store(other.path(), "pw")
            .load(&name("a"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("key file"), "{error}");
    }

    #[test]
    fn a_tampered_file_fails_to_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        let store = key_file_store(dir.path());
        store.store(&name("a"), "value").unwrap();
        let path = dir.path().join("secrets.enc");
        let original = fs::read_to_string(&path).unwrap();

        let mut envelope: serde_json::Value = serde_json::from_str(&original).unwrap();
        let mut bytes = BASE64
            .decode(envelope["ciphertext"].as_str().unwrap())
            .unwrap();
        bytes[0] ^= 1;
        envelope["ciphertext"] = BASE64.encode(bytes).into();
        fs::write(&path, envelope.to_string()).unwrap();
        let error = store.load(&name("a")).unwrap_err().to_string();
        assert!(error.contains("modified"), "{error}");

        // The header is authenticated too: a different version is refused.
        let mut envelope: serde_json::Value = serde_json::from_str(&original).unwrap();
        envelope["version"] = 2.into();
        fs::write(&path, envelope.to_string()).unwrap();
        assert!(store.load(&name("a")).is_err());
    }

    #[test]
    fn each_write_uses_a_fresh_nonce() {
        let dir = tempfile::tempdir().unwrap();
        let store = key_file_store(dir.path());
        let path = dir.path().join("secrets.enc");
        store.store(&name("a"), "value").unwrap();
        let first = fs::read_to_string(&path).unwrap();
        store.store(&name("a"), "value").unwrap();
        let second = fs::read_to_string(&path).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn auto_secrets_fall_back_to_the_file_without_a_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let (_keychain, auto) = fake(dir.path());
        assert_eq!(auto.backend(), SecretBackend::EncryptedFile);
        auto.store(&name("a"), "value").unwrap();
        assert_eq!(auto.load(&name("a")).unwrap().as_deref(), Some("value"));
        assert!(auto.fallback_warning().contains("weaker than a keychain"));
        assert!(auto.delete(&name("a")).unwrap());
        assert_eq!(auto.load(&name("a")).unwrap(), None);
        assert_eq!(auto.migrate().unwrap(), MigrationReport::default());
    }

    #[test]
    fn secrets_move_into_a_keychain_that_appears_later() {
        let dir = tempfile::tempdir().unwrap();
        let (keychain, auto) = fake(dir.path());
        auto.store(&name("a"), "one").unwrap();
        auto.store(&name("b"), "two").unwrap();
        auto.store(&name("c"), "three").unwrap();
        assert!(auto.file.exists());

        keychain.on.store(true, Ordering::SeqCst);
        assert_eq!(auto.backend(), SecretBackend::Keychain);
        // One secret moves as it is read.
        assert_eq!(auto.load(&name("a")).unwrap().as_deref(), Some("one"));
        assert_eq!(
            keychain.inner.load(&name("a")).unwrap().as_deref(),
            Some("one")
        );
        assert_eq!(auto.file.load(&name("a")).unwrap(), None);
        // The rest move together.
        assert_eq!(auto.migrate().unwrap(), MigrationReport { moved: 2 });
        assert!(!auto.file.exists(), "the emptied file is removed");
        assert_eq!(
            keychain.inner.load(&name("c")).unwrap().as_deref(),
            Some("three")
        );
        assert_eq!(auto.migrate().unwrap().moved, 0);
    }

    #[test]
    fn a_keychain_write_replaces_a_stale_file_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (keychain, auto) = fake(dir.path());
        auto.store(&name("a"), "old").unwrap();
        keychain.on.store(true, Ordering::SeqCst);
        auto.store(&name("a"), "new").unwrap();
        assert_eq!(auto.file.load(&name("a")).unwrap(), None);
        assert_eq!(auto.load(&name("a")).unwrap().as_deref(), Some("new"));
        assert!(auto.delete(&name("a")).unwrap());
        assert_eq!(auto.load(&name("a")).unwrap(), None);
    }
}
