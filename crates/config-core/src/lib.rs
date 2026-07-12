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
    pub visual_theme: VisualThemeConfig,
    pub cursor: CursorConfig,
    pub scrollback: ScrollbackConfig,
    pub command_blocks: CommandBlocksConfig,
    pub prompt_decorations: PromptDecorationsConfig,
    pub shell_integration: ShellIntegrationConfig,
    pub keyboard: KeyboardConfig,
    pub mouse: MouseConfig,
    pub clipboard: ClipboardConfig,
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

        self.validate_visual_theme(&mut report);

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
        if self.cursor.corner_radius > 0.5 {
            report.error(
                "cursor.corner_radius",
                "cursor corner radius must be between 0.0 and 0.5 terminal cells",
            );
        }
        if self.cursor.image.enabled {
            if self.cursor.image.path.trim().is_empty() {
                report.error(
                    "cursor.image.path",
                    "animated image cursors require a non-empty asset path",
                );
            }
            if self.cursor.image.fps == 0 || self.cursor.image.fps > 60 {
                report.error(
                    "cursor.image.fps",
                    "cursor image FPS must be between 1 and 60",
                );
            }
            if self.cursor.image.fps > self.performance.max_animation_fps {
                report.warning(
                    "cursor.image.fps",
                    "cursor image FPS exceeds the configured animation FPS budget",
                );
            }
        }

        if self.scrollback.lines > 1_000_000 {
            report.warning(
                "scrollback.lines",
                "very large scrollback can consume substantial memory",
            );
        }

        self.validate_clipboard(&mut report);
        self.validate_keybindings(&mut report);
        self.validate_shell_integration(&mut report);
        self.validate_shell_profiles(&mut report);
        self.validate_ssh_profiles(&mut report);
        self.validate_mux(&mut report);
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
        if self.window.title != next.window.title {
            plan.live.push(ReloadableSection::WindowTitle);
        }
        if self.keyboard != next.keyboard {
            plan.live.push(ReloadableSection::Keybindings);
        }
        if self.mouse != next.mouse || self.clipboard != next.clipboard || self.paste != next.paste
        {
            plan.live.push(ReloadableSection::Input);
        }
        if self.visual_theme != next.visual_theme
            || self.command_blocks != next.command_blocks
            || self.prompt_decorations != next.prompt_decorations
            || self.shell_integration != next.shell_integration
        {
            plan.live.push(ReloadableSection::VisualSemantics);
        }
        if self.diagnostics != next.diagnostics {
            plan.live.push(ReloadableSection::Diagnostics);
        }
        if self.performance != next.performance {
            plan.live.push(ReloadableSection::Performance);
        }

        if self.renderer.backend != next.renderer.backend {
            plan.restart_required.push(RestartRequiredChange {
                path: "renderer.backend".to_owned(),
                reason: "GPU backend changes require renderer reinitialization".to_owned(),
            });
        }
        if self.renderer.present_mode != next.renderer.present_mode
            || self.renderer.damage_tracking != next.renderer.damage_tracking
            || self.renderer.gpu_timestamps != next.renderer.gpu_timestamps
        {
            plan.restart_required.push(RestartRequiredChange {
                path: "renderer".to_owned(),
                reason: "renderer scheduling, damage policy, and GPU timestamp changes require renderer reinitialization"
                    .to_owned(),
            });
        }
        if self.window.columns != next.window.columns
            || self.window.rows != next.window.rows
            || self.window.initial_width != next.window.initial_width
            || self.window.initial_height != next.window.initial_height
            || self.window.opacity != next.window.opacity
            || self.window.mode != next.window.mode
            || self.window.decoration_strategy != next.window.decoration_strategy
        {
            plan.restart_required.push(RestartRequiredChange {
                path: "window".to_owned(),
                reason: "window geometry, opacity, mode, and decoration changes require a window update or restart"
                    .to_owned(),
            });
        }
        if self.window.linux_backend != next.window.linux_backend {
            plan.restart_required.push(RestartRequiredChange {
                path: "window.linux_backend".to_owned(),
                reason: "major window backend changes require a new event loop".to_owned(),
            });
        }
        if self.scrollback != next.scrollback {
            plan.restart_required.push(RestartRequiredChange {
                path: "scrollback".to_owned(),
                reason: "scrollback storage policy changes only affect new sessions in this phase"
                    .to_owned(),
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
        if self.mux != next.mux {
            plan.live.push(ReloadableSection::Mux);
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

    fn validate_clipboard(&self, report: &mut ValidationReport) {
        if self.clipboard.osc52.max_bytes == 0 {
            report.error(
                "clipboard.osc52.max_bytes",
                "OSC 52 clipboard byte cap must be greater than zero",
            );
        }
        if self.clipboard.osc52.max_bytes > 16 * 1024 * 1024 {
            report.warning(
                "clipboard.osc52.max_bytes",
                "large OSC 52 clipboard caps increase accidental clipboard-write risk",
            );
        }
        if self.clipboard.osc52.allow_remote && !self.clipboard.osc52.confirm_remote_writes {
            report.warning(
                "clipboard.osc52.confirm_remote_writes",
                "remote OSC 52 writes without confirmation should be used only for trusted hosts",
            );
        }
        if self.clipboard.copy_on_select {
            report.warning(
                "clipboard.copy_on_select",
                "copy_on_select can overwrite clipboard contents frequently",
            );
        }
    }

    fn validate_visual_theme(&self, report: &mut ValidationReport) {
        if self.visual_theme.name.trim().is_empty() {
            report.error("visual_theme.name", "visual theme name cannot be empty");
        }
        if self.visual_theme.spacing.cell_gap_px > 24
            || self.visual_theme.spacing.block_padding_px > 64
        {
            report.error(
                "visual_theme.spacing",
                "visual spacing values must stay within conservative overlay bounds",
            );
        }
        if self.visual_theme.borders.width_px > 8 {
            report.error(
                "visual_theme.borders.width_px",
                "visual border width must be between 0 and 8 pixels",
            );
        }
        if self.command_blocks.allow_in_alternate_screen {
            report.warning(
                "command_blocks.allow_in_alternate_screen",
                "command block overlays in alternate screen applications can obscure TUIs",
            );
        }
        if self.prompt_decorations.allow_in_alternate_screen {
            report.warning(
                "prompt_decorations.allow_in_alternate_screen",
                "prompt overlays in alternate screen applications can obscure TUIs",
            );
        }
    }

    fn validate_shell_integration(&self, report: &mut ValidationReport) {
        for (index, shell) in self.shell_integration.enabled_shells.iter().enumerate() {
            if shell.trim().is_empty() {
                report.error(
                    format!("shell_integration.enabled_shells[{index}]"),
                    "shell name cannot be empty",
                );
            } else if !is_supported_shell_integration_name(shell) {
                report.warning(
                    format!("shell_integration.enabled_shells[{index}]"),
                    format!("shell '{shell}' is not supported by the shell integration layer"),
                );
            }
        }

        for profile in &self.shell_integration.disabled_shell_profiles {
            if !self
                .shell_profiles
                .iter()
                .any(|shell_profile| shell_profile.name == *profile)
            {
                report.warning(
                    "shell_integration.disabled_shell_profiles",
                    format!("disabled shell integration profile '{profile}' is not defined"),
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
            if let Some(username) = &profile.username
                && username.trim().is_empty()
            {
                report.error(
                    format!("ssh_profiles.{}", profile.name),
                    "SSH username cannot be empty when provided",
                );
            }
            if matches!(profile.auth_method, SshAuthMethod::PublicKey)
                && profile
                    .identity_file
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
            {
                report.error(
                    format!("ssh_profiles.{}", profile.name),
                    "public key SSH auth requires identity_file",
                );
            }
            if matches!(profile.auth_method, SshAuthMethod::None) {
                report.warning(
                    format!("ssh_profiles.{}", profile.name),
                    "none SSH authentication is represented for portability but is not supported by the current backend",
                );
            }
            if let Some(identity_file) = &profile.identity_file
                && identity_file.trim().is_empty()
            {
                report.error(
                    format!("ssh_profiles.{}", profile.name),
                    "SSH identity_file cannot be empty when provided",
                );
            }
            if let SshKnownHostsPolicy::PinFingerprint { sha256 } = &profile.known_hosts_policy
                && !sha256.starts_with("SHA256:")
            {
                report.error(
                    format!("ssh_profiles.{}", profile.name),
                    "pinned SSH fingerprints must use the SHA256:<base64> form",
                );
            }
            if profile.agent_forwarding {
                report.warning(
                    format!("ssh_profiles.{}", profile.name),
                    "agent forwarding exposes local agent authority to the remote host; enable only for trusted hosts",
                );
            }
            if profile.proxy_jump.is_some() {
                report.warning(
                    format!("ssh_profiles.{}", profile.name),
                    "proxy_jump is reserved for a later SSH phase and is not active yet",
                );
            }
        }
    }

    fn validate_mux(&self, report: &mut ValidationReport) {
        if self.mux.default_workspace.trim().is_empty() {
            report.error("mux.default_workspace", "default workspace cannot be empty");
        }
        if self.mux.tab_title_format.trim().is_empty() {
            report.error("mux.tab_title_format", "tab title format cannot be empty");
        }
        if self.mux.status_format.trim().is_empty() {
            report.error("mux.status_format", "status format cannot be empty");
        }
        if !(0.01..=0.5).contains(&self.mux.pane_resize_step) {
            report.error(
                "mux.pane_resize_step",
                "pane resize step must be between 0.01 and 0.5",
            );
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
        if !(1..=240).contains(&self.performance.max_animation_fps) {
            report.error(
                "performance.max_animation_fps",
                "animation FPS cap must be between 1 and 240",
            );
        }
        if self.performance.max_cursor_asset_size_kb > 4096 {
            report.error(
                "performance.max_cursor_asset_size_kb",
                "cursor animation assets must be capped at 4096 KiB or less",
            );
        }
        if self.performance.max_active_animations > 256 {
            report.error(
                "performance.max_active_animations",
                "active animation budget must be 256 or less",
            );
        }
        if self.performance.max_animated_region_pixels > 8_294_400 {
            report.warning(
                "performance.max_animated_region_pixels",
                "animated visual regions above 4K frame size can consume substantial GPU time",
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

fn is_supported_shell_integration_name(shell: &str) -> bool {
    matches!(
        shell.trim().to_ascii_lowercase().as_str(),
        "bash" | "zsh" | "fish" | "powershell" | "windows_powershell" | "pwsh" | "cmd"
    )
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
    pub gpu_timestamps: bool,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            backend: RendererBackendPreference::Auto,
            vsync: true,
            damage_tracking: true,
            present_mode: PresentModePreference::Auto,
            gpu_timestamps: false,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisualThemeConfig {
    pub name: String,
    pub cursor_profile: String,
    pub prompt_decoration_profile: String,
    pub command_block_profile: String,
    pub animation_profile: String,
    pub grouping_style: InputOutputGroupingStyle,
    pub spacing: VisualSpacingConfig,
    pub borders: VisualBorderConfig,
    pub badges: VisualBadgeConfig,
    pub success_color: RgbaColor,
    pub error_color: RgbaColor,
}

impl Default for VisualThemeConfig {
    fn default() -> Self {
        Self {
            name: "balanced".to_owned(),
            cursor_profile: "default".to_owned(),
            prompt_decoration_profile: "minimal".to_owned(),
            command_block_profile: "subtle".to_owned(),
            animation_profile: "off".to_owned(),
            grouping_style: InputOutputGroupingStyle::Traditional,
            spacing: VisualSpacingConfig::default(),
            borders: VisualBorderConfig::default(),
            badges: VisualBadgeConfig::default(),
            success_color: RgbaColor::rgb(43, 185, 115),
            error_color: RgbaColor::rgb(230, 72, 86),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InputOutputGroupingStyle {
    #[default]
    Traditional,
    SubtleSeparators,
    CommandCards,
    InputOutputSplit,
    MinimalHeaders,
    CustomTheme,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisualSpacingConfig {
    pub cell_gap_px: u8,
    pub block_padding_px: u8,
    pub badge_gap_px: u8,
}

impl Default for VisualSpacingConfig {
    fn default() -> Self {
        Self {
            cell_gap_px: 0,
            block_padding_px: 6,
            badge_gap_px: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisualBorderConfig {
    pub width_px: u8,
    pub radius_px: u8,
    pub color: RgbaColor,
}

impl Default for VisualBorderConfig {
    fn default() -> Self {
        Self {
            width_px: 1,
            radius_px: 4,
            color: RgbaColor {
                red: 180,
                green: 190,
                blue: 205,
                alpha: 80,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisualBadgeConfig {
    pub shell: bool,
    pub current_directory: bool,
    pub remote: bool,
    pub admin: bool,
    pub status: bool,
}

impl Default for VisualBadgeConfig {
    fn default() -> Self {
        Self {
            shell: false,
            current_directory: true,
            remote: true,
            admin: true,
            status: true,
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
    HollowBlock,
    Custom,
    CustomStaticShape,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CursorConfig {
    pub shape: CursorShape,
    pub blink: bool,
    pub blink_interval_ms: u16,
    pub thickness: f64,
    pub corner_radius: f64,
    pub color: Option<RgbaColor>,
    pub inactive_shape: CursorShape,
    pub mode_specific_styles: BTreeMap<String, CursorShape>,
    pub animations_enabled: bool,
    pub smooth_movement: bool,
    pub typing_pulse: bool,
    pub typing_stretch: bool,
    pub trail: bool,
    pub blink_easing: bool,
    pub short_lived_glow: bool,
    pub image: CursorImageConfig,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blink: true,
            blink_interval_ms: 600,
            thickness: 0.15,
            corner_radius: 0.0,
            color: None,
            inactive_shape: CursorShape::HollowBlock,
            mode_specific_styles: BTreeMap::new(),
            animations_enabled: false,
            smooth_movement: false,
            typing_pulse: false,
            typing_stretch: false,
            trail: false,
            blink_easing: false,
            short_lived_glow: false,
            image: CursorImageConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CursorImageConfig {
    pub enabled: bool,
    pub path: String,
    pub fps: u16,
    pub warn_if_expensive: bool,
}

impl Default for CursorImageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: String::new(),
            fps: 24,
            warn_if_expensive: true,
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
    pub style: CommandBlockStyle,
    pub separate_prompt_input_output: bool,
    pub show_duration: bool,
    pub show_exit_status: bool,
    pub show_current_directory: bool,
    pub show_shell_host: bool,
    pub allow_in_alternate_screen: bool,
    pub copy_actions_enabled: bool,
    pub jump_actions_enabled: bool,
    pub collapse_long_output: bool,
}

impl Default for CommandBlocksConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            style: CommandBlockStyle::Subtle,
            separate_prompt_input_output: true,
            show_duration: true,
            show_exit_status: true,
            show_current_directory: true,
            show_shell_host: true,
            allow_in_alternate_screen: false,
            copy_actions_enabled: true,
            jump_actions_enabled: true,
            collapse_long_output: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommandBlockStyle {
    #[default]
    Subtle,
    Card,
    Split,
    MinimalHeader,
    CustomTheme,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PromptDecorationsConfig {
    pub enabled: bool,
    pub style: PromptDecorationStyle,
    pub show_shell_badge: bool,
    pub show_current_directory: bool,
    pub show_remote_host: bool,
    pub show_admin_badge: bool,
    pub show_previous_status_accent: bool,
    pub allow_in_alternate_screen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromptDecorationStyle {
    #[default]
    MinimalSeparator,
    RoundedBox,
    PillHeader,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellIntegrationConfig {
    pub enabled: bool,
    pub activation: ShellIntegrationActivationConfig,
    pub auto_install: bool,
    pub enabled_shells: Vec<String>,
    pub disabled_shell_profiles: Vec<String>,
    pub remote_instructions: bool,
}

impl Default for ShellIntegrationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            activation: ShellIntegrationActivationConfig::AutoDetect,
            auto_install: false,
            enabled_shells: vec![
                "bash".to_owned(),
                "zsh".to_owned(),
                "fish".to_owned(),
                "powershell".to_owned(),
                "pwsh".to_owned(),
            ],
            disabled_shell_profiles: Vec::new(),
            remote_instructions: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShellIntegrationActivationConfig {
    Full,
    #[default]
    #[serde(alias = "auto")]
    AutoDetect,
    Manual,
    Heuristic,
    #[serde(alias = "off")]
    Disabled,
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
                KeyBinding::new("Ctrl+Shift+T", "new_tab"),
                KeyBinding::new("Ctrl+Shift+Q", "close_tab"),
                KeyBinding::new("Ctrl+PageDown", "next_tab"),
                KeyBinding::new("Ctrl+PageUp", "previous_tab"),
                KeyBinding::new("Ctrl+Shift+H", "split_horizontal"),
                KeyBinding::new("Ctrl+Shift+E", "split_vertical"),
                KeyBinding::new("Ctrl+Shift+X", "close_pane"),
                KeyBinding::new("Alt+Left", "focus_left"),
                KeyBinding::new("Alt+Right", "focus_right"),
                KeyBinding::new("Alt+Up", "focus_up"),
                KeyBinding::new("Alt+Down", "focus_down"),
                KeyBinding::new("Alt+Shift+Left", "resize_pane_left"),
                KeyBinding::new("Alt+Shift+Right", "resize_pane_right"),
                KeyBinding::new("Alt+Shift+Up", "resize_pane_up"),
                KeyBinding::new("Alt+Shift+Down", "resize_pane_down"),
                KeyBinding::new("Ctrl+Shift+Z", "zoom_pane"),
                KeyBinding::new("Ctrl+Shift+R", "rename_tab"),
                KeyBinding::new("Ctrl+Shift+O", "move_pane"),
                KeyBinding::new("Ctrl+Shift+Up", "jump_to_previous_command"),
                KeyBinding::new("Ctrl+Shift+Down", "jump_to_next_command"),
                KeyBinding::new("Ctrl+Shift+Y", "select_current_command_output"),
                KeyBinding::new("Ctrl+Shift+U", "copy_current_command_output"),
                KeyBinding::new("Ctrl+Shift+A", "copy_command_and_output"),
                KeyBinding::new("Shift+PageUp", "scroll_page_up"),
                KeyBinding::new("Shift+PageDown", "scroll_page_down"),
                KeyBinding::new("Ctrl+Shift+Home", "scroll_to_top"),
                KeyBinding::new("Ctrl+Shift+End", "scroll_to_bottom"),
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
pub struct ClipboardConfig {
    pub enabled: bool,
    pub copy_on_select: bool,
    pub paste_protection: bool,
    pub bracketed_paste: bool,
    pub middle_click_paste: bool,
    pub prefer_primary_selection_on_linux: bool,
    pub log_operations: bool,
    pub osc52: Osc52ClipboardConfig,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            copy_on_select: false,
            paste_protection: true,
            bracketed_paste: true,
            middle_click_paste: true,
            prefer_primary_selection_on_linux: true,
            log_operations: false,
            osc52: Osc52ClipboardConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Osc52ClipboardConfig {
    pub enabled: bool,
    pub allow_local: bool,
    pub allow_remote: bool,
    pub max_bytes: usize,
    pub confirm_remote_writes: bool,
}

impl Default for Osc52ClipboardConfig {
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
    #[serde(alias = "user")]
    pub username: Option<String>,
    pub port: u16,
    pub auth_method: SshAuthMethod,
    pub identity_file: Option<String>,
    pub known_hosts_policy: SshKnownHostsPolicy,
    pub remote_command: Option<String>,
    pub remote_working_directory: Option<String>,
    pub shell_integration: bool,
    pub agent_forwarding: bool,
    pub proxy_jump: Option<String>,
}

impl Default for SshProfile {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            username: None,
            port: 22,
            auth_method: SshAuthMethod::Agent,
            identity_file: None,
            known_hosts_policy: SshKnownHostsPolicy::Ask,
            remote_command: None,
            remote_working_directory: None,
            shell_integration: true,
            agent_forwarding: false,
            proxy_jump: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SshAuthMethod {
    #[default]
    Agent,
    PublicKey,
    Password,
    KeyboardInteractive,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SshKnownHostsPolicy {
    #[default]
    Ask,
    RequireKnown,
    TrustOnFirstUse,
    PinFingerprint {
        sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MuxConfig {
    pub enabled: bool,
    pub restore_sessions: bool,
    pub default_workspace: String,
    pub show_tab_bar: bool,
    pub tab_title_format: String,
    pub status_format: String,
    pub pane_resize_step: f64,
    pub remember_working_directory: bool,
}

impl Default for MuxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            restore_sessions: false,
            default_workspace: "default".to_owned(),
            show_tab_bar: true,
            tab_title_format: "{index}: {title}".to_owned(),
            status_format: "{workspace} {shell}".to_owned(),
            pane_resize_step: 0.05,
            remember_working_directory: true,
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
    #[serde(alias = "battery_conscious")]
    BatterySaver,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceConfig {
    pub profile: PerformanceProfile,
    pub frame_rate_limit: Option<u16>,
    pub glyph_cache_entries: usize,
    pub max_frame_time_ms: u16,
    pub expensive_effect_warnings: bool,
    pub max_animation_fps: u16,
    pub max_cursor_asset_size_kb: u32,
    pub max_active_animations: u16,
    pub max_animated_region_pixels: u32,
    pub disable_expensive_effects_on_battery: bool,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            profile: PerformanceProfile::Balanced,
            frame_rate_limit: None,
            glyph_cache_entries: 8192,
            max_frame_time_ms: 16,
            expensive_effect_warnings: true,
            max_animation_fps: 60,
            max_cursor_asset_size_kb: 256,
            max_active_animations: 8,
            max_animated_region_pixels: 250_000,
            disable_expensive_effects_on_battery: true,
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
    pub visual_theme: Option<VisualThemeConfigPatch>,
    pub cursor: Option<CursorConfigPatch>,
    pub command_blocks: Option<CommandBlocksConfigPatch>,
    pub prompt_decorations: Option<PromptDecorationsConfigPatch>,
    pub shell_integration: Option<ShellIntegrationConfigPatch>,
    pub clipboard: Option<ClipboardConfigPatch>,
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
        if let Some(visual_theme) = &self.visual_theme {
            visual_theme.apply_to(&mut config.visual_theme);
        }
        if let Some(cursor) = &self.cursor {
            cursor.apply_to(&mut config.cursor);
        }
        if let Some(command_blocks) = &self.command_blocks {
            command_blocks.apply_to(&mut config.command_blocks);
        }
        if let Some(prompt_decorations) = &self.prompt_decorations {
            prompt_decorations.apply_to(&mut config.prompt_decorations);
        }
        if let Some(shell_integration) = &self.shell_integration {
            shell_integration.apply_to(&mut config.shell_integration);
        }
        if let Some(clipboard) = &self.clipboard {
            clipboard.apply_to(&mut config.clipboard);
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
    pub gpu_timestamps: Option<bool>,
}

impl RendererConfigPatch {
    fn apply_to(&self, config: &mut RendererConfig) {
        apply_opt(&mut config.backend, &self.backend);
        apply_opt(&mut config.vsync, &self.vsync);
        apply_opt(&mut config.damage_tracking, &self.damage_tracking);
        apply_opt(&mut config.present_mode, &self.present_mode);
        apply_opt(&mut config.gpu_timestamps, &self.gpu_timestamps);
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
pub struct VisualThemeConfigPatch {
    pub name: Option<String>,
    pub cursor_profile: Option<String>,
    pub prompt_decoration_profile: Option<String>,
    pub command_block_profile: Option<String>,
    pub animation_profile: Option<String>,
    pub grouping_style: Option<InputOutputGroupingStyle>,
    pub spacing: Option<VisualSpacingConfig>,
    pub borders: Option<VisualBorderConfig>,
    pub badges: Option<VisualBadgeConfig>,
    pub success_color: Option<RgbaColor>,
    pub error_color: Option<RgbaColor>,
}

impl VisualThemeConfigPatch {
    fn apply_to(&self, config: &mut VisualThemeConfig) {
        apply_opt(&mut config.name, &self.name);
        apply_opt(&mut config.cursor_profile, &self.cursor_profile);
        apply_opt(
            &mut config.prompt_decoration_profile,
            &self.prompt_decoration_profile,
        );
        apply_opt(
            &mut config.command_block_profile,
            &self.command_block_profile,
        );
        apply_opt(&mut config.animation_profile, &self.animation_profile);
        apply_opt(&mut config.grouping_style, &self.grouping_style);
        apply_opt(&mut config.spacing, &self.spacing);
        apply_opt(&mut config.borders, &self.borders);
        apply_opt(&mut config.badges, &self.badges);
        apply_opt(&mut config.success_color, &self.success_color);
        apply_opt(&mut config.error_color, &self.error_color);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CursorConfigPatch {
    pub shape: Option<CursorShape>,
    pub blink: Option<bool>,
    pub blink_interval_ms: Option<u16>,
    pub thickness: Option<f64>,
    pub corner_radius: Option<f64>,
    pub color: Option<Option<RgbaColor>>,
    pub inactive_shape: Option<CursorShape>,
    pub mode_specific_styles: Option<BTreeMap<String, CursorShape>>,
    pub animations_enabled: Option<bool>,
    pub smooth_movement: Option<bool>,
    pub typing_pulse: Option<bool>,
    pub typing_stretch: Option<bool>,
    pub trail: Option<bool>,
    pub blink_easing: Option<bool>,
    pub short_lived_glow: Option<bool>,
    pub image: Option<CursorImageConfigPatch>,
}

impl CursorConfigPatch {
    fn apply_to(&self, config: &mut CursorConfig) {
        apply_opt(&mut config.shape, &self.shape);
        apply_opt(&mut config.blink, &self.blink);
        apply_opt(&mut config.blink_interval_ms, &self.blink_interval_ms);
        apply_opt(&mut config.thickness, &self.thickness);
        apply_opt(&mut config.corner_radius, &self.corner_radius);
        if let Some(color) = self.color {
            config.color = color;
        }
        apply_opt(&mut config.inactive_shape, &self.inactive_shape);
        apply_opt(&mut config.mode_specific_styles, &self.mode_specific_styles);
        apply_opt(&mut config.animations_enabled, &self.animations_enabled);
        apply_opt(&mut config.smooth_movement, &self.smooth_movement);
        apply_opt(&mut config.typing_pulse, &self.typing_pulse);
        apply_opt(&mut config.typing_stretch, &self.typing_stretch);
        apply_opt(&mut config.trail, &self.trail);
        apply_opt(&mut config.blink_easing, &self.blink_easing);
        apply_opt(&mut config.short_lived_glow, &self.short_lived_glow);
        if let Some(image) = &self.image {
            image.apply_to(&mut config.image);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CursorImageConfigPatch {
    pub enabled: Option<bool>,
    pub path: Option<String>,
    pub fps: Option<u16>,
    pub warn_if_expensive: Option<bool>,
}

impl CursorImageConfigPatch {
    fn apply_to(&self, config: &mut CursorImageConfig) {
        apply_opt(&mut config.enabled, &self.enabled);
        apply_opt(&mut config.path, &self.path);
        apply_opt(&mut config.fps, &self.fps);
        apply_opt(&mut config.warn_if_expensive, &self.warn_if_expensive);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CommandBlocksConfigPatch {
    pub enabled: Option<bool>,
    pub style: Option<CommandBlockStyle>,
    pub separate_prompt_input_output: Option<bool>,
    pub show_duration: Option<bool>,
    pub show_exit_status: Option<bool>,
    pub show_current_directory: Option<bool>,
    pub show_shell_host: Option<bool>,
    pub allow_in_alternate_screen: Option<bool>,
    pub copy_actions_enabled: Option<bool>,
    pub jump_actions_enabled: Option<bool>,
    pub collapse_long_output: Option<bool>,
}

impl CommandBlocksConfigPatch {
    fn apply_to(&self, config: &mut CommandBlocksConfig) {
        apply_opt(&mut config.enabled, &self.enabled);
        apply_opt(&mut config.style, &self.style);
        apply_opt(
            &mut config.separate_prompt_input_output,
            &self.separate_prompt_input_output,
        );
        apply_opt(&mut config.show_duration, &self.show_duration);
        apply_opt(&mut config.show_exit_status, &self.show_exit_status);
        apply_opt(
            &mut config.show_current_directory,
            &self.show_current_directory,
        );
        apply_opt(&mut config.show_shell_host, &self.show_shell_host);
        apply_opt(
            &mut config.allow_in_alternate_screen,
            &self.allow_in_alternate_screen,
        );
        apply_opt(&mut config.copy_actions_enabled, &self.copy_actions_enabled);
        apply_opt(&mut config.jump_actions_enabled, &self.jump_actions_enabled);
        apply_opt(&mut config.collapse_long_output, &self.collapse_long_output);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PromptDecorationsConfigPatch {
    pub enabled: Option<bool>,
    pub style: Option<PromptDecorationStyle>,
    pub show_shell_badge: Option<bool>,
    pub show_current_directory: Option<bool>,
    pub show_remote_host: Option<bool>,
    pub show_admin_badge: Option<bool>,
    pub show_previous_status_accent: Option<bool>,
    pub allow_in_alternate_screen: Option<bool>,
}

impl PromptDecorationsConfigPatch {
    fn apply_to(&self, config: &mut PromptDecorationsConfig) {
        apply_opt(&mut config.enabled, &self.enabled);
        apply_opt(&mut config.style, &self.style);
        apply_opt(&mut config.show_shell_badge, &self.show_shell_badge);
        apply_opt(
            &mut config.show_current_directory,
            &self.show_current_directory,
        );
        apply_opt(&mut config.show_remote_host, &self.show_remote_host);
        apply_opt(&mut config.show_admin_badge, &self.show_admin_badge);
        apply_opt(
            &mut config.show_previous_status_accent,
            &self.show_previous_status_accent,
        );
        apply_opt(
            &mut config.allow_in_alternate_screen,
            &self.allow_in_alternate_screen,
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ShellIntegrationConfigPatch {
    pub enabled: Option<bool>,
    pub activation: Option<ShellIntegrationActivationConfig>,
    pub auto_install: Option<bool>,
    pub enabled_shells: Option<Vec<String>>,
    pub disabled_shell_profiles: Option<Vec<String>>,
    pub remote_instructions: Option<bool>,
}

impl ShellIntegrationConfigPatch {
    fn apply_to(&self, config: &mut ShellIntegrationConfig) {
        apply_opt(&mut config.enabled, &self.enabled);
        apply_opt(&mut config.activation, &self.activation);
        apply_opt(&mut config.auto_install, &self.auto_install);
        apply_opt(&mut config.enabled_shells, &self.enabled_shells);
        apply_opt(
            &mut config.disabled_shell_profiles,
            &self.disabled_shell_profiles,
        );
        apply_opt(&mut config.remote_instructions, &self.remote_instructions);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ClipboardConfigPatch {
    pub enabled: Option<bool>,
    pub copy_on_select: Option<bool>,
    pub paste_protection: Option<bool>,
    pub bracketed_paste: Option<bool>,
    pub middle_click_paste: Option<bool>,
    pub prefer_primary_selection_on_linux: Option<bool>,
    pub log_operations: Option<bool>,
    pub osc52: Option<Osc52ClipboardConfigPatch>,
}

impl ClipboardConfigPatch {
    fn apply_to(&self, config: &mut ClipboardConfig) {
        apply_opt(&mut config.enabled, &self.enabled);
        apply_opt(&mut config.copy_on_select, &self.copy_on_select);
        apply_opt(&mut config.paste_protection, &self.paste_protection);
        apply_opt(&mut config.bracketed_paste, &self.bracketed_paste);
        apply_opt(&mut config.middle_click_paste, &self.middle_click_paste);
        apply_opt(
            &mut config.prefer_primary_selection_on_linux,
            &self.prefer_primary_selection_on_linux,
        );
        apply_opt(&mut config.log_operations, &self.log_operations);
        if let Some(osc52) = &self.osc52 {
            osc52.apply_to(&mut config.osc52);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Osc52ClipboardConfigPatch {
    pub enabled: Option<bool>,
    pub allow_local: Option<bool>,
    pub allow_remote: Option<bool>,
    pub max_bytes: Option<usize>,
    pub confirm_remote_writes: Option<bool>,
}

impl Osc52ClipboardConfigPatch {
    fn apply_to(&self, config: &mut Osc52ClipboardConfig) {
        apply_opt(&mut config.enabled, &self.enabled);
        apply_opt(&mut config.allow_local, &self.allow_local);
        apply_opt(&mut config.allow_remote, &self.allow_remote);
        apply_opt(&mut config.max_bytes, &self.max_bytes);
        apply_opt(
            &mut config.confirm_remote_writes,
            &self.confirm_remote_writes,
        );
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
    pub max_animation_fps: Option<u16>,
    pub max_cursor_asset_size_kb: Option<u32>,
    pub max_active_animations: Option<u16>,
    pub max_animated_region_pixels: Option<u32>,
    pub disable_expensive_effects_on_battery: Option<bool>,
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
        apply_opt(&mut config.max_animation_fps, &self.max_animation_fps);
        apply_opt(
            &mut config.max_cursor_asset_size_kb,
            &self.max_cursor_asset_size_kb,
        );
        apply_opt(
            &mut config.max_active_animations,
            &self.max_active_animations,
        );
        apply_opt(
            &mut config.max_animated_region_pixels,
            &self.max_animated_region_pixels,
        );
        apply_opt(
            &mut config.disable_expensive_effects_on_battery,
            &self.disable_expensive_effects_on_battery,
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
    Mux,
    Performance,
    VisualSemantics,
    WindowPadding,
    WindowTitle,
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
                name: "visual_theme",
                fields: vec![
                    field(
                        "visual_theme.name",
                        "string",
                        &default.visual_theme.name,
                        true,
                        false,
                    ),
                    field(
                        "visual_theme.cursor_profile",
                        "string",
                        &default.visual_theme.cursor_profile,
                        true,
                        false,
                    ),
                    field(
                        "visual_theme.prompt_decoration_profile",
                        "string",
                        &default.visual_theme.prompt_decoration_profile,
                        true,
                        false,
                    ),
                    field(
                        "visual_theme.command_block_profile",
                        "string",
                        &default.visual_theme.command_block_profile,
                        true,
                        false,
                    ),
                    field(
                        "visual_theme.animation_profile",
                        "string",
                        &default.visual_theme.animation_profile,
                        true,
                        false,
                    ),
                    field(
                        "visual_theme.grouping_style",
                        "input_output_grouping_style",
                        format!("{:?}", default.visual_theme.grouping_style),
                        true,
                        false,
                    ),
                ],
            },
            ConfigSchemaSection {
                name: "cursor",
                fields: vec![
                    field(
                        "cursor.shape",
                        "cursor_shape",
                        format!("{:?}", default.cursor.shape),
                        true,
                        false,
                    ),
                    field("cursor.blink", "boolean", default.cursor.blink, true, false),
                    field(
                        "cursor.thickness",
                        "number",
                        default.cursor.thickness,
                        true,
                        false,
                    ),
                    field(
                        "cursor.corner_radius",
                        "number",
                        default.cursor.corner_radius,
                        true,
                        false,
                    ),
                    field(
                        "cursor.animations_enabled",
                        "boolean",
                        default.cursor.animations_enabled,
                        true,
                        false,
                    ),
                    field(
                        "cursor.image.enabled",
                        "boolean",
                        default.cursor.image.enabled,
                        true,
                        false,
                    ),
                    field(
                        "cursor.image.path",
                        "string",
                        &default.cursor.image.path,
                        true,
                        false,
                    ),
                    field(
                        "cursor.image.fps",
                        "integer",
                        default.cursor.image.fps,
                        true,
                        false,
                    ),
                    field(
                        "cursor.image.warn_if_expensive",
                        "boolean",
                        default.cursor.image.warn_if_expensive,
                        true,
                        false,
                    ),
                ],
            },
            ConfigSchemaSection {
                name: "semantic_visuals",
                fields: vec![
                    field(
                        "prompt_decorations.enabled",
                        "boolean",
                        default.prompt_decorations.enabled,
                        true,
                        false,
                    ),
                    field(
                        "prompt_decorations.style",
                        "prompt_decoration_style",
                        format!("{:?}", default.prompt_decorations.style),
                        true,
                        false,
                    ),
                    field(
                        "command_blocks.enabled",
                        "boolean",
                        default.command_blocks.enabled,
                        true,
                        false,
                    ),
                    field(
                        "command_blocks.style",
                        "command_block_style",
                        format!("{:?}", default.command_blocks.style),
                        true,
                        false,
                    ),
                    field(
                        "command_blocks.allow_in_alternate_screen",
                        "boolean",
                        default.command_blocks.allow_in_alternate_screen,
                        true,
                        false,
                    ),
                    field(
                        "prompt_decorations.allow_in_alternate_screen",
                        "boolean",
                        default.prompt_decorations.allow_in_alternate_screen,
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
                name: "ssh_profiles",
                fields: vec![
                    field("ssh_profiles", "array<ssh_profile>", "[]", false, true),
                    field("ssh_profiles.name", "string", "", false, true),
                    field("ssh_profiles.host", "string", "", false, true),
                    field("ssh_profiles.port", "integer", 22, false, true),
                    field("ssh_profiles.username", "string?", "none", false, true),
                    field(
                        "ssh_profiles.auth_method",
                        "ssh_auth_method",
                        "agent",
                        false,
                        true,
                    ),
                    field("ssh_profiles.identity_file", "path?", "none", false, true),
                    field(
                        "ssh_profiles.known_hosts_policy",
                        "ssh_known_hosts_policy",
                        "ask",
                        false,
                        true,
                    ),
                    field(
                        "ssh_profiles.remote_command",
                        "string?",
                        "none",
                        false,
                        true,
                    ),
                    field(
                        "ssh_profiles.remote_working_directory",
                        "path?",
                        "none",
                        false,
                        true,
                    ),
                    field(
                        "ssh_profiles.shell_integration",
                        "boolean",
                        true,
                        false,
                        true,
                    ),
                    field(
                        "ssh_profiles.agent_forwarding",
                        "boolean",
                        false,
                        false,
                        true,
                    ),
                    field("ssh_profiles.proxy_jump", "string?", "later", false, true),
                ],
            },
            ConfigSchemaSection {
                name: "shell_integration",
                fields: vec![
                    field(
                        "shell_integration.enabled",
                        "boolean",
                        default.shell_integration.enabled,
                        true,
                        false,
                    ),
                    field(
                        "shell_integration.activation",
                        "shell_integration_activation",
                        format!("{:?}", default.shell_integration.activation),
                        true,
                        false,
                    ),
                    field(
                        "shell_integration.auto_install",
                        "boolean",
                        default.shell_integration.auto_install,
                        true,
                        false,
                    ),
                    field(
                        "shell_integration.enabled_shells",
                        "array<string>",
                        default.shell_integration.enabled_shells.join(","),
                        true,
                        false,
                    ),
                    field(
                        "shell_integration.disabled_shell_profiles",
                        "array<string>",
                        "[]",
                        true,
                        false,
                    ),
                    field(
                        "shell_integration.remote_instructions",
                        "boolean",
                        default.shell_integration.remote_instructions,
                        true,
                        false,
                    ),
                ],
            },
            ConfigSchemaSection {
                name: "clipboard",
                fields: vec![
                    field(
                        "clipboard.enabled",
                        "boolean",
                        default.clipboard.enabled,
                        true,
                        false,
                    ),
                    field(
                        "clipboard.copy_on_select",
                        "boolean",
                        default.clipboard.copy_on_select,
                        true,
                        false,
                    ),
                    field(
                        "clipboard.paste_protection",
                        "boolean",
                        default.clipboard.paste_protection,
                        true,
                        false,
                    ),
                    field(
                        "clipboard.bracketed_paste",
                        "boolean",
                        default.clipboard.bracketed_paste,
                        true,
                        false,
                    ),
                    field(
                        "clipboard.middle_click_paste",
                        "boolean",
                        default.clipboard.middle_click_paste,
                        true,
                        false,
                    ),
                    field(
                        "clipboard.osc52.enabled",
                        "boolean",
                        default.clipboard.osc52.enabled,
                        true,
                        false,
                    ),
                    field(
                        "clipboard.osc52.allow_local",
                        "boolean",
                        default.clipboard.osc52.allow_local,
                        true,
                        false,
                    ),
                    field(
                        "clipboard.osc52.allow_remote",
                        "boolean",
                        default.clipboard.osc52.allow_remote,
                        true,
                        false,
                    ),
                    field(
                        "clipboard.osc52.max_bytes",
                        "integer",
                        default.clipboard.osc52.max_bytes,
                        true,
                        false,
                    ),
                    field(
                        "clipboard.osc52.confirm_remote_writes",
                        "boolean",
                        default.clipboard.osc52.confirm_remote_writes,
                        true,
                        false,
                    ),
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
                    field(
                        "renderer.gpu_timestamps",
                        "boolean",
                        default.renderer.gpu_timestamps,
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
                    field(
                        "performance.max_animation_fps",
                        "integer",
                        default.performance.max_animation_fps,
                        true,
                        false,
                    ),
                    field(
                        "performance.max_cursor_asset_size_kb",
                        "integer",
                        default.performance.max_cursor_asset_size_kb,
                        true,
                        false,
                    ),
                    field(
                        "performance.max_active_animations",
                        "integer",
                        default.performance.max_active_animations,
                        true,
                        false,
                    ),
                ],
            },
            ConfigSchemaSection {
                name: "mux",
                fields: vec![
                    field("mux.enabled", "boolean", default.mux.enabled, false, false),
                    field(
                        "mux.restore_sessions",
                        "boolean",
                        default.mux.restore_sessions,
                        false,
                        false,
                    ),
                    field(
                        "mux.default_workspace",
                        "string",
                        &default.mux.default_workspace,
                        false,
                        false,
                    ),
                    field(
                        "mux.show_tab_bar",
                        "boolean",
                        default.mux.show_tab_bar,
                        false,
                        false,
                    ),
                    field(
                        "mux.tab_title_format",
                        "string",
                        &default.mux.tab_title_format,
                        true,
                        false,
                    ),
                    field(
                        "mux.status_format",
                        "string",
                        &default.mux.status_format,
                        true,
                        false,
                    ),
                    field(
                        "mux.pane_resize_step",
                        "number",
                        default.mux.pane_resize_step,
                        false,
                        false,
                    ),
                    field(
                        "mux.remember_working_directory",
                        "boolean",
                        default.mux.remember_working_directory,
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

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedAppConfig {
    pub config: AppConfig,
    pub diagnostics: Vec<ConfigDiagnostic>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProviderError {
    pub source: Option<String>,
    pub message: String,
}

impl ConfigProviderError {
    #[must_use]
    pub fn new(source: Option<String>, message: impl Into<String>) -> Self {
        Self {
            source,
            message: message.into(),
        }
    }
}

/// Config loading boundary. Frontends compile into `AppConfig` before hot paths.
pub trait ConfigProvider {
    fn load_config(&self) -> Result<LoadedAppConfig, ConfigProviderError>;

    fn validate_config(&self, config: &AppConfig) -> ValidationReport {
        config.validate()
    }

    fn reload_plan(&self, current: &AppConfig) -> Result<ReloadPlan, ConfigProviderError> {
        let loaded = self.load_config()?;
        Ok(current.reload_plan_from(&loaded.config))
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
    fn alternate_screen_semantic_visuals_are_warned_not_defaulted() {
        let mut config = AppConfig::default();
        assert!(!config.command_blocks.allow_in_alternate_screen);
        assert!(!config.prompt_decorations.allow_in_alternate_screen);

        config.command_blocks.allow_in_alternate_screen = true;
        config.prompt_decorations.allow_in_alternate_screen = true;
        let report = config.validate();

        assert!(!report.has_errors());
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "command_blocks.allow_in_alternate_screen"
                && diagnostic.severity == ConfigDiagnosticSeverity::Warning
        }));
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "prompt_decorations.allow_in_alternate_screen"
                && diagnostic.severity == ConfigDiagnosticSeverity::Warning
        }));
    }

    #[test]
    fn animated_cursor_image_is_opt_in_and_budgeted() {
        let mut config = AppConfig::default();
        assert!(!config.cursor.image.enabled);
        assert_eq!(config.cursor.image.fps, 24);

        config.cursor.image.enabled = true;
        config.cursor.image.fps = 48;
        config.performance.max_animation_fps = 24;
        let report = config.validate();

        assert!(report.has_errors());
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "cursor.image.path"
                && diagnostic.severity == ConfigDiagnosticSeverity::Error
        }));
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "cursor.image.fps"
                && diagnostic.severity == ConfigDiagnosticSeverity::Warning
        }));
    }

    #[test]
    fn reload_plan_distinguishes_live_and_restart_changes() {
        let mut next = AppConfig::default();
        next.colors.background = RgbaColor::rgb(1, 2, 3);
        next.performance.max_frame_time_ms = 20;
        next.window.title = "Reloaded".to_owned();
        next.window.mode = WindowModeConfig::Maximized;
        next.renderer.backend = RendererBackendPreference::Dx12;

        let plan = AppConfig::default().reload_plan_from(&next);

        assert!(plan.live.contains(&ReloadableSection::Colors));
        assert!(plan.live.contains(&ReloadableSection::Performance));
        assert!(plan.live.contains(&ReloadableSection::WindowTitle));
        assert!(plan.requires_restart());
        assert!(
            plan.restart_required
                .iter()
                .any(|change| change.path == "renderer.backend")
        );
        assert!(
            plan.restart_required
                .iter()
                .any(|change| change.path == "window")
        );
    }

    #[test]
    fn ssh_profile_validation_blocks_reckless_defaults() {
        let mut config = AppConfig {
            ssh_profiles: vec![SshProfile {
                name: "prod".to_owned(),
                host: "example.com".to_owned(),
                auth_method: SshAuthMethod::PublicKey,
                known_hosts_policy: SshKnownHostsPolicy::PinFingerprint {
                    sha256: "bad".to_owned(),
                },
                ..SshProfile::default()
            }],
            ..AppConfig::default()
        };

        let report = config.validate();
        assert!(report.has_errors());
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("public key SSH auth requires identity_file")
        }));
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("pinned SSH fingerprints must use")
        }));

        config.ssh_profiles[0].identity_file = Some("~/.ssh/id_ed25519".to_owned());
        config.ssh_profiles[0].known_hosts_policy = SshKnownHostsPolicy::RequireKnown;

        assert!(!config.validate().has_errors());
    }

    #[test]
    fn clipboard_policy_defaults_are_safe_for_remote_sessions() {
        let config = AppConfig::default();

        assert!(config.clipboard.enabled);
        assert!(!config.clipboard.copy_on_select);
        assert!(config.clipboard.paste_protection);
        assert!(config.clipboard.bracketed_paste);
        assert!(config.clipboard.osc52.enabled);
        assert!(config.clipboard.osc52.allow_local);
        assert!(!config.clipboard.osc52.allow_remote);
        assert!(config.clipboard.osc52.confirm_remote_writes);
        assert_eq!(config.clipboard.osc52.max_bytes, 1_048_576);
    }

    #[test]
    fn clipboard_validation_rejects_zero_osc52_cap() {
        let mut config = AppConfig::default();
        config.clipboard.osc52.max_bytes = 0;

        let report = config.validate();

        assert!(report.has_errors());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path == "clipboard.osc52.max_bytes")
        );
    }

    #[test]
    fn platform_override_can_refine_clipboard_policy() {
        let config: AppConfig = toml::from_str(
            r#"
            [clipboard.osc52]
            allow_remote = false

            [platform.linux_wayland.clipboard.osc52]
            allow_remote = true
            confirm_remote_writes = true
            "#,
        )
        .expect("config should deserialize");

        let resolved = config.resolved_for_platform(ConfigPlatform::LinuxWayland);

        assert!(resolved.clipboard.osc52.allow_remote);
        assert!(resolved.clipboard.osc52.confirm_remote_writes);
    }

    #[test]
    fn shell_integration_activation_aliases_parse() {
        let auto: AppConfig = toml::from_str(
            r#"
            [shell_integration]
            activation = "auto"
            "#,
        )
        .expect("auto alias should parse");
        assert_eq!(
            auto.shell_integration.activation,
            ShellIntegrationActivationConfig::AutoDetect
        );

        let off: AppConfig = toml::from_str(
            r#"
            [shell_integration]
            activation = "off"
            "#,
        )
        .expect("off alias should parse");
        assert_eq!(
            off.shell_integration.activation,
            ShellIntegrationActivationConfig::Disabled
        );
    }

    #[test]
    fn shell_integration_validation_warns_for_unknown_shell_names() {
        let mut config = AppConfig::default();
        config.shell_integration.enabled_shells = vec!["bash".to_owned(), "bogus".to_owned()];

        let report = config.validate();

        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "shell_integration.enabled_shells[1]"
                && diagnostic.message.contains("not supported")
        }));
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
        assert!(
            schema
                .sections
                .iter()
                .flat_map(|section| section.fields.iter())
                .any(|field| field.path == "ssh_profiles.known_hosts_policy")
        );
        assert!(
            schema
                .sections
                .iter()
                .flat_map(|section| section.fields.iter())
                .any(|field| field.path == "clipboard.osc52.allow_remote")
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

    #[derive(Debug)]
    struct DefaultConfigProvider;

    impl ConfigProvider for DefaultConfigProvider {
        fn load_config(&self) -> Result<LoadedAppConfig, ConfigProviderError> {
            Ok(LoadedAppConfig {
                config: AppConfig::default(),
                diagnostics: Vec::new(),
                source: "test-default".to_owned(),
            })
        }
    }

    #[test]
    fn config_provider_compiles_into_portable_app_config() {
        let provider = DefaultConfigProvider;
        let loaded = provider
            .load_config()
            .expect("default config provider should load");
        let validation = provider.validate_config(&loaded.config);

        assert_eq!(loaded.source, "test-default");
        assert!(!validation.has_errors());
        assert!(
            !provider
                .reload_plan(&loaded.config)
                .unwrap()
                .requires_restart()
        );
    }
}
