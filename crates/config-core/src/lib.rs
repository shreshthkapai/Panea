//! Portable internal configuration model.

pub const LAYER: &str = "config portability";

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppConfig {
    pub window: WindowConfig,
    pub renderer: RendererConfig,
    #[serde(alias = "fonts")]
    pub font: FontConfig,
    pub colors: ColorConfig,
    pub cursor: CursorConfig,
    pub scrollback: ScrollbackConfig,
    pub command_blocks: CommandBlocksConfig,
    pub prompt_decorations: PromptDecorationsConfig,
    pub keyboard: KeyboardConfig,
    pub mouse: MouseConfig,
    pub paste: PasteConfig,
    pub default_shell_profile: Option<String>,
    pub shell_profiles: Vec<ShellProfile>,
    pub ssh_profiles: Vec<SshProfile>,
    pub mux: MuxConfig,
    pub performance: PerformanceConfig,
    #[serde(rename = "platform", alias = "platform_overrides")]
    pub platform_overrides: PlatformOverrides,
    pub diagnostics: DiagnosticsConfig,
}

impl AppConfig {
    #[must_use]
    pub fn resolved_for_platform(&self, platform: ConfigPlatform) -> Self {
        let mut resolved = self.clone();
        if let Some(override_config) = self.platform_overrides.for_platform(platform) {
            override_config.apply_to(&mut resolved);
        }
        resolved
    }

    #[must_use]
    pub fn validate(&self) -> ValidationReport {
        let mut report = ValidationReport::default();

        if self.window.title.trim().is_empty() {
            report.error("window.title", "window title cannot be empty");
        }
        if self.window.columns == 0 || self.window.rows == 0 {
            report.error(
                "window",
                "terminal columns and rows must be greater than zero",
            );
        }
        if self.window.initial_width < 160 || self.window.initial_height < 120 {
            report.warning(
                "window",
                "initial size is very small and may make recovery controls hard to reach",
            );
        }
        if !(0.2..=1.0).contains(&self.window.opacity) {
            report.error("window.opacity", "opacity must be between 0.2 and 1.0");
        }
        if matches!(
            self.window.mode,
            WindowModeConfig::FramelessFullscreen | WindowModeConfig::FramelessWindowed
        ) && !self
            .keyboard
            .keybindings
            .iter()
            .any(|binding| binding.action == "restore_window_decorations")
        {
            report.error(
                "keyboard.keybindings",
                "frameless modes require a restore_window_decorations keybinding",
            );
        }

        if self.font.family.trim().is_empty() {
            report.error("font.family", "font family cannot be empty");
        }
        for (index, family) in self.font.fallback_families.iter().enumerate() {
            if family.trim().is_empty() {
                report.error(
                    format!("font.fallback_families[{index}]"),
                    "fallback font family cannot be empty",
                );
            }
        }
        if !(6.0..=72.0).contains(&self.font.size) {
            report.error("font.size", "font size must be between 6 and 72 points");
        }
        if !(0.75..=3.0).contains(&self.font.line_height) {
            report.error(
                "font.line_height",
                "line height must be between 0.75 and 3.0",
            );
        }

        if self.colors.palette.len() != 16 && !self.colors.palette.is_empty() {
            report.warning(
                "colors.palette",
                "palette should be empty for built-in defaults or contain exactly 16 ANSI colors",
            );
        }

        if self.cursor.blink_interval_ms < 150 || self.cursor.blink_interval_ms > 2000 {
            report.error(
                "cursor.blink_interval_ms",
                "cursor blink interval must be between 150 and 2000 ms",
            );
        }
        if self.cursor.thickness < 0.05 || self.cursor.thickness > 1.0 {
            report.error(
                "cursor.thickness",
                "cursor thickness must be between 0.05 and 1.0",
            );
        }

        if self.scrollback.lines > 1_000_000 {
            report.warning(
                "scrollback.lines",
                "very large scrollback can consume substantial memory",
            );
        }

        self.validate_keybindings(&mut report);
        self.validate_shell_profiles(&mut report);
        self.validate_ssh_profiles(&mut report);
        self.validate_performance(&mut report);
        self.validate_platform_overrides(&mut report);

        report
    }

    #[must_use]
    pub fn reload_plan_from(&self, next: &Self) -> ReloadPlan {
        let mut plan = ReloadPlan::default();

        if self.colors != next.colors {
            plan.live.push(ReloadableSection::Colors);
        }
        if self.font != next.font {
            plan.live.push(ReloadableSection::Font);
        }
        if self.cursor != next.cursor {
            plan.live.push(ReloadableSection::Cursor);
        }
        if self.window.padding_x != next.window.padding_x
            || self.window.padding_y != next.window.padding_y
        {
            plan.live.push(ReloadableSection::WindowPadding);
        }
        if self.keyboard != next.keyboard {
            plan.live.push(ReloadableSection::Keybindings);
        }
        if self.mouse != next.mouse || self.paste != next.paste {
            plan.live.push(ReloadableSection::Input);
        }
        if self.command_blocks != next.command_blocks
            || self.prompt_decorations != next.prompt_decorations
        {
            plan.live.push(ReloadableSection::VisualSemantics);
        }
        if self.diagnostics != next.diagnostics {
            plan.live.push(ReloadableSection::Diagnostics);
        }

        if self.renderer.backend != next.renderer.backend {
            plan.restart_required.push(RestartRequiredChange {
                path: "renderer.backend".to_owned(),
                reason: "GPU backend changes require renderer reinitialization".to_owned(),
            });
        }
        if self.window.linux_backend != next.window.linux_backend {
            plan.restart_required.push(RestartRequiredChange {
                path: "window.linux_backend".to_owned(),
                reason: "major window backend changes require a new event loop".to_owned(),
            });
        }
        if self.shell_profiles != next.shell_profiles
            || self.default_shell_profile != next.default_shell_profile
        {
            plan.restart_required.push(RestartRequiredChange {
                path: "shell_profiles".to_owned(),
                reason: "shell profile startup settings only affect new sessions".to_owned(),
            });
        }
        if self.ssh_profiles != next.ssh_profiles {
            plan.restart_required.push(RestartRequiredChange {
                path: "ssh_profiles".to_owned(),
                reason: "SSH profile changes only affect new sessions".to_owned(),
            });
        }
        if self.platform_overrides != next.platform_overrides {
            plan.restart_required.push(RestartRequiredChange {
                path: "platform".to_owned(),
                reason: "platform override changes may affect startup-only choices".to_owned(),
            });
        }

        plan.live.sort();
        plan.live.dedup();
        plan
    }

    fn validate_keybindings(&self, report: &mut ValidationReport) {
        let mut seen = BTreeMap::<String, String>::new();
        for binding in &self.keyboard.keybindings {
            let keys = binding.keys.trim();
            let action = binding.action.trim();
            if keys.is_empty() {
                report.error("keyboard.keybindings", "keybinding keys cannot be empty");
            }
            if action.is_empty() {
                report.error("keyboard.keybindings", "keybinding action cannot be empty");
            }
            if let Some(previous_action) = seen.insert(keys.to_ascii_lowercase(), action.to_owned())
            {
                report.error(
                    "keyboard.keybindings",
                    format!("keybinding conflict for {keys}: {previous_action} and {action}"),
                );
            }
        }
    }

    fn validate_shell_profiles(&self, report: &mut ValidationReport) {
        let mut names = BTreeSet::new();
        for profile in &self.shell_profiles {
            if profile.name.trim().is_empty() {
                report.error("shell_profiles", "shell profile name cannot be empty");
            }
            if !names.insert(profile.name.clone()) {
                report.error(
                    "shell_profiles",
                    format!("duplicate shell profile name '{}'", profile.name),
                );
            }
            if profile.program.trim().is_empty() && matches!(profile.kind, ShellProfileKind::Custom)
            {
                report.error(
                    format!("shell_profiles.{}", profile.name),
                    "custom shell profile program cannot be empty",
                );
            }
            if profile.startup_command.is_some() && !profile.args.is_empty() {
                report.warning(
                    format!("shell_profiles.{}", profile.name),
                    "startup_command combined with args may not be portable across shells",
                );
            }
        }

        if let Some(default_shell_profile) = &self.default_shell_profile
            && !names.contains(default_shell_profile)
        {
            report.error(
                "default_shell_profile",
                format!("default shell profile '{default_shell_profile}' does not exist"),
            );
        }
    }

    fn validate_ssh_profiles(&self, report: &mut ValidationReport) {
        let mut names = BTreeSet::new();
        for profile in &self.ssh_profiles {
            if profile.name.trim().is_empty() {
                report.error("ssh_profiles", "SSH profile name cannot be empty");
            }
            if !names.insert(profile.name.clone()) {
                report.error(
                    "ssh_profiles",
                    format!("duplicate SSH profile name '{}'", profile.name),
                );
            }
            if profile.host.trim().is_empty() {
                report.error(
                    format!("ssh_profiles.{}", profile.name),
                    "SSH host cannot be empty",
                );
            }
            if profile.port == 0 {
                report.error(
                    format!("ssh_profiles.{}", profile.name),
                    "SSH port must be greater than zero",
                );
            }
        }
    }

    fn validate_performance(&self, report: &mut ValidationReport) {
        if let Some(limit) = self.performance.frame_rate_limit
            && !(15..=360).contains(&limit)
        {
            report.error(
                "performance.frame_rate_limit",
                "frame rate limit must be between 15 and 360 FPS",
            );
        }
        if self.performance.glyph_cache_entries < 512 {
            report.warning(
                "performance.glyph_cache_entries",
                "small glyph caches can cause avoidable rerasterization",
            );
        }
        if self.performance.max_frame_time_ms == 0 || self.performance.max_frame_time_ms > 100 {
            report.error(
                "performance.max_frame_time_ms",
                "max frame time budget must be between 1 and 100 ms",
            );
        }
    }

    fn validate_platform_overrides(&self, report: &mut ValidationReport) {
        for (name, platform_override) in self.platform_overrides.entries() {
            if let Some(shell_profile) = &platform_override.default_shell_profile {
                let exists = self
                    .shell_profiles
                    .iter()
                    .any(|profile| &profile.name == shell_profile);
                if !exists {
                    report.error(
                        format!("platform.{name}.default_shell_profile"),
                        format!("shell profile '{shell_profile}' does not exist"),
                    );
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    pub title: String,
    pub columns: u16,
    pub rows: u16,
    pub initial_width: u32,
    pub initial_height: u32,
    pub padding_x: u16,
    pub padding_y: u16,
    pub opacity: f64,
    pub mode: WindowModeConfig,
    pub linux_backend: LinuxBackendConfig,
    pub decoration_strategy: DecorationStrategyConfig,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Panea".to_owned(),
            columns: 100,
            rows: 32,
            initial_width: 960,
            initial_height: 560,
            padding_x: 8,
            padding_y: 6,
            opacity: 1.0,
            mode: WindowModeConfig::Windowed,
            linux_backend: LinuxBackendConfig::Auto,
            decoration_strategy: DecorationStrategyConfig::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WindowModeConfig {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
    BorderlessFullscreen,
    FramelessWindowed,
    FramelessFullscreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LinuxBackendConfig {
    #[default]
    Auto,
    X11,
    Wayland,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DecorationStrategyConfig {
    #[default]
    Auto,
    Native,
    ClientSide,
    Custom,
    None,
    FallbackDecorated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RendererBackendPreference {
    #[default]
    Auto,
    Vulkan,
    Metal,
    Dx12,
    Gl,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RendererConfig {
    pub backend: RendererBackendPreference,
    pub vsync: bool,
    pub damage_tracking: bool,
    pub present_mode: PresentModePreference,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            backend: RendererBackendPreference::Auto,
            vsync: true,
            damage_tracking: true,
            present_mode: PresentModePreference::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PresentModePreference {
    #[default]
    Auto,
    Fifo,
    Mailbox,
    Immediate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    pub family: String,
    pub size: f64,
    pub line_height: f64,
    pub fallback_families: Vec<String>,
    pub ligatures: bool,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "monospace".to_owned(),
            size: 13.0,
            line_height: 1.2,
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
#[serde(default)]
pub struct ColorConfig {
    pub foreground: RgbaColor,
    pub background: RgbaColor,
    pub cursor: RgbaColor,
    pub selection_background: RgbaColor,
    pub palette: Vec<RgbaColor>,
}

impl Default for ColorConfig {
    fn default() -> Self {
        Self {
            foreground: RgbaColor::rgb(230, 230, 230),
            background: RgbaColor::rgb(12, 12, 12),
            cursor: RgbaColor::rgb(235, 235, 235),
            selection_background: RgbaColor {
                red: 80,
                green: 150,
                blue: 255,
                alpha: 96,
            },
            palette: default_ansi_palette(),
        }
    }
}

#[must_use]
pub fn default_ansi_palette() -> Vec<RgbaColor> {
    vec![
        RgbaColor::rgb(12, 12, 12),
        RgbaColor::rgb(197, 15, 31),
        RgbaColor::rgb(19, 161, 14),
        RgbaColor::rgb(193, 156, 0),
        RgbaColor::rgb(0, 55, 218),
        RgbaColor::rgb(136, 23, 152),
        RgbaColor::rgb(58, 150, 221),
        RgbaColor::rgb(204, 204, 204),
        RgbaColor::rgb(118, 118, 118),
        RgbaColor::rgb(231, 72, 86),
        RgbaColor::rgb(22, 198, 12),
        RgbaColor::rgb(249, 241, 165),
        RgbaColor::rgb(59, 120, 255),
        RgbaColor::rgb(180, 0, 158),
        RgbaColor::rgb(97, 214, 214),
        RgbaColor::rgb(242, 242, 242),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    #[default]
    Block,
    Beam,
    Underline,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CursorConfig {
    pub shape: CursorShape,
    pub blink: bool,
    pub blink_interval_ms: u16,
    pub thickness: f64,
    pub animations_enabled: bool,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blink: true,
            blink_interval_ms: 600,
            thickness: 0.15,
            animations_enabled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScrollbackConfig {
    pub lines: usize,
    pub preserve_on_resize: bool,
}

impl Default for ScrollbackConfig {
    fn default() -> Self {
        Self {
            lines: 10_000,
            preserve_on_resize: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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
#[serde(default)]
pub struct PromptDecorationsConfig {
    pub enabled: bool,
    pub show_current_directory: bool,
    pub show_remote_host: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyboardConfig {
    pub keybindings: Vec<KeyBinding>,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            keybindings: vec![
                KeyBinding::new("Ctrl+Shift+C", "copy"),
                KeyBinding::new("Ctrl+Shift+V", "paste"),
                KeyBinding::new("Ctrl+Shift+F", "toggle_fullscreen"),
                KeyBinding::new("Ctrl+Shift+D", "restore_window_decorations"),
                KeyBinding::new("Ctrl+Shift+M", "toggle_frameless"),
                KeyBinding::new("Ctrl+Shift+W", "close_window"),
                KeyBinding::new("Ctrl+Shift+P", "open_command_palette_later"),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    pub keys: String,
    pub action: String,
}

impl KeyBinding {
    #[must_use]
    pub fn new(keys: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            keys: keys.into(),
            action: action.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MouseConfig {
    pub bindings: Vec<MouseBinding>,
    pub copy_on_select: bool,
    pub hide_cursor_when_typing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseBinding {
    pub gesture: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PasteConfig {
    pub bracketed_paste: bool,
    pub normalize_newlines: bool,
    pub strip_control_characters: bool,
}

impl Default for PasteConfig {
    fn default() -> Self {
        Self {
            bracketed_paste: true,
            normalize_newlines: true,
            strip_control_characters: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellProfile {
    pub name: String,
    pub kind: ShellProfileKind,
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub working_directory: Option<String>,
    pub startup_command: Option<String>,
    pub platform_overrides: ShellProfilePlatformOverrides,
}

impl Default for ShellProfile {
    fn default() -> Self {
        Self {
            name: "default".to_owned(),
            kind: ShellProfileKind::Default,
            program: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            working_directory: None,
            startup_command: None,
            platform_overrides: ShellProfilePlatformOverrides::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShellProfileKind {
    #[default]
    Default,
    PowerShell,
    Cmd,
    Wsl,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ShellProfilePlatformOverrides {
    pub macos: Option<ShellProfileOverride>,
    pub linux: Option<ShellProfileOverride>,
    pub linux_x11: Option<ShellProfileOverride>,
    pub linux_wayland: Option<ShellProfileOverride>,
    pub windows: Option<ShellProfileOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ShellProfileOverride {
    pub program: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub working_directory: Option<String>,
    pub startup_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshProfile {
    pub name: String,
    pub host: String,
    pub user: Option<String>,
    pub port: u16,
    pub identity_file: Option<String>,
}

impl Default for SshProfile {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            user: None,
            port: 22,
            identity_file: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceProfile {
    MaximumPerformance,
    #[default]
    Balanced,
    Visual,
    BatteryConscious,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceConfig {
    pub profile: PerformanceProfile,
    pub frame_rate_limit: Option<u16>,
    pub glyph_cache_entries: usize,
    pub max_frame_time_ms: u16,
    pub expensive_effect_warnings: bool,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            profile: PerformanceProfile::Balanced,
            frame_rate_limit: None,
            glyph_cache_entries: 8192,
            max_frame_time_ms: 16,
            expensive_effect_warnings: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PlatformOverrides {
    pub macos: Option<PlatformOverride>,
    pub linux: Option<PlatformOverride>,
    pub windows: Option<PlatformOverride>,
    pub linux_x11: Option<PlatformOverride>,
    pub linux_wayland: Option<PlatformOverride>,
}

impl PlatformOverrides {
    #[must_use]
    pub fn for_platform(&self, platform: ConfigPlatform) -> Option<&PlatformOverride> {
        match platform {
            ConfigPlatform::MacOs => self.macos.as_ref(),
            ConfigPlatform::Windows => self.windows.as_ref(),
            ConfigPlatform::Linux => self.linux.as_ref(),
            ConfigPlatform::LinuxX11 => self.linux_x11.as_ref().or(self.linux.as_ref()),
            ConfigPlatform::LinuxWayland => self.linux_wayland.as_ref().or(self.linux.as_ref()),
            ConfigPlatform::Unknown => None,
        }
    }

    fn entries(&self) -> Vec<(&'static str, &PlatformOverride)> {
        let mut entries = Vec::new();
        if let Some(entry) = &self.macos {
            entries.push(("macos", entry));
        }
        if let Some(entry) = &self.linux {
            entries.push(("linux", entry));
        }
        if let Some(entry) = &self.windows {
            entries.push(("windows", entry));
        }
        if let Some(entry) = &self.linux_x11 {
            entries.push(("linux.x11", entry));
        }
        if let Some(entry) = &self.linux_wayland {
            entries.push(("linux.wayland", entry));
        }
        entries
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PlatformOverride {
    pub default_shell_profile: Option<String>,
    pub window: Option<WindowConfigPatch>,
    pub renderer: Option<RendererConfigPatch>,
    pub font: Option<FontConfigPatch>,
    pub performance: Option<PerformanceConfigPatch>,
    pub diagnostics: Option<DiagnosticsConfigPatch>,
}

impl PlatformOverride {
    fn apply_to(&self, config: &mut AppConfig) {
        if let Some(default_shell_profile) = &self.default_shell_profile {
            config.default_shell_profile = Some(default_shell_profile.clone());
        }
        if let Some(window) = &self.window {
            window.apply_to(&mut config.window);
        }
        if let Some(renderer) = &self.renderer {
            renderer.apply_to(&mut config.renderer);
        }
        if let Some(font) = &self.font {
            font.apply_to(&mut config.font);
        }
        if let Some(performance) = &self.performance {
            performance.apply_to(&mut config.performance);
        }
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.apply_to(&mut config.diagnostics);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WindowConfigPatch {
    pub title: Option<String>,
    pub columns: Option<u16>,
    pub rows: Option<u16>,
    pub initial_width: Option<u32>,
    pub initial_height: Option<u32>,
    pub padding_x: Option<u16>,
    pub padding_y: Option<u16>,
    pub mode: Option<WindowModeConfig>,
    pub linux_backend: Option<LinuxBackendConfig>,
    pub decoration_strategy: Option<DecorationStrategyConfig>,
}

impl WindowConfigPatch {
    fn apply_to(&self, config: &mut WindowConfig) {
        apply_opt(&mut config.title, &self.title);
        apply_opt(&mut config.columns, &self.columns);
        apply_opt(&mut config.rows, &self.rows);
        apply_opt(&mut config.initial_width, &self.initial_width);
        apply_opt(&mut config.initial_height, &self.initial_height);
        apply_opt(&mut config.padding_x, &self.padding_x);
        apply_opt(&mut config.padding_y, &self.padding_y);
        apply_opt(&mut config.mode, &self.mode);
        apply_opt(&mut config.linux_backend, &self.linux_backend);
        apply_opt(&mut config.decoration_strategy, &self.decoration_strategy);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RendererConfigPatch {
    pub backend: Option<RendererBackendPreference>,
    pub vsync: Option<bool>,
    pub damage_tracking: Option<bool>,
    pub present_mode: Option<PresentModePreference>,
}

impl RendererConfigPatch {
    fn apply_to(&self, config: &mut RendererConfig) {
        apply_opt(&mut config.backend, &self.backend);
        apply_opt(&mut config.vsync, &self.vsync);
        apply_opt(&mut config.damage_tracking, &self.damage_tracking);
        apply_opt(&mut config.present_mode, &self.present_mode);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FontConfigPatch {
    pub family: Option<String>,
    pub size: Option<f64>,
    pub line_height: Option<f64>,
    pub fallback_families: Option<Vec<String>>,
    pub ligatures: Option<bool>,
}

impl FontConfigPatch {
    fn apply_to(&self, config: &mut FontConfig) {
        apply_opt(&mut config.family, &self.family);
        apply_opt(&mut config.size, &self.size);
        apply_opt(&mut config.line_height, &self.line_height);
        apply_opt(&mut config.fallback_families, &self.fallback_families);
        apply_opt(&mut config.ligatures, &self.ligatures);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PerformanceConfigPatch {
    pub profile: Option<PerformanceProfile>,
    pub frame_rate_limit: Option<Option<u16>>,
    pub glyph_cache_entries: Option<usize>,
    pub max_frame_time_ms: Option<u16>,
    pub expensive_effect_warnings: Option<bool>,
}

impl PerformanceConfigPatch {
    fn apply_to(&self, config: &mut PerformanceConfig) {
        apply_opt(&mut config.profile, &self.profile);
        if let Some(frame_rate_limit) = self.frame_rate_limit {
            config.frame_rate_limit = frame_rate_limit;
        }
        apply_opt(&mut config.glyph_cache_entries, &self.glyph_cache_entries);
        apply_opt(&mut config.max_frame_time_ms, &self.max_frame_time_ms);
        apply_opt(
            &mut config.expensive_effect_warnings,
            &self.expensive_effect_warnings,
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DiagnosticsConfigPatch {
    pub enabled: Option<bool>,
    pub performance_overlay: Option<bool>,
    pub capability_report: Option<bool>,
    pub log_level: Option<LogLevel>,
}

impl DiagnosticsConfigPatch {
    fn apply_to(&self, config: &mut DiagnosticsConfig) {
        apply_opt(&mut config.enabled, &self.enabled);
        apply_opt(&mut config.performance_overlay, &self.performance_overlay);
        apply_opt(&mut config.capability_report, &self.capability_report);
        apply_opt(&mut config.log_level, &self.log_level);
    }
}

fn apply_opt<T: Clone>(target: &mut T, value: &Option<T>) {
    if let Some(value) = value {
        *target = value.clone();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigPlatform {
    MacOs,
    Linux,
    LinuxX11,
    LinuxWayland,
    Windows,
    Unknown,
}

impl ConfigPlatform {
    #[must_use]
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            match std::env::var("WAYLAND_DISPLAY") {
                Ok(value) if !value.is_empty() => Self::LinuxWayland,
                _ => match std::env::var("DISPLAY") {
                    Ok(value) if !value.is_empty() => Self::LinuxX11,
                    _ => Self::Linux,
                },
            }
        } else {
            Self::Unknown
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticsConfig {
    pub enabled: bool,
    pub performance_overlay: bool,
    pub capability_report: bool,
    pub log_level: LogLevel,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            performance_overlay: false,
            capability_report: true,
            log_level: LogLevel::Info,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationReport {
    pub diagnostics: Vec<ConfigDiagnostic>,
}

impl ValidationReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Error)
    }

    fn error(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.diagnostics.push(ConfigDiagnostic {
            severity: ConfigDiagnosticSeverity::Error,
            path: path.into(),
            message: message.into(),
        });
    }

    fn warning(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.diagnostics.push(ConfigDiagnostic {
            severity: ConfigDiagnosticSeverity::Warning,
            path: path.into(),
            message: message.into(),
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDiagnostic {
    pub severity: ConfigDiagnosticSeverity,
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigDiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReloadPlan {
    pub live: Vec<ReloadableSection>,
    pub restart_required: Vec<RestartRequiredChange>,
}

impl ReloadPlan {
    #[must_use]
    pub fn requires_restart(&self) -> bool {
        !self.restart_required.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReloadableSection {
    Colors,
    Cursor,
    Diagnostics,
    Font,
    Input,
    Keybindings,
    VisualSemantics,
    WindowPadding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartRequiredChange {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigSchema {
    pub schema_version: u16,
    pub sections: Vec<ConfigSchemaSection>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigSchemaSection {
    pub name: &'static str,
    pub fields: Vec<ConfigSchemaField>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigSchemaField {
    pub path: &'static str,
    pub value_type: &'static str,
    pub default: String,
    pub live_reload: bool,
    pub restart_required: bool,
}

#[must_use]
pub fn export_schema() -> ConfigSchema {
    let default = AppConfig::default();
    ConfigSchema {
        schema_version: 1,
        sections: vec![
            ConfigSchemaSection {
                name: "window",
                fields: vec![
                    field(
                        "window.title",
                        "string",
                        &default.window.title,
                        false,
                        false,
                    ),
                    field(
                        "window.columns",
                        "integer",
                        default.window.columns,
                        false,
                        false,
                    ),
                    field("window.rows", "integer", default.window.rows, false, false),
                    field(
                        "window.initial_width",
                        "integer",
                        default.window.initial_width,
                        false,
                        false,
                    ),
                    field(
                        "window.initial_height",
                        "integer",
                        default.window.initial_height,
                        false,
                        false,
                    ),
                    field(
                        "window.padding_x",
                        "integer",
                        default.window.padding_x,
                        true,
                        false,
                    ),
                    field(
                        "window.padding_y",
                        "integer",
                        default.window.padding_y,
                        true,
                        false,
                    ),
                    field(
                        "window.mode",
                        "window_mode",
                        format!("{:?}", default.window.mode),
                        false,
                        false,
                    ),
                    field(
                        "window.linux_backend",
                        "linux_backend",
                        format!("{:?}", default.window.linux_backend),
                        false,
                        true,
                    ),
                ],
            },
            ConfigSchemaSection {
                name: "font",
                fields: vec![
                    field("font.family", "string", &default.font.family, true, false),
                    field("font.size", "number", default.font.size, true, false),
                    field(
                        "font.line_height",
                        "number",
                        default.font.line_height,
                        true,
                        false,
                    ),
                    field("font.fallback_families", "array<string>", "[]", true, false),
                ],
            },
            ConfigSchemaSection {
                name: "colors",
                fields: vec![
                    field(
                        "colors.foreground",
                        "rgba",
                        "default foreground",
                        true,
                        false,
                    ),
                    field(
                        "colors.background",
                        "rgba",
                        "default background",
                        true,
                        false,
                    ),
                    field(
                        "colors.palette",
                        "array<rgba>",
                        "16 ANSI colors",
                        true,
                        false,
                    ),
                ],
            },
            ConfigSchemaSection {
                name: "shell_profiles",
                fields: vec![
                    field("default_shell_profile", "string?", "none", false, true),
                    field("shell_profiles", "array<shell_profile>", "[]", false, true),
                ],
            },
            ConfigSchemaSection {
                name: "renderer",
                fields: vec![
                    field(
                        "renderer.backend",
                        "renderer_backend",
                        format!("{:?}", default.renderer.backend),
                        false,
                        true,
                    ),
                    field(
                        "renderer.vsync",
                        "boolean",
                        default.renderer.vsync,
                        false,
                        false,
                    ),
                    field(
                        "renderer.damage_tracking",
                        "boolean",
                        default.renderer.damage_tracking,
                        false,
                        false,
                    ),
                ],
            },
            ConfigSchemaSection {
                name: "performance",
                fields: vec![
                    field(
                        "performance.profile",
                        "performance_profile",
                        format!("{:?}", default.performance.profile),
                        false,
                        false,
                    ),
                    field(
                        "performance.frame_rate_limit",
                        "integer?",
                        "none",
                        false,
                        false,
                    ),
                    field(
                        "performance.glyph_cache_entries",
                        "integer",
                        default.performance.glyph_cache_entries,
                        false,
                        false,
                    ),
                ],
            },
        ],
    }
}

fn field(
    path: &'static str,
    value_type: &'static str,
    default: impl ToString,
    live_reload: bool,
    restart_required: bool,
) -> ConfigSchemaField {
    ConfigSchemaField {
        path,
        value_type,
        default: default.to_string(),
        live_reload,
        restart_required,
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
    fn partial_config_uses_safe_defaults() {
        let config: AppConfig = toml::from_str(
            r#"
            [window]
            title = "Configured"

            [font]
            size = 14.0
            "#,
        )
        .expect("partial config should deserialize");

        assert_eq!(config.window.title, "Configured");
        assert_eq!(config.window.rows, WindowConfig::default().rows);
        assert_eq!(config.font.family, FontConfig::default().family);
        assert_eq!(config.font.size, 14.0);
    }

    #[test]
    fn platform_override_refines_base_config() {
        let config: AppConfig = toml::from_str(
            r#"
            [window]
            title = "Base"

            [platform.windows.window]
            title = "Windows"
            initial_width = 1200
            "#,
        )
        .expect("config should deserialize");

        let resolved = config.resolved_for_platform(ConfigPlatform::Windows);

        assert_eq!(resolved.window.title, "Windows");
        assert_eq!(resolved.window.initial_width, 1200);
        assert_eq!(resolved.window.rows, WindowConfig::default().rows);
    }

    #[test]
    fn validation_reports_conflicts_and_bad_ranges() {
        let mut config = AppConfig::default();
        config.font.size = 2.0;
        config
            .keyboard
            .keybindings
            .push(KeyBinding::new("Ctrl+X", "a"));
        config
            .keyboard
            .keybindings
            .push(KeyBinding::new("ctrl+x", "b"));

        let report = config.validate();

        assert!(report.has_errors());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path == "font.size")
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("keybinding conflict"))
        );
    }

    #[test]
    fn reload_plan_distinguishes_live_and_restart_changes() {
        let mut next = AppConfig::default();
        next.colors.background = RgbaColor::rgb(1, 2, 3);
        next.renderer.backend = RendererBackendPreference::Dx12;

        let plan = AppConfig::default().reload_plan_from(&next);

        assert!(plan.live.contains(&ReloadableSection::Colors));
        assert!(plan.requires_restart());
        assert_eq!(plan.restart_required[0].path, "renderer.backend");
    }

    #[test]
    fn schema_exports_machine_readable_fields() {
        let schema = export_schema();

        assert_eq!(schema.schema_version, 1);
        assert!(
            schema
                .sections
                .iter()
                .flat_map(|section| section.fields.iter())
                .any(|field| field.path == "font.family")
        );
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
            "config-core must not import runtime layer crates"
        );
    }
}
