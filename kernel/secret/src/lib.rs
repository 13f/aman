#![forbid(unsafe_code)]
#![doc = "Secret resolution and prompt-injection safeguards for aman."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


use kernel::{AmanResult, Error};
use kernel::retry::RetryBackoff;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::Mutex;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::rngs::OsRng;
use rand::TryRngCore;
use secrets::SecretVec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Secret {
    pub key: String,
    pub value: String,
}

pub trait SecretBackend: Send + Sync {
    fn get(&self, key: &str) -> AmanResult<Option<String>>;

    /// Write a secret value. Backends that do not support writes return an error by default.
    fn set(&self, key: &str, value: &str) -> AmanResult<()> {
        let _ = (key, value);
        Err(Error::Unrecoverable {
            message: format!("backend '{}' does not support writes", self.name()),
        })
    }

    fn priority(&self) -> u32;
    fn name(&self) -> &'static str;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnePasswordCliBackend {
    pub op_path: PathBuf,
}

impl Default for OnePasswordCliBackend {
    fn default() -> Self {
        Self {
            op_path: PathBuf::from("op"),
        }
    }
}

impl SecretBackend for OnePasswordCliBackend {
    fn get(&self, key: &str) -> AmanResult<Option<String>> {
        if !key.starts_with("op://") {
            return Ok(None);
        }
        let output = Command::new(&self.op_path)
            .arg("read")
            .arg(key)
            .output()?;
        if !output.status.success() {
            return Err(Error::Unrecoverable {
                message: "1password cli returned non-zero exit status".to_string(),
            });
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(if value.is_empty() { None } else { Some(value) })
    }

    fn priority(&self) -> u32 {
        40
    }

    fn name(&self) -> &'static str {
        "1password_cli"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsSecretsManagerCliBackend {
    pub aws_path: PathBuf,
    pub region: Option<String>,
    pub profile: Option<String>,
}

impl Default for AwsSecretsManagerCliBackend {
    fn default() -> Self {
        Self {
            aws_path: PathBuf::from("aws"),
            region: None,
            profile: None,
        }
    }
}

impl SecretBackend for AwsSecretsManagerCliBackend {
    fn get(&self, key: &str) -> AmanResult<Option<String>> {
        let Some(secret_id) = key.strip_prefix("aws-sm://") else {
            return Ok(None);
        };
        let secret_id = secret_id.trim();
        if secret_id.is_empty() {
            return Err(Error::config_invalid("aws-sm:// secret id cannot be empty"));
        }

        let mut cmd = Command::new(&self.aws_path);
        cmd.arg("secretsmanager")
            .arg("get-secret-value")
            .arg("--secret-id")
            .arg(secret_id)
            .arg("--query")
            .arg("SecretString")
            .arg("--output")
            .arg("text");

        if let Some(profile) = &self.profile {
            cmd.arg("--profile").arg(profile);
        }
        if let Some(region) = &self.region {
            cmd.arg("--region").arg(region);
        }

        let output = cmd.output()?;
        if !output.status.success() {
            return Err(Error::Unrecoverable {
                message: "aws cli returned non-zero exit status".to_string(),
            });
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(if value.is_empty() { None } else { Some(value) })
    }

    fn priority(&self) -> u32 {
        50
    }

    fn name(&self) -> &'static str {
        "aws_sm_cli"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultCliBackend {
    pub vault_path: PathBuf,
}

impl Default for VaultCliBackend {
    fn default() -> Self {
        Self {
            vault_path: PathBuf::from("vault"),
        }
    }
}

impl SecretBackend for VaultCliBackend {
    fn get(&self, key: &str) -> AmanResult<Option<String>> {
        let Some(reference) = key.strip_prefix("vault://") else {
            return Ok(None);
        };
        let reference = reference.trim();
        let Some((path, field)) = reference.split_once('#') else {
            return Err(Error::config_invalid(
                "vault:// reference must be vault://path#field",
            ));
        };
        let path = path.trim();
        let field = field.trim();
        if path.is_empty() || field.is_empty() {
            return Err(Error::config_invalid(
                "vault:// reference must include non-empty path and field",
            ));
        }

        let output = Command::new(&self.vault_path)
            .arg("kv")
            .arg("get")
            .arg(format!("-field={field}"))
            .arg(path)
            .output()?;
        if !output.status.success() {
            return Err(Error::Unrecoverable {
                message: "vault cli returned non-zero exit status".to_string(),
            });
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(if value.is_empty() { None } else { Some(value) })
    }

    fn priority(&self) -> u32 {
        60
    }

    fn name(&self) -> &'static str {
        "vault_cli"
    }
}

/// Cross-platform OS-native credential store using the `keyring` crate.
///
/// macOS → Security framework (no subprocess, no `security` CLI prompts)
/// Windows → Win32 Credential Manager API
/// Linux → libsecret (GNOME Keyring / KDE Wallet)
///
/// Keychain entry layout (macOS):
///   service : <key>         (e.g. "aman.3rd.tavily.api_key")
///   account : "aman-desktop"
///
/// This matches the layout previously written by the `security` CLI,
/// so existing credentials are automatically forward-compatible.
#[derive(Debug, Clone)]
pub struct KeychainBackend;

/// Process-wide encrypted cache for keychain values.
///
/// Values are stored in `SecretVec<u8>` which provides:
/// - `mlock(2)` to prevent swapping to disk
/// - auto `zeroize` on drop / cache clear
/// - guard pages and underflow canaries
/// - core dump exclusion (in release builds)
///
/// Combined with one-time keychain access, this eliminates repeated
/// OS authorization prompts while keeping secrets protected in memory.
static KEYCHAIN_CACHE: LazyLock<Mutex<HashMap<String, SecretVec<u8>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

impl SecretBackend for KeychainBackend {
    fn get(&self, key: &str) -> AmanResult<Option<String>> {
        // Check protected cache first — avoids repeated macOS authorization prompts
        {
            let cache = KEYCHAIN_CACHE.lock().unwrap();
            if let Some(secret) = cache.get(key) {
                return Ok(Some(String::from_utf8_lossy(&secret.borrow()).to_string()));
            }
        }

        let entry = keyring::Entry::new(key, "aman-desktop")
            .map_err(|e| Error::Unrecoverable {
                message: format!("keychain entry create failed: {e}"),
            })?;
        match entry.get_password() {
            Ok(value) => {
                let bytes = value.as_bytes();
                let secret = SecretVec::<u8>::new(bytes.len(), |buf: &mut [u8]| {
                    buf.copy_from_slice(bytes);
                });
                let mut cache = KEYCHAIN_CACHE.lock().unwrap();
                cache.insert(key.to_owned(), secret);
                Ok(Some(value))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Unrecoverable {
                message: format!("keychain read failed: {e}"),
            }),
        }
    }

    fn set(&self, key: &str, value: &str) -> AmanResult<()> {
        // Invalidate cache so next read picks up the new value
        {
            let mut cache = KEYCHAIN_CACHE.lock().unwrap();
            // Removing drops the SecretVec which zeroes the memory
            cache.remove(key);
        }

        let entry = keyring::Entry::new(key, "aman-desktop")
            .map_err(|e| Error::Unrecoverable {
                message: format!("keychain entry create failed: {e}"),
            })?;
        entry.set_password(value).map_err(|e| Error::Unrecoverable {
            message: format!("keychain write failed: {e}"),
        })?;

        // Update protected cache with the new value
        let bytes = value.as_bytes();
        let secret = SecretVec::<u8>::new(bytes.len(), |buf: &mut [u8]| {
            buf.copy_from_slice(bytes);
        });
        let mut cache = KEYCHAIN_CACHE.lock().unwrap();
        cache.insert(key.to_owned(), secret);
        Ok(())
    }

    fn priority(&self) -> u32 {
        30
    }

    fn name(&self) -> &'static str {
        "keychain"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretResolverConfig {
    pub retry_count: u32,
    pub retry_backoff: RetryBackoff,
    pub cache_ttl_ms: u64,
    pub cache_fallback: Option<SecretCacheFallbackConfig>,
}

impl Default for SecretResolverConfig {
    fn default() -> Self {
        Self {
            retry_count: 3,
            retry_backoff: RetryBackoff::Exponential,
            cache_ttl_ms: 300_000,
            cache_fallback: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretCacheFallbackConfig {
    pub dir: PathBuf,
    pub ttl_ms: u64,
    pub key_hex: String,
}

impl SecretCacheFallbackConfig {
    pub fn key_bytes(&self) -> AmanResult<[u8; 32]> {
        parse_key_hex(&self.key_hex)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedMemory<T> {
    encrypted: Vec<u8>,
    nonce: [u8; 12],
    key: [u8; 32],
    _marker: std::marker::PhantomData<T>,
}

impl<T> EncryptedMemory<T>
where
    T: Serialize + DeserializeOwned,
{
    pub fn seal(value: &T, key: &[u8; 32]) -> AmanResult<Self> {
        let plaintext = serde_json::to_vec(value)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
        let mut nonce = [0u8; 12];
        let mut rng = OsRng;
        rng.try_fill_bytes(&mut nonce).map_err(|_| Error::Unrecoverable {
            message: "os rng unavailable".to_string(),
        })?;
        let encrypted = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_ref())
            .map_err(|_| Error::Unrecoverable {
                message: "secret encryption failed".to_string(),
            })?;
        Ok(Self {
            encrypted,
            nonce,
            key: *key,
            _marker: std::marker::PhantomData,
        })
    }

    pub fn open(&self) -> AmanResult<T> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key));
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&self.nonce), self.encrypted.as_ref())
            .map_err(|_| Error::Unrecoverable {
                message: "secret decryption failed".to_string(),
            })?;
        Ok(serde_json::from_slice::<T>(&plaintext)?)
    }
}

#[derive(Debug)]
pub struct SecretCache {
    key: [u8; 32],
    ttl_ms: u64,
    entries: HashMap<String, (EncryptedMemory<String>, u128)>,
}

impl Default for SecretCache {
    fn default() -> Self {
        Self::new(300_000) // 5-minute default TTL
    }
}

impl SecretCache {
    pub fn new(ttl_ms: u64) -> Self {
        let mut key = [0u8; 32];
        let mut rng = OsRng;
        if rng.try_fill_bytes(&mut key).is_err() {
            key = *blake3::hash(&now_ms().to_le_bytes()).as_bytes();
        }
        Self {
            key,
            ttl_ms,
            entries: HashMap::new(),
        }
    }

    pub fn get(&self, key: &str) -> AmanResult<Option<String>> {
        let Some((sealed, stored_at_ms)) = self.entries.get(key) else {
            return Ok(None);
        };
        let now = now_ms();
        if now.saturating_sub(*stored_at_ms) > u128::from(self.ttl_ms) {
            return Ok(None);
        }
        Ok(Some(sealed.open()?))
    }

    pub fn put(&mut self, key: &str, value: &str) -> AmanResult<()> {
        let sealed = EncryptedMemory::<String>::seal(&value.to_string(), &self.key)?;
        self.entries.insert(key.to_string(), (sealed, now_ms()));
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRotationAudit {
    pub affected_keys: Vec<String>,
    pub fingerprint_created_at_ms: u128,
    pub resolved_at_ms: u128,
    pub backend_hits: Vec<String>,
    pub trigger_source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRotation {
    pub id: Uuid,
    pub keys: Vec<String>,
    pub values: HashMap<String, String>,
    pub prepared_at_ms: u128,
    pub effective_at_ms: u128,
    pub backend_hits: Vec<String>,
    pub trigger_source: String,
}

#[derive(Default)]
pub struct SecretResolver {
    backends: Vec<Box<dyn SecretBackend>>,
    cache: SecretCache,
    config: SecretResolverConfig,
    pending_rotations: HashMap<Uuid, PendingRotation>,
    audit_log: Vec<SecretRotationAudit>,
}

impl SecretResolver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            backends: Vec::new(),
            cache: SecretCache::new(SecretResolverConfig::default().cache_ttl_ms),
            config: SecretResolverConfig::default(),
            pending_rotations: HashMap::new(),
            audit_log: Vec::new(),
        }
    }

    pub fn with_backend(mut self, backend: Box<dyn SecretBackend>) -> Self {
        self.backends.push(backend);
        self.backends.sort_by_key(|b| b.priority());
        self
    }

    pub fn with_config(mut self, config: SecretResolverConfig) -> Self {
        self.cache = SecretCache::new(config.cache_ttl_ms);
        self.config = config;
        self
    }

    pub fn resolve_all(&mut self, value: &mut Value) -> AmanResult<Vec<String>> {
        let mut resolved_keys = Vec::new();
        let mut backend_hits = Vec::new();
        self.resolve_value(value, &mut resolved_keys, &mut backend_hits)?;
        if !resolved_keys.is_empty() {
            let now = now_ms();
            backend_hits.sort();
            backend_hits.dedup();
            self.audit_log.push(SecretRotationAudit {
                affected_keys: resolved_keys.clone(),
                fingerprint_created_at_ms: now,
                resolved_at_ms: now,
                backend_hits,
                trigger_source: "config_load".to_string(),
            });
        }
        Ok(resolved_keys)
    }

    pub fn prepare_rotation(
        &mut self,
        keys: &[String],
        trigger_source: &str,
        grace_period_sec: u64,
    ) -> AmanResult<Uuid> {
        let prepared_at_ms = now_ms();
        let effective_at_ms = prepared_at_ms + u128::from(grace_period_sec) * 1000;
        let mut backend_hits = Vec::new();
        let mut values = HashMap::new();
        for key in keys {
            let (value, backend) = self.resolve_key(key)?;
            values.insert(key.clone(), value);
            backend_hits.push(backend);
        }
        backend_hits.sort();
        backend_hits.dedup();
        let id = Uuid::now_v7();
        self.pending_rotations.insert(
            id,
            PendingRotation {
                id,
                keys: keys.to_vec(),
                values,
                prepared_at_ms,
                effective_at_ms,
                backend_hits,
                trigger_source: trigger_source.to_string(),
            },
        );
        Ok(id)
    }

    pub fn commit_rotation(&mut self, id: Uuid) -> AmanResult<()> {
        let Some(pending) = self.pending_rotations.remove(&id) else {
            return Err(Error::NotFound {
                name: format!("pending rotation {id}"),
            });
        };

        for key in &pending.keys {
            if let Some(value) = pending.values.get(key) {
                self.cache.put(key, value)?;
                if let Some(fallback) = &self.config.cache_fallback {
                    write_file_cache_entry(fallback, key, value)?;
                }
            }
        }

        let now = now_ms();
        self.audit_log.push(SecretRotationAudit {
            affected_keys: pending.keys,
            fingerprint_created_at_ms: pending.effective_at_ms,
            resolved_at_ms: now,
            backend_hits: pending.backend_hits,
            trigger_source: pending.trigger_source,
        });
        Ok(())
    }

    #[must_use]
    pub fn cancel_rotation(&mut self, id: Uuid) -> bool {
        self.pending_rotations.remove(&id).is_some()
    }

    pub fn rotate(
        &mut self,
        keys: &[String],
        trigger_source: &str,
        grace_period_sec: u64,
    ) -> AmanResult<()> {
        let id = self.prepare_rotation(keys, trigger_source, grace_period_sec)?;
        self.commit_rotation(id)
    }

    #[must_use]
    pub fn audit_log(&self) -> &[SecretRotationAudit] {
        &self.audit_log
    }

    fn resolve_value(
        &mut self,
        value: &mut Value,
        resolved_keys: &mut Vec<String>,
        backend_hits: &mut Vec<String>,
    ) -> AmanResult<()> {
        match value {
            Value::Object(map) => {
                for nested in map.values_mut() {
                    self.resolve_value(nested, resolved_keys, backend_hits)?;
                }
                Ok(())
            }
            Value::Array(items) => {
                for nested in items {
                    self.resolve_value(nested, resolved_keys, backend_hits)?;
                }
                Ok(())
            }
            Value::String(content) => {
                let Some(key) = extract_env_placeholder(content) else {
                    return Ok(());
                };
                let (secret_value, backend) = self.resolve_key(&key)?;
                *content = secret_value;
                resolved_keys.push(key);
                backend_hits.push(backend);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn resolve_key(&mut self, key: &str) -> AmanResult<(String, String)> {
        if let Some(cached) = self.cache.get(key)? {
            return Ok((cached, "cache".to_string()));
        }

        let mut last_backend_error: Option<Error> = None;
        for backend in &self.backends {
            match self.get_with_retry(backend.as_ref(), key) {
                Ok(Some(value)) => {
                    self.cache.put(key, &value)?;
                    if let Some(fallback) = &self.config.cache_fallback {
                        write_file_cache_entry(fallback, key, &value)?;
                    }
                    return Ok((value, backend.name().to_string()));
                }
                Ok(None) => {}
                Err(error) => {
                    last_backend_error = Some(error);
                }
            }
        }

        if let Some(fallback) = &self.config.cache_fallback
            && let Some(value) = read_file_cache_entry(fallback, key)?
        {
            self.cache.put(key, &value)?;
            return Ok((value, "file_cache".to_string()));
        }

        if let Some(error) = last_backend_error {
            return Err(error);
        }

        Err(Error::SecretUnresolved { key: key.to_string() })
    }

    fn get_with_retry(&self, backend: &dyn SecretBackend, key: &str) -> AmanResult<Option<String>> {
        let attempts = self.config.retry_count.max(1);
        for attempt in 0..attempts {
            match backend.get(key) {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if attempt + 1 >= attempts {
                        return Err(error);
                    }
                    if let Some(delay) = retry_delay(&self.config.retry_backoff, attempt)
                        && !delay.is_zero() {
                            thread::sleep(delay);
                        }
                }
            }
        }
        Ok(None)
    }
}

#[derive(Default)]
pub struct EnvSecretBackend;

impl SecretBackend for EnvSecretBackend {
    fn get(&self, key: &str) -> AmanResult<Option<String>> {
        Ok(std::env::var(key).ok())
    }

    fn priority(&self) -> u32 {
        100
    }

    fn name(&self) -> &'static str {
        "env"
    }
}

// ---------------------------------------------------------------------------
// Re-exports from kernel::core (migrated from this crate to consolidate
// security types in one place). These are provided for backward compatibility;
// new code should import directly from `kernel`.
// ---------------------------------------------------------------------------

pub use kernel::sanitizer::{
    InjectionAuditRecord, InjectionDetector, InjectionWarning, SanitizedInput,
};
pub use kernel::system_prompt_guard::SystemPromptHardener;
pub use kernel::types::TrustLevel;
pub use kernel::validator::{OutputAuditRecord, OutputValidator};

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

pub trait RotationTarget {
    fn apply_rotation(&mut self, keys: &[String]) -> AmanResult<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub struct RollingUpdateCoordinator {
    pub per_target_delay_ms: u64,
}


impl RollingUpdateCoordinator {
    pub fn apply<T>(&self, keys: &[String], targets: &mut [T]) -> AmanResult<()>
    where
        T: RotationTarget,
    {
        for target in targets {
            target.apply_rotation(keys)?;
            if self.per_target_delay_ms > 0 {
                thread::sleep(Duration::from_millis(self.per_target_delay_ms));
            }
        }
        Ok(())
    }
}

fn extract_env_placeholder(value: &str) -> Option<String> {
    if !value.starts_with("${") || !value.ends_with('}') {
        return None;
    }
    let key = value.trim_start_matches("${").trim_end_matches('}').trim();
    if key.is_empty() {
        return None;
    }
    Some(key.to_string())
}

fn retry_delay(backoff: &RetryBackoff, attempt: u32) -> Option<Duration> {
    match backoff {
        RetryBackoff::Immediate => Some(Duration::from_millis(0)),
        RetryBackoff::Fixed(ms) => Some(Duration::from_millis(*ms)),
        RetryBackoff::Exponential => {
            let base = 100u64;
            let shift = attempt.min(10);
            Some(Duration::from_millis(base.saturating_mul(1u64 << shift)))
        }
        RetryBackoff::Sequence(seq) => {
            if seq.is_empty() {
                None
            } else {
                let index = attempt as usize;
                let delay_ms = seq.get(index).copied().unwrap_or_else(|| {
                    seq.last().copied().unwrap_or(0)
                });
                Some(Duration::from_millis(delay_ms))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileCacheEntry {
    nonce_hex: String,
    ciphertext_hex: String,
    stored_at_ms: u128,
    ttl_ms: u64,
}

fn write_file_cache_entry(cfg: &SecretCacheFallbackConfig, key: &str, value: &str) -> AmanResult<()> {
    let dir = &cfg.dir;
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }

    let key_bytes = cfg.key_bytes()?;
    let sealed = EncryptedMemory::<String>::seal(&value.to_string(), &key_bytes)?;
    let entry = FileCacheEntry {
        nonce_hex: hex_encode(&sealed.nonce),
        ciphertext_hex: hex_encode(&sealed.encrypted),
        stored_at_ms: now_ms(),
        ttl_ms: cfg.ttl_ms,
    };
    let content = serde_json::to_vec(&entry)?;
    let path = file_cache_path(cfg, key);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content)?;
    #[cfg(unix)]
    {
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(tmp, path)?;
    Ok(())
}

fn read_file_cache_entry(cfg: &SecretCacheFallbackConfig, key: &str) -> AmanResult<Option<String>> {
    let path = file_cache_path(cfg, key);
    let content = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let entry = serde_json::from_slice::<FileCacheEntry>(&content)?;
    let now = now_ms();
    if now.saturating_sub(entry.stored_at_ms) > u128::from(entry.ttl_ms) {
        return Ok(None);
    }
    let key_bytes = cfg.key_bytes()?;
    let nonce = hex_decode_12(&entry.nonce_hex)?;
    let ciphertext = hex_decode(&entry.ciphertext_hex)?;
    let sealed = EncryptedMemory::<String> {
        encrypted: ciphertext,
        nonce,
        key: key_bytes,
        _marker: std::marker::PhantomData,
    };
    Ok(Some(sealed.open()?))
}

fn file_cache_path(cfg: &SecretCacheFallbackConfig, key: &str) -> PathBuf {
    let digest = blake3::hash(key.as_bytes()).to_hex().to_string();
    cfg.dir.join(format!("{digest}.json"))
}

fn parse_key_hex(hex: &str) -> AmanResult<[u8; 32]> {
    let cleaned = hex.trim();
    if cleaned.len() != 64 {
        return Err(Error::config_invalid("secret cache key must be 64 hex chars"));
    }
    let bytes = hex_decode(cleaned)?;
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes[..32]);
    Ok(key)
}

fn hex_encode(bytes: &[u8]) -> String {
    const LUT: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(LUT[(b >> 4) as usize]);
        out.push(LUT[(b & 0x0f) as usize]);
    }
    String::from_utf8(out).unwrap_or_default()
}

fn hex_decode(hex: &str) -> AmanResult<Vec<u8>> {
    let bytes = hex.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::config_invalid("hex string length must be even"));
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_val(bytes[i])?;
        let lo = hex_val(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_decode_12(hex: &str) -> AmanResult<[u8; 12]> {
    let bytes = hex_decode(hex)?;
    if bytes.len() != 12 {
        return Err(Error::config_invalid("nonce must be 12 bytes"));
    }
    let mut out = [0u8; 12];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn hex_val(byte: u8) -> AmanResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(Error::config_invalid("invalid hex character")),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EncryptedMemory, SecretBackend, SecretCache, SecretCacheFallbackConfig,
        SecretResolver, RollingUpdateCoordinator, RotationTarget,
        SecretResolverConfig,
    };
    use kernel::retry::RetryBackoff;
    use kernel::{AmanResult, Error};
    use serde_json::json;
    use std::collections::HashMap;

    struct StaticBackend {
        values: HashMap<String, String>,
    }

    impl SecretBackend for StaticBackend {
        fn get(&self, key: &str) -> AmanResult<Option<String>> {
            Ok(self.values.get(key).cloned())
        }

        fn priority(&self) -> u32 {
            10
        }

        fn name(&self) -> &'static str {
            "static"
        }
    }

    #[test]
    fn resolves_env_secret_placeholders_recursively() {
        let mut resolver = SecretResolver::new()
            .with_backend(Box::new(StaticBackend {
                values: HashMap::from([(
                    "AMAN_TEST_API_KEY".to_string(),
                    "super-secret".to_string(),
                )]),
            }))
            .with_config(SecretResolverConfig {
                retry_count: 3,
                retry_backoff: RetryBackoff::Immediate,
                cache_ttl_ms: 300_000,
                cache_fallback: None,
            });
        let mut payload = json!({
            "tool": {
                "token": "${AMAN_TEST_API_KEY}",
                "nested": ["x", "${AMAN_TEST_API_KEY}"]
            }
        });

        let resolved_keys = resolver
            .resolve_all(&mut payload)
            .expect("secret resolving should succeed");
        assert_eq!(resolved_keys.len(), 2);
        assert_eq!(payload["tool"]["token"], "super-secret");
        assert_eq!(payload["tool"]["nested"][1], "super-secret");
        assert_eq!(resolver.audit_log().len(), 1);
        assert!(
            resolver.audit_log()[0]
                .backend_hits
                .iter()
                .any(|backend| backend == "static"),
            "backend name should be tracked in audit"
        );
    }

    #[test]
    fn unresolved_placeholder_returns_error() {
        let mut resolver = SecretResolver::new().with_backend(Box::new(StaticBackend {
            values: HashMap::new(),
        }));
        let mut payload = json!({
            "token": "${AMAN_NOT_EXIST}"
        });

        let error = resolver
            .resolve_all(&mut payload)
            .expect_err("missing secret should fail");
        assert!(
            error
                .to_string()
                .contains("secret could not be resolved: AMAN_NOT_EXIST"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rotate_records_audit_with_grace_period() {
        let mut resolver = SecretResolver::new().with_backend(Box::new(StaticBackend {
            values: HashMap::from([("DB_PASSWORD".to_string(), "p@ss-2".to_string())]),
        }));
        let id = resolver
            .prepare_rotation(&["DB_PASSWORD".to_string()], "manual", 60)
            .expect("prepare should succeed");
        resolver.commit_rotation(id).expect("commit should succeed");
        let last = resolver
            .audit_log()
            .last()
            .expect("rotate should append audit");
        assert_eq!(last.trigger_source, "manual");
        assert!(
            last.fingerprint_created_at_ms > last.resolved_at_ms,
            "fingerprint effective time should be after commit time (grace period applied)"
        );
        assert!(
            last.backend_hits.iter().any(|backend| backend == "static"),
            "rotation audit should record backend"
        );
    }

    #[test]
    fn encrypted_memory_round_trips() {
        let key = [7u8; 32];
        let sealed = EncryptedMemory::<String>::seal(&"hello".to_string(), &key).unwrap();
        let opened = sealed.open().unwrap();
        assert_eq!(opened, "hello");
    }

    struct FailingBackend {
        attempts: std::sync::Mutex<u32>,
    }

    impl SecretBackend for FailingBackend {
        fn get(&self, _key: &str) -> AmanResult<Option<String>> {
            let mut guard = self.attempts.lock().unwrap();
            *guard += 1;
            Err(Error::Unrecoverable {
                message: "backend down".to_string(),
            })
        }

        fn priority(&self) -> u32 {
            1
        }

        fn name(&self) -> &'static str {
            "fail"
        }
    }

    #[test]
    fn fallback_reads_encrypted_file_cache_when_backend_errors() {
        let dir = std::env::temp_dir().join(format!("aman-secret-cache-{}", super::now_ms()));
        let cfg = SecretResolverConfig {
            retry_count: 2,
            retry_backoff: RetryBackoff::Immediate,
            cache_ttl_ms: 300_000,
            cache_fallback: Some(SecretCacheFallbackConfig {
                dir: dir.clone(),
                ttl_ms: 300_000,
                key_hex:
                    "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f".to_string(),
            }),
        };
        super::write_file_cache_entry(
            cfg.cache_fallback.as_ref().unwrap(),
            "DB_URL",
            "postgres://cached",
        )
        .unwrap();
        let mut resolver = SecretResolver::new()
            .with_backend(Box::new(FailingBackend {
                attempts: std::sync::Mutex::new(0),
            }))
            .with_config(cfg);
        let mut payload = json!({ "url": "${DB_URL}" });
        resolver.resolve_all(&mut payload).unwrap();
        assert_eq!(payload["url"], "postgres://cached");
        let _ = std::fs::remove_dir_all(dir);
    }

    struct RecordingTarget {
        calls: Vec<Vec<String>>,
    }

    impl RotationTarget for RecordingTarget {
        fn apply_rotation(&mut self, keys: &[String]) -> AmanResult<()> {
            self.calls.push(keys.to_vec());
            Ok(())
        }
    }

    #[test]
    fn rolling_update_applies_targets_sequentially() {
        let coordinator = RollingUpdateCoordinator::default();
        let keys = vec!["DB_URL".to_string()];
        let mut targets = vec![
            RecordingTarget { calls: Vec::new() },
            RecordingTarget { calls: Vec::new() },
        ];
        coordinator.apply(&keys, &mut targets).unwrap();
        assert_eq!(targets[0].calls.len(), 1);
        assert_eq!(targets[1].calls.len(), 1);
        assert_eq!(targets[0].calls[0], keys);
        assert_eq!(targets[1].calls[0], vec!["DB_URL".to_string()]);
    }

    #[test]
    fn keychain_backend_roundtrip() {
        let backend = super::KeychainBackend;
        let test_key = "aman.test.secret_crate_test";

        // Ensure clean state
        let _ = keyring::Entry::new(test_key, "aman-desktop")
            .and_then(|e| e.delete_password());

        // Initially should be None
        assert_eq!(backend.get(test_key).unwrap(), None);

        // Set and verify
        backend.set(test_key, "roundtrip_value").unwrap();
        let result = backend.get(test_key).unwrap();
        assert_eq!(result, Some("roundtrip_value".to_string()));

        // Update existing
        backend.set(test_key, "updated_value").unwrap();
        let result = backend.get(test_key).unwrap();
        assert_eq!(result, Some("updated_value".to_string()));

        // Cleanup
        let _ = keyring::Entry::new(test_key, "aman-desktop")
            .and_then(|e| e.delete_password());
    }

    #[test]
    fn secret_cache_default_produces_random_key() {
        let cache1 = SecretCache::default();
        let cache2 = SecretCache::default();
        // Two default-constructed caches must have different keys
        assert_ne!(cache1.key, cache2.key);
        // The key must not be all zeros
        assert_ne!(cache1.key, [0u8; 32]);
    }
}
