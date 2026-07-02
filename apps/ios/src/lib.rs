//! iOS SSH companion shell contracts.

pub const LAYER: &str = "platform parity";

use std::{path::PathBuf, time::Duration};

use config_core::{AppConfig, SshAuthMethod, SshKnownHostsPolicy, SshProfile, VisualThemeConfig};
use font_system::{CellMetrics, FontConfig as RuntimeFontConfig};
use render_core::{DamageRegion, FrameRequest, FrameRequestReason, RenderScene};
use security::{AuthMethod, KnownHostsPolicy};
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
    fn shared_engine_list_covers_phase_15_components() {
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
