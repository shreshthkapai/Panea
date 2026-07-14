//! Diagnostics, capability reporting, and performance reporting boundaries.

pub const LAYER: &str = "diagnostics";

use std::{collections::VecDeque, fmt, time::Duration};

use config_core::{
    AppConfig, ConfigDiagnostic, ConfigDiagnosticSeverity, ConfigPlatform,
    DecorationStrategyConfig, SshAuthMethod, SshKnownHostsPolicy, WindowModeConfig,
};
use platform_core::{DesktopPlatform, DpiBehavior};
use render_core::{
    FeatureCostSample, GpuTimingStatus, OptionalFeatureCostMode, RenderInstrumentation,
};
use semantics::{CommandBlockConfidence, IntegrationMode, SemanticDiagnostics, SemanticEventKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportLevel {
    Full,
    Partial,
    Fallback,
    UnsupportedByPlatform,
    NotImplementedYet,
}

impl fmt::Display for SupportLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Full => "full",
            Self::Partial => "partial",
            Self::Fallback => "fallback",
            Self::UnsupportedByPlatform => "unsupported by platform",
            Self::NotImplementedYet => "not implemented yet",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformFeatureStatus {
    pub feature: &'static str,
    pub macos: SupportLevel,
    pub windows: SupportLevel,
    pub linux_x11: SupportLevel,
    pub linux_wayland: SupportLevel,
    pub notes: &'static str,
}

#[must_use]
pub fn feature_parity_matrix() -> Vec<PlatformFeatureStatus> {
    use SupportLevel::{Fallback, Full, NotImplementedYet, Partial};

    vec![
        PlatformFeatureStatus {
            feature: "window modes",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "windowed, maximized, borderless fullscreen, and frameless states are modeled; real compositor validation remains open",
        },
        PlatformFeatureStatus {
            feature: "frameless modes",
            macos: Partial,
            windows: Partial,
            linux_x11: Fallback,
            linux_wayland: Fallback,
            notes: "implemented through winit decorations with Linux decoration negotiation still requiring compositor tests",
        },
        PlatformFeatureStatus {
            feature: "fullscreen modes",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Fallback,
            notes: "exclusive fullscreen currently falls back to borderless fullscreen",
        },
        PlatformFeatureStatus {
            feature: "clipboard",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "system clipboard bridge, paste protection, and OSC 52 policy exist; primary selection and cross-OS smoke remain open",
        },
        PlatformFeatureStatus {
            feature: "IME",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "platform-neutral IME events are represented; real composed-input validation is still required",
        },
        PlatformFeatureStatus {
            feature: "DPI/fractional scaling",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "monitor scale snapshots exist; fractional behavior needs real host verification",
        },
        PlatformFeatureStatus {
            feature: "font fallback",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "OpenType shaping, per-grapheme fallback, real style faces, color emoji, and source diagnostics exist; non-Windows installed-font and screenshot reports remain open",
        },
        PlatformFeatureStatus {
            feature: "GPU backend",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "wgpu surface/device path exists; GPU backend inventory and screenshot verification remain open",
        },
        PlatformFeatureStatus {
            feature: "local PTY",
            macos: Partial,
            windows: Full,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "Windows real-shell smoke passed on current host; macOS/Linux real PTY smoke remains unverified",
        },
        PlatformFeatureStatus {
            feature: "PowerShell/cmd/WSL",
            macos: NotImplementedYet,
            windows: Partial,
            linux_x11: NotImplementedYet,
            linux_wayland: NotImplementedYet,
            notes: "Windows shell profile groundwork exists; WSL runtime smoke is not verified",
        },
        PlatformFeatureStatus {
            feature: "shell integration",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "semantic parsers and scripts exist; desktop startup activation and real shell validation remain open",
        },
        PlatformFeatureStatus {
            feature: "tabs/panes",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "mux state model exists; full desktop multi-pane runtime is deferred",
        },
        PlatformFeatureStatus {
            feature: "command blocks",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "semantic storage and basic overlays exist; real shell-driven UI verification remains open",
        },
        PlatformFeatureStatus {
            feature: "cursor animations",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "opt-in cursor animations, bounded image decode/cache/upload, and bounded data-only static vector cursor batching exist; cross-OS visual verification remains open",
        },
        PlatformFeatureStatus {
            feature: "SSH",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "secure transport backend and real-server smoke harness exist; interactive trust UI and collected server reports remain open",
        },
        PlatformFeatureStatus {
            feature: "config reload",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "debounced TOML watcher and live applier exist; Windows tests pass, while macOS/Linux runtime validation remains open",
        },
        PlatformFeatureStatus {
            feature: "notifications",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "native backends and bounded background delivery exist; real-host permission and delivery verification remains open",
        },
        PlatformFeatureStatus {
            feature: "OSC clipboard",
            macos: Partial,
            windows: Partial,
            linux_x11: Partial,
            linux_wayland: Partial,
            notes: "OSC 52 parser, bounded policy, and remote one-time confirmation overlay exist; real app/platform smoke remains open",
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxDisplayServer {
    X11,
    Wayland,
}

impl fmt::Display for LinuxDisplayServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::X11 => "X11",
            Self::Wayland => "Wayland",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxCompositorTarget {
    pub key: &'static str,
    pub display_server: LinuxDisplayServer,
    pub desktop_or_compositor: &'static str,
    pub required: bool,
    pub notes: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxWindowVerificationFeature {
    pub feature: &'static str,
    pub fallback_policy: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxCompositorRuntimeSnapshot {
    pub os: String,
    pub detected_backend: Option<DesktopPlatform>,
    pub xdg_session_type: Option<String>,
    pub xdg_current_desktop: Option<String>,
    pub desktop_session: Option<String>,
    pub wayland_display: Option<String>,
    pub display: Option<String>,
    pub winit_unix_backend: Option<String>,
    pub warnings: Vec<String>,
}

impl LinuxCompositorRuntimeSnapshot {
    #[must_use]
    pub fn detect() -> Self {
        Self::from_env(
            std::env::consts::OS,
            std::env::var("XDG_SESSION_TYPE").ok(),
            std::env::var("XDG_CURRENT_DESKTOP").ok(),
            std::env::var("DESKTOP_SESSION").ok(),
            std::env::var("WAYLAND_DISPLAY").ok(),
            std::env::var("DISPLAY").ok(),
            std::env::var("WINIT_UNIX_BACKEND").ok(),
        )
    }

    #[must_use]
    pub fn from_env(
        os: impl Into<String>,
        xdg_session_type: Option<String>,
        xdg_current_desktop: Option<String>,
        desktop_session: Option<String>,
        wayland_display: Option<String>,
        display: Option<String>,
        winit_unix_backend: Option<String>,
    ) -> Self {
        let os = os.into();
        let detected_backend = if os != "linux" {
            None
        } else if xdg_session_type
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("wayland"))
            || wayland_display.is_some()
        {
            Some(DesktopPlatform::LinuxWayland)
        } else if xdg_session_type
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("x11"))
            || display.is_some()
        {
            Some(DesktopPlatform::LinuxX11)
        } else {
            Some(DesktopPlatform::Unknown)
        };

        let mut warnings = Vec::new();
        if os != "linux" {
            warnings.push(
                "current host is not Linux; compositor behavior cannot be verified here".to_owned(),
            );
        } else if detected_backend == Some(DesktopPlatform::Unknown) {
            warnings.push(
                "Linux display backend could not be determined from XDG_SESSION_TYPE, WAYLAND_DISPLAY, or DISPLAY"
                    .to_owned(),
            );
        }

        if os == "linux" && xdg_current_desktop.is_none() && desktop_session.is_none() {
            warnings.push(
                "desktop/compositor name is unknown; report XDG_CURRENT_DESKTOP or DESKTOP_SESSION in manual verification"
                    .to_owned(),
            );
        }

        Self {
            os,
            detected_backend,
            xdg_session_type,
            xdg_current_desktop,
            desktop_session,
            wayland_display,
            display,
            winit_unix_backend,
            warnings,
        }
    }

    #[must_use]
    pub fn compositor_label(&self) -> String {
        self.xdg_current_desktop
            .as_deref()
            .or(self.desktop_session.as_deref())
            .unwrap_or("unknown")
            .to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxCompositorVerificationReport {
    pub runtime: LinuxCompositorRuntimeSnapshot,
    pub targets: Vec<LinuxCompositorTarget>,
    pub features: Vec<LinuxWindowVerificationFeature>,
}

impl LinuxCompositorVerificationReport {
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut lines = vec!["Panea Linux compositor verification".to_owned()];
        lines.extend([
            format!("os={}", self.runtime.os),
            format!(
                "detected_backend={}",
                self.runtime
                    .detected_backend
                    .map_or_else(|| "n/a".to_owned(), |backend| format!("{backend:?}"))
            ),
            format!(
                "xdg_session_type={}",
                self.runtime
                    .xdg_session_type
                    .as_deref()
                    .unwrap_or("unknown")
            ),
            format!("compositor={}", self.runtime.compositor_label()),
            format!(
                "wayland_display={}",
                self.runtime.wayland_display.as_deref().unwrap_or("unset")
            ),
            format!(
                "display={}",
                self.runtime.display.as_deref().unwrap_or("unset")
            ),
            format!(
                "winit_unix_backend={}",
                self.runtime
                    .winit_unix_backend
                    .as_deref()
                    .unwrap_or("unset")
            ),
        ]);

        if !self.runtime.warnings.is_empty() {
            lines.push("warnings:".to_owned());
            lines.extend(
                self.runtime
                    .warnings
                    .iter()
                    .map(|warning| format!("- {warning}")),
            );
        }

        lines.push("required targets:".to_owned());
        lines.extend(self.targets.iter().map(|target| {
            format!(
                "- [{}] {} {}: {}",
                if target.required {
                    "required"
                } else {
                    "optional"
                },
                target.display_server,
                target.desktop_or_compositor,
                target.notes
            )
        }));

        lines.push("features to verify:".to_owned());
        lines.extend(
            self.features
                .iter()
                .map(|feature| format!("- {}: {}", feature.feature, feature.fallback_policy)),
        );

        lines.join("\n")
    }
}

#[must_use]
pub fn linux_compositor_targets() -> Vec<LinuxCompositorTarget> {
    vec![
        LinuxCompositorTarget {
            key: "gnome-xorg",
            display_server: LinuxDisplayServer::X11,
            desktop_or_compositor: "GNOME Xorg",
            required: true,
            notes: "validate decorated window, fullscreen, DPI, clipboard, keyboard, mouse, and IME/dead-key behavior",
        },
        LinuxCompositorTarget {
            key: "kde-x11",
            display_server: LinuxDisplayServer::X11,
            desktop_or_compositor: "KDE X11",
            required: true,
            notes: "validate KWin X11 decorations, fullscreen, scaling, and input behavior",
        },
        LinuxCompositorTarget {
            key: "xfce",
            display_server: LinuxDisplayServer::X11,
            desktop_or_compositor: "XFCE",
            required: true,
            notes: "validate lightweight desktop behavior and clipboard availability",
        },
        LinuxCompositorTarget {
            key: "i3",
            display_server: LinuxDisplayServer::X11,
            desktop_or_compositor: "i3",
            required: true,
            notes: "validate tiling WM resize/fullscreen behavior and decoration fallback",
        },
        LinuxCompositorTarget {
            key: "openbox",
            display_server: LinuxDisplayServer::X11,
            desktop_or_compositor: "Openbox or similar",
            required: true,
            notes: "validate lightweight floating WM behavior and fallback diagnostics",
        },
        LinuxCompositorTarget {
            key: "gnome-wayland",
            display_server: LinuxDisplayServer::Wayland,
            desktop_or_compositor: "GNOME/Mutter",
            required: true,
            notes: "validate Wayland decorations, fullscreen behavior, fractional scaling, clipboard, and IME",
        },
        LinuxCompositorTarget {
            key: "kde-wayland",
            display_server: LinuxDisplayServer::Wayland,
            desktop_or_compositor: "KDE/KWin",
            required: true,
            notes: "validate decoration negotiation, fractional scaling, fullscreen, and input behavior",
        },
        LinuxCompositorTarget {
            key: "sway",
            display_server: LinuxDisplayServer::Wayland,
            desktop_or_compositor: "Sway/wlroots",
            required: true,
            notes: "validate wlroots behavior, server/client decorations, clipboard, and tiling resize",
        },
        LinuxCompositorTarget {
            key: "hyprland",
            display_server: LinuxDisplayServer::Wayland,
            desktop_or_compositor: "Hyprland",
            required: true,
            notes: "validate compositor-specific fullscreen, decorations, scaling, and input quirks",
        },
        LinuxCompositorTarget {
            key: "cosmic",
            display_server: LinuxDisplayServer::Wayland,
            desktop_or_compositor: "COSMIC",
            required: false,
            notes: "verify when available; absence must be recorded rather than treated as a pass",
        },
    ]
}

#[must_use]
pub fn linux_window_verification_features() -> Vec<LinuxWindowVerificationFeature> {
    vec![
        LinuxWindowVerificationFeature {
            feature: "window creation",
            fallback_policy: "failure is blocking; report backend, compositor, and window creation error",
        },
        LinuxWindowVerificationFeature {
            feature: "resize",
            fallback_policy: "resize events must update logical and physical dimensions without panics",
        },
        LinuxWindowVerificationFeature {
            feature: "DPI/fractional scaling",
            fallback_policy: "report scale factor and compositor when scaling differs from request",
        },
        LinuxWindowVerificationFeature {
            feature: "clipboard",
            fallback_policy: "report unavailable clipboard provider instead of silently dropping copy/paste",
        },
        LinuxWindowVerificationFeature {
            feature: "fullscreen",
            fallback_policy: "report requested/effective mode when exclusive fullscreen falls back",
        },
        LinuxWindowVerificationFeature {
            feature: "borderless fullscreen",
            fallback_policy: "report compositor-specific behavior and monitor selection",
        },
        LinuxWindowVerificationFeature {
            feature: "frameless window mode",
            fallback_policy: "fall back to decorated window if compositor blocks decoration removal",
        },
        LinuxWindowVerificationFeature {
            feature: "custom titlebar mode",
            fallback_policy: "fall back to native/fallback decorated mode until custom drag regions are verified",
        },
        LinuxWindowVerificationFeature {
            feature: "decorations fallback",
            fallback_policy: "always report requested and effective decoration strategies",
        },
        LinuxWindowVerificationFeature {
            feature: "keyboard input",
            fallback_policy: "record layout/modifier issues, especially AltGr and compositor shortcuts",
        },
        LinuxWindowVerificationFeature {
            feature: "mouse input",
            fallback_policy: "record button, wheel, motion, drag, and focus behavior per compositor",
        },
        LinuxWindowVerificationFeature {
            feature: "IME/dead keys",
            fallback_policy: "mark unsupported or partial when composed input cannot be verified",
        },
    ]
}

#[must_use]
pub fn linux_compositor_verification_report() -> LinuxCompositorVerificationReport {
    LinuxCompositorVerificationReport {
        runtime: LinuxCompositorRuntimeSnapshot::detect(),
        targets: linux_compositor_targets(),
        features: linux_window_verification_features(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorTopic {
    All,
    Renderer,
    Config,
    Platform,
    ShellIntegration,
    Performance,
    Ssh,
    Window,
    Fonts,
    Clipboard,
    Notifications,
}

impl DoctorTopic {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "renderer" => Some(Self::Renderer),
            "config" => Some(Self::Config),
            "platform" => Some(Self::Platform),
            "shell-integration" | "shell_integration" | "shell" => Some(Self::ShellIntegration),
            "performance" => Some(Self::Performance),
            "ssh" => Some(Self::Ssh),
            "window" => Some(Self::Window),
            "fonts" | "font" => Some(Self::Fonts),
            "clipboard" => Some(Self::Clipboard),
            "notifications" | "notification" => Some(Self::Notifications),
            _ => None,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::All => "doctor",
            Self::Renderer => "doctor renderer",
            Self::Config => "doctor config",
            Self::Platform => "doctor platform",
            Self::ShellIntegration => "doctor shell-integration",
            Self::Performance => "doctor performance",
            Self::Ssh => "doctor ssh",
            Self::Window => "doctor window",
            Self::Fonts => "doctor fonts",
            Self::Clipboard => "doctor clipboard",
            Self::Notifications => "doctor notifications",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformSnapshot {
    pub platform: ConfigPlatform,
    pub os: String,
    pub arch: String,
    pub linux_backend: Option<DesktopPlatform>,
    pub compositor_or_desktop: Option<String>,
    pub dpi_behavior: DpiBehavior,
    pub known_fallbacks: Vec<String>,
}

impl PlatformSnapshot {
    #[must_use]
    pub fn detect() -> Self {
        let platform = ConfigPlatform::current();
        let linux_backend = match platform {
            ConfigPlatform::LinuxX11 => Some(DesktopPlatform::LinuxX11),
            ConfigPlatform::LinuxWayland => Some(DesktopPlatform::LinuxWayland),
            ConfigPlatform::Linux => Some(DesktopPlatform::Unknown),
            ConfigPlatform::MacOs => Some(DesktopPlatform::MacOs),
            ConfigPlatform::Windows => Some(DesktopPlatform::Windows),
            ConfigPlatform::Unknown => None,
        };
        let compositor_or_desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .or_else(|_| std::env::var("DESKTOP_SESSION"))
            .or_else(|_| std::env::var("WAYLAND_DISPLAY"))
            .or_else(|_| std::env::var("DISPLAY"))
            .ok();
        let dpi_behavior = match platform {
            ConfigPlatform::Windows => DpiBehavior::PerMonitor,
            ConfigPlatform::MacOs | ConfigPlatform::LinuxX11 | ConfigPlatform::LinuxWayland => {
                DpiBehavior::FractionalScale
            }
            ConfigPlatform::Linux | ConfigPlatform::Unknown => DpiBehavior::Unknown,
        };
        let mut known_fallbacks = Vec::new();
        if matches!(platform, ConfigPlatform::Linux | ConfigPlatform::Unknown) {
            known_fallbacks.push(
                "Linux display backend could not be distinguished from environment".to_owned(),
            );
        }

        Self {
            platform,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            linux_backend,
            compositor_or_desktop,
            dpi_behavior,
            known_fallbacks,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DoctorInput {
    pub app_version: String,
    pub config_source: String,
    pub config: AppConfig,
    pub config_diagnostics: Vec<ConfigDiagnostic>,
    pub platform: PlatformSnapshot,
    pub runtime: DoctorRuntimeSnapshot,
    pub recent_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorRuntimeSnapshot {
    pub renderer_backend: String,
    pub gpu_adapter: Option<String>,
    pub gpu_features: Vec<String>,
    pub window_backend: Option<String>,
    pub x11_wayland_status: Option<String>,
    pub dpi_scale: Option<String>,
    pub font_discovery: String,
    pub config_parse_status: String,
    pub shell_integration_status: String,
    pub performance_overlay_status: String,
    pub clipboard_provider: String,
    pub notification_provider: String,
    pub keychain_provider: String,
    pub pty_backend: String,
    pub ssh_provider_status: String,
}

impl Default for DoctorRuntimeSnapshot {
    fn default() -> Self {
        Self {
            renderer_backend: "not probed".to_owned(),
            gpu_adapter: None,
            gpu_features: Vec::new(),
            window_backend: None,
            x11_wayland_status: None,
            dpi_scale: None,
            font_discovery: "not probed".to_owned(),
            config_parse_status: "loaded".to_owned(),
            shell_integration_status: "runtime session not active during doctor".to_owned(),
            performance_overlay_status: "disabled; config defaults active".to_owned(),
            clipboard_provider: "not probed".to_owned(),
            notification_provider: "not probed".to_owned(),
            keychain_provider: "not probed".to_owned(),
            pty_backend: "not probed".to_owned(),
            ssh_provider_status: "not probed".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorSeverity {
    Info,
    Warning,
    Error,
}

impl fmt::Display for DoctorSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorFinding {
    pub severity: DoctorSeverity,
    pub area: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub topic: DoctorTopic,
    pub lines: Vec<String>,
    pub findings: Vec<DoctorFinding>,
}

impl DoctorReport {
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut output = vec![format!("Panea {}", self.topic.name())];
        output.extend(self.lines.iter().cloned());
        if !self.findings.is_empty() {
            output.push("findings:".to_owned());
            output.extend(self.findings.iter().map(|finding| {
                format!(
                    "- [{}] {}: {}",
                    finding.severity, finding.area, finding.message
                )
            }));
        }
        output.join("\n")
    }

    #[must_use]
    pub fn render_json(&self) -> String {
        let lines = self
            .lines
            .iter()
            .map(|line| format!("\"{}\"", json_escape(line)))
            .collect::<Vec<_>>()
            .join(",");
        let findings = self
            .findings
            .iter()
            .map(|finding| {
                format!(
                    "{{\"severity\":\"{}\",\"area\":\"{}\",\"message\":\"{}\"}}",
                    finding.severity,
                    json_escape(finding.area),
                    json_escape(&finding.message)
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        format!(
            "{{\"name\":\"{}\",\"topic\":\"{}\",\"lines\":[{}],\"findings\":[{}]}}",
            json_escape(self.topic.name()),
            json_escape(doctor_topic_key(self.topic)),
            lines,
            findings
        )
    }
}

#[must_use]
pub fn doctor_report(input: &DoctorInput, topic: DoctorTopic) -> DoctorReport {
    let mut report = DoctorReport {
        topic,
        lines: vec![format!("version: {}", input.app_version)],
        findings: Vec::new(),
    };

    if matches!(topic, DoctorTopic::All | DoctorTopic::Platform) {
        append_platform_report(input, &mut report);
    }
    if matches!(topic, DoctorTopic::All | DoctorTopic::Window) {
        append_window_report(input, &mut report);
    }
    if matches!(topic, DoctorTopic::All | DoctorTopic::Renderer) {
        append_renderer_report(input, &mut report);
    }
    if matches!(topic, DoctorTopic::All | DoctorTopic::Fonts) {
        append_fonts_report(input, &mut report);
    }
    if matches!(topic, DoctorTopic::All | DoctorTopic::Config) {
        append_config_report(input, &mut report);
    }
    if matches!(topic, DoctorTopic::All | DoctorTopic::ShellIntegration) {
        append_shell_integration_report(input, &mut report);
    }
    if matches!(topic, DoctorTopic::All | DoctorTopic::Clipboard) {
        append_clipboard_report(input, &mut report);
    }
    if matches!(topic, DoctorTopic::All | DoctorTopic::Notifications) {
        append_notification_report(input, &mut report);
    }
    if matches!(topic, DoctorTopic::All | DoctorTopic::Performance) {
        append_performance_report(input, &mut report);
    }
    if matches!(topic, DoctorTopic::All | DoctorTopic::Ssh) {
        append_ssh_report(input, &mut report);
    }

    for error in &input.recent_errors {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Warning,
            area: "recent_errors",
            message: error.clone(),
        });
    }

    report
}

/// Diagnostics boundary for app code and future installed `terminal doctor`.
pub trait DiagnosticsProvider {
    fn doctor_report(&self, topic: DoctorTopic) -> DoctorReport;

    fn bug_report_snapshot(&self) -> BugReportSnapshot;
}

#[derive(Debug, Clone)]
pub struct StaticDiagnosticsProvider {
    input: DoctorInput,
}

impl StaticDiagnosticsProvider {
    #[must_use]
    pub fn new(input: DoctorInput) -> Self {
        Self { input }
    }
}

impl DiagnosticsProvider for StaticDiagnosticsProvider {
    fn doctor_report(&self, topic: DoctorTopic) -> DoctorReport {
        doctor_report(&self.input, topic)
    }

    fn bug_report_snapshot(&self) -> BugReportSnapshot {
        BugReportSnapshot::from_doctor_input(&self.input)
    }
}

fn append_platform_report(input: &DoctorInput, report: &mut DoctorReport) {
    report.lines.extend([
        "platform:".to_owned(),
        format!(
            "  os={} arch={} config_platform={:?}",
            input.platform.os, input.platform.arch, input.platform.platform
        ),
        format!(
            "  backend={}",
            input
                .platform
                .linux_backend
                .map_or_else(|| "n/a".to_owned(), |backend| format!("{backend:?}"))
        ),
        format!(
            "  compositor_or_desktop={}",
            input
                .platform
                .compositor_or_desktop
                .as_deref()
                .unwrap_or("unknown")
        ),
        format!("  dpi_behavior={:?}", input.platform.dpi_behavior),
        format!(
            "  x11_wayland_status={}",
            input.runtime.x11_wayland_status.as_deref().unwrap_or("n/a")
        ),
        format!(
            "  dpi_scale={}",
            input.runtime.dpi_scale.as_deref().unwrap_or("not measured")
        ),
    ]);

    for fallback in &input.platform.known_fallbacks {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Warning,
            area: "platform",
            message: fallback.clone(),
        });
    }
}

fn append_window_report(input: &DoctorInput, report: &mut DoctorReport) {
    report.lines.extend([
        "window:".to_owned(),
        format!(
            "  size={}x{} cells={}x{}",
            input.config.window.initial_width,
            input.config.window.initial_height,
            input.config.window.columns,
            input.config.window.rows
        ),
        format!(
            "  mode={:?} linux_backend={:?} decoration_strategy={:?}",
            input.config.window.mode,
            input.config.window.linux_backend,
            input.config.window.decoration_strategy
        ),
        format!(
            "  fullscreen_titlebar enabled={} height={} reveal_height={} controls={}",
            input.config.window.fullscreen_titlebar.enabled,
            input.config.window.fullscreen_titlebar.height,
            input.config.window.fullscreen_titlebar.reveal_height,
            input.config.window.fullscreen_titlebar.show_window_controls
        ),
        format!(
            "  provider_backend={}",
            input
                .runtime
                .window_backend
                .as_deref()
                .unwrap_or("not initialized by doctor")
        ),
    ]);

    if matches!(
        input.config.window.mode,
        WindowModeConfig::FramelessWindowed | WindowModeConfig::FramelessFullscreen
    ) {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Info,
            area: "window",
            message: "frameless recovery depends on restore_window_decorations keybinding"
                .to_owned(),
        });
    }
    if matches!(
        input.config.window.decoration_strategy,
        DecorationStrategyConfig::ClientSide
            | DecorationStrategyConfig::Custom
            | DecorationStrategyConfig::FallbackDecorated
    ) {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Info,
            area: "window",
            message: "the requested decoration strategy is capability-resolved at window startup; unsupported exact behavior falls back to native decorated mode"
                .to_owned(),
        });
    }
    if matches!(input.config.window.mode, WindowModeConfig::Fullscreen) {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Info,
            area: "window",
            message: "exclusive fullscreen uses the active monitor video modes and falls back to borderless fullscreen when none is exposed"
                .to_owned(),
        });
    }
    if input.config.window.fullscreen_titlebar.enabled
        && !matches!(
            input.config.window.mode,
            WindowModeConfig::BorderlessFullscreen | WindowModeConfig::FramelessFullscreen
        )
    {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Info,
            area: "window",
            message: "auto-hidden fullscreen titlebar is configured but activates only in borderless_fullscreen or frameless_fullscreen mode"
                .to_owned(),
        });
    }
}

fn append_renderer_report(input: &DoctorInput, report: &mut DoctorReport) {
    report.lines.extend([
        "renderer:".to_owned(),
        format!(
            "  backend_preference={:?} present_mode={:?} damage_tracking={}",
            input.config.renderer.backend,
            input.config.renderer.present_mode,
            input.config.renderer.damage_tracking
        ),
        format!("  gpu_backend={}", input.runtime.renderer_backend),
        format!(
            "  gpu_adapter={}",
            input
                .runtime
                .gpu_adapter
                .as_deref()
                .unwrap_or("not detected")
        ),
        format!(
            "  gpu_features={}",
            if input.runtime.gpu_features.is_empty() {
                "not detected".to_owned()
            } else {
                input.runtime.gpu_features.join(", ")
            }
        ),
        format!(
            "  font_family={} fallback_chain={}",
            input.config.font.family,
            if input.config.font.fallback_families.is_empty() {
                "system".to_owned()
            } else {
                input.config.font.fallback_families.join(", ")
            }
        ),
    ]);

    if !input.config.renderer.damage_tracking {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Warning,
            area: "renderer",
            message: "damage tracking is disabled; renderer may redraw more than necessary"
                .to_owned(),
        });
    }
}

fn append_fonts_report(input: &DoctorInput, report: &mut DoctorReport) {
    report.lines.extend([
        "fonts:".to_owned(),
        format!(
            "  configured_family={} size={} line_height={}",
            input.config.font.family, input.config.font.size, input.config.font.line_height
        ),
        format!(
            "  fallback_chain={}",
            if input.config.font.fallback_families.is_empty() {
                "system defaults".to_owned()
            } else {
                input.config.font.fallback_families.join(", ")
            }
        ),
        format!("  discovery={}", input.runtime.font_discovery),
    ]);

    if input.runtime.font_discovery.contains("unresolved") {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Warning,
            area: "fonts",
            message: "one or more configured font families could not be resolved on this host"
                .to_owned(),
        });
    }
}

fn append_config_report(input: &DoctorInput, report: &mut DoctorReport) {
    report.lines.extend([
        "config:".to_owned(),
        format!("  source={}", input.config_source),
        format!("  parse_status={}", input.runtime.config_parse_status),
        format!(
            "  diagnostics={} platform_overrides macos={} windows={} linux={} x11={} wayland={}",
            input.config_diagnostics.len(),
            input.config.platform_overrides.macos.is_some(),
            input.config.platform_overrides.windows.is_some(),
            input.config.platform_overrides.linux.is_some(),
            input.config.platform_overrides.linux_x11.is_some(),
            input.config.platform_overrides.linux_wayland.is_some()
        ),
    ]);

    for diagnostic in &input.config_diagnostics {
        report.findings.push(DoctorFinding {
            severity: match diagnostic.severity {
                ConfigDiagnosticSeverity::Error => DoctorSeverity::Error,
                ConfigDiagnosticSeverity::Warning => DoctorSeverity::Warning,
            },
            area: "config",
            message: format!("{}: {}", diagnostic.path, diagnostic.message),
        });
    }
}

fn append_shell_integration_report(input: &DoctorInput, report: &mut DoctorReport) {
    report.lines.extend([
        "shell-integration:".to_owned(),
        format!(
            "  enabled={} activation={:?} auto_install={} remote_instructions={}",
            input.config.shell_integration.enabled,
            input.config.shell_integration.activation,
            input.config.shell_integration.auto_install,
            input.config.shell_integration.remote_instructions
        ),
        format!(
            "  enabled_shells={}",
            input.config.shell_integration.enabled_shells.join(", ")
        ),
        format!(
            "  runtime_status={}",
            input.runtime.shell_integration_status
        ),
    ]);

    if !input.config.shell_integration.enabled {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Info,
            area: "shell-integration",
            message: "semantic command features are disabled by config".to_owned(),
        });
    }
    for profile in &input.config.ssh_profiles {
        report.lines.push(format!(
            "  remote_profile={} integration={} status={}",
            profile.name,
            profile.shell_integration,
            if !profile.shell_integration || !input.config.shell_integration.enabled {
                "disabled"
            } else if matches!(
                input.config.shell_integration.activation,
                config_core::ShellIntegrationActivationConfig::Heuristic
            ) {
                "heuristic (low confidence; no remote metadata or exit status)"
            } else {
                "configured; runtime remains inactive until remote markers are observed"
            }
        ));
    }
}

fn append_clipboard_report(input: &DoctorInput, report: &mut DoctorReport) {
    report.lines.extend([
        "clipboard:".to_owned(),
        format!(
            "  enabled={} copy_on_select={} paste_protection={} bracketed_paste={}",
            input.config.clipboard.enabled,
            input.config.clipboard.copy_on_select,
            input.config.clipboard.paste_protection,
            input.config.clipboard.bracketed_paste
        ),
        format!(
            "  osc52 enabled={} allow_local={} allow_remote={} max_bytes={} confirm_remote={}",
            input.config.clipboard.osc52.enabled,
            input.config.clipboard.osc52.allow_local,
            input.config.clipboard.osc52.allow_remote,
            input.config.clipboard.osc52.max_bytes,
            input.config.clipboard.osc52.confirm_remote_writes
        ),
        format!("  provider={}", input.runtime.clipboard_provider),
    ]);

    if input.config.clipboard.osc52.allow_remote
        && !input.config.clipboard.osc52.confirm_remote_writes
    {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Warning,
            area: "clipboard",
            message: "remote OSC 52 clipboard writes are allowed without confirmation".to_owned(),
        });
    }
}

fn append_notification_report(input: &DoctorInput, report: &mut DoctorReport) {
    report.lines.extend([
        "notifications:".to_owned(),
        format!(
            "  enabled={} only_when_unfocused={} session_closed={} transport_errors={}",
            input.config.notifications.enabled,
            input.config.notifications.only_when_unfocused,
            input.config.notifications.session_closed,
            input.config.notifications.transport_errors
        ),
        format!("  provider={}", input.runtime.notification_provider),
    ]);

    if input.config.notifications.enabled
        && input.runtime.notification_provider.contains("unavailable")
    {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Warning,
            area: "notifications",
            message: "native notifications are enabled but the active provider is unavailable"
                .to_owned(),
        });
    }
}

fn append_performance_report(input: &DoctorInput, report: &mut DoctorReport) {
    report.lines.extend([
        "performance:".to_owned(),
        format!(
            "  profile={:?} max_frame_time_ms={} glyph_cache_entries={} frame_rate_limit={}",
            input.config.performance.profile,
            input.config.performance.max_frame_time_ms,
            input.config.performance.glyph_cache_entries,
            input
                .config
                .performance
                .frame_rate_limit
                .map_or_else(|| "none".to_owned(), |limit| limit.to_string())
        ),
        format!(
            "  visual_budget fps={} cursor_asset_kb={} active_animations={} animated_pixels={}",
            input.config.performance.max_animation_fps,
            input.config.performance.max_cursor_asset_size_kb,
            input.config.performance.max_active_animations,
            input.config.performance.max_animated_region_pixels
        ),
        format!("  overlay={}", input.runtime.performance_overlay_status),
    ]);

    if input.config.diagnostics.performance_overlay {
        report.findings.push(DoctorFinding {
            severity: DoctorSeverity::Info,
            area: "performance",
            message: "performance overlay is enabled and may emit runtime diagnostics".to_owned(),
        });
    }
}

fn append_ssh_report(input: &DoctorInput, report: &mut DoctorReport) {
    report.lines.extend([
        "ssh:".to_owned(),
        format!("  profiles={}", input.config.ssh_profiles.len()),
        format!("  provider={}", input.runtime.ssh_provider_status),
        format!("  keychain_provider={}", input.runtime.keychain_provider),
        "  credential_storage=credential/keychain provider boundary; plaintext config credentials unsupported"
            .to_owned(),
        "  host_trust=unknown hosts require explicit approval; changed keys are blocking"
            .to_owned(),
        format!("  pty_backend={}", input.runtime.pty_backend),
    ]);

    for profile in &input.config.ssh_profiles {
        report.lines.push(format!(
            "  profile={} target={}:{} auth={:?} host_key_policy={} shell_integration={} proxy_jump={}",
            profile.name,
            profile.host,
            profile.port,
            profile.auth_method,
            known_hosts_policy_name(&profile.known_hosts_policy),
            profile.shell_integration,
            profile.proxy_jump.as_deref().unwrap_or("none")
        ));
        if profile.proxy_jump.is_some() {
            report.findings.push(DoctorFinding {
                severity: DoctorSeverity::Warning,
                area: "ssh",
                message: format!(
                    "profile '{}' requests proxy_jump, which remains a later transport extension and is rejected before connection",
                    profile.name
                ),
            });
        }
        if matches!(
            profile.known_hosts_policy,
            SshKnownHostsPolicy::TrustOnFirstUse
        ) {
            report.findings.push(DoctorFinding {
                severity: DoctorSeverity::Warning,
                area: "ssh",
                message: format!(
                    "profile '{}' uses trust_on_first_use; changed host keys still block, but first trust must be intentional",
                    profile.name
                ),
            });
        }
        if matches!(profile.auth_method, SshAuthMethod::None) {
            report.findings.push(DoctorFinding {
                severity: DoctorSeverity::Warning,
                area: "ssh",
                message: format!(
                    "profile '{}' requests none authentication, which the current backend does not support",
                    profile.name
                ),
            });
        }
        if profile.agent_forwarding {
            report.findings.push(DoctorFinding {
                severity: DoctorSeverity::Warning,
                area: "ssh",
                message: format!(
                    "profile '{}' enables agent forwarding; use only with trusted hosts",
                    profile.name
                ),
            });
        }
    }
}

fn doctor_topic_key(topic: DoctorTopic) -> &'static str {
    match topic {
        DoctorTopic::All => "all",
        DoctorTopic::Renderer => "renderer",
        DoctorTopic::Config => "config",
        DoctorTopic::Platform => "platform",
        DoctorTopic::ShellIntegration => "shell-integration",
        DoctorTopic::Performance => "performance",
        DoctorTopic::Ssh => "ssh",
        DoctorTopic::Window => "window",
        DoctorTopic::Fonts => "fonts",
        DoctorTopic::Clipboard => "clipboard",
        DoctorTopic::Notifications => "notifications",
    }
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped
}

fn known_hosts_policy_name(policy: &SshKnownHostsPolicy) -> &'static str {
    match policy {
        SshKnownHostsPolicy::Ask => "ask",
        SshKnownHostsPolicy::RequireKnown => "require_known",
        SshKnownHostsPolicy::TrustOnFirstUse => "trust_on_first_use",
        SshKnownHostsPolicy::PinFingerprint { .. } => "pin_fingerprint",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BugReportSnapshot {
    pub app_version: String,
    pub os: String,
    pub arch: String,
    pub platform: ConfigPlatform,
    pub renderer_backend_preference: String,
    pub config_source: String,
    pub config_diagnostic_count: usize,
    pub platform_capabilities: Vec<String>,
    pub shell_integration_enabled: bool,
    pub ssh_profile_count: usize,
    pub recent_errors: Vec<String>,
}

impl BugReportSnapshot {
    #[must_use]
    pub fn from_doctor_input(input: &DoctorInput) -> Self {
        let mut platform_capabilities = vec![format!("dpi={:?}", input.platform.dpi_behavior)];
        if let Some(backend) = input.platform.linux_backend {
            platform_capabilities.push(format!("backend={backend:?}"));
        }
        if let Some(compositor) = &input.platform.compositor_or_desktop {
            platform_capabilities.push(format!("desktop={compositor}"));
        }

        Self {
            app_version: input.app_version.clone(),
            os: input.platform.os.clone(),
            arch: input.platform.arch.clone(),
            platform: input.platform.platform,
            renderer_backend_preference: format!("{:?}", input.config.renderer.backend),
            config_source: input.config_source.clone(),
            config_diagnostic_count: input.config_diagnostics.len(),
            platform_capabilities,
            shell_integration_enabled: input.config.shell_integration.enabled,
            ssh_profile_count: input.config.ssh_profiles.len(),
            recent_errors: input.recent_errors.clone(),
        }
    }

    #[must_use]
    pub fn render_text(&self) -> String {
        [
            "Panea bug-report snapshot".to_owned(),
            "privacy: terminal contents, command output, environment variables, secrets, SSH keys, and clipboard contents are not included".to_owned(),
            format!("version: {}", self.app_version),
            format!("os: {} {}", self.os, self.arch),
            format!("platform: {:?}", self.platform),
            format!(
                "renderer_backend_preference: {}",
                self.renderer_backend_preference
            ),
            format!("config_source: {}", self.config_source),
            format!("config_diagnostic_count: {}", self.config_diagnostic_count),
            format!(
                "platform_capabilities: {}",
                self.platform_capabilities.join(", ")
            ),
            format!(
                "shell_integration_enabled: {}",
                self.shell_integration_enabled
            ),
            format!("ssh_profile_count: {}", self.ssh_profile_count),
            format!("recent_errors: {}", self.recent_errors.len()),
        ]
        .join("\n")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessStatus {
    Pass,
    Warning,
    Blocked,
    NotVerified,
}

impl fmt::Display for ReadinessStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pass => "pass",
            Self::Warning => "warning",
            Self::Blocked => "blocked",
            Self::NotVerified => "not verified",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessItem {
    pub area: &'static str,
    pub status: ReadinessStatus,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessReport {
    pub title: &'static str,
    pub items: Vec<ReadinessItem>,
}

impl ReadinessReport {
    #[must_use]
    pub fn has_blockers(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.status == ReadinessStatus::Blocked)
    }

    #[must_use]
    pub fn render_text(&self) -> String {
        let mut lines = vec![self.title.to_owned()];
        lines.extend(
            self.items
                .iter()
                .map(|item| format!("- [{}] {}: {}", item.status, item.area, item.message)),
        );
        lines.join("\n")
    }
}

#[must_use]
pub fn stability_hardening_report(_input: &DoctorInput) -> ReadinessReport {
    ReadinessReport {
        title: "Panea stability hardening",
        items: vec![
            ReadinessItem {
                area: "panic boundaries",
                status: ReadinessStatus::Pass,
                message:
                    "desktop app catches panics at platform, renderer, transport, and parser edges"
                        .to_owned(),
            },
            ReadinessItem {
                area: "session cleanup",
                status: ReadinessStatus::Pass,
                message:
                    "local PTY and SSH transports expose bounded explicit shutdown contracts"
                        .to_owned(),
            },
            ReadinessItem {
                area: "renderer recovery",
                status: ReadinessStatus::Warning,
                message:
                    "renderer device-loss recovery contract and WGPU resource recreation path exist; real sleep/wake, monitor-change, and cross-OS GPU failure validation remain"
                        .to_owned(),
            },
            ReadinessItem {
                area: "config reload",
                status: ReadinessStatus::Pass,
                message:
                    "config reload keeps the previous active config after parse, validation, or runtime apply failures; macOS/Linux runtime validation remains open"
                        .to_owned(),
            },
            ReadinessItem {
                area: "window close",
                status: ReadinessStatus::Pass,
                message:
                    "close requests call bounded transport shutdown before exiting the event loop"
                        .to_owned(),
            },
            ReadinessItem {
                area: "user errors",
                status: ReadinessStatus::Pass,
                message:
                    "config, renderer, transport, SSH, and platform diagnostics have human-readable surfaces"
                        .to_owned(),
            },
        ],
    }
}

#[must_use]
pub fn security_review_report(input: &DoctorInput) -> ReadinessReport {
    let mut items = Vec::new();

    let ssh_status = if input.config.ssh_profiles.iter().any(|profile| {
        matches!(
            profile.known_hosts_policy,
            SshKnownHostsPolicy::TrustOnFirstUse
        )
    }) {
        ReadinessStatus::Warning
    } else {
        ReadinessStatus::Pass
    };
    items.push(ReadinessItem {
        area: "SSH host verification",
        status: ssh_status,
        message:
            "host keys are never silently skipped; unknown and changed keys require explicit policy"
                .to_owned(),
    });
    items.push(ReadinessItem {
        area: "key storage",
        status: if input.runtime.keychain_provider.contains("available=true") {
            ReadinessStatus::Pass
        } else {
            ReadinessStatus::Warning
        },
        message: "desktop secrets use the native OS keychain when available; unavailable providers are explicit and never fall back to plaintext config"
            .to_owned(),
    });
    items.push(ReadinessItem {
        area: "passphrases",
        status: ReadinessStatus::Pass,
        message: "passphrases use masked desktop prompts, redacted SecretProvider boundaries, and opt-in native keychain persistence"
            .to_owned(),
    });
    items.push(ReadinessItem {
        area: "clipboard",
        status: ReadinessStatus::Warning,
        message:
            "system and Linux primary-selection bridges, paste sanitization, and OSC 52 policy exist; cross-OS smoke remains separate work"
                .to_owned(),
    });
    items.push(ReadinessItem {
        area: "OSC clipboard",
        status: ReadinessStatus::Warning,
        message:
            "OSC 52 writes are policy-controlled and bounded; opted-in remote writes require an explicit one-time confirmation, while cross-OS smoke remains separate work"
                .to_owned(),
    });
    items.push(ReadinessItem {
        area: "shell integration",
        status: ReadinessStatus::Warning,
        message:
            "scripts emit semantic OSC events without mutating terminal text; installer trust and update policy need release review"
                .to_owned(),
    });
    items.push(ReadinessItem {
        area: "remote helpers",
        status: ReadinessStatus::Warning,
        message:
            "remote shell integration is optional and must be treated as code running on the remote account"
                .to_owned(),
    });
    items.push(ReadinessItem {
        area: "logs and diagnostics",
        status: ReadinessStatus::Pass,
        message:
            "bug-report snapshots exclude terminal contents, command output, environment variables, secrets, SSH keys, and clipboard contents"
                .to_owned(),
    });

    ReadinessReport {
        title: "Panea security review",
        items,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageTarget {
    pub target: &'static str,
    pub status: ReadinessStatus,
    pub requirements: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagingPlan {
    pub targets: Vec<PackageTarget>,
}

impl PackagingPlan {
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut lines = vec!["Panea packaging plan".to_owned()];
        for target in &self.targets {
            lines.push(format!("- [{}] {}", target.status, target.target));
            for requirement in &target.requirements {
                lines.push(format!("  - {requirement}"));
            }
        }
        lines.join("\n")
    }
}

#[must_use]
pub fn packaging_plan() -> PackagingPlan {
    PackagingPlan {
        targets: vec![
            PackageTarget {
                target: "macOS app bundle, ZIP, and DMG",
                status: ReadinessStatus::Warning,
                requirements: vec![
                    "artifact builders include binary, assets, config, and shell scripts",
                    "run package smoke on a macOS release host",
                    "codesign/notarytool hooks exist; supply release-owner credentials and collect host evidence",
                ],
            },
            PackageTarget {
                target: "Windows installer",
                status: ReadinessStatus::Pass,
                requirements: vec![
                    "per-user install, atomic upgrade, Start menu, PATH, and uninstall are implemented",
                    "current-host install/doctor/shell/GUI/uninstall smoke is the release gate",
                    "Authenticode sign/verify hooks require a release certificate",
                ],
            },
            PackageTarget {
                target: "Windows portable build",
                status: ReadinessStatus::Pass,
                requirements: vec![
                    "portable ZIP includes binary, assets, themes, cursor profiles, and scripts",
                    "current-host packaged doctor, shell, and first-frame GUI smokes are required",
                ],
            },
            PackageTarget {
                target: "Linux portable tarball and AppImage",
                status: ReadinessStatus::Warning,
                requirements: vec![
                    "portable tarball builder includes binary, assets, themes, and scripts",
                    "run package smoke on Linux X11 and Wayland hosts",
                    "AppImage builder emits a desktop-integrated AppDir artifact through appimagetool",
                ],
            },
            PackageTarget {
                target: "Linux distro packages (deb and rpm)",
                status: ReadinessStatus::Warning,
                requirements: vec![
                    "deb builder includes /usr binary, desktop entry, icon, and resources",
                    "validate package dependencies and installation on supported distributions",
                    "rpm builder emits the same /usr layout through rpmbuild",
                ],
            },
        ],
    }
}

#[must_use]
pub fn release_validation_report(input: &DoctorInput) -> ReadinessReport {
    let config_status = if input
        .config_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Error)
    {
        ReadinessStatus::Blocked
    } else {
        ReadinessStatus::Pass
    };

    ReadinessReport {
        title: "Panea release validation",
        items: vec![
            ReadinessItem {
                area: "unit tests",
                status: ReadinessStatus::NotVerified,
                message: "run cargo test --workspace for this release candidate".to_owned(),
            },
            ReadinessItem {
                area: "integration tests",
                status: ReadinessStatus::NotVerified,
                message:
                    "run PTY, SSH, shell integration, and desktop smoke tests with bounded timeouts"
                        .to_owned(),
            },
            ReadinessItem {
                area: "parser and conformance",
                status: ReadinessStatus::NotVerified,
                message:
                    "run parser fixtures and terminal conformance goldens before release"
                        .to_owned(),
            },
            ReadinessItem {
                area: "renderer smoke",
                status: ReadinessStatus::NotVerified,
                message:
                    "run GPU window smoke and screenshots on macOS, Windows, Linux X11, and Linux Wayland"
                        .to_owned(),
            },
            ReadinessItem {
                area: "benchmarks",
                status: ReadinessStatus::NotVerified,
                message: "run cargo xtask bench all and review disabled-feature costs".to_owned(),
            },
            ReadinessItem {
                area: "config compatibility",
                status: config_status,
                message: format!(
                    "current config diagnostics: {}",
                    input.config_diagnostics.len()
                ),
            },
            ReadinessItem {
                area: "platform parity",
                status: ReadinessStatus::Blocked,
                message:
                    "release requires manual smoke tests on macOS, Windows, Linux X11, and Linux Wayland"
                        .to_owned(),
            },
            ReadinessItem {
                area: "packaging",
                status: ReadinessStatus::Warning,
                message:
                    "release automation builds Windows ZIP/installer, macOS app/ZIP/DMG, and Linux tarball/deb; non-Windows validation and release signing remain"
                        .to_owned(),
            },
            ReadinessItem {
                area: "performance comparison",
                status: ReadinessStatus::NotVerified,
                message:
                    "do not compare publicly against other terminals until fair benchmark fixtures are published"
                        .to_owned(),
            },
        ],
    }
}

#[must_use]
pub fn ios_companion_readiness_report() -> ReadinessReport {
    ReadinessReport {
        title: "Panea iOS SSH companion readiness",
        items: vec![
            ReadinessItem {
                area: "shared engine",
                status: ReadinessStatus::Pass,
                message:
                    "iOS shell foundation reuses terminal core, parser, semantics, render-core, config-core, transport-core, and SSH contracts"
                        .to_owned(),
            },
            ReadinessItem {
                area: "native app shell",
                status: ReadinessStatus::Blocked,
                message:
                    "Rust bridge contracts exist for lifecycle, frame requests, diagnostics, host-key decisions, and secret prompts; UIKit/SwiftUI host, settings UI, and iPad multitasking are not implemented"
                        .to_owned(),
            },
            ReadinessItem {
                area: "iOS render surface",
                status: ReadinessStatus::Blocked,
                message:
                    "render-core scene reuse and iOS GPU surface specs exist, but a native damage-driven iOS GPU backend has not been implemented or profiled"
                        .to_owned(),
            },
            ReadinessItem {
                area: "SSH security",
                status: ReadinessStatus::Warning,
                message:
                    "SSH profile validation, host-trust prompt modeling, and iOS Keychain capability handoff exist; native Keychain-backed SecretProvider and approval UI are still required"
                        .to_owned(),
            },
            ReadinessItem {
                area: "SSH profile UI",
                status: ReadinessStatus::Warning,
                message:
                    "portable SSH profiles map to mobile connection plans with validation; native profile editing and key import UI remain required"
                        .to_owned(),
            },
            ReadinessItem {
                area: "lifecycle honesty",
                status: ReadinessStatus::Pass,
                message:
                    "mobile policy explicitly avoids promising indefinite background SSH sessions and prefers graceful disconnect plus quick reconnect"
                        .to_owned(),
            },
            ReadinessItem {
                area: "remote semantics",
                status: ReadinessStatus::Warning,
                message:
                    "semantic command-block concepts are shared, but remote shell integration install/activation remains follow-up work"
                        .to_owned(),
            },
            ReadinessItem {
                area: "real device validation",
                status: ReadinessStatus::NotVerified,
                message:
                    "iPhone/iPad/simulator checklist is documented, but SSH, rendering, keyboard, secure storage, touch selection, and reconnect behavior have not been run on device"
                        .to_owned(),
            },
        ],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerformanceBudget {
    pub max_frame_time: Duration,
    pub max_idle_wakeups_per_second: u64,
    pub max_damage_regions: usize,
}

impl Default for PerformanceBudget {
    fn default() -> Self {
        Self {
            max_frame_time: Duration::from_millis(16),
            max_idle_wakeups_per_second: 2,
            max_damage_regions: 256,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualBudget {
    pub max_animation_fps: u16,
    pub max_cursor_asset_size_kb: u32,
    pub max_active_animations: u16,
    pub max_animated_region_pixels: u32,
}

impl Default for VisualBudget {
    fn default() -> Self {
        Self {
            max_animation_fps: 60,
            max_cursor_asset_size_kb: 256,
            max_active_animations: 8,
            max_animated_region_pixels: 250_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VisualRuntimeStats {
    pub requested_animation_fps: u16,
    pub cursor_asset_size_kb: u32,
    pub active_animations: u16,
    pub animated_region_pixels: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisualWarningKind {
    AnimationFpsOverBudget,
    CursorAssetTooLarge,
    TooManyAnimations,
    AnimatedRegionTooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualWarning {
    pub kind: VisualWarningKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualBudgetReport {
    pub passed: bool,
    pub warnings: Vec<VisualWarning>,
}

pub fn evaluate_visual_budget(
    stats: VisualRuntimeStats,
    budget: VisualBudget,
) -> VisualBudgetReport {
    let mut warnings = Vec::new();

    if stats.requested_animation_fps > budget.max_animation_fps {
        warnings.push(VisualWarning {
            kind: VisualWarningKind::AnimationFpsOverBudget,
            message: format!(
                "animation FPS {} exceeded cap {}",
                stats.requested_animation_fps, budget.max_animation_fps
            ),
        });
    }
    if stats.cursor_asset_size_kb > budget.max_cursor_asset_size_kb {
        warnings.push(VisualWarning {
            kind: VisualWarningKind::CursorAssetTooLarge,
            message: format!(
                "cursor asset {} KiB exceeded cap {} KiB",
                stats.cursor_asset_size_kb, budget.max_cursor_asset_size_kb
            ),
        });
    }
    if stats.active_animations > budget.max_active_animations {
        warnings.push(VisualWarning {
            kind: VisualWarningKind::TooManyAnimations,
            message: format!(
                "active animations {} exceeded cap {}",
                stats.active_animations, budget.max_active_animations
            ),
        });
    }
    if stats.animated_region_pixels > budget.max_animated_region_pixels {
        warnings.push(VisualWarning {
            kind: VisualWarningKind::AnimatedRegionTooLarge,
            message: format!(
                "animated region {} px exceeded cap {} px",
                stats.animated_region_pixels, budget.max_animated_region_pixels
            ),
        });
    }

    VisualBudgetReport {
        passed: warnings.is_empty(),
        warnings,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteSessionSecurityState {
    NotConnected,
    HostKeyUnknown,
    HostKeyTrusted,
    HostKeyMismatch,
    Authenticated,
    AuthenticationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSessionDiagnostics {
    pub profile_name: String,
    pub host: String,
    pub port: u16,
    pub security_state: RemoteSessionSecurityState,
    pub remote_pty_requested: bool,
    pub bytes_received: usize,
    pub disconnected: bool,
    pub last_error: Option<String>,
}

impl RemoteSessionDiagnostics {
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = vec![format!(
            "ssh profile={} target={}:{} state={:?}",
            self.profile_name, self.host, self.port, self.security_state
        )];
        if self.remote_pty_requested {
            parts.push("remote_pty=requested".to_owned());
        }
        parts.push(format!("bytes_received={}", self.bytes_received));
        if self.disconnected {
            parts.push("disconnected=true".to_owned());
        }
        if let Some(error) = &self.last_error {
            parts.push(format!("error={error}"));
        }
        parts.join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerformanceWarningKind {
    FrameOverBudget,
    ExcessiveIdleWakeups,
    ExcessiveDamageRegions,
    DisabledFeatureHasCost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformanceWarning {
    pub kind: PerformanceWarningKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformanceGateReport {
    pub passed: bool,
    pub warnings: Vec<PerformanceWarning>,
}

impl PerformanceGateReport {
    #[must_use]
    pub fn pass() -> Self {
        Self {
            passed: true,
            warnings: Vec::new(),
        }
    }
}

pub fn evaluate_performance_gate(
    sample: RenderInstrumentation,
    budget: PerformanceBudget,
) -> PerformanceGateReport {
    let mut warnings = Vec::new();

    if sample.frame_time > budget.max_frame_time {
        warnings.push(PerformanceWarning {
            kind: PerformanceWarningKind::FrameOverBudget,
            message: format!(
                "frame time {:?} exceeded budget {:?}",
                sample.frame_time, budget.max_frame_time
            ),
        });
    }

    if sample.idle_wakeups > budget.max_idle_wakeups_per_second {
        warnings.push(PerformanceWarning {
            kind: PerformanceWarningKind::ExcessiveIdleWakeups,
            message: format!(
                "idle wakeups {} exceeded budget {}",
                sample.idle_wakeups, budget.max_idle_wakeups_per_second
            ),
        });
    }

    if sample.damage_region_count > budget.max_damage_regions {
        warnings.push(PerformanceWarning {
            kind: PerformanceWarningKind::ExcessiveDamageRegions,
            message: format!(
                "damage regions {} exceeded budget {}",
                sample.damage_region_count, budget.max_damage_regions
            ),
        });
    }

    PerformanceGateReport {
        passed: warnings.is_empty(),
        warnings,
    }
}

pub fn evaluate_feature_cost(sample: &FeatureCostSample) -> PerformanceGateReport {
    if sample.mode != OptionalFeatureCostMode::Disabled {
        return PerformanceGateReport::pass();
    }

    let has_cost = sample.instrumentation.animated_region_count > 0
        || !sample.instrumentation.frame_time.is_zero()
        || sample.instrumentation.draw_call_count > 0;

    if has_cost {
        PerformanceGateReport {
            passed: false,
            warnings: vec![PerformanceWarning {
                kind: PerformanceWarningKind::DisabledFeatureHasCost,
                message: format!(
                    "{:?} recorded work while disabled: frame={:?}, draw_calls={}, animations={}",
                    sample.feature,
                    sample.instrumentation.frame_time,
                    sample.instrumentation.draw_call_count,
                    sample.instrumentation.animated_region_count
                ),
            }],
        }
    } else {
        PerformanceGateReport::pass()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellIntegrationWarningKind {
    Disabled,
    Inactive,
    HeuristicMode,
    RemoteInactive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegrationWarning {
    pub kind: ShellIntegrationWarningKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegrationReport {
    pub shell_detected: Option<String>,
    pub integration_active: bool,
    pub last_event: Option<SemanticEventKind>,
    pub last_event_age: Option<Duration>,
    pub command_block_confidence: CommandBlockConfidence,
    pub remote_integration_active: bool,
    pub warnings: Vec<ShellIntegrationWarning>,
}

impl ShellIntegrationReport {
    #[must_use]
    pub fn from_semantic_diagnostics(diagnostics: &SemanticDiagnostics) -> Self {
        let mut warnings = Vec::new();

        if diagnostics.mode == IntegrationMode::Disabled {
            warnings.push(ShellIntegrationWarning {
                kind: ShellIntegrationWarningKind::Disabled,
                message: "shell integration is disabled; semantic command features are unavailable"
                    .to_owned(),
            });
        } else if !diagnostics.integration_active {
            warnings.push(ShellIntegrationWarning {
                kind: ShellIntegrationWarningKind::Inactive,
                message: "shell integration has not emitted semantic events for this session"
                    .to_owned(),
            });
        }

        if diagnostics.heuristic_mode {
            warnings.push(ShellIntegrationWarning {
                kind: ShellIntegrationWarningKind::HeuristicMode,
                message: "command regions are heuristic because shell integration is inactive"
                    .to_owned(),
            });
        }

        if diagnostics.shell_detected.is_some() && !diagnostics.remote_integration_active {
            warnings.push(ShellIntegrationWarning {
                kind: ShellIntegrationWarningKind::RemoteInactive,
                message: "remote shell integration status is unknown".to_owned(),
            });
        }

        Self {
            shell_detected: diagnostics.shell_detected.clone(),
            integration_active: diagnostics.integration_active,
            last_event: diagnostics.last_event,
            last_event_age: diagnostics.last_event_age,
            command_block_confidence: diagnostics.command_block_confidence,
            remote_integration_active: diagnostics.remote_integration_active,
            warnings,
        }
    }

    #[must_use]
    pub fn render_text(&self) -> String {
        let shell = self.shell_detected.as_deref().unwrap_or("unknown");
        let last_event = self
            .last_event
            .map_or_else(|| "none".to_owned(), |event| format!("{event:?}"));
        let warning = self
            .warnings
            .first()
            .map_or("ok", |warning| warning.message.as_str());

        format!(
            "shell={shell} active={} last_event={} confidence={:?} remote_active={} status={warning}",
            self.integration_active,
            last_event,
            self.command_block_confidence,
            self.remote_integration_active
        )
    }
}

#[derive(Debug, Clone)]
pub struct PerformanceOverlay {
    enabled: bool,
    samples: VecDeque<RenderInstrumentation>,
    capacity: usize,
    backend: String,
    runtime_profile: &'static str,
    power_source: &'static str,
}

impl PerformanceOverlay {
    #[must_use]
    pub fn new(enabled: bool, backend: impl Into<String>) -> Self {
        Self {
            enabled,
            samples: VecDeque::new(),
            capacity: 120,
            backend: backend.into(),
            runtime_profile: "balanced",
            power_source: "unknown",
        }
    }

    pub fn record(&mut self, sample: RenderInstrumentation) {
        if !self.enabled {
            return;
        }

        while self.samples.len() >= self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn set_runtime_context(&mut self, profile: &'static str, power_source: &'static str) {
        if !self.enabled {
            return;
        }
        self.runtime_profile = profile;
        self.power_source = power_source;
    }

    #[must_use]
    pub fn latest(&self) -> Option<RenderInstrumentation> {
        self.samples.back().copied()
    }

    #[must_use]
    pub fn render_lines(&self, budget: PerformanceBudget) -> Option<Vec<String>> {
        if !self.enabled {
            return None;
        }

        let latest = self.latest()?;
        let fps = if latest.frame_time.is_zero() {
            0.0
        } else {
            1.0 / latest.frame_time.as_secs_f64()
        };
        let gpu = latest
            .gpu_time
            .map_or_else(|| "n/a".to_owned(), format_duration_ms);
        let submit = latest
            .gpu_submit_time
            .map_or_else(|| "n/a".to_owned(), format_duration_ms);
        let warning = evaluate_performance_gate(latest, budget)
            .warnings
            .first()
            .map_or("ok".to_owned(), |warning| warning.message.clone());

        Some(vec![
            format!(
                "fps {fps:.1} frame {} cpu {} gpu {gpu} submit {submit}",
                format_duration_ms(latest.frame_time),
                format_duration_ms(latest.cpu_prepare_time),
            ),
            format!(
                "backend {} profile {} power {} timestamps {} draws {} damage {} anim {} idle {}",
                self.backend,
                self.runtime_profile,
                self.power_source,
                gpu_timing_label(latest.gpu_timing_status),
                latest.draw_call_count,
                latest.damage_region_count,
                latest.animated_region_count,
                latest.idle_wakeups,
            ),
            format!(
                "glyph hit {} miss {} uploads {} atlas {}",
                latest.glyphs.cache_hits,
                latest.glyphs.cache_misses,
                latest.glyphs.atlas_uploads,
                atlas_usage_label(
                    latest.glyphs.atlas_used_bytes,
                    latest.glyphs.atlas_capacity_bytes
                ),
            ),
            format!(
                "pty {}/s parser {}/s mem {} scrollback {} status {warning}",
                format_bytes(latest.pty_read_bytes_per_second),
                format_bytes(latest.parser_bytes_per_second),
                latest
                    .memory_usage_bytes
                    .map_or_else(|| "n/a".to_owned(), format_bytes),
                format_bytes(latest.scrollback_memory_bytes),
            ),
        ])
    }

    #[must_use]
    pub fn render_text(&self, budget: PerformanceBudget) -> Option<String> {
        self.render_lines(budget).map(|lines| lines.join(" | "))
    }
}

#[must_use]
fn format_duration_ms(duration: Duration) -> String {
    format!("{:.2}ms", duration.as_secs_f64() * 1000.0)
}

#[must_use]
fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes >= 1024 * 1024 {
        format!("{:.1}MiB", bytes as f64 / MIB)
    } else if bytes >= 1024 {
        format!("{:.1}KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes}B")
    }
}

#[must_use]
fn atlas_usage_label(used: u64, capacity: u64) -> String {
    if capacity == 0 {
        return "n/a".to_owned();
    }
    let percent = (used as f64 / capacity as f64) * 100.0;
    format!("{used}/{capacity}B {percent:.1}%")
}

#[must_use]
fn gpu_timing_label(status: GpuTimingStatus) -> &'static str {
    match status {
        GpuTimingStatus::Disabled => "disabled",
        GpuTimingStatus::Unsupported => "unsupported",
        GpuTimingStatus::Pending => "pending",
        GpuTimingStatus::Available => "available",
        GpuTimingStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_core::{RendererBackendPreference, SshProfile};
    use render_core::{FeatureCostSample, OptionalFeature, OptionalFeatureCostMode};

    #[test]
    fn gate_reports_over_budget_frame() {
        let report = evaluate_performance_gate(
            RenderInstrumentation {
                frame_time: Duration::from_millis(25),
                ..RenderInstrumentation::default()
            },
            PerformanceBudget::default(),
        );

        assert!(!report.passed);
        assert_eq!(
            report.warnings[0].kind,
            PerformanceWarningKind::FrameOverBudget
        );
    }

    #[test]
    fn overlay_formats_latest_sample() {
        let mut overlay = PerformanceOverlay::new(true, "test-backend");
        overlay.record(RenderInstrumentation {
            frame_time: Duration::from_millis(10),
            cpu_prepare_time: Duration::from_millis(7),
            draw_call_count: 3,
            ..RenderInstrumentation::default()
        });

        let text = overlay
            .render_text(PerformanceBudget::default())
            .expect("overlay text");
        assert!(text.contains("backend test-backend"));
        assert!(text.contains("draws 3"));
        assert!(text.contains("timestamps disabled"));
    }

    #[test]
    fn disabled_feature_cost_fails_gate() {
        let report = evaluate_feature_cost(&FeatureCostSample {
            feature: OptionalFeature::CursorAnimation,
            mode: OptionalFeatureCostMode::Disabled,
            instrumentation: RenderInstrumentation {
                draw_call_count: 1,
                ..RenderInstrumentation::default()
            },
        });

        assert!(!report.passed);
    }

    #[test]
    fn visual_budget_reports_expensive_animation_regions() {
        let report = evaluate_visual_budget(
            VisualRuntimeStats {
                requested_animation_fps: 120,
                active_animations: 12,
                animated_region_pixels: 300_000,
                ..VisualRuntimeStats::default()
            },
            VisualBudget::default(),
        );

        assert!(!report.passed);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.kind == VisualWarningKind::AnimationFpsOverBudget)
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.kind == VisualWarningKind::AnimatedRegionTooLarge)
        );
    }

    #[test]
    fn remote_session_diagnostics_summarize_without_secrets() {
        let report = RemoteSessionDiagnostics {
            profile_name: "prod".to_owned(),
            host: "example.com".to_owned(),
            port: 22,
            security_state: RemoteSessionSecurityState::HostKeyMismatch,
            remote_pty_requested: true,
            bytes_received: 128,
            disconnected: true,
            last_error: Some("host key mismatch".to_owned()),
        };

        let summary = report.summary();

        assert!(summary.contains("profile=prod"));
        assert!(summary.contains("state=HostKeyMismatch"));
        assert!(!summary.contains("password"));
    }

    #[test]
    fn shell_integration_report_explains_inactive_state() {
        let report = ShellIntegrationReport::from_semantic_diagnostics(&SemanticDiagnostics {
            mode: IntegrationMode::EscapeSequences,
            shell_detected: Some("bash".to_owned()),
            integration_active: false,
            last_event: None,
            last_event_age: None,
            command_block_confidence: CommandBlockConfidence::None,
            remote_integration_active: false,
            heuristic_mode: false,
        });

        assert_eq!(report.shell_detected.as_deref(), Some("bash"));
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.kind == ShellIntegrationWarningKind::Inactive)
        );
        assert!(report.render_text().contains("shell=bash"));
    }

    #[test]
    fn parity_matrix_uses_contract_statuses() {
        let matrix = feature_parity_matrix();

        assert!(matrix.iter().any(|row| row.feature == "SSH"));
        assert!(matrix.iter().any(|row| row.feature == "OSC clipboard"));
        assert!(matrix.iter().all(|row| !row.notes.is_empty()));
    }

    #[test]
    fn linux_compositor_targets_cover_x11_and_wayland() {
        let targets = linux_compositor_targets();

        assert!(
            targets.iter().any(|target| target.key == "gnome-xorg"
                && target.display_server == LinuxDisplayServer::X11)
        );
        assert!(
            targets.iter().any(|target| target.key == "sway"
                && target.display_server == LinuxDisplayServer::Wayland)
        );
        assert!(targets.iter().any(|target| target.key == "hyprland"
            && target.display_server == LinuxDisplayServer::Wayland));
        assert!(targets.iter().all(|target| !target.notes.is_empty()));
    }

    #[test]
    fn linux_runtime_snapshot_is_honest_off_linux() {
        let snapshot =
            LinuxCompositorRuntimeSnapshot::from_env("windows", None, None, None, None, None, None);

        assert_eq!(snapshot.detected_backend, None);
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("not Linux"))
        );
    }

    #[test]
    fn linux_runtime_snapshot_detects_wayland_env() {
        let snapshot = LinuxCompositorRuntimeSnapshot::from_env(
            "linux",
            Some("wayland".to_owned()),
            Some("sway".to_owned()),
            None,
            Some("wayland-1".to_owned()),
            None,
            None,
        );

        assert_eq!(
            snapshot.detected_backend,
            Some(DesktopPlatform::LinuxWayland)
        );
        assert_eq!(snapshot.compositor_label(), "sway");
        assert!(snapshot.warnings.is_empty());
    }

    #[test]
    fn linux_compositor_report_names_fallback_features() {
        let report = linux_compositor_verification_report().render_text();

        assert!(report.contains("Panea Linux compositor verification"));
        assert!(report.contains("frameless window mode"));
        assert!(report.contains("decorations fallback"));
    }

    #[test]
    fn doctor_reports_config_and_ssh_without_secrets() {
        let config = AppConfig {
            renderer: config_core::RendererConfig {
                backend: RendererBackendPreference::Dx12,
                ..config_core::RendererConfig::default()
            },
            ssh_profiles: vec![SshProfile {
                name: "prod".to_owned(),
                host: "example.com".to_owned(),
                auth_method: SshAuthMethod::Password,
                known_hosts_policy: SshKnownHostsPolicy::Ask,
                ..SshProfile::default()
            }],
            ..AppConfig::default()
        };
        let input = DoctorInput {
            app_version: "0.1.0".to_owned(),
            config_source: "default".to_owned(),
            config,
            config_diagnostics: Vec::new(),
            platform: PlatformSnapshot {
                platform: ConfigPlatform::Windows,
                os: "windows".to_owned(),
                arch: "x86_64".to_owned(),
                linux_backend: Some(DesktopPlatform::Windows),
                compositor_or_desktop: None,
                dpi_behavior: DpiBehavior::PerMonitor,
                known_fallbacks: Vec::new(),
            },
            runtime: DoctorRuntimeSnapshot::default(),
            recent_errors: Vec::new(),
        };

        let text = doctor_report(&input, DoctorTopic::All).render_text();

        assert!(text.contains("backend_preference=Dx12"));
        assert!(text.contains("profile=prod"));
        assert!(!text.to_ascii_lowercase().contains("secret"));
    }

    #[test]
    fn doctor_topics_cover_installed_command_aliases() {
        assert_eq!(
            DoctorTopic::parse("shell"),
            Some(DoctorTopic::ShellIntegration)
        );
        assert_eq!(DoctorTopic::parse("fonts"), Some(DoctorTopic::Fonts));
        assert_eq!(
            DoctorTopic::parse("clipboard"),
            Some(DoctorTopic::Clipboard)
        );
        assert_eq!(
            DoctorTopic::parse("notifications"),
            Some(DoctorTopic::Notifications)
        );
        assert!(DoctorTopic::parse("unknown").is_none());
    }

    #[test]
    fn window_doctor_reports_dormant_fullscreen_titlebar() {
        let mut input = DoctorInput {
            app_version: "0.1.0".to_owned(),
            config_source: "default".to_owned(),
            config: AppConfig::default(),
            config_diagnostics: Vec::new(),
            platform: PlatformSnapshot {
                platform: ConfigPlatform::Windows,
                os: "windows".to_owned(),
                arch: "x86_64".to_owned(),
                linux_backend: Some(DesktopPlatform::Windows),
                compositor_or_desktop: None,
                dpi_behavior: DpiBehavior::PerMonitor,
                known_fallbacks: Vec::new(),
            },
            runtime: DoctorRuntimeSnapshot::default(),
            recent_errors: Vec::new(),
        };
        input.config.window.fullscreen_titlebar.enabled = true;

        let report = doctor_report(&input, DoctorTopic::Window);
        let text = report.render_text();
        assert!(text.contains("fullscreen_titlebar enabled=true"));
        assert!(text.contains("activates only in borderless_fullscreen"));
    }

    #[test]
    fn doctor_json_is_machine_readable_and_escaped() {
        let report = DoctorReport {
            topic: DoctorTopic::Clipboard,
            lines: vec!["clipboard:".to_owned(), "value=\"quoted\"".to_owned()],
            findings: vec![DoctorFinding {
                severity: DoctorSeverity::Warning,
                area: "clipboard",
                message: "remote\nwrite".to_owned(),
            }],
        };

        let json = report.render_json();

        assert!(json.contains("\"topic\":\"clipboard\""));
        assert!(json.contains("value=\\\"quoted\\\""));
        assert!(json.contains("remote\\nwrite"));
    }

    #[test]
    fn doctor_fonts_and_clipboard_reports_use_runtime_snapshot() {
        let input = DoctorInput {
            app_version: "0.1.0".to_owned(),
            config_source: "default".to_owned(),
            config: AppConfig::default(),
            config_diagnostics: Vec::new(),
            platform: PlatformSnapshot::detect(),
            runtime: DoctorRuntimeSnapshot {
                font_discovery: "primary:monospace=unresolved".to_owned(),
                clipboard_provider: "system unavailable".to_owned(),
                ..DoctorRuntimeSnapshot::default()
            },
            recent_errors: Vec::new(),
        };

        let fonts = doctor_report(&input, DoctorTopic::Fonts).render_text();
        let clipboard = doctor_report(&input, DoctorTopic::Clipboard).render_text();

        assert!(fonts.contains("discovery=primary:monospace=unresolved"));
        assert!(fonts.contains("could not be resolved"));
        assert!(clipboard.contains("provider=system unavailable"));
    }

    #[test]
    fn doctor_notifications_reports_config_and_provider_fallback() {
        let input = DoctorInput {
            app_version: "0.1.0".to_owned(),
            config_source: "default".to_owned(),
            config: AppConfig::default(),
            config_diagnostics: Vec::new(),
            platform: PlatformSnapshot::detect(),
            runtime: DoctorRuntimeSnapshot {
                notification_provider: "freedesktop unavailable".to_owned(),
                ..DoctorRuntimeSnapshot::default()
            },
            recent_errors: Vec::new(),
        };

        let report = doctor_report(&input, DoctorTopic::Notifications);
        let text = report.render_text();
        assert!(text.contains("only_when_unfocused=true"));
        assert!(text.contains("freedesktop unavailable"));
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.area == "notifications")
        );
    }

    #[test]
    fn diagnostics_provider_exposes_doctor_and_privacy_snapshot() {
        let input = DoctorInput {
            app_version: "0.1.0".to_owned(),
            config_source: "default".to_owned(),
            config: AppConfig::default(),
            config_diagnostics: Vec::new(),
            platform: PlatformSnapshot::detect(),
            runtime: DoctorRuntimeSnapshot::default(),
            recent_errors: Vec::new(),
        };
        let provider = StaticDiagnosticsProvider::new(input);

        let doctor = provider.doctor_report(DoctorTopic::Config).render_text();
        let bug_report = provider.bug_report_snapshot().render_text();

        assert!(doctor.contains("config:"));
        assert!(bug_report.contains("terminal contents"));
        assert!(bug_report.contains("secrets"));
    }

    #[test]
    fn bug_report_snapshot_excludes_terminal_content() {
        let input = DoctorInput {
            app_version: "0.1.0".to_owned(),
            config_source: "default".to_owned(),
            config: AppConfig::default(),
            config_diagnostics: Vec::new(),
            platform: PlatformSnapshot {
                platform: ConfigPlatform::LinuxWayland,
                os: "linux".to_owned(),
                arch: "x86_64".to_owned(),
                linux_backend: Some(DesktopPlatform::LinuxWayland),
                compositor_or_desktop: Some("sway".to_owned()),
                dpi_behavior: DpiBehavior::FractionalScale,
                known_fallbacks: Vec::new(),
            },
            runtime: DoctorRuntimeSnapshot::default(),
            recent_errors: vec!["render surface lost".to_owned()],
        };

        let text = BugReportSnapshot::from_doctor_input(&input).render_text();

        assert!(text.contains("terminal contents"));
        assert!(text.contains("recent_errors: 1"));
        assert!(!text.contains("render surface lost"));
    }

    #[test]
    fn hardening_report_names_remaining_renderer_and_reload_work() {
        let input = DoctorInput {
            app_version: "0.1.0".to_owned(),
            config_source: "default".to_owned(),
            config: AppConfig::default(),
            config_diagnostics: Vec::new(),
            platform: PlatformSnapshot::detect(),
            runtime: DoctorRuntimeSnapshot::default(),
            recent_errors: Vec::new(),
        };

        let text = stability_hardening_report(&input).render_text();

        assert!(text.contains("panic boundaries"));
        assert!(text.contains("device-loss"));
        assert!(text.contains("config reload"));
    }

    #[test]
    fn security_review_reports_native_keychain_and_osc_confirmation() {
        let input = DoctorInput {
            app_version: "0.1.0".to_owned(),
            config_source: "default".to_owned(),
            config: AppConfig::default(),
            config_diagnostics: Vec::new(),
            platform: PlatformSnapshot::detect(),
            runtime: DoctorRuntimeSnapshot::default(),
            recent_errors: Vec::new(),
        };

        let report = security_review_report(&input);
        let text = report.render_text();

        assert!(!report.has_blockers());
        assert!(text.contains("native OS keychain"));
        assert!(text.contains("masked desktop prompts"));
        assert!(text.contains("OSC 52"));
        assert!(text.contains("explicit one-time confirmation"));
    }

    #[test]
    fn packaging_plan_lists_all_required_desktop_targets() {
        let text = packaging_plan().render_text();

        assert!(text.contains("macOS app bundle"));
        assert!(text.contains("Windows installer"));
        assert!(text.contains("Linux portable tarball"));
        assert!(text.contains("Linux distro packages"));
    }

    #[test]
    fn release_validation_blocks_platform_parity_until_verified() {
        let input = DoctorInput {
            app_version: "0.1.0".to_owned(),
            config_source: "default".to_owned(),
            config: AppConfig::default(),
            config_diagnostics: Vec::new(),
            platform: PlatformSnapshot::detect(),
            runtime: DoctorRuntimeSnapshot::default(),
            recent_errors: Vec::new(),
        };

        let report = release_validation_report(&input);

        assert!(report.has_blockers());
        assert!(report.render_text().contains("Linux Wayland"));
    }

    #[test]
    fn ios_readiness_blocks_native_shell_and_renderer() {
        let report = ios_companion_readiness_report();
        let text = report.render_text();

        assert!(report.has_blockers());
        assert!(text.contains("shared engine"));
        assert!(text.contains("UIKit/SwiftUI"));
        assert!(text.contains("GPU surface"));
    }
}
