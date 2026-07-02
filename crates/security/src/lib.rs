//! Security policy and secret handling boundaries.

pub const LAYER: &str = "security";

use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum KnownHostsPolicy {
    #[default]
    Ask,
    RequireKnown,
    TrustOnFirstUse,
    PinFingerprint {
        sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    #[default]
    Agent,
    PublicKey,
    Password,
    KeyboardInteractive,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub sha256_fingerprint: String,
}

impl HostKey {
    #[must_use]
    pub fn from_raw(
        host: impl Into<String>,
        port: u16,
        algorithm: impl Into<String>,
        bytes: &[u8],
    ) -> Self {
        let digest = Sha256::digest(bytes);
        Self {
            host: host.into(),
            port,
            algorithm: algorithm.into(),
            sha256_fingerprint: format!("SHA256:{}", STANDARD.encode(digest)),
        }
    }

    #[must_use]
    pub fn storage_key(&self) -> String {
        format!("[{}]:{}", self.host, self.port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyDecision {
    Trusted,
    TrustAndStore,
    Unknown { expected_decision: String },
    Mismatch { expected: String, actual: String },
}

impl HostKeyDecision {
    #[must_use]
    pub const fn is_trusted(&self) -> bool {
        matches!(self, Self::Trusted | Self::TrustAndStore)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownHostEntry {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub sha256_fingerprint: String,
}

impl KnownHostEntry {
    #[must_use]
    pub fn matches_key(&self, key: &HostKey) -> bool {
        self.host == key.host
            && self.port == key.port
            && self.algorithm == key.algorithm
            && self.sha256_fingerprint == key.sha256_fingerprint
    }
}

impl From<&HostKey> for KnownHostEntry {
    fn from(key: &HostKey) -> Self {
        Self {
            host: key.host.clone(),
            port: key.port,
            algorithm: key.algorithm.clone(),
            sha256_fingerprint: key.sha256_fingerprint.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnownHosts {
    entries: BTreeMap<String, KnownHostEntry>,
}

impl KnownHosts {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn load(path: &Path) -> SecurityResult<Self> {
        match fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).map_err(|error| {
                SecurityError::new(format!("failed to parse known hosts store: {error}"))
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(error) => Err(SecurityError::new(format!(
                "failed to read known hosts store '{}': {error}",
                path.display()
            ))),
        }
    }

    pub fn save(&self, path: &Path) -> SecurityResult<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                SecurityError::new(format!(
                    "failed to create known hosts directory '{}': {error}",
                    parent.display()
                ))
            })?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|error| {
            SecurityError::new(format!("failed to serialize known hosts store: {error}"))
        })?;
        fs::write(path, json).map_err(|error| {
            SecurityError::new(format!(
                "failed to write known hosts store '{}': {error}",
                path.display()
            ))
        })
    }

    #[must_use]
    pub fn entry_for(&self, host: &str, port: u16) -> Option<&KnownHostEntry> {
        self.entries.get(&format!("[{host}]:{port}"))
    }

    pub fn trust(&mut self, key: &HostKey) {
        self.entries
            .insert(key.storage_key(), KnownHostEntry::from(key));
    }

    #[must_use]
    pub fn verify(&self, key: &HostKey, policy: &KnownHostsPolicy) -> HostKeyDecision {
        match policy {
            KnownHostsPolicy::PinFingerprint { sha256 } => {
                if key.sha256_fingerprint == *sha256 {
                    HostKeyDecision::Trusted
                } else {
                    HostKeyDecision::Mismatch {
                        expected: sha256.clone(),
                        actual: key.sha256_fingerprint.clone(),
                    }
                }
            }
            KnownHostsPolicy::RequireKnown | KnownHostsPolicy::Ask => {
                match self.entry_for(&key.host, key.port) {
                    Some(entry) if entry.matches_key(key) => HostKeyDecision::Trusted,
                    Some(entry) => HostKeyDecision::Mismatch {
                        expected: entry.sha256_fingerprint.clone(),
                        actual: key.sha256_fingerprint.clone(),
                    },
                    None => HostKeyDecision::Unknown {
                        expected_decision: format!(
                            "{} {} {} requires explicit trust before connecting",
                            key.host, key.algorithm, key.sha256_fingerprint
                        ),
                    },
                }
            }
            KnownHostsPolicy::TrustOnFirstUse => match self.entry_for(&key.host, key.port) {
                Some(entry) if entry.matches_key(key) => HostKeyDecision::Trusted,
                Some(entry) => HostKeyDecision::Mismatch {
                    expected: entry.sha256_fingerprint.clone(),
                    actual: key.sha256_fingerprint.clone(),
                },
                None => HostKeyDecision::TrustAndStore,
            },
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecretString {
    value: String,
}

impl SecretString {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.value
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(**redacted**)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretRequest {
    SshPassword {
        profile: String,
        username: String,
    },
    SshKeyPassphrase {
        profile: String,
        identity_file: PathBuf,
    },
}

pub trait SecretProvider: Send {
    fn request_secret(&mut self, request: SecretRequest) -> SecurityResult<Option<SecretString>>;
}

#[derive(Debug, Default)]
pub struct EmptySecretProvider;

impl SecretProvider for EmptySecretProvider {
    fn request_secret(&mut self, _request: SecretRequest) -> SecurityResult<Option<SecretString>> {
        Ok(None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityError {
    message: String,
}

impl SecurityError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SecurityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for SecurityError {}

pub type SecurityResult<T> = Result<T, SecurityError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_key_fingerprint_is_stable() {
        let key = HostKey::from_raw("example.com", 22, "ssh-ed25519", b"host-key");

        assert_eq!(
            key.sha256_fingerprint,
            "SHA256:CfEOS9w3pHE4KlqjcQFwWyWMmyRvvPoehydyMhTxpzg="
        );
    }

    #[test]
    fn unknown_host_requires_explicit_decision_by_default() {
        let known_hosts = KnownHosts::empty();
        let key = HostKey::from_raw("example.com", 22, "ssh-ed25519", b"host-key");

        assert!(matches!(
            known_hosts.verify(&key, &KnownHostsPolicy::default()),
            HostKeyDecision::Unknown { .. }
        ));
    }

    #[test]
    fn changed_host_key_is_mismatch() {
        let mut known_hosts = KnownHosts::empty();
        let original = HostKey::from_raw("example.com", 22, "ssh-ed25519", b"first");
        let changed = HostKey::from_raw("example.com", 22, "ssh-ed25519", b"second");
        known_hosts.trust(&original);

        let decision = known_hosts.verify(&changed, &KnownHostsPolicy::RequireKnown);

        assert!(matches!(decision, HostKeyDecision::Mismatch { .. }));
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = SecretString::new("hunter2");

        assert_eq!(format!("{secret:?}"), "SecretString(**redacted**)");
    }
}
