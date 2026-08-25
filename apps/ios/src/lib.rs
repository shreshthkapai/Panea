//! iOS SSH companion shell contracts.

pub const LAYER: &str = "platform parity";

use std::{path::PathBuf, time::Duration};

use config_core::{AppConfig, SshAuthMethod, SshKnownHostsPolicy, SshProfile, VisualThemeConfig};
use font_system::{CellMetrics, FontConfig as RuntimeFontConfig};
use render_core::{DamageRegion, FrameRequest, FrameRequestReason, RenderScene};
use security::{
    AuthMethod, HostKeyTrustAction, HostKeyTrustRequest, KeychainEntry, KeychainProvider,
    KeychainProviderCapability, KnownHostsPolicy, PlatformKeychainProvider, SecretRequest,
    SecurityPlatform,
};
use semantics::{SemanticAction, SemanticTimelineStore};
use term_core::{TerminalCore, TerminalResult, TerminalSize as CoreTerminalSize};
use term_parser::TerminalEmulator;
use transport_core::{TerminalSize as TransportTerminalSize, TransportKind};
use transport_ssh::SshConnectionProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedEngineStatus {
    Reused,
    RequiresIosAdapter,
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedEngineComponent {
    pub crate_name: &'static str,
    pub category: &'static str,
    pub status: SharedEngineStatus,
    pub note: &'static str,
}

#[must_use]
pub fn shared_engine_components() -> Vec<SharedEngineComponent> {
    vec![
        SharedEngineComponent {
            crate_name: "term-core",
            category: "core correctness",
            status: SharedEngineStatus::Reused,
            note: "pure terminal grid, modes, cursor, scrollback, selection, and resize state",
        },
        SharedEngineComponent {
            crate_name: "term-parser",
            category: "core correctness",
            status: SharedEngineStatus::Reused,
            note: "ANSI/VT byte streams apply to the same TerminalEmulator used on desktop",
        },
        SharedEngineComponent {
            crate_name: "semantics",
            category: "semantic meaning",
            status: SharedEngineStatus::Reused,
            note: "command regions and shell metadata stay attached to terminal positions",
        },
        SharedEngineComponent {
            crate_name: "render-core",
            category: "render performance",
            status: SharedEngineStatus::Reused,
            note: "mobile rendering consumes the same renderer-independent scene contract",
        },
        SharedEngineComponent {
            crate_name: "font-system",
            category: "render performance",
            status: SharedEngineStatus::RequiresIosAdapter,
            note: "metrics and cache policy are shared; native iOS font discovery needs an adapter",
        },
        SharedEngineComponent {
            crate_name: "config-core",
            category: "config portability",
            status: SharedEngineStatus::Reused,
            note: "mobile settings compile into the same AppConfig and SSH profile model",
        },
        SharedEngineComponent {
            crate_name: "transport-core",
            category: "session transport",
            status: SharedEngineStatus::Reused,
            note: "iOS sessions satisfy the same byte/resize/lifecycle transport contract",
        },
        SharedEngineComponent {
            crate_name: "transport-ssh",
            category: "session transport",
            status: SharedEngineStatus::RequiresIosAdapter,
            note: "profile and lifecycle contract are shared; final iOS link backend must be verified",
        },
        SharedEngineComponent {
            crate_name: "visual theme model",
            category: "visual overlay",
            status: SharedEngineStatus::Reused,
            note: "themes and command-block visual settings remain portable overlays",
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SafeAreaInsets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Default for SafeAreaInsets {
    fn default() -> Self {
        Self {
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IosRenderSurface {
    pub width_points: f32,
    pub height_points: f32,
    pub scale_factor: f32,
    pub safe_area: SafeAreaInsets,
    pub keyboard_height_points: f32,
}

impl IosRenderSurface {
    #[must_use]
    pub fn terminal_size(self, metrics: CellMetrics) -> TransportTerminalSize {
        let usable_width =
            (self.width_points - self.safe_area.left - self.safe_area.right).max(1.0);
        let usable_height = (self.height_points
            - self.safe_area.top
            - self.safe_area.bottom
            - self.keyboard_height_points)
            .max(1.0);
        let cols = terminal_cells(usable_width, metrics.cell_width);
        let rows = terminal_cells(usable_height, metrics.cell_height);
        let pixel_width = physical_pixels(usable_width, self.scale_factor);
        let pixel_height = physical_pixels(usable_height, self.scale_factor);

        TransportTerminalSize::new(cols, rows, pixel_width, pixel_height)
    }

    #[must_use]
    pub fn damage_for_full_surface(self) -> DamageRegion {
        DamageRegion {
            x: 0,
            y: 0,
            width: physical_pixels(self.width_points.max(1.0), self.scale_factor),
            height: physical_pixels(self.height_points.max(1.0), self.scale_factor),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IosRendererBackend {
    SharedWgpu,
    NativeMetal,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IosGpuSurfaceSpec {
    pub backend: IosRendererBackend,
    pub surface: IosRenderSurface,
    pub damage_driven: bool,
    pub idle_redraws_allowed: bool,
    pub supports_gpu_timing: bool,
    pub note: String,
}

impl IosGpuSurfaceSpec {
    #[must_use]
    pub fn planned_native_metal(surface: IosRenderSurface) -> Self {
        Self {
            backend: IosRendererBackend::NativeMetal,
            surface,
            damage_driven: true,
            idle_redraws_allowed: false,
            supports_gpu_timing: false,
            note: "native iOS GPU surface must consume render-core scenes and preserve damage-driven redraws"
                .to_owned(),
        }
    }

    #[must_use]
    pub fn unavailable(surface: IosRenderSurface, reason: impl Into<String>) -> Self {
        Self {
            backend: IosRendererBackend::Unavailable,
            surface,
            damage_driven: false,
            idle_redraws_allowed: false,
            supports_gpu_timing: false,
            note: reason.into(),
        }
    }

    #[must_use]
    pub const fn is_release_ready(&self) -> bool {
        !matches!(self.backend, IosRendererBackend::Unavailable)
            && self.damage_driven
            && !self.idle_redraws_allowed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IosLifecycleState {
    Launching,
    ForegroundLive,
    Pausing,
    Disconnected,
    Reconnecting,
    Suspended,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundSessionPolicy {
    GracefulDisconnect,
    PauseAndReconnect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IosLifecyclePolicy {
    pub allow_indefinite_background_sessions: bool,
    pub background_policy: BackgroundSessionPolicy,
    pub quick_reconnect: bool,
    pub recommend_remote_persistence: bool,
    pub pause_timeout: Duration,
}

impl Default for IosLifecyclePolicy {
    fn default() -> Self {
        Self {
            allow_indefinite_background_sessions: false,
            background_policy: BackgroundSessionPolicy::GracefulDisconnect,
            quick_reconnect: true,
            recommend_remote_persistence: true,
            pause_timeout: Duration::from_secs(5),
        }
    }
}

/// Native UIKit/SwiftUI host boundary.
///
/// This trait is intentionally small: the native app owns lifecycle, gestures,
/// keyboard presentation, and modal UI, while the shared engine owns terminal
/// bytes, render scenes, SSH profile shape, and security policy.
pub trait IosAppShellBridge {
    fn lifecycle_state(&self) -> IosLifecycleState;

    fn present_host_key_decision(
        &mut self,
        request: HostKeyTrustRequest,
    ) -> Option<HostKeyTrustAction>;

    fn present_secret_prompt(&mut self, request: SecretRequest) -> Option<bool>;

    fn request_frame(&mut self, request: FrameRequest);

    fn show_diagnostic(&mut self, message: &str);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchPhase {
    Began,
    Moved,
    Ended,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchInput {
    pub id: u64,
    pub phase: TouchPhase,
    pub x_points: f32,
    pub y_points: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileKeyInput {
    pub text: Option<String>,
    pub key: Option<String>,
    pub command: bool,
    pub control: bool,
    pub option: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftwareKeyboardMode {
    Hidden,
    TextInput,
    ShortcutBar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareKeyboardBehavior {
    TerminalFirst,
    SystemShortcutsFirst,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IosInputPolicy {
    pub software_keyboard_mode: SoftwareKeyboardMode,
    pub hardware_keyboard_behavior: HardwareKeyboardBehavior,
    pub paste_requires_user_action: bool,
}

impl Default for IosInputPolicy {
    fn default() -> Self {
        Self {
            software_keyboard_mode: SoftwareKeyboardMode::TextInput,
            hardware_keyboard_behavior: HardwareKeyboardBehavior::TerminalFirst,
            paste_requires_user_action: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobileSecretStorageStatus {
    Required,
    Available,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IosSshUxPolicy {
    pub host_key_decision_required: bool,
    pub changed_host_key_blocks: bool,
    pub secure_storage: MobileSecretStorageStatus,
    pub supports_key_import: bool,
    pub supports_reconnect: bool,
}

impl Default for IosSshUxPolicy {
    fn default() -> Self {
        Self {
            host_key_decision_required: true,
            changed_host_key_blocks: true,
            secure_storage: MobileSecretStorageStatus::Required,
            supports_key_import: true,
            supports_reconnect: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IosSshProfileForm {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub auth_method: AuthMethod,
    pub identity_file: Option<PathBuf>,
    pub known_hosts_policy: KnownHostsPolicy,
    pub shell_integration: bool,
}

impl IosSshProfileForm {
    #[must_use]
    pub fn from_profile(profile: &SshProfile) -> Self {
        Self {
            name: profile.name.clone(),
            host: profile.host.clone(),
            port: profile.port,
            username: profile.username.clone(),
            auth_method: map_auth_method(profile.auth_method),
            identity_file: profile.identity_file.as_ref().map(PathBuf::from),
            known_hosts_policy: map_known_hosts_policy(&profile.known_hosts_policy),
            shell_integration: profile.shell_integration,
        }
    }

    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.name.trim().is_empty() {
            errors.push("SSH profile name is required".to_owned());
        }
        if self.host.trim().is_empty() {
            errors.push("SSH host is required".to_owned());
        }
        if self.port == 0 {
            errors.push("SSH port must be greater than zero".to_owned());
        }
        if self.auth_method == AuthMethod::PublicKey && self.identity_file.is_none() {
            errors.push("public-key SSH auth requires an identity file reference".to_owned());
        }
        errors
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IosTrustPromptModel {
    pub title: String,
    pub fingerprint: String,
    pub changed_key: bool,
    pub default_action: HostKeyTrustAction,
    pub destructive: bool,
}

impl IosTrustPromptModel {
    #[must_use]
    pub fn from_request(request: &HostKeyTrustRequest) -> Self {
        let changed_key = request.expected_fingerprint.is_some();
        Self {
            title: if changed_key {
                "SSH host key changed".to_owned()
            } else {
                "Trust SSH host?".to_owned()
            },
            fingerprint: request.key.sha256_fingerprint.clone(),
            changed_key,
            default_action: HostKeyTrustAction::Reject,
            destructive: changed_key,
        }
    }
}

#[must_use]
pub fn ios_keychain_capability() -> KeychainProviderCapability {
    PlatformKeychainProvider::for_platform(SecurityPlatform::Ios).capability()
}

#[must_use]
pub fn ios_keychain_entry_for_secret(request: &SecretRequest) -> KeychainEntry {
    request.keychain_entry()
}

#[derive(Debug, Clone, PartialEq)]
pub struct IosAppShellConfig {
    pub lifecycle: IosLifecyclePolicy,
    pub input: IosInputPolicy,
    pub ssh: IosSshUxPolicy,
    pub font: RuntimeFontConfig,
    pub visual_theme: VisualThemeConfig,
}

impl IosAppShellConfig {
    #[must_use]
    pub fn from_app_config(config: &AppConfig) -> Self {
        Self {
            lifecycle: IosLifecyclePolicy::default(),
            input: IosInputPolicy::default(),
            ssh: IosSshUxPolicy::default(),
            font: RuntimeFontConfig {
                family: config.font.family.clone(),
                fallback_families: config.font.fallback_families.clone(),
                size: config.font.size as f32,
                line_height: config.font.line_height as f32,
                ligatures: config.font.ligatures,
            },
            visual_theme: config.visual_theme.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IosSshSessionSpec {
    pub profile_name: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub auth_method: AuthMethod,
    pub identity_file: Option<PathBuf>,
    pub known_hosts_policy: KnownHostsPolicy,
    pub remote_command: Option<String>,
    pub remote_working_directory: Option<String>,
    pub shell_integration: bool,
    pub agent_forwarding: bool,
    pub proxy_jump: Option<String>,
    pub transport_kind: TransportKind,
}

impl IosSshSessionSpec {
    #[must_use]
    pub fn from_config_profile(profile: &SshProfile) -> Self {
        Self {
            profile_name: profile.name.clone(),
            host: profile.host.clone(),
            port: profile.port,
            username: profile.username.clone(),
            auth_method: map_auth_method(profile.auth_method),
            identity_file: profile.identity_file.as_ref().map(PathBuf::from),
            known_hosts_policy: map_known_hosts_policy(&profile.known_hosts_policy),
            remote_command: profile.remote_command.clone(),
            remote_working_directory: profile.remote_working_directory.clone(),
            shell_integration: profile.shell_integration,
            agent_forwarding: profile.agent_forwarding,
            proxy_jump: profile.proxy_jump.clone(),
            transport_kind: TransportKind::FutureMobileSsh,
        }
    }

    #[must_use]
    pub fn to_shared_ssh_profile(&self) -> SshConnectionProfile {
        SshConnectionProfile {
            name: self.profile_name.clone(),
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            auth_method: self.auth_method.clone(),
            identity_file: self.identity_file.clone(),
            known_hosts_policy: self.known_hosts_policy.clone(),
            remote_command: self.remote_command.clone(),
            remote_working_directory: self.remote_working_directory.clone(),
            shell_integration: self.shell_integration,
            agent_forwarding: self.agent_forwarding,
            proxy_jump: self.proxy_jump.clone(),
            connect_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IosEngine {
    terminal: TerminalEmulator,
    semantics: SemanticTimelineStore,
    scene: RenderScene,
    font: RuntimeFontConfig,
    visual_theme: VisualThemeConfig,
}

impl IosEngine {
    #[must_use]
    pub fn new(size: CoreTerminalSize, shell_config: IosAppShellConfig) -> Self {
        Self {
            terminal: TerminalEmulator::new(size),
            semantics: SemanticTimelineStore::new(),
            scene: RenderScene::default(),
            font: shell_config.font,
            visual_theme: shell_config.visual_theme,
        }
    }

    pub fn apply_remote_bytes(&mut self, bytes: &[u8]) -> TerminalResult<()> {
        self.terminal.apply_bytes(bytes)
    }

    pub fn resize(&mut self, size: CoreTerminalSize) -> TerminalResult<()> {
        self.terminal.resize(size)
    }

    #[must_use]
    pub fn visible_text(&self) -> String {
        self.terminal
            .visible_grid()
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    #[must_use]
    pub const fn semantics(&self) -> &SemanticTimelineStore {
        &self.semantics
    }

    #[must_use]
    pub const fn scene(&self) -> &RenderScene {
        &self.scene
    }

    #[must_use]
    pub const fn font(&self) -> &RuntimeFontConfig {
        &self.font
    }

    #[must_use]
    pub const fn visual_theme(&self) -> &VisualThemeConfig {
        &self.visual_theme
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IosConnectionPlanStatus {
    Ready,
    RequiresHostTrust,
    RequiresSecret,
    Blocked(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IosConnectionPlan {
    pub session: IosSshSessionSpec,
    pub status: IosConnectionPlanStatus,
    pub requires_keychain: bool,
    pub requires_native_ui: bool,
}

impl IosConnectionPlan {
    #[must_use]
    pub fn from_profile(profile: &SshProfile, keychain: KeychainProviderCapability) -> Self {
        let form = IosSshProfileForm::from_profile(profile);
        let validation = form.validate();
        let session = IosSshSessionSpec::from_config_profile(profile);
        if !validation.is_empty() {
            return Self {
                session,
                status: IosConnectionPlanStatus::Blocked(validation.join("; ")),
                requires_keychain: false,
                requires_native_ui: true,
            };
        }

        let requires_keychain = matches!(
            form.auth_method,
            AuthMethod::Password | AuthMethod::KeyboardInteractive | AuthMethod::PublicKey
        );
        let status = if requires_keychain && !keychain.secure_storage {
            IosConnectionPlanStatus::RequiresSecret
        } else if matches!(form.known_hosts_policy, KnownHostsPolicy::Ask) {
            IosConnectionPlanStatus::RequiresHostTrust
        } else {
            IosConnectionPlanStatus::Ready
        };

        Self {
            session,
            status,
            requires_keychain,
            requires_native_ui: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IosUserAction {
    ConnectSshProfile(String),
    Disconnect,
    QuickReconnect,
    AcceptHostKey {
        profile: String,
        fingerprint: String,
    },
    RejectHostKey {
        profile: String,
    },
    ImportKeyReference(PathBuf),
    Semantic(SemanticAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IosDeviceClass {
    Iphone,
    Ipad,
    Simulator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IosDeviceTestCategory {
    Ssh,
    Rendering,
    Keyboard,
    Touch,
    Lifecycle,
    Security,
    Semantics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IosDeviceTestCase {
    pub id: &'static str,
    pub category: IosDeviceTestCategory,
    pub required_device: Option<IosDeviceClass>,
    pub description: &'static str,
    pub expected: &'static str,
}

#[must_use]
pub fn ios_device_test_checklist() -> Vec<IosDeviceTestCase> {
    vec![
        IosDeviceTestCase {
            id: "ios-ssh-known-host",
            category: IosDeviceTestCategory::Ssh,
            required_device: None,
            description: "connect to a controlled SSH server after accepting the displayed host fingerprint",
            expected: "remote PTY opens and the trusted host key is persisted through Keychain-backed storage policy",
        },
        IosDeviceTestCase {
            id: "ios-ssh-changed-host-blocks",
            category: IosDeviceTestCategory::Security,
            required_device: None,
            description: "connect after replacing the server host key in the controlled test host",
            expected: "connection is blocked with a changed-key warning until the user explicitly resolves it",
        },
        IosDeviceTestCase {
            id: "ios-render-output",
            category: IosDeviceTestCategory::Rendering,
            required_device: None,
            description: "render ASCII, CJK, emoji, cursor, selection, and command-block fixtures",
            expected: "terminal output uses shared render-core scenes without idle redraw loops",
        },
        IosDeviceTestCase {
            id: "ios-software-keyboard-resize",
            category: IosDeviceTestCategory::Keyboard,
            required_device: Some(IosDeviceClass::Iphone),
            description: "show and hide the software keyboard during an active SSH session",
            expected: "remote PTY resizes from safe-area and keyboard-aware terminal dimensions",
        },
        IosDeviceTestCase {
            id: "ios-hardware-keyboard",
            category: IosDeviceTestCategory::Keyboard,
            required_device: Some(IosDeviceClass::Ipad),
            description: "type with a hardware keyboard, including control/meta-style terminal shortcuts",
            expected: "input is delivered to the SSH transport without blocking rendering",
        },
        IosDeviceTestCase {
            id: "ios-touch-selection",
            category: IosDeviceTestCategory::Touch,
            required_device: None,
            description: "drag to select mixed ASCII, CJK, and emoji terminal output",
            expected: "selection respects shared grapheme/cell boundaries and copies valid UTF-8",
        },
        IosDeviceTestCase {
            id: "ios-background-reconnect",
            category: IosDeviceTestCategory::Lifecycle,
            required_device: None,
            description: "send app to background past the pause timeout and return",
            expected: "session disconnect/reconnect is explicit; app does not promise indefinite background SSH",
        },
        IosDeviceTestCase {
            id: "ios-remote-semantics",
            category: IosDeviceTestCategory::Semantics,
            required_device: None,
            description: "run shell integration on the remote host and emit prompt/input/output markers",
            expected: "semantic command regions attach to shared terminal positions without rewriting terminal text",
        },
    ]
}

#[must_use]
pub fn frame_request_for_mobile_resize(surface: IosRenderSurface) -> FrameRequest {
    FrameRequest {
        reason: FrameRequestReason::WindowResized,
        damage: Some(surface.damage_for_full_surface()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IosReadinessItem {
    pub area: &'static str,
    pub ready: bool,
    pub note: &'static str,
}

#[must_use]
pub fn ios_foundation_readiness() -> Vec<IosReadinessItem> {
    vec![
        IosReadinessItem {
            area: "shared engine",
            ready: true,
            note: "mobile shell uses shared terminal/parser/semantic/render/config/transport contracts",
        },
        IosReadinessItem {
            area: "native app shell",
            ready: false,
            note: "Swift/UIKit or SwiftUI host is not implemented in this Rust workspace",
        },
        IosReadinessItem {
            area: "iOS render backend",
            ready: false,
            note: "render-core contract is shared; native iOS GPU surface is not implemented",
        },
        IosReadinessItem {
            area: "secure storage",
            ready: false,
            note: "iOS Keychain-backed SecretProvider is required before real mobile SSH release",
        },
        IosReadinessItem {
            area: "SSH lifecycle",
            ready: true,
            note: "mobile lifecycle policy avoids indefinite background session promises",
        },
        IosReadinessItem {
            area: "SSH profile UI",
            ready: false,
            note: "Rust-side profile form validation exists; native profile editing UI is not implemented",
        },
        IosReadinessItem {
            area: "host trust UI",
            ready: false,
            note: "trust prompt model defaults to reject; native approval UI is not implemented",
        },
        IosReadinessItem {
            area: "device validation checklist",
            ready: true,
            note: "required iPhone/iPad/simulator validation cases are enumerated for future runs",
        },
    ]
}

fn terminal_cells(points: f32, cell_points: f32) -> u16 {
    if !cell_points.is_finite() || cell_points <= 0.0 {
        return 1;
    }
    (points / cell_points)
        .floor()
        .clamp(1.0, f32::from(u16::MAX)) as u16
}

fn physical_pixels(points: f32, scale_factor: f32) -> u32 {
    let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    (points * scale).round().clamp(1.0, u32::MAX as f32) as u32
}

fn map_auth_method(method: SshAuthMethod) -> AuthMethod {
    match method {
        SshAuthMethod::Agent => AuthMethod::Agent,
        SshAuthMethod::PublicKey => AuthMethod::PublicKey,
        SshAuthMethod::Password => AuthMethod::Password,
        SshAuthMethod::KeyboardInteractive => AuthMethod::KeyboardInteractive,
        SshAuthMethod::None => AuthMethod::None,
    }
}

fn map_known_hosts_policy(policy: &SshKnownHostsPolicy) -> KnownHostsPolicy {
    match policy {
        SshKnownHostsPolicy::Ask => KnownHostsPolicy::Ask,
        SshKnownHostsPolicy::RequireKnown => KnownHostsPolicy::RequireKnown,
        SshKnownHostsPolicy::TrustOnFirstUse => KnownHostsPolicy::TrustOnFirstUse,
        SshKnownHostsPolicy::PinFingerprint { sha256 } => KnownHostsPolicy::PinFingerprint {
            sha256: sha256.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_engine_list_covers_phase_22_components() {
        let components = shared_engine_components();

        for expected in [
            "term-core",
            "term-parser",
            "semantics",
            "render-core",
            "font-system",
            "config-core",
            "transport-core",
            "transport-ssh",
            "visual theme model",
        ] {
            assert!(
                components
                    .iter()
                    .any(|component| component.crate_name == expected),
                "{expected} should be represented in mobile shared-engine inventory"
            );
        }
    }

    #[test]
    fn mobile_surface_accounts_for_safe_area_and_keyboard() {
        let surface = IosRenderSurface {
            width_points: 390.0,
            height_points: 844.0,
            scale_factor: 3.0,
            safe_area: SafeAreaInsets {
                top: 47.0,
                right: 0.0,
                bottom: 34.0,
                left: 0.0,
            },
            keyboard_height_points: 300.0,
        };
        let size = surface.terminal_size(CellMetrics {
            font_size: 13.0,
            cell_width: 8.0,
            cell_height: 16.0,
            ascent: 10.0,
            descent: -3.0,
            line_gap: 2.0,
            baseline: 11.5,
            underline_position: 14.0,
            strikethrough_position: 7.0,
            decoration_thickness: 1.0,
        });

        assert_eq!(size.cols, 48);
        assert_eq!(size.rows, 28);
        assert_eq!(size.pixel_width, 1170);
        assert_eq!(size.pixel_height, 1389);
    }

    #[test]
    fn ios_engine_uses_shared_terminal_parser() {
        let config = IosAppShellConfig::from_app_config(&AppConfig::default());
        let mut engine = IosEngine::new(CoreTerminalSize::new(20, 4), config);

        engine
            .apply_remote_bytes(b"panea-mobile\r\n")
            .expect("terminal bytes should apply");

        assert!(engine.visible_text().contains("panea-mobile"));
    }

    #[test]
    fn ssh_profile_conversion_preserves_security_policy() {
        let profile = SshProfile {
            name: "prod".to_owned(),
            host: "example.com".to_owned(),
            username: Some("deploy".to_owned()),
            auth_method: SshAuthMethod::PublicKey,
            identity_file: Some("id_ed25519".to_owned()),
            known_hosts_policy: SshKnownHostsPolicy::PinFingerprint {
                sha256: "SHA256:abc".to_owned(),
            },
            ..SshProfile::default()
        };

        let spec = IosSshSessionSpec::from_config_profile(&profile);
        let shared = spec.to_shared_ssh_profile();

        assert_eq!(spec.transport_kind, TransportKind::FutureMobileSsh);
        assert_eq!(shared.host, "example.com");
        assert_eq!(shared.username.as_deref(), Some("deploy"));
        assert_eq!(shared.auth_method, AuthMethod::PublicKey);
        assert_eq!(
            shared.known_hosts_policy,
            KnownHostsPolicy::PinFingerprint {
                sha256: "SHA256:abc".to_owned()
            }
        );
    }

    #[test]
    fn ssh_profile_form_rejects_missing_public_key_identity() {
        let profile = SshProfile {
            name: "prod".to_owned(),
            host: "example.com".to_owned(),
            auth_method: SshAuthMethod::PublicKey,
            identity_file: None,
            ..SshProfile::default()
        };

        let form = IosSshProfileForm::from_profile(&profile);

        assert!(
            form.validate()
                .iter()
                .any(|error| error.contains("identity file"))
        );
    }

    #[test]
    fn ios_connection_plan_requires_secure_secret_ui_for_password_auth() {
        let profile = SshProfile {
            name: "prod".to_owned(),
            host: "example.com".to_owned(),
            auth_method: SshAuthMethod::Password,
            ..SshProfile::default()
        };

        let plan = IosConnectionPlan::from_profile(&profile, ios_keychain_capability());

        assert!(plan.requires_keychain);
        assert_eq!(plan.status, IosConnectionPlanStatus::RequiresSecret);
    }

    #[test]
    fn trust_prompt_defaults_to_reject_and_flags_changed_keys() {
        let key = security::HostKey::from_raw("example.com", 22, "ssh-ed25519", b"new-key");
        let request = HostKeyTrustRequest::changed(key, "SHA256:old");

        let prompt = IosTrustPromptModel::from_request(&request);

        assert_eq!(prompt.default_action, HostKeyTrustAction::Reject);
        assert!(prompt.changed_key);
        assert!(prompt.destructive);
    }

    #[test]
    fn ios_keychain_capability_points_at_ios_keychain_but_is_not_native_yet() {
        let capability = ios_keychain_capability();

        assert_eq!(capability.platform, SecurityPlatform::Ios);
        assert_eq!(capability.backend, security::KeychainBackend::IosKeychain);
        assert!(!capability.available);
        assert!(!capability.secure_storage);
    }

    #[test]
    fn gpu_surface_spec_requires_damage_driven_rendering() {
        let surface = IosRenderSurface {
            width_points: 1024.0,
            height_points: 768.0,
            scale_factor: 2.0,
            safe_area: SafeAreaInsets::default(),
            keyboard_height_points: 0.0,
        };

        let spec = IosGpuSurfaceSpec::planned_native_metal(surface);

        assert_eq!(spec.backend, IosRendererBackend::NativeMetal);
        assert!(spec.damage_driven);
        assert!(!spec.idle_redraws_allowed);
        assert!(spec.is_release_ready());
    }

    #[test]
    fn device_checklist_covers_required_mobile_validation_surfaces() {
        let checklist = ios_device_test_checklist();

        for category in [
            IosDeviceTestCategory::Ssh,
            IosDeviceTestCategory::Rendering,
            IosDeviceTestCategory::Keyboard,
            IosDeviceTestCategory::Touch,
            IosDeviceTestCategory::Lifecycle,
            IosDeviceTestCategory::Security,
            IosDeviceTestCategory::Semantics,
        ] {
            assert!(
                checklist.iter().any(|case| case.category == category),
                "{category:?} should be covered by the iOS device checklist"
            );
        }
    }

    #[test]
    fn lifecycle_policy_does_not_promise_background_ssh() {
        let policy = IosLifecyclePolicy::default();

        assert!(!policy.allow_indefinite_background_sessions);
        assert_eq!(
            policy.background_policy,
            BackgroundSessionPolicy::GracefulDisconnect
        );
        assert!(policy.recommend_remote_persistence);
    }

    #[test]
    fn ios_crate_does_not_import_desktop_or_local_pty_layers() {
        let manifest = include_str!("../Cargo.toml");

        assert!(!manifest.contains("platform-winit"));
        assert!(!manifest.contains("render-wgpu"));
        assert!(!manifest.contains("transport-pty"));
        assert!(!manifest.contains("apps/desktop"));
    }
}
