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
//! - **A passphrase**, read from the file named by `IMPERIUM_SECRETS_PASSPHRASE_FILE` and
//!   stretched with Argon2id. Without the passphrase nothing can be decrypted, so it is the
//!   stronger choice. The environment holds only the path: a passphrase in the environment would
//!   be inherited by every program the daemon starts for an agent.
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

/// The environment variable naming a file that holds the passphrase, which selects passphrase
/// mode. The variable holds a path, never the passphrase itself: a passphrase in the
/// environment would be inherited by every program the daemon starts for an agent.
pub const PASSPHRASE_FILE_ENV: &str = "IMPERIUM_SECRETS_PASSPHRASE_FILE";

const FORMAT_VERSION: u32 = 1;
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const SALT_LEN: usize = 16;
const MAX_PRIVATE_FILE_LEN: usize = 4096;
/// What a file's recorded Argon2id cost may be, checked before deriving anything: a hostile or
/// corrupt file must not be able to ask for 4 TiB of memory.
const MIN_MEMORY_KIB: u32 = 8 * 1024;
const MAX_MEMORY_KIB: u32 = 1024 * 1024;
const MAX_ITERATIONS: u32 = 10;
const MAX_LANES: u32 = 8;
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
#[serde(deny_unknown_fields)]
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
    /// passphrase mode when [`PASSPHRASE_FILE_ENV`] names a file holding the passphrase.
    pub fn for_paths(paths: &Paths) -> Result<Self, SecretError> {
        let passphrase_file = std::env::var_os(PASSPHRASE_FILE_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        // Off Linux the OS keychain is the only store and this file is never used.
        let passphrase_file = if cfg!(target_os = "linux") {
            passphrase_file
        } else {
            if passphrase_file.is_some() {
                tracing::warn!(
                    "{PASSPHRASE_FILE_ENV} is ignored: this platform always uses its OS keychain"
                );
            }
            None
        };
        FileSecrets::for_paths_with(paths, passphrase_file.as_deref())
    }

    /// Like [`FileSecrets::for_paths`] with the passphrase file given rather than read from the
    /// environment.
    pub fn for_paths_with(
        paths: &Paths,
        passphrase_file: Option<&Path>,
    ) -> Result<Self, SecretError> {
        let source = match passphrase_file {
            Some(file) => {
                let bytes = read_private_file(file)?.ok_or_else(|| {
                    file_error(format!(
                        "the passphrase file {} does not exist",
                        file.display()
                    ))
                })?;
                let text = std::str::from_utf8(&bytes).map_err(|_| {
                    file_error(format!(
                        "the passphrase file {} is not UTF-8",
                        file.display()
                    ))
                })?;
                let passphrase = text.trim_end_matches(['\r', '\n']);
                if passphrase.is_empty() {
                    return Err(file_error(format!(
                        "the passphrase file {} is empty",
                        file.display()
                    )));
                }
                KeySource::Passphrase(Arc::new(Zeroizing::new(passphrase.to_string())))
            }
            None => KeySource::KeyFile(paths.data_dir().join("secrets.key")),
        };
        Ok(FileSecrets::new(
            paths.data_dir().join("secrets.enc"),
            source,
        ))
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
            memory_kib: MIN_MEMORY_KIB,
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
            // The file's copy is the newer one, so it replaces whatever the target holds.
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
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
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
        let envelope = parse_envelope(&text, &self.path)?;
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
        let envelope = parse_envelope(&text, &self.path)?;
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
                "the secret file is encrypted with a passphrase; set {PASSPHRASE_FILE_ENV} to open it"
            ))),
            (KeySource::Passphrase(_), KdfHeader::Keyfile) => Err(file_error(format!(
                "the secret file is encrypted with a key file; unset {PASSPHRASE_FILE_ENV} to open it"
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

/// Parses the file strictly: no unknown or extra fields anywhere, including the KDF header (a
/// key-file header carries nothing, a passphrase header carries exactly a salt and a cost).
fn parse_envelope(text: &str, path: &Path) -> Result<Envelope, SecretError> {
    let invalid = |error: &dyn fmt::Display| {
        file_error(format!(
            "{} is not a valid secret file: {error}",
            path.display()
        ))
    };
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| invalid(&e))?;
    let envelope: Envelope = serde_json::from_value(value.clone()).map_err(|e| invalid(&e))?;
    let canonical = serde_json::to_value(&envelope.kdf).map_err(|e| invalid(&e))?;
    if value.get("kdf") != Some(&canonical) {
        return Err(invalid(
            &"the key derivation header has missing or extra fields",
        ));
    }
    Ok(envelope)
}

fn derive_key(
    passphrase: &[u8],
    salt: &[u8],
    cost: KdfCost,
) -> Result<Zeroizing<[u8; KEY_LEN]>, SecretError> {
    let in_bounds = (MIN_MEMORY_KIB..=MAX_MEMORY_KIB).contains(&cost.memory_kib)
        && (1..=MAX_ITERATIONS).contains(&cost.iterations)
        && (1..=MAX_LANES).contains(&cost.lanes)
        && (8..=64).contains(&salt.len());
    if !in_bounds {
        return Err(file_error(
            "the secret file's key settings are outside the allowed range (memory 8 MiB to 1 GiB, 1 to 10 passes, 1 to 8 lanes)",
        ));
    }
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
    match read_private_file(path)? {
        Some(bytes) => {
            let key: [u8; KEY_LEN] = bytes[..].try_into().map_err(|_| {
                file_error(format!(
                    "{} is not a valid key file (expected {KEY_LEN} bytes)",
                    path.display()
                ))
            })?;
            Ok(Zeroizing::new(key))
        }
        None if !may_create => Err(file_error(format!(
            "the key file {} is missing, so the existing secrets cannot be decrypted; restore it from a backup",
            path.display()
        ))),
        None => {
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            OsRng.fill_bytes(&mut key[..]);
            if let Some(parent) = path.parent() {
                create_private_dir(parent)?;
            }
            match open_private(path) {
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
    }
}

/// Reads a small file that must be private: not a symbolic link, a regular file, and (on Unix)
/// unreadable by other users, judged on the opened handle rather than the path so it cannot be
/// swapped in between. `None` when it does not exist.
fn read_private_file(path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
    use std::io::Read as _;
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(file_error(format!(
                "{} is a symbolic link; refusing to follow it",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("inspect", path, &error)),
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Fails rather than follows a symbolic link swapped in after the check above.
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("read", path, &error)),
    };
    let meta = file
        .metadata()
        .map_err(|error| io_error("inspect", path, &error))?;
    if !meta.is_file() {
        return Err(file_error(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    check_private(path, &meta)?;
    let mut bytes = Zeroizing::new(Vec::new());
    (&mut file)
        .take(MAX_PRIVATE_FILE_LEN as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read", path, &error))?;
    if bytes.len() > MAX_PRIVATE_FILE_LEN {
        return Err(file_error(format!(
            "{} is larger than {MAX_PRIVATE_FILE_LEN} bytes, which a key or passphrase file never is",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

/// Refuses a file other users can read (like ssh does for private keys). Windows has no mode
/// bits; there the file inherits the per-user profile folder's access list (see the docs for
/// shared `IMPERIUM_HOME` folders).
#[cfg(unix)]
fn check_private(path: &Path, meta: &fs::Metadata) -> Result<(), SecretError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(file_error(format!(
            "{} has mode {:o}, so other users could read it; run `chmod 600` on it",
            path.display(),
            mode & 0o777
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private(_path: &Path, _meta: &fs::Metadata) -> Result<(), SecretError> {
    Ok(())
}

/// Creates a new file only its owner can read (mode `0600` on Unix), failing if the path
/// exists, symbolic links included.
fn open_private(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Replaces `path` with `bytes` through a new, randomly named, owner-only temporary file, and
/// syncs the folder so the rename survives a crash.
fn write_private_atomically(path: &Path, bytes: &[u8]) -> Result<(), SecretError> {
    let mut suffix = [0u8; 8];
    OsRng.fill_bytes(&mut suffix);
    let suffix: String = suffix.iter().map(|byte| format!("{byte:02x}")).collect();
    let temp = path.with_extension(format!("{suffix}.tmp"));
    let write = || -> std::io::Result<()> {
        let mut file = open_private(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)?;
        #[cfg(unix)]
        {
            if let Some(parent) = path.parent() {
                fs::File::open(parent)?.sync_all()?;
            }
        }
        Ok(())
    };
    write().map_err(|error| {
        let _ = fs::remove_file(&temp);
        io_error("write", path, &error)
    })
}

/// Creates the folder (and parents) owner-only on Unix when it does not exist yet.
fn create_private_dir(path: &Path) -> Result<(), SecretError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|error| io_error("create", path, &error))
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

/// Which store is holding secrets. Decided once when the store is opened and then kept: a
/// keychain that fails later is an error to report, never a reason to start writing secrets to
/// a file.
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
/// encrypted file. The daemon makes the choice once, at [`AutoSecrets::open_daemon`], records it,
/// and the CLI follows the record; it is made only on Linux (the only
/// platform where a keychain can be missing); everywhere else the keychain is the only store.
/// When the daemon finds a keychain at a later start, secrets left in the file move into it
/// ([`AutoSecrets::migrate`]); until then a copy still in the file is the one read.
pub struct AutoSecrets {
    keychain: Arc<dyn SecretStore>,
    file: FileSecrets,
    backend: SecretBackend,
    notice: Option<String>,
}

impl fmt::Debug for AutoSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AutoSecrets")
            .field("backend", &self.backend)
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

/// Decides which store to use: on Linux, the keychain when reading a name nobody writes does not
/// report the keychain as missing; on every other platform, always the keychain.
pub(crate) fn detect_backend(keychain: &dyn SecretStore) -> SecretBackend {
    if !cfg!(target_os = "linux") {
        return SecretBackend::Keychain;
    }
    match keychain.load(&SecretName::from_valid("imperium.keychain-probe")) {
        Err(error) if error.is_keychain_unavailable() => SecretBackend::EncryptedFile,
        _ => SecretBackend::Keychain,
    }
}

/// Who is opening the store. The daemon decides the backend and records it; every other process
/// follows the record, so the CLI and the daemon cannot keep secrets in different places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opener {
    Daemon,
    Client,
}

/// The backend to use: the daemon's own detection; a client follows the daemon's record when
/// there is one and detects for itself only when there is none.
fn choose_backend(
    opener: Opener,
    recorded: Option<SecretBackend>,
    detected: SecretBackend,
) -> SecretBackend {
    match opener {
        Opener::Daemon => detected,
        Opener::Client => recorded.unwrap_or(detected),
    }
}

fn read_backend_record(path: &Path) -> Option<SecretBackend> {
    match fs::read_to_string(path).ok()?.trim() {
        "file" => Some(SecretBackend::EncryptedFile),
        "keychain" => Some(SecretBackend::Keychain),
        _ => None,
    }
}

fn write_backend_record(path: &Path, backend: SecretBackend) -> Result<(), SecretError> {
    let text = match backend {
        SecretBackend::Keychain => "keychain\n",
        SecretBackend::EncryptedFile => "file\n",
    };
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    // Not a secret, but written like one: a new owner-only temporary file renamed into place,
    // which replaces a symbolic link at the path instead of following it.
    write_private_atomically(path, text.as_bytes())
}

impl AutoSecrets {
    /// For the daemon: decides the backend now, records it in the data folder (`secrets.backend`)
    /// for the CLI to follow, and keeps it. Fails when passphrase mode is asked for and the
    /// passphrase file cannot be read.
    pub fn open_daemon(paths: &Paths) -> Result<Self, SecretError> {
        AutoSecrets::open_as(paths, Opener::Daemon)
    }

    /// For every other process: follows the daemon's recorded backend, or detects one when the
    /// daemon has not recorded any.
    pub fn open(paths: &Paths) -> Result<Self, SecretError> {
        AutoSecrets::open_as(paths, Opener::Client)
    }

    fn open_as(paths: &Paths, opener: Opener) -> Result<Self, SecretError> {
        let keychain = Arc::new(KeychainSecrets);
        let detected = detect_backend(keychain.as_ref());
        let record_path = paths.data_dir().join("secrets.backend");
        // Only Linux can lack a keychain, so only Linux has anything to record or follow.
        let recorded = if cfg!(target_os = "linux") {
            read_backend_record(&record_path)
        } else {
            None
        };
        let backend = choose_backend(opener, recorded, detected);
        let mut notice = None;
        if opener == Opener::Daemon && cfg!(target_os = "linux") {
            if let Err(error) = write_backend_record(&record_path, backend) {
                tracing::warn!(%error, "could not record which secret store the daemon uses; the CLI will detect one itself");
            }
        } else if opener == Opener::Client && cfg!(target_os = "linux") {
            if let Some(recorded) = recorded {
                if recorded != detected {
                    notice = Some(format!(
                        "the daemon uses the {} for secrets but this process would pick the {}; following the daemon",
                        backend_name(recorded),
                        backend_name(detected)
                    ));
                }
            } else if backend == SecretBackend::EncryptedFile
                && paths.daemon_endpoint_file().exists()
            {
                notice = Some(
                    "a daemon may be running that uses the OS keychain, which would not see secrets saved to the encrypted file until it restarts"
                        .to_string(),
                );
            }
        }
        let mut auto = AutoSecrets::with_parts(keychain, FileSecrets::for_paths(paths)?, backend);
        auto.notice = notice;
        Ok(auto)
    }

    pub fn with_parts(
        keychain: Arc<dyn SecretStore>,
        file: FileSecrets,
        backend: SecretBackend,
    ) -> Self {
        AutoSecrets {
            keychain,
            file,
            backend,
            notice: None,
        }
    }

    /// The encrypted file's location.
    pub fn file_path(&self) -> &Path {
        self.file.path()
    }

    /// The store in use, as decided at open.
    pub fn backend(&self) -> SecretBackend {
        self.backend
    }

    /// A warning about this process and the daemon using different stores, when they would.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Moves every secret in the encrypted file into the keychain, when the keychain is the
    /// store in use; run by the daemon at start, and only there. The file's copy wins: it is the
    /// newer one, since a write that reaches the keychain removes any file copy first or fails.
    pub fn migrate(&self) -> Result<MigrationReport, SecretError> {
        if self.backend != SecretBackend::Keychain || !self.file.exists() {
            return Ok(MigrationReport::default());
        }
        let moved = self
            .file
            .drain_into(self.keychain.as_ref())
            .map_err(|error| self.leftover(error))?;
        Ok(MigrationReport { moved })
    }

    /// Adds what to do to an error from the leftover secret file while the keychain is in use.
    fn leftover(&self, error: SecretError) -> SecretError {
        match error {
            SecretError::File(message) => SecretError::File(format!(
                "{message}. The OS keychain is in use but the older encrypted file {} cannot be used: set {PASSPHRASE_FILE_ENV} to its passphrase file if it was made with a passphrase, or delete it if you no longer need the secrets in it",
                self.file.path().display()
            )),
            other => other,
        }
    }

    /// The warning to show wherever the fallback is in use.
    pub fn fallback_warning(&self) -> String {
        let how = if std::env::var_os(PASSPHRASE_FILE_ENV).is_some_and(|value| !value.is_empty()) {
            "under a key derived from the passphrase file (Argon2id)"
        } else {
            "under a key file next to it, which any program running as you can read"
        };
        format!(
            "no OS keychain is available, so secrets are kept in {} encrypted {how}; this is weaker than a keychain. They move into the keychain at the daemon's next start once one is available",
            self.file.path().display()
        )
    }
}

fn backend_name(backend: SecretBackend) -> &'static str {
    match backend {
        SecretBackend::Keychain => "OS keychain",
        SecretBackend::EncryptedFile => "encrypted file",
    }
}

impl SecretStore for AutoSecrets {
    fn load(&self, name: &SecretName) -> Result<Option<String>, SecretError> {
        if self.backend == SecretBackend::EncryptedFile {
            return self.file.load(name);
        }
        // A copy still in the file is the newer one (see `migrate`); reading never moves it.
        if self.file.exists() {
            match self.file.load(name) {
                Ok(Some(value)) => return Ok(Some(value)),
                Ok(None) => {}
                Err(error) => {
                    // The keychain is the store in use; an unreadable leftover file does not
                    // make a secret that is not in the keychain exist.
                    tracing::warn!(%error, secret = %name, "the encrypted secret file could not be read, so it is ignored for this secret");
                }
            }
        }
        self.keychain.load(name)
    }

    fn store(&self, name: &SecretName, value: &str) -> Result<(), SecretError> {
        if self.backend == SecretBackend::EncryptedFile {
            return self.file.store(name, value);
        }
        self.keychain.store(name, value)?;
        // An older copy in the file would be read in preference to this one, so the write has
        // not taken effect until it is gone: report a failure to remove it.
        if self.file.exists() {
            self.file
                .delete(name)
                .map_err(|error| self.leftover(error))?;
        }
        Ok(())
    }

    fn delete(&self, name: &SecretName) -> Result<bool, SecretError> {
        if self.backend == SecretBackend::EncryptedFile {
            return self.file.delete(name);
        }
        let in_keychain = self.keychain.delete(name)?;
        let in_file = if self.file.exists() {
            self.file
                .delete(name)
                .map_err(|error| self.leftover(error))?
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

    /// A keychain that fails every call until switched on.
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

    /// Opens a store over the fake keychain with the backend pinned, as a daemon start would.
    fn open(dir: &Path, keychain: &Arc<FakeKeychain>, backend: SecretBackend) -> AutoSecrets {
        AutoSecrets::with_parts(
            Arc::new(SharedKeychain(keychain.clone())),
            key_file_store(dir),
            backend,
        )
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
        // No temporary file is left behind.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|file| file.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_key_secret_and_lock_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        key_file_store(&data).store(&name("a"), "value").unwrap();
        for file in ["secrets.key", "secrets.enc", "secrets.lock"] {
            let mode = fs::metadata(data.join(file)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{file}");
        }
        let mode = fs::metadata(&data).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "a data folder made for the secrets");
        fs::set_permissions(data.join("secrets.key"), fs::Permissions::from_mode(0o644)).unwrap();
        let error = key_file_store(&data)
            .load(&name("a"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("chmod 600"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_key_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = key_file_store(dir.path());
        store.store(&name("a"), "value").unwrap();
        let real = dir.path().join("real.key");
        fs::rename(dir.path().join("secrets.key"), &real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("secrets.key")).unwrap();
        let error = store.load(&name("a")).unwrap_err().to_string();
        assert!(error.contains("symbolic link"), "{error}");
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
    fn the_passphrase_comes_from_a_file_never_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let phrase = dir.path().join("phrase");
        fs::write(&phrase, "correct horse\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&phrase, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let store = FileSecrets::for_paths_with(&paths, Some(&phrase)).unwrap();
        assert!(matches!(store.source, KeySource::Passphrase(_)));
        let default = FileSecrets::for_paths_with(&paths, None).unwrap();
        assert!(matches!(default.source, KeySource::KeyFile(_)));
        // The passphrase itself is never in the debug output.
        assert!(!format!("{store:?}").contains("correct horse"));
        fs::write(&phrase, "\n").unwrap();
        assert!(FileSecrets::for_paths_with(&paths, Some(&phrase)).is_err());
        assert!(FileSecrets::for_paths_with(&paths, Some(&dir.path().join("none"))).is_err());
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
        assert!(error.contains(PASSPHRASE_FILE_ENV), "{error}");
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
    fn a_hostile_key_derivation_cost_is_refused_before_deriving() {
        let dir = tempfile::tempdir().unwrap();
        let store = passphrase_store(dir.path(), "pw");
        store.store(&name("a"), "v").unwrap();
        let path = dir.path().join("secrets.enc");
        let original: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for (field, value) in [
            ("memory_kib", 4_294_967_295u64),
            ("memory_kib", 1024),
            ("iterations", 1000),
            ("iterations", 0),
            ("lanes", 100),
        ] {
            let mut envelope = original.clone();
            envelope["kdf"][field] = value.into();
            fs::write(&path, envelope.to_string()).unwrap();
            // A fresh store has no cached key, so this would otherwise derive.
            let started = std::time::Instant::now();
            let error = passphrase_store(dir.path(), "pw")
                .load(&name("a"))
                .unwrap_err()
                .to_string();
            assert!(error.contains("allowed range"), "{field}={value}: {error}");
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
        }
    }

    #[test]
    fn a_key_file_header_with_extra_fields_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = key_file_store(dir.path());
        store.store(&name("a"), "v").unwrap();
        let path = dir.path().join("secrets.enc");
        let mut envelope: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        envelope["kdf"]["salt"] = "AAAAAAAAAAAAAAAAAAAAAA==".into();
        fs::write(&path, envelope.to_string()).unwrap();
        let error = store.load(&name("a")).unwrap_err().to_string();
        assert!(error.contains("extra fields"), "{error}");
        // So are extra fields on the file itself.
        let mut envelope: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        envelope["kdf"].as_object_mut().unwrap().remove("salt");
        envelope["surprise"] = 1.into();
        fs::write(&path, envelope.to_string()).unwrap();
        assert!(store.load(&name("a")).is_err());
        // A passphrase header missing its salt is rejected too.
        let other = tempfile::tempdir().unwrap();
        let pass = passphrase_store(other.path(), "pw");
        pass.store(&name("a"), "v").unwrap();
        let path = other.path().join("secrets.enc");
        let mut envelope: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        envelope["kdf"].as_object_mut().unwrap().remove("salt");
        fs::write(&path, envelope.to_string()).unwrap();
        assert!(
            passphrase_store(other.path(), "pw")
                .load(&name("a"))
                .is_err()
        );
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
    fn the_backend_is_decided_once_and_only_on_linux() {
        let keychain = SharedKeychain(Arc::new(FakeKeychain::default()));
        let expected = if cfg!(target_os = "linux") {
            SecretBackend::EncryptedFile
        } else {
            SecretBackend::Keychain
        };
        assert_eq!(detect_backend(&keychain), expected);
        keychain.0.on.store(true, Ordering::SeqCst);
        assert_eq!(detect_backend(&keychain), SecretBackend::Keychain);
    }

    #[test]
    fn a_file_backed_store_works_without_touching_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(FakeKeychain::default());
        let auto = open(dir.path(), &keychain, SecretBackend::EncryptedFile);
        auto.store(&name("a"), "value").unwrap();
        assert_eq!(auto.load(&name("a")).unwrap().as_deref(), Some("value"));
        assert!(auto.fallback_warning().contains("weaker than a keychain"));
        assert!(auto.delete(&name("a")).unwrap());
        assert!(!auto.delete(&name("a")).unwrap());
        assert_eq!(auto.load(&name("a")).unwrap(), None);
        assert_eq!(auto.migrate().unwrap(), MigrationReport::default());
    }

    #[test]
    fn a_failing_keychain_is_an_error_and_never_a_reason_to_write_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(FakeKeychain::default());
        let auto = open(dir.path(), &keychain, SecretBackend::Keychain);
        // The keychain fails (locked, say) after the backend was decided.
        assert!(auto.store(&name("a"), "value").is_err());
        assert!(auto.load(&name("a")).is_err());
        assert!(!dir.path().join("secrets.enc").exists());
        assert!(!dir.path().join("secrets.key").exists());
        // A delete during the error reports it rather than "not stored".
        assert!(auto.delete(&name("a")).is_err());
    }

    #[test]
    fn secrets_move_into_a_keychain_found_at_a_later_start() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(FakeKeychain::default());
        let before = open(dir.path(), &keychain, SecretBackend::EncryptedFile);
        before.store(&name("a"), "one").unwrap();
        before.store(&name("b"), "two").unwrap();
        before.store(&name("c"), "three").unwrap();
        assert!(before.file.exists());

        keychain.on.store(true, Ordering::SeqCst);
        // Still file-backed until the next start: nothing moves behind its back.
        assert_eq!(before.load(&name("a")).unwrap().as_deref(), Some("one"));
        assert_eq!(keychain.inner.load(&name("a")).unwrap(), None);

        let after = open(dir.path(), &keychain, SecretBackend::Keychain);
        // Reading before the move finds the file's copy and moves nothing.
        assert_eq!(after.load(&name("a")).unwrap().as_deref(), Some("one"));
        assert_eq!(keychain.inner.load(&name("a")).unwrap(), None);
        assert!(after.file.exists());
        // The daemon's migration moves them all.
        assert_eq!(after.migrate().unwrap(), MigrationReport { moved: 3 });
        assert!(!after.file.exists(), "the emptied file is removed");
        assert_eq!(
            keychain.inner.load(&name("c")).unwrap().as_deref(),
            Some("three")
        );
        assert_eq!(after.load(&name("a")).unwrap().as_deref(), Some("one"));
        assert_eq!(after.migrate().unwrap().moved, 0);
    }

    #[test]
    fn migration_lets_the_newer_file_copy_replace_the_keychain_value() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(FakeKeychain::default());
        keychain.on.store(true, Ordering::SeqCst);
        keychain.inner.store(&name("a"), "v1").unwrap();
        open(dir.path(), &keychain, SecretBackend::EncryptedFile)
            .store(&name("a"), "v2")
            .unwrap();
        let auto = open(dir.path(), &keychain, SecretBackend::Keychain);
        // Until migrated, the file's newer value is the one read.
        assert_eq!(auto.load(&name("a")).unwrap().as_deref(), Some("v2"));
        assert_eq!(auto.migrate().unwrap().moved, 1);
        assert_eq!(
            keychain.inner.load(&name("a")).unwrap().as_deref(),
            Some("v2")
        );
        assert!(!auto.file.exists());
    }

    #[test]
    fn a_store_that_cannot_remove_the_older_file_copy_fails() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(FakeKeychain::default());
        keychain.on.store(true, Ordering::SeqCst);
        fs::write(dir.path().join("secrets.enc"), "not json").unwrap();
        let auto = open(dir.path(), &keychain, SecretBackend::Keychain);
        let error = auto.store(&name("a"), "new").unwrap_err().to_string();
        assert!(
            error.contains(PASSPHRASE_FILE_ENV) && error.contains("delete it"),
            "{error}"
        );
        let error = auto.migrate().unwrap_err().to_string();
        assert!(error.contains("delete it"), "{error}");
    }

    #[test]
    fn a_client_follows_the_daemons_recorded_backend() {
        use SecretBackend::{EncryptedFile, Keychain};
        for detected in [Keychain, EncryptedFile] {
            // The daemon always uses what it detected.
            assert_eq!(
                choose_backend(Opener::Daemon, Some(EncryptedFile), detected),
                detected
            );
            // A client follows the record, and detects only when there is none.
            for recorded in [Keychain, EncryptedFile] {
                assert_eq!(
                    choose_backend(Opener::Client, Some(recorded), detected),
                    recorded
                );
            }
            assert_eq!(choose_backend(Opener::Client, None, detected), detected);
        }
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("data").join("secrets.backend");
        assert_eq!(read_backend_record(&record), None);
        write_backend_record(&record, EncryptedFile).unwrap();
        assert_eq!(read_backend_record(&record), Some(EncryptedFile));
        write_backend_record(&record, Keychain).unwrap();
        assert_eq!(read_backend_record(&record), Some(Keychain));
        fs::write(&record, "nonsense").unwrap();
        assert_eq!(read_backend_record(&record), None);
        // A symbolic link at the record is replaced, not written through.
        #[cfg(unix)]
        {
            let target = dir.path().join("target");
            fs::write(&target, "untouched").unwrap();
            fs::remove_file(&record).unwrap();
            std::os::unix::fs::symlink(&target, &record).unwrap();
            write_backend_record(&record, Keychain).unwrap();
            assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
            assert_eq!(read_backend_record(&record), Some(Keychain));
        }
    }

    #[test]
    fn key_and_passphrase_files_over_the_size_limit_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big");
        fs::write(&big, vec![b'x'; MAX_PRIVATE_FILE_LEN + 1]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&big, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let error = read_private_file(&big).unwrap_err().to_string();
        assert!(error.contains("larger than"), "{error}");
    }

    #[test]
    fn an_unreadable_leftover_file_makes_a_missing_secret_absent_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(FakeKeychain::default());
        keychain.on.store(true, Ordering::SeqCst);
        keychain.inner.store(&name("here"), "v").unwrap();
        fs::write(dir.path().join("secrets.enc"), "not json").unwrap();
        let auto = open(dir.path(), &keychain, SecretBackend::Keychain);
        assert_eq!(auto.load(&name("here")).unwrap().as_deref(), Some("v"));
        assert_eq!(auto.load(&name("missing")).unwrap(), None);
    }

    #[test]
    fn a_keychain_write_replaces_a_stale_file_copy() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(FakeKeychain::default());
        open(dir.path(), &keychain, SecretBackend::EncryptedFile)
            .store(&name("a"), "old")
            .unwrap();
        keychain.on.store(true, Ordering::SeqCst);
        let auto = open(dir.path(), &keychain, SecretBackend::Keychain);
        auto.store(&name("a"), "new").unwrap();
        assert!(!auto.file.exists());
        assert_eq!(auto.load(&name("a")).unwrap().as_deref(), Some("new"));
        assert!(auto.delete(&name("a")).unwrap());
        assert_eq!(auto.load(&name("a")).unwrap(), None);
    }
}
