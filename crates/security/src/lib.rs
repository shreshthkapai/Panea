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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyTrustReason {
    UnknownHost,
    ChangedHostKey,
    PinnedFingerprintMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyTrustRequest {
    pub key: HostKey,
    pub reason: HostKeyTrustReason,
    pub expected_fingerprint: Option<String>,
    pub message: String,
}

impl HostKeyTrustRequest {
    #[must_use]
    pub fn unknown(key: HostKey, message: impl Into<String>) -> Self {
        Self {
            key,
            reason: HostKeyTrustReason::UnknownHost,
            expected_fingerprint: None,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn changed(key: HostKey, expected: impl Into<String>) -> Self {
        let expected = expected.into();
        Self {
            message: format!(
                "host key changed for {}:{}; expected {}, observed {}",
                key.host, key.port, expected, key.sha256_fingerprint
            ),
            key,
            reason: HostKeyTrustReason::ChangedHostKey,
            expected_fingerprint: Some(expected),
        }
    }

    #[must_use]
    pub fn pinned_mismatch(key: HostKey, expected: impl Into<String>) -> Self {
        let expected = expected.into();
        Self {
            message: format!(
                "host key does not match pinned fingerprint for {}:{}; expected {}, observed {}",
                key.host, key.port, expected, key.sha256_fingerprint
            ),
            key,
            reason: HostKeyTrustReason::PinnedFingerprintMismatch,
            expected_fingerprint: Some(expected),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyTrustAction {
    Reject,
    TrustOnce,
    TrustAndStore,
    ReplaceStoredKey,
}

pub trait HostTrustProvider: Send {
    fn decide_host_trust(
        &mut self,
        request: HostKeyTrustRequest,
    ) -> SecurityResult<HostKeyTrustAction>;
}

#[derive(Debug, Default)]
pub struct RejectingHostTrustProvider;

impl HostTrustProvider for RejectingHostTrustProvider {
    fn decide_host_trust(
        &mut self,
        _request: HostKeyTrustRequest,
    ) -> SecurityResult<HostKeyTrustAction> {
        Ok(HostKeyTrustAction::Reject)
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
        host: String,
        username: String,
    },
    SshKeyPassphrase {
        profile: String,
        host: String,
        identity_file: PathBuf,
    },
}

impl SecretRequest {
    #[must_use]
    pub fn keychain_entry(&self) -> KeychainEntry {
        match self {
            Self::SshPassword {
                profile,
                host,
                username,
            } => KeychainEntry::new(
                "panea.ssh",
                format!("{profile}:{username}@{host}"),
                KeychainSecretKind::SshPassword,
            ),
            Self::SshKeyPassphrase {
                profile,
                host,
                identity_file,
            } => KeychainEntry::new(
                "panea.ssh",
                format!("{profile}:{host}:{}", identity_file.display()),
                KeychainSecretKind::SshKeyPassphrase,
            ),
        }
    }

    #[must_use]
    pub fn prompt_label(&self) -> String {
        match self {
            Self::SshPassword {
                profile,
                host,
                username,
            } => format!("SSH password for profile '{profile}' as {username}@{host}"),
            Self::SshKeyPassphrase {
                profile,
                host,
                identity_file,
            } => format!(
                "SSH key passphrase for profile '{profile}' on {host} ({})",
                identity_file.display()
            ),
        }
    }
}

pub trait SecretProvider: Send {
    fn request_secret(&mut self, request: SecretRequest) -> SecurityResult<Option<SecretString>>;
}

pub struct SecretPromptResponse {
    pub secret: SecretString,
    pub save_to_keychain: bool,
}

impl SecretPromptResponse {
    #[must_use]
    pub fn transient(secret: SecretString) -> Self {
        Self {
            secret,
            save_to_keychain: false,
        }
    }

    #[must_use]
    pub fn persistent(secret: SecretString) -> Self {
        Self {
            secret,
            save_to_keychain: true,
        }
    }
}

impl fmt::Debug for SecretPromptResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretPromptResponse")
            .field("secret", &self.secret)
            .field("save_to_keychain", &self.save_to_keychain)
            .finish()
    }
}

pub trait SecretPromptProvider: Send {
    fn prompt_secret(
        &mut self,
        request: &SecretRequest,
    ) -> SecurityResult<Option<SecretPromptResponse>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeychainSecretKind {
    SshPassword,
    SshKeyPassphrase,
    GenericToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeychainEntry {
    pub service: String,
    pub account: String,
    pub kind: KeychainSecretKind,
}

impl KeychainEntry {
    #[must_use]
    pub fn new(
        service: impl Into<String>,
        account: impl Into<String>,
        kind: KeychainSecretKind,
    ) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
            kind,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityPlatform {
    Windows,
    MacOs,
    Linux,
    Ios,
    Unknown,
}

impl SecurityPlatform {
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "ios") {
            Self::Ios
        } else {
            Self::Unknown
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeychainBackend {
    WindowsCredentialManager,
    MacOsKeychain,
    LinuxSecretService,
    IosKeychain,
    MemoryOnly,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeychainProviderCapability {
    pub platform: SecurityPlatform,
    pub backend: KeychainBackend,
    pub available: bool,
    pub persistent: bool,
    pub secure_storage: bool,
    pub message: String,
}

impl KeychainProviderCapability {
    #[must_use]
    pub fn unavailable(platform: SecurityPlatform, backend: KeychainBackend) -> Self {
        Self {
            platform,
            backend,
            available: false,
            persistent: false,
            secure_storage: false,
            message: "native keychain provider is not available in this build".to_owned(),
        }
    }
}

/// OS keychain boundary for platform-backed secret storage providers.
pub trait KeychainProvider: Send {
    fn capability(&self) -> KeychainProviderCapability {
        KeychainProviderCapability::unavailable(
            SecurityPlatform::current(),
            KeychainBackend::Unavailable,
        )
    }

    fn get_secret(&mut self, entry: &KeychainEntry) -> SecurityResult<Option<SecretString>>;

    fn set_secret(&mut self, entry: &KeychainEntry, secret: SecretString) -> SecurityResult<()>;

    fn delete_secret(&mut self, entry: &KeychainEntry) -> SecurityResult<()>;
}

pub struct KeychainBackedSecretProvider<K, P> {
    pub keychain: K,
    pub prompt_provider: P,
}

impl<K, P> KeychainBackedSecretProvider<K, P> {
    #[must_use]
    pub fn new(keychain: K, prompt_provider: P) -> Self {
        Self {
            keychain,
            prompt_provider,
        }
    }
}

impl<K, P> SecretProvider for KeychainBackedSecretProvider<K, P>
where
    K: KeychainProvider,
    P: SecretPromptProvider,
{
    fn request_secret(&mut self, request: SecretRequest) -> SecurityResult<Option<SecretString>> {
        let entry = request.keychain_entry();
        if let Some(secret) = self.keychain.get_secret(&entry)? {
            return Ok(Some(secret));
        }

        let Some(response) = self.prompt_provider.prompt_secret(&request)? else {
            return Ok(None);
        };

        if response.save_to_keychain {
            self.keychain.set_secret(&entry, response.secret.clone())?;
        }

        Ok(Some(response.secret))
    }
}

#[derive(Debug, Clone)]
pub struct PlatformKeychainProvider {
    capability: KeychainProviderCapability,
}

impl PlatformKeychainProvider {
    #[must_use]
    pub fn for_current_platform() -> Self {
        let platform = SecurityPlatform::current();
        let backend = keychain_backend(platform);
        let capability = match keyring::Entry::new("panea.capability", "provider-probe") {
            Ok(_) => KeychainProviderCapability {
                platform,
                backend,
                available: true,
                persistent: true,
                secure_storage: true,
                message: format!(
                    "native {} credential store initialized; individual operations may still be denied by OS policy",
                    keychain_backend_name(backend)
                ),
            },
            Err(error) => KeychainProviderCapability {
                platform,
                backend,
                available: false,
                persistent: false,
                secure_storage: false,
                message: format!(
                    "native {} credential store is unavailable: {error}",
                    keychain_backend_name(backend)
                ),
            },
        };
        Self { capability }
    }

    #[must_use]
    pub fn for_platform(platform: SecurityPlatform) -> Self {
        if platform == SecurityPlatform::current() {
            return Self::for_current_platform();
        }
        let backend = keychain_backend(platform);
        Self {
            capability: KeychainProviderCapability::unavailable(platform, backend),
        }
    }

    fn entry(&self, entry: &KeychainEntry) -> SecurityResult<keyring::Entry> {
        if !self.capability.available {
            return Err(SecurityError::new(self.capability.message.clone()));
        }
        keyring::Entry::new(&entry.service, &entry.account).map_err(|error| {
            SecurityError::new(format!(
                "failed to open native {} credential entry: {error}",
                keychain_backend_name(self.capability.backend)
            ))
        })
    }
}

impl KeychainProvider for PlatformKeychainProvider {
    fn capability(&self) -> KeychainProviderCapability {
        self.capability.clone()
    }

    fn get_secret(&mut self, entry: &KeychainEntry) -> SecurityResult<Option<SecretString>> {
        if !self.capability.available {
            return Ok(None);
        }
        match self.entry(entry)?.get_password() {
            Ok(secret) => Ok(Some(SecretString::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecurityError::new(format!(
                "failed to read native {} credential: {error}",
                keychain_backend_name(self.capability.backend)
            ))),
        }
    }

    fn set_secret(&mut self, entry: &KeychainEntry, secret: SecretString) -> SecurityResult<()> {
        self.entry(entry)?
            .set_password(secret.expose())
            .map_err(|error| {
                SecurityError::new(format!(
                    "failed to store native {} credential: {error}",
                    keychain_backend_name(self.capability.backend)
                ))
            })
    }

    fn delete_secret(&mut self, entry: &KeychainEntry) -> SecurityResult<()> {
        if !self.capability.available {
            return Ok(());
        }
        match self.entry(entry)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SecurityError::new(format!(
                "failed to delete native {} credential: {error}",
                keychain_backend_name(self.capability.backend)
            ))),
        }
    }
}

const fn keychain_backend(platform: SecurityPlatform) -> KeychainBackend {
    match platform {
        SecurityPlatform::Windows => KeychainBackend::WindowsCredentialManager,
        SecurityPlatform::MacOs => KeychainBackend::MacOsKeychain,
        SecurityPlatform::Linux => KeychainBackend::LinuxSecretService,
        SecurityPlatform::Ios => KeychainBackend::IosKeychain,
        SecurityPlatform::Unknown => KeychainBackend::Unavailable,
    }
}

const fn keychain_backend_name(backend: KeychainBackend) -> &'static str {
    match backend {
        KeychainBackend::WindowsCredentialManager => "Windows Credential Manager",
        KeychainBackend::MacOsKeychain => "macOS Keychain",
        KeychainBackend::LinuxSecretService => "Linux Secret Service",
        KeychainBackend::IosKeychain => "iOS Keychain",
        KeychainBackend::MemoryOnly => "memory-only",
        KeychainBackend::Unavailable => "platform",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Osc52ClipboardTarget {
    Clipboard,
    PrimarySelection,
    Select,
    Unknown(char),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Osc52ClipboardPolicy {
    pub enabled: bool,
    pub allow_local: bool,
    pub allow_remote: bool,
    pub max_bytes: usize,
    pub confirm_remote_writes: bool,
}

impl Default for Osc52ClipboardPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_local: true,
            allow_remote: false,
            max_bytes: 1_048_576,
            confirm_remote_writes: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Osc52ClipboardRequest {
    pub target: Osc52ClipboardTarget,
    pub payload_base64: String,
    pub remote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc52ClipboardDecision {
    Allow { text: String, bytes: usize },
    PromptRequired { reason: String, bytes: usize },
    Deny { reason: String },
}

impl Osc52ClipboardDecision {
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }
}

#[must_use]
pub fn evaluate_osc52_clipboard_write(
    request: &Osc52ClipboardRequest,
    policy: &Osc52ClipboardPolicy,
) -> Osc52ClipboardDecision {
    if !policy.enabled {
        return Osc52ClipboardDecision::Deny {
            reason: "OSC 52 clipboard writes are disabled by policy".to_owned(),
        };
    }
    if request.remote && !policy.allow_remote {
        return Osc52ClipboardDecision::Deny {
            reason: "remote OSC 52 clipboard writes are disabled by policy".to_owned(),
        };
    }
    if !request.remote && !policy.allow_local {
        return Osc52ClipboardDecision::Deny {
            reason: "local OSC 52 clipboard writes are disabled by policy".to_owned(),
        };
    }
    if matches!(request.target, Osc52ClipboardTarget::Unknown(_)) {
        return Osc52ClipboardDecision::Deny {
            reason: "OSC 52 requested an unknown clipboard target".to_owned(),
        };
    }
    if request.payload_base64.trim() == "?" {
        return Osc52ClipboardDecision::Deny {
            reason: "OSC 52 clipboard read requests are not supported".to_owned(),
        };
    }

    let max_encoded = policy.max_bytes.saturating_mul(4).saturating_div(3) + 8;
    if request.payload_base64.len() > max_encoded {
        return Osc52ClipboardDecision::Deny {
            reason: format!(
                "OSC 52 payload exceeds configured clipboard cap of {} bytes",
                policy.max_bytes
            ),
        };
    }

    let decoded = match STANDARD.decode(request.payload_base64.as_bytes()) {
        Ok(decoded) => decoded,
        Err(error) => {
            return Osc52ClipboardDecision::Deny {
                reason: format!("OSC 52 payload is not valid base64: {error}"),
            };
        }
    };

    if decoded.len() > policy.max_bytes {
        return Osc52ClipboardDecision::Deny {
            reason: format!(
                "OSC 52 decoded payload is {} bytes, above configured cap of {} bytes",
                decoded.len(),
                policy.max_bytes
            ),
        };
    }

    match String::from_utf8(decoded) {
        Ok(text) if request.remote && policy.confirm_remote_writes => {
            Osc52ClipboardDecision::PromptRequired {
                reason: "remote OSC 52 clipboard write requires explicit confirmation".to_owned(),
                bytes: text.len(),
            }
        }
        Ok(text) => Osc52ClipboardDecision::Allow {
            bytes: text.len(),
            text,
        },
        Err(error) => Osc52ClipboardDecision::Deny {
            reason: format!("OSC 52 decoded payload is not valid UTF-8: {error}"),
        },
    }
}

/// Re-evaluates a previously prompted request after one explicit user approval.
/// All target, size, encoding, locality, and enablement checks still apply.
#[must_use]
pub fn approve_osc52_clipboard_write(
    request: &Osc52ClipboardRequest,
    policy: &Osc52ClipboardPolicy,
) -> Osc52ClipboardDecision {
    let mut approved = policy.clone();
    approved.confirm_remote_writes = false;
    evaluate_osc52_clipboard_write(request, &approved)
}

#[derive(Debug, Default)]
pub struct EmptySecretProvider;

impl SecretProvider for EmptySecretProvider {
    fn request_secret(&mut self, _request: SecretRequest) -> SecurityResult<Option<SecretString>> {
        Ok(None)
    }
}

impl KeychainProvider for EmptySecretProvider {
    fn capability(&self) -> KeychainProviderCapability {
        KeychainProviderCapability::unavailable(
            SecurityPlatform::current(),
            KeychainBackend::Unavailable,
        )
    }

    fn get_secret(&mut self, _entry: &KeychainEntry) -> SecurityResult<Option<SecretString>> {
        Ok(None)
    }

    fn set_secret(&mut self, _entry: &KeychainEntry, _secret: SecretString) -> SecurityResult<()> {
        Err(SecurityError::new(
            "no OS keychain provider is available in this build",
        ))
    }

    fn delete_secret(&mut self, _entry: &KeychainEntry) -> SecurityResult<()> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct MemoryKeychainProvider {
    entries: BTreeMap<String, SecretString>,
}

impl MemoryKeychainProvider {
    fn key(entry: &KeychainEntry) -> String {
        format!("{}:{}:{:?}", entry.service, entry.account, entry.kind)
    }
}

impl KeychainProvider for MemoryKeychainProvider {
    fn capability(&self) -> KeychainProviderCapability {
        KeychainProviderCapability {
            platform: SecurityPlatform::Unknown,
            backend: KeychainBackend::MemoryOnly,
            available: true,
            persistent: false,
            secure_storage: false,
            message: "in-memory test keychain; not persistent and not secure storage".to_owned(),
        }
    }

    fn get_secret(&mut self, entry: &KeychainEntry) -> SecurityResult<Option<SecretString>> {
        Ok(self.entries.get(&Self::key(entry)).cloned())
    }

    fn set_secret(&mut self, entry: &KeychainEntry, secret: SecretString) -> SecurityResult<()> {
        self.entries.insert(Self::key(entry), secret);
        Ok(())
    }

    fn delete_secret(&mut self, entry: &KeychainEntry) -> SecurityResult<()> {
        self.entries.remove(&Self::key(entry));
        Ok(())
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

    #[test]
    fn keychain_provider_contract_keeps_secret_debug_redacted() {
        let entry = KeychainEntry::new("panea", "prod", KeychainSecretKind::SshPassword);
        let mut keychain = MemoryKeychainProvider::default();

        keychain
            .set_secret(&entry, SecretString::new("secret"))
            .expect("memory keychain should store test secret");
        let secret = keychain
            .get_secret(&entry)
            .expect("memory keychain should read test secret")
            .expect("secret should exist");

        assert_eq!(secret.expose(), "secret");
        assert_eq!(format!("{secret:?}"), "SecretString(**redacted**)");
        keychain
            .delete_secret(&entry)
            .expect("memory keychain delete should work");
        assert!(keychain.get_secret(&entry).unwrap().is_none());
    }

    #[derive(Default)]
    struct RecordingPromptProvider {
        calls: usize,
        response: Option<SecretPromptResponse>,
    }

    impl SecretPromptProvider for RecordingPromptProvider {
        fn prompt_secret(
            &mut self,
            _request: &SecretRequest,
        ) -> SecurityResult<Option<SecretPromptResponse>> {
            self.calls += 1;
            Ok(self.response.take())
        }
    }

    #[test]
    fn keychain_backed_secret_provider_uses_prompt_then_stores_when_requested() {
        let keychain = MemoryKeychainProvider::default();
        let prompt_provider = RecordingPromptProvider {
            response: Some(SecretPromptResponse::persistent(SecretString::new(
                "passphrase",
            ))),
            ..RecordingPromptProvider::default()
        };
        let mut provider = KeychainBackedSecretProvider::new(keychain, prompt_provider);
        let request = SecretRequest::SshKeyPassphrase {
            profile: "prod".to_owned(),
            host: "example.com".to_owned(),
            identity_file: PathBuf::from("/keys/id_ed25519"),
        };

        let first = provider
            .request_secret(request.clone())
            .expect("prompted secret should be returned")
            .expect("secret should exist");
        let second = provider
            .request_secret(request)
            .expect("stored secret should be returned")
            .expect("secret should exist");

        assert_eq!(first.expose(), "passphrase");
        assert_eq!(second.expose(), "passphrase");
        assert_eq!(provider.prompt_provider.calls, 1);
    }

    #[test]
    fn non_current_platform_keychain_reports_explicit_unavailable_capability() {
        let platform = if SecurityPlatform::current() == SecurityPlatform::Linux {
            SecurityPlatform::Windows
        } else {
            SecurityPlatform::Linux
        };
        let keychain = PlatformKeychainProvider::for_platform(platform);
        let capability = keychain.capability();

        assert_eq!(capability.backend, keychain_backend(platform));
        assert!(!capability.available);
        assert!(capability.message.contains("not available"));
    }

    #[test]
    fn unavailable_native_keychain_still_allows_transient_prompt_secret() {
        let platform = if SecurityPlatform::current() == SecurityPlatform::Linux {
            SecurityPlatform::Windows
        } else {
            SecurityPlatform::Linux
        };
        let keychain = PlatformKeychainProvider::for_platform(platform);
        let prompt_provider = RecordingPromptProvider {
            response: Some(SecretPromptResponse::transient(SecretString::new(
                "transient",
            ))),
            ..RecordingPromptProvider::default()
        };
        let mut provider = KeychainBackedSecretProvider::new(keychain, prompt_provider);
        let secret = provider
            .request_secret(SecretRequest::SshPassword {
                profile: "prod".to_owned(),
                host: "example.com".to_owned(),
                username: "alice".to_owned(),
            })
            .expect("unavailable keychain should fall back to prompt")
            .expect("transient secret");

        assert_eq!(secret.expose(), "transient");
    }

    #[test]
    #[ignore = "writes and deletes one temporary credential in the native OS keychain"]
    fn native_platform_keychain_round_trip() {
        let mut keychain = PlatformKeychainProvider::for_current_platform();
        let capability = keychain.capability();
        assert!(
            capability.available,
            "native keychain unavailable: {}",
            capability.message
        );
        let entry = KeychainEntry::new(
            "panea.native-keychain-smoke",
            format!("process-{}", std::process::id()),
            KeychainSecretKind::GenericToken,
        );
        let _ = keychain.delete_secret(&entry);
        keychain
            .set_secret(&entry, SecretString::new("panea-keychain-smoke"))
            .expect("native keychain write");
        let stored = keychain
            .get_secret(&entry)
            .expect("native keychain read")
            .expect("native keychain entry");
        assert_eq!(stored.expose(), "panea-keychain-smoke");
        keychain
            .delete_secret(&entry)
            .expect("native keychain cleanup");
        assert!(
            keychain
                .get_secret(&entry)
                .expect("native keychain read after delete")
                .is_none()
        );
    }

    #[test]
    fn rejecting_host_trust_provider_never_silently_accepts_unknown_hosts() {
        let key = HostKey::from_raw("example.com", 22, "ssh-ed25519", b"host-key");
        let request = HostKeyTrustRequest::unknown(key, "unknown host");
        let mut provider = RejectingHostTrustProvider;

        assert_eq!(
            provider.decide_host_trust(request).unwrap(),
            HostKeyTrustAction::Reject
        );
    }

    #[test]
    fn osc52_allows_bounded_local_clipboard_write_by_default() {
        let request = Osc52ClipboardRequest {
            target: Osc52ClipboardTarget::Clipboard,
            payload_base64: "cGFuZWE=".to_owned(),
            remote: false,
        };

        let decision = evaluate_osc52_clipboard_write(&request, &Osc52ClipboardPolicy::default());

        assert_eq!(
            decision,
            Osc52ClipboardDecision::Allow {
                text: "panea".to_owned(),
                bytes: 5
            }
        );
    }

    #[test]
    fn osc52_denies_remote_clipboard_write_by_default() {
        let request = Osc52ClipboardRequest {
            target: Osc52ClipboardTarget::Clipboard,
            payload_base64: "cGFuZWE=".to_owned(),
            remote: true,
        };

        let decision = evaluate_osc52_clipboard_write(&request, &Osc52ClipboardPolicy::default());

        assert!(
            matches!(decision, Osc52ClipboardDecision::Deny { reason } if reason.contains("remote"))
        );
    }

    #[test]
    fn osc52_remote_confirmation_requires_explicit_one_time_approval() {
        let request = Osc52ClipboardRequest {
            target: Osc52ClipboardTarget::Clipboard,
            payload_base64: "cGFuZWE=".to_owned(),
            remote: true,
        };
        let policy = Osc52ClipboardPolicy {
            allow_remote: true,
            ..Osc52ClipboardPolicy::default()
        };

        assert!(matches!(
            evaluate_osc52_clipboard_write(&request, &policy),
            Osc52ClipboardDecision::PromptRequired { bytes: 5, .. }
        ));
        assert_eq!(
            approve_osc52_clipboard_write(&request, &policy),
            Osc52ClipboardDecision::Allow {
                text: "panea".to_owned(),
                bytes: 5,
            }
        );
    }

    #[test]
    fn osc52_malformed_remote_payload_is_denied_before_prompting() {
        let request = Osc52ClipboardRequest {
            target: Osc52ClipboardTarget::Clipboard,
            payload_base64: "not base64".to_owned(),
            remote: true,
        };
        let policy = Osc52ClipboardPolicy {
            allow_remote: true,
            ..Osc52ClipboardPolicy::default()
        };

        assert!(matches!(
            evaluate_osc52_clipboard_write(&request, &policy),
            Osc52ClipboardDecision::Deny { reason } if reason.contains("base64")
        ));
    }

    #[test]
    fn osc52_caps_large_clipboard_writes() {
        let policy = Osc52ClipboardPolicy {
            max_bytes: 4,
            ..Osc52ClipboardPolicy::default()
        };
        let request = Osc52ClipboardRequest {
            target: Osc52ClipboardTarget::Clipboard,
            payload_base64: "cGFuZWE=".to_owned(),
            remote: false,
        };

        let decision = evaluate_osc52_clipboard_write(&request, &policy);

        assert!(
            matches!(decision, Osc52ClipboardDecision::Deny { reason } if reason.contains("cap"))
        );
    }
}
