//! Portable internal configuration model.

pub const LAYER: &str = "config portability";

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub window: WindowConfig,
    pub renderer: RendererConfig,
    pub font: FontConfig,
    pub colors: ColorConfig,
    pub cursor: CursorConfig,
    pub command_blocks: CommandBlocksConfig,
    pub prompt_decorations: PromptDecorationsConfig,
    pub keyboard: KeyboardConfig,
    pub mouse: MouseConfig,
    pub shell_profiles: Vec<ShellProfile>,
    pub ssh_profiles: Vec<SshProfile>,
    pub mux: MuxConfig,
    pub performance: PerformanceConfig,
    pub platform_overrides: PlatformOverrides,
    pub diagnostics: DiagnosticsConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowConfig {
    pub title: String,
    pub columns: u16,
    pub rows: u16,
    pub opacity: f32,
    pub fullscreen: bool,
    pub frameless: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Panea".to_owned(),
            columns: 100,
            rows: 32,
            opacity: 1.0,
            fullscreen: false,
            frameless: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RendererBackendPreference {
    Auto,
    Vulkan,
    Metal,
    Dx12,
    Gl,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RendererConfig {
    pub backend: RendererBackendPreference,
    pub vsync: bool,
    pub damage_tracking: bool,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            backend: RendererBackendPreference::Auto,
            vsync: true,
            damage_tracking: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
    pub fallback_families: Vec<String>,
    pub ligatures: bool,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "monospace".to_owned(),
            size: 13.0,
            fallback_families: Vec::new(),
            ligatures: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RgbaColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl RgbaColor {
    #[must_use]
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: u8::MAX,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorConfig {
    pub foreground: RgbaColor,
    pub background: RgbaColor,
    pub palette: Vec<RgbaColor>,
}

impl Default for ColorConfig {
    fn default() -> Self {
        Self {
            foreground: RgbaColor::rgb(230, 230, 230),
            background: RgbaColor::rgb(12, 12, 12),
            palette: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorConfig {
    pub shape: CursorShape,
    pub blink: bool,
    pub animations_enabled: bool,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blink: true,
            animations_enabled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandBlocksConfig {
    pub enabled: bool,
    pub show_duration: bool,
    pub show_exit_status: bool,
}

impl Default for CommandBlocksConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            show_duration: true,
            show_exit_status: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PromptDecorationsConfig {
    pub enabled: bool,
    pub show_current_directory: bool,
    pub show_remote_host: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct KeyboardConfig {
    pub keybindings: Vec<KeyBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    pub keys: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MouseConfig {
    pub bindings: Vec<MouseBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseBinding {
    pub gesture: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellProfile {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshProfile {
    pub name: String,
    pub host: String,
    pub user: Option<String>,
    pub port: u16,
    pub identity_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuxConfig {
    pub enabled: bool,
    pub restore_sessions: bool,
}

impl Default for MuxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            restore_sessions: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PerformanceProfile {
    MaximumPerformance,
    Balanced,
    Visual,
    BatteryConscious,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerformanceConfig {
    pub profile: PerformanceProfile,
    pub frame_rate_limit: Option<u16>,
    pub expensive_effect_warnings: bool,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            profile: PerformanceProfile::Balanced,
            frame_rate_limit: None,
            expensive_effect_warnings: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlatformOverrides {
    pub macos: Option<PlatformOverride>,
    pub linux_x11: Option<PlatformOverride>,
    pub linux_wayland: Option<PlatformOverride>,
    pub windows: Option<PlatformOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlatformOverride {
    pub shell_profile: Option<String>,
    pub renderer_backend: Option<String>,
    pub window_decorations: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsConfig {
    pub enabled: bool,
    pub performance_overlay: bool,
    pub capability_report: bool,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            performance_overlay: false,
            capability_report: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_config_round_trips_through_toml() {
        let config = AppConfig::default();

        let serialized = toml::to_string(&config).expect("config should serialize");
        let deserialized: AppConfig =
            toml::from_str(&serialized).expect("config should deserialize");

        assert_eq!(deserialized, config);
    }

    #[test]
    fn config_core_only_depends_on_serde() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            manifest.contains("serde.workspace = true"),
            "config-core must expose a serializable portable config contract"
        );
        assert!(
            !manifest.contains("render-")
                && !manifest.contains("platform-")
                && !manifest.contains("transport-")
                && !manifest.contains("term-core"),
            "config-core must not import runtime layer crates in the skeleton phase"
        );
    }
}
