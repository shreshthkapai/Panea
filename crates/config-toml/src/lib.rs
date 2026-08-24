//! Static TOML configuration frontend.

pub const LAYER: &str = "config portability";

use std::{
    collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher},
    error::Error,
    fmt, fs,
    hash::{Hash, Hasher},
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use config_core::{
    AppConfig, CURRENT_CONFIG_SCHEMA_VERSION, ConfigDiagnostic, ConfigDiagnosticSeverity,
    ConfigPlatform, PerformanceProfile, ValidationReport, export_schema,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLoadOptions {
    pub explicit_path: Option<PathBuf>,
    pub platform: ConfigPlatform,
}

impl Default for ConfigLoadOptions {
    fn default() -> Self {
        Self {
            explicit_path: std::env::var_os("PANEA_CONFIG").map(PathBuf::from),
            platform: ConfigPlatform::current(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub config: AppConfig,
    pub source: ConfigSource,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    Default,
    File(PathBuf),
    ExplicitFile(PathBuf),
}

#[derive(Debug, Clone)]
pub struct ConfigWatcher {
    options: ConfigLoadOptions,
    poll_interval: Duration,
    debounce: Duration,
    last_poll: Option<Instant>,
    last_seen: Option<FileFingerprint>,
    pending: Option<PendingReload>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigWatchEvent {
    Unchanged,
    Pending {
        path: Option<PathBuf>,
    },
    Reloaded(Box<LoadedConfig>),
    Failed {
        path: Option<PathBuf>,
        error: ConfigTomlError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingReload {
    fingerprint: Option<FileFingerprint>,
    first_seen: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    path: PathBuf,
    exists: bool,
    modified: Option<SystemTime>,
    len: u64,
    content_hash: Option<u64>,
}

impl ConfigWatcher {
    #[must_use]
    pub fn new(options: ConfigLoadOptions) -> Self {
        let last_seen = current_fingerprint(&options).ok().flatten();
        Self {
            options,
            poll_interval: Duration::from_millis(500),
            debounce: Duration::from_millis(150),
            last_poll: None,
            last_seen,
            pending: None,
        }
    }

    #[must_use]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    #[must_use]
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    pub fn poll(&mut self) -> ConfigWatchEvent {
        let now = Instant::now();
        if let Some(last_poll) = self.last_poll
            && now.duration_since(last_poll) < self.poll_interval
        {
            return ConfigWatchEvent::Unchanged;
        }
        self.last_poll = Some(now);

        let fingerprint = match current_fingerprint(&self.options) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                return ConfigWatchEvent::Failed { path: None, error };
            }
        };

        if fingerprint == self.last_seen {
            self.pending = None;
            return ConfigWatchEvent::Unchanged;
        }

        let pending = self.pending.get_or_insert_with(|| PendingReload {
            fingerprint: fingerprint.clone(),
            first_seen: now,
        });
        if pending.fingerprint != fingerprint {
            *pending = PendingReload {
                fingerprint: fingerprint.clone(),
                first_seen: now,
            };
        }

        if now.duration_since(pending.first_seen) < self.debounce {
            return ConfigWatchEvent::Pending {
                path: fingerprint
                    .as_ref()
                    .map(|fingerprint| fingerprint.path.clone()),
            };
        }

        self.pending = None;
        match load(self.options.clone()) {
            Ok(loaded) => {
                self.last_seen = fingerprint;
                ConfigWatchEvent::Reloaded(Box::new(loaded))
            }
            Err(error) => {
                self.last_seen = fingerprint.clone();
                ConfigWatchEvent::Failed {
                    path: fingerprint.map(|fingerprint| fingerprint.path),
                    error,
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigTomlError {
    Io {
        path: PathBuf,
        message: String,
    },
    Parse {
        path: Option<PathBuf>,
        message: String,
        line: Option<usize>,
        column: Option<usize>,
    },
    Validation {
        diagnostics: Vec<ConfigDiagnostic>,
    },
    Schema {
        message: String,
    },
    UnsupportedSchema {
        found: u16,
        supported: u16,
    },
}

impl fmt::Display for ConfigTomlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => {
                write!(f, "failed to read config '{}': {message}", path.display())
            }
            Self::Parse {
                path,
                message,
                line,
                column,
            } => {
                if let Some(path) = path {
                    write!(f, "failed to parse config '{}'", path.display())?;
                } else {
                    write!(f, "failed to parse config")?;
                }
                if let (Some(line), Some(column)) = (line, column) {
                    write!(f, " at {line}:{column}")?;
                }
                write!(f, ": {message}")
            }
            Self::Validation { diagnostics } => {
                write!(
                    f,
                    "config validation failed with {} error(s)",
                    diagnostics.len()
                )
            }
            Self::Schema { message } => write!(f, "failed to export config schema: {message}"),
            Self::UnsupportedSchema { found, supported } => write!(
                f,
                "config schema version {found} is newer than supported version {supported}"
            ),
        }
    }
}

impl Error for ConfigTomlError {}

fn current_fingerprint(
    options: &ConfigLoadOptions,
) -> Result<Option<FileFingerprint>, ConfigTomlError> {
    if let Some(path) = &options.explicit_path {
        return fingerprint_for_path(path).map(Some);
    }

    for path in candidate_paths_from_platform(options.platform) {
        if path.exists() {
            return fingerprint_for_path(&path).map(Some);
        }
    }

    Ok(None)
}

fn candidate_paths_from_platform(platform: ConfigPlatform) -> Vec<PathBuf> {
    candidate_paths_from_env(platform, |key| std::env::var_os(key))
}

fn fingerprint_for_path(path: &Path) -> Result<FileFingerprint, ConfigTomlError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(FileFingerprint {
                path: path.to_path_buf(),
                exists: false,
                modified: None,
                len: 0,
                content_hash: None,
            });
        }
        Err(error) => {
            return Err(ConfigTomlError::Io {
                path: path.to_path_buf(),
                message: error.to_string(),
            });
        }
    };

    let contents = fs::read(path).map_err(|error| ConfigTomlError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let mut hasher = DefaultHasher::new();
    contents.hash(&mut hasher);

    Ok(FileFingerprint {
        path: path.to_path_buf(),
        exists: true,
        modified: metadata.modified().ok(),
        len: metadata.len(),
        content_hash: Some(hasher.finish()),
    })
}

pub fn load(options: ConfigLoadOptions) -> Result<LoadedConfig, ConfigTomlError> {
    if let Some(path) = options.explicit_path {
        return load_path(path, true, options.platform);
    }

    for path in candidate_paths_from_platform(options.platform) {
        if path.exists() {
            return load_path(path, false, options.platform);
        }
    }

    let config = AppConfig::default().resolved_for_platform(options.platform);
    Ok(LoadedConfig {
        config,
        source: ConfigSource::Default,
        diagnostics: Vec::new(),
    })
}

pub fn load_path(
    path: impl Into<PathBuf>,
    explicit: bool,
    platform: ConfigPlatform,
) -> Result<LoadedConfig, ConfigTomlError> {
    let path = path.into();
    let contents = fs::read_to_string(&path).map_err(|error| ConfigTomlError::Io {
        path: path.clone(),
        message: error.to_string(),
    })?;
    let mut loaded = parse_str(&contents, Some(path.clone()), platform)?;
    loaded.source = if explicit {
        ConfigSource::ExplicitFile(path)
    } else {
        ConfigSource::File(path)
    };
    Ok(loaded)
}

pub fn parse_str(
    contents: &str,
    path: Option<PathBuf>,
    platform: ConfigPlatform,
) -> Result<LoadedConfig, ConfigTomlError> {
    let mut value = contents
        .parse::<toml::Value>()
        .map_err(|error| parse_error(error, path.clone(), contents))?;
    // Preserve source locations for type errors before the value-based migration pass.
    let _ = toml::from_str::<AppConfig>(contents)
        .map_err(|error| parse_error(error, path.clone(), contents))?;

    let mut diagnostics = Vec::new();
    diagnostics.extend(detect_deprecated_settings(&value));
    diagnostics.extend(migrate_config_value(&mut value)?);
    apply_profile_defaults(&mut value);
    diagnostics.extend(detect_unknown_settings(&value));

    let config = value
        .try_into::<AppConfig>()
        .map_err(|error| parse_error(error, path.clone(), contents))?
        .resolved_for_platform(platform);
    let ValidationReport {
        diagnostics: validation,
    } = config.validate();
    let has_validation_errors = validation
        .iter()
        .any(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Error);
    diagnostics.extend(validation);

    if has_validation_errors {
        return Err(ConfigTomlError::Validation { diagnostics });
    }

    Ok(LoadedConfig {
        config,
        source: ConfigSource::Default,
        diagnostics,
    })
}

fn apply_profile_defaults(value: &mut toml::Value) {
    let visual_name = value
        .get("visual_theme")
        .and_then(|theme| theme.get("name"))
        .and_then(toml::Value::as_str);
    let performance_profile = value
        .get("performance")
        .and_then(|performance| performance.get("profile"))
        .and_then(toml::Value::as_str)
        .and_then(parse_performance_profile);

    let mut resolved = AppConfig::default();
    let visual_applied = visual_name.is_some_and(|name| resolved.apply_visual_profile(name));
    if let Some(profile) = performance_profile {
        resolved.performance.apply_profile(profile);
    }
    if !visual_applied && performance_profile.is_none() {
        return;
    }

    let Ok(mut defaults) = toml::Value::try_from(resolved) else {
        return;
    };
    merge_toml(&mut defaults, value.clone());
    *value = defaults;
}

fn merge_toml(target: &mut toml::Value, explicit: toml::Value) {
    match (target, explicit) {
        (toml::Value::Table(target), toml::Value::Table(explicit)) => {
            for (key, value) in explicit {
                if let Some(existing) = target.get_mut(&key) {
                    merge_toml(existing, value);
                } else {
                    target.insert(key, value);
                }
            }
        }
        (target, explicit) => *target = explicit,
    }
}

fn parse_performance_profile(value: &str) -> Option<PerformanceProfile> {
    match value.trim().to_ascii_lowercase().as_str() {
        "maximum_performance" => Some(PerformanceProfile::MaximumPerformance),
        "balanced" => Some(PerformanceProfile::Balanced),
        "visual" => Some(PerformanceProfile::Visual),
        "battery_saver" | "battery_conscious" => Some(PerformanceProfile::BatterySaver),
        _ => None,
    }
}

pub fn default_config_toml() -> Result<String, ConfigTomlError> {
    toml::to_string_pretty(&AppConfig::default()).map_err(|error| ConfigTomlError::Parse {
        path: None,
        message: error.to_string(),
        line: None,
        column: None,
    })
}

pub fn write_default_config(path: impl AsRef<Path>) -> Result<(), ConfigTomlError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| ConfigTomlError::Io {
            path: parent.to_path_buf(),
            message: error.to_string(),
        })?;
    }
    fs::write(path, default_config_toml()?).map_err(|error| ConfigTomlError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

pub fn schema_json() -> Result<String, ConfigTomlError> {
    serde_json::to_string_pretty(&export_schema()).map_err(|error| ConfigTomlError::Schema {
        message: error.to_string(),
    })
}

#[must_use]
pub fn candidate_paths_for_current_platform() -> Vec<PathBuf> {
    candidate_paths_from_env(ConfigPlatform::current(), |key| std::env::var_os(key))
}

#[must_use]
pub fn candidate_paths_from_env(
    platform: ConfigPlatform,
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    match platform {
        ConfigPlatform::Windows => {
            if let Some(appdata) = env("APPDATA") {
                paths.push(PathBuf::from(appdata).join("Panea").join("config.toml"));
            }
            if let Some(user_profile) = env("USERPROFILE") {
                paths.push(
                    PathBuf::from(user_profile)
                        .join(".config")
                        .join("panea")
                        .join("config.toml"),
                );
            }
        }
        ConfigPlatform::MacOs
        | ConfigPlatform::Linux
        | ConfigPlatform::LinuxX11
        | ConfigPlatform::LinuxWayland
        | ConfigPlatform::Unknown => {
            if let Some(config_home) = env("XDG_CONFIG_HOME") {
                paths.push(PathBuf::from(config_home).join("panea").join("config.toml"));
            }
            if let Some(home) = env("HOME") {
                paths.push(
                    PathBuf::from(home)
                        .join(".config")
                        .join("panea")
                        .join("config.toml"),
                );
            }
        }
    }

    paths
}

fn parse_error(error: toml::de::Error, path: Option<PathBuf>, contents: &str) -> ConfigTomlError {
    let location = error
        .span()
        .map(|span| line_column_for_offset(contents, span.start));
    let (line, column) = match location {
        Some((line, column)) => (Some(line), Some(column)),
        None => (None, None),
    };
    ConfigTomlError::Parse {
        path,
        message: error.message().to_owned(),
        line,
        column,
    }
}

fn line_column_for_offset(contents: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for (index, ch) in contents.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

fn detect_unknown_settings(value: &toml::Value) -> Vec<ConfigDiagnostic> {
    let mut diagnostics = Vec::new();
    let known = known_paths();
    collect_unknown(value, "", &known, &mut diagnostics);
    diagnostics
}

fn collect_unknown(
    value: &toml::Value,
    prefix: &str,
    known: &BTreeSet<&'static str>,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) {
    let Some(table) = value.as_table() else {
        return;
    };

    for (key, child) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };

        if !known.contains(path.as_str()) && !is_known_dynamic_path(&path) {
            diagnostics.push(ConfigDiagnostic {
                severity: ConfigDiagnosticSeverity::Warning,
                path: path.clone(),
                message: "unknown config setting will be ignored".to_owned(),
            });
        }

        match child {
            toml::Value::Table(_) => collect_unknown(child, &path, known, diagnostics),
            toml::Value::Array(items) => {
                for item in items {
                    collect_unknown(item, &path, known, diagnostics);
                }
            }
            _ => {}
        }
    }
}

fn detect_deprecated_settings(value: &toml::Value) -> Vec<ConfigDiagnostic> {
    let mut diagnostics = Vec::new();
    let deprecated = BTreeMap::from([
        ("font.font_size", "font.size"),
        ("fonts.font_size", "font.size"),
        ("window.decorations", "window.decoration_strategy"),
        ("platform_overrides", "platform"),
        ("shells", "shell_profiles"),
    ]);

    for (old, new) in deprecated {
        if value_at_path(value, old).is_some() {
            diagnostics.push(ConfigDiagnostic {
                severity: ConfigDiagnosticSeverity::Warning,
                path: old.to_owned(),
                message: format!("deprecated setting; use {new}"),
            });
        }
    }

    diagnostics
}

fn migrate_config_value(value: &mut toml::Value) -> Result<Vec<ConfigDiagnostic>, ConfigTomlError> {
    let Some(root) = value.as_table_mut() else {
        return Ok(Vec::new());
    };
    let found = root
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .unwrap_or(1);
    let found = u16::try_from(found).unwrap_or(u16::MAX);
    if found > CURRENT_CONFIG_SCHEMA_VERSION {
        return Err(ConfigTomlError::UnsupportedSchema {
            found,
            supported: CURRENT_CONFIG_SCHEMA_VERSION,
        });
    }

    let mut diagnostics = Vec::new();
    if found < 2 {
        migrate_table_key(root, "fonts", "font", &mut diagnostics);
        migrate_table_key(root, "platform_overrides", "platform", &mut diagnostics);
        migrate_table_key(root, "shells", "shell_profiles", &mut diagnostics);
        if let Some(font) = root.get_mut("font").and_then(toml::Value::as_table_mut) {
            migrate_table_key(font, "font_size", "size", &mut diagnostics);
        }
        if let Some(window) = root.get_mut("window").and_then(toml::Value::as_table_mut) {
            migrate_table_key(
                window,
                "decorations",
                "decoration_strategy",
                &mut diagnostics,
            );
        }
        diagnostics.push(ConfigDiagnostic {
            severity: ConfigDiagnosticSeverity::Warning,
            path: "schema_version".to_owned(),
            message: format!(
                "migrated config schema from version {found} to {} in memory; regenerate the config to persist the current schema",
                CURRENT_CONFIG_SCHEMA_VERSION
            ),
        });
    }
    root.insert(
        "schema_version".to_owned(),
        toml::Value::Integer(i64::from(CURRENT_CONFIG_SCHEMA_VERSION)),
    );
    Ok(diagnostics)
}

fn migrate_table_key(
    table: &mut toml::map::Map<String, toml::Value>,
    old: &str,
    new: &str,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) {
    let Some(old_value) = table.remove(old) else {
        return;
    };
    if table.contains_key(new) {
        diagnostics.push(ConfigDiagnostic {
            severity: ConfigDiagnosticSeverity::Warning,
            path: old.to_owned(),
            message: format!("ignored deprecated setting because {new} is also present"),
        });
    } else {
        table.insert(new.to_owned(), old_value);
    }
}

fn value_at_path<'a>(value: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.as_table()?.get(part)?;
    }
    Some(current)
}

fn known_paths() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "schema_version",
        "window",
        "window.title",
        "window.columns",
        "window.rows",
        "window.initial_width",
        "window.initial_height",
        "window.padding_x",
        "window.padding_y",
        "window.margin_x",
        "window.margin_y",
        "window.opacity",
        "window.mode",
        "window.linux_backend",
        "window.decoration_strategy",
        "window.fullscreen_titlebar",
        "window.fullscreen_titlebar.enabled",
        "window.fullscreen_titlebar.height",
        "window.fullscreen_titlebar.reveal_height",
        "window.fullscreen_titlebar.show_window_controls",
        "window.fullscreen_titlebar.animation",
        "window.fullscreen_titlebar.animation_duration_ms",
        "window.fullscreen_titlebar.hide_delay_ms",
        "renderer",
        "renderer.backend",
        "renderer.vsync",
        "renderer.damage_tracking",
        "renderer.present_mode",
        "renderer.gpu_timestamps",
        "font",
        "font.family",
        "font.size",
        "font.line_height",
        "font.fallback_families",
        "font.ligatures",
        "fonts",
        "fonts.family",
        "fonts.size",
        "fonts.line_height",
        "fonts.fallback_families",
        "fonts.ligatures",
        "colors",
        "colors.foreground",
        "colors.foreground.red",
        "colors.foreground.green",
        "colors.foreground.blue",
        "colors.foreground.alpha",
        "colors.background",
        "colors.background.red",
        "colors.background.green",
        "colors.background.blue",
        "colors.background.alpha",
        "colors.cursor",
        "colors.cursor.red",
        "colors.cursor.green",
        "colors.cursor.blue",
        "colors.cursor.alpha",
        "colors.cursor_text",
        "colors.selection_foreground",
        "colors.selection_background",
        "colors.selection_background.red",
        "colors.selection_background.green",
        "colors.selection_background.blue",
        "colors.selection_background.alpha",
        "colors.palette",
        "visual_theme",
        "visual_theme.name",
        "visual_theme.cursor_profile",
        "visual_theme.prompt_decoration_profile",
        "visual_theme.command_block_profile",
        "visual_theme.animation_profile",
        "visual_theme.grouping_style",
        "visual_theme.spacing",
        "visual_theme.spacing.cell_gap_px",
        "visual_theme.spacing.block_margin_px",
        "visual_theme.spacing.block_padding_px",
        "visual_theme.spacing.badge_gap_px",
        "visual_theme.borders",
        "visual_theme.borders.width_px",
        "visual_theme.borders.radius_px",
        "visual_theme.borders.color",
        "visual_theme.badges",
        "visual_theme.badges.shell",
        "visual_theme.badges.current_directory",
        "visual_theme.badges.remote",
        "visual_theme.badges.admin",
        "visual_theme.badges.status",
        "visual_theme.prompt_background",
        "visual_theme.command_background",
        "visual_theme.input_background",
        "visual_theme.output_background",
        "visual_theme.badge_background",
        "visual_theme.badge_foreground",
        "visual_theme.success_color",
        "visual_theme.error_color",
        "scrollback",
        "scrollback.lines",
        "scrollback.preserve_on_resize",
        "cursor",
        "cursor.shape",
        "cursor.blink",
        "cursor.blink_interval_ms",
        "cursor.thickness",
        "cursor.corner_radius",
        "cursor.color",
        "cursor.inactive_shape",
        "cursor.inactive_color",
        "cursor.mode_specific_styles",
        "cursor.animations_enabled",
        "cursor.smooth_movement",
        "cursor.typing_pulse",
        "cursor.typing_stretch",
        "cursor.trail",
        "cursor.blink_easing",
        "cursor.short_lived_glow",
        "cursor.shadow",
        "cursor.image",
        "cursor.image.enabled",
        "cursor.image.path",
        "cursor.image.fps",
        "cursor.image.warn_if_expensive",
        "cursor.vector",
        "cursor.vector.enabled",
        "cursor.vector.path",
        "command_blocks",
        "command_blocks.enabled",
        "command_blocks.style",
        "command_blocks.separate_prompt_input_output",
        "command_blocks.show_duration",
        "command_blocks.show_exit_status",
        "command_blocks.show_current_directory",
        "command_blocks.show_shell_host",
        "command_blocks.allow_in_alternate_screen",
        "command_blocks.copy_actions_enabled",
        "command_blocks.jump_actions_enabled",
        "command_blocks.collapse_long_output",
        "command_blocks.collapse_after_lines",
        "command_blocks.collapsed_preview_lines",
        "prompt_decorations",
        "prompt_decorations.enabled",
        "prompt_decorations.style",
        "prompt_decorations.show_shell_badge",
        "prompt_decorations.show_current_directory",
        "prompt_decorations.show_remote_host",
        "prompt_decorations.show_admin_badge",
        "prompt_decorations.show_previous_status_accent",
        "prompt_decorations.allow_in_alternate_screen",
        "shell_integration",
        "shell_integration.enabled",
        "shell_integration.activation",
        "shell_integration.auto_install",
        "shell_integration.enabled_shells",
        "shell_integration.disabled_shell_profiles",
        "shell_integration.remote_instructions",
        "keyboard",
        "keyboard.keybindings",
        "mouse",
        "mouse.bindings",
        "mouse.copy_on_select",
        "mouse.hide_cursor_when_typing",
        "clipboard",
        "clipboard.enabled",
        "clipboard.copy_on_select",
        "clipboard.paste_protection",
        "clipboard.bracketed_paste",
        "clipboard.middle_click_paste",
        "clipboard.prefer_primary_selection_on_linux",
        "clipboard.log_operations",
        "clipboard.osc52",
        "clipboard.osc52.enabled",
        "clipboard.osc52.allow_local",
        "clipboard.osc52.allow_remote",
        "clipboard.osc52.max_bytes",
        "clipboard.osc52.confirm_remote_writes",
        "notifications",
        "notifications.enabled",
        "notifications.only_when_unfocused",
        "notifications.session_closed",
        "notifications.transport_errors",
        "paste",
        "paste.bracketed_paste",
        "paste.normalize_newlines",
        "paste.strip_control_characters",
        "default_shell_profile",
        "shell_profiles",
        "ssh_profiles",
        "ssh_profiles.name",
        "ssh_profiles.host",
        "ssh_profiles.port",
        "ssh_profiles.username",
        "ssh_profiles.user",
        "ssh_profiles.auth_method",
        "ssh_profiles.identity_file",
        "ssh_profiles.known_hosts_policy",
        "ssh_profiles.remote_command",
        "ssh_profiles.remote_working_directory",
        "ssh_profiles.shell_integration",
        "ssh_profiles.agent_forwarding",
        "ssh_profiles.proxy_jump",
        "mux",
        "mux.enabled",
        "mux.restore_sessions",
        "mux.default_workspace",
        "mux.show_tab_bar",
        "mux.drag_tabs",
        "mux.drag_panes",
        "mux.tab_title_format",
        "mux.status_format",
        "mux.pane_resize_step",
        "mux.remember_working_directory",
        "mux.startup_workspaces",
        "mux.startup_workspaces.name",
        "mux.startup_workspaces.tabs",
        "mux.startup_workspaces.tabs.name",
        "mux.startup_workspaces.tabs.layout",
        "mux.startup_workspaces.tabs.layout.kind",
        "mux.startup_workspaces.tabs.layout.profile",
        "mux.startup_workspaces.tabs.layout.transport",
        "mux.startup_workspaces.tabs.layout.working_directory",
        "mux.startup_workspaces.tabs.layout.axis",
        "mux.startup_workspaces.tabs.layout.ratio",
        "mux.startup_workspaces.tabs.layout.first",
        "mux.startup_workspaces.tabs.layout.second",
        "mux.appearance",
        "mux.appearance.tab_bar_background",
        "mux.appearance.active_tab_foreground",
        "mux.appearance.active_tab_background",
        "mux.appearance.inactive_tab_foreground",
        "mux.appearance.inactive_tab_background",
        "mux.appearance.active_pane_border",
        "mux.appearance.inactive_pane_border",
        "mux.appearance.pane_border_width",
        "performance",
        "performance.profile",
        "performance.frame_rate_limit",
        "performance.glyph_cache_entries",
        "performance.max_frame_time_ms",
        "performance.expensive_effect_warnings",
        "performance.max_animation_fps",
        "performance.max_cursor_asset_size_kb",
        "performance.max_active_animations",
        "performance.max_animated_region_pixels",
        "performance.disable_expensive_effects_on_battery",
        "platform",
        "platform.macos",
        "platform.linux",
        "platform.windows",
        "platform.linux_x11",
        "platform.linux_wayland",
        "platform_overrides",
        "diagnostics",
        "diagnostics.enabled",
        "diagnostics.performance_overlay",
        "diagnostics.performance_overlay_position",
        "diagnostics.performance_overlay_detail",
        "diagnostics.persist_performance_overlay",
        "diagnostics.capability_report",
        "diagnostics.log_level",
    ])
}

fn is_known_dynamic_path(path: &str) -> bool {
    let dynamic_prefixes = [
        "shell_profiles.",
        "ssh_profiles.",
        "keyboard.keybindings.",
        "mouse.bindings.",
        "platform.macos.",
        "platform.linux.",
        "platform.windows.",
        "platform.linux_x11.",
        "platform.linux_wayland.",
        "platform_overrides.",
        "clipboard.osc52.",
        "colors.palette.",
        "cursor.color.",
        "cursor.inactive_color.",
        "colors.cursor_text.",
        "colors.selection_foreground.",
        "cursor.mode_specific_styles.",
        "visual_theme.borders.color.",
        "visual_theme.success_color.",
        "visual_theme.error_color.",
        "mux.startup_workspaces.",
        "mux.appearance.tab_bar_background.",
        "mux.appearance.active_tab_foreground.",
        "mux.appearance.active_tab_background.",
        "mux.appearance.inactive_tab_foreground.",
        "mux.appearance.inactive_tab_background.",
        "mux.appearance.active_pane_border.",
        "mux.appearance.inactive_pane_border.",
    ];

    dynamic_prefixes
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

impl From<io::Error> for ConfigTomlError {
    fn from(error: io::Error) -> Self {
        Self::Io {
            path: PathBuf::new(),
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_loads_defaults() {
        let loaded = load(ConfigLoadOptions {
            explicit_path: None,
            platform: ConfigPlatform::Unknown,
        })
        .expect("missing config should use defaults");

        assert_eq!(loaded.source, ConfigSource::Default);
        assert_eq!(loaded.config, AppConfig::default());
    }

    #[test]
    fn partial_toml_uses_defaults_and_platform_overrides() {
        let loaded = parse_str(
            r#"
            [window]
            title = "Base"

            [platform.windows.window]
            title = "Windows"
            "#,
            None,
            ConfigPlatform::Windows,
        )
        .expect("config should parse");

        assert_eq!(loaded.config.window.title, "Windows");
        assert_eq!(loaded.config.window.rows, AppConfig::default().window.rows);
    }

    #[test]
    fn legacy_schema_is_migrated_before_deserialization() {
        let loaded = parse_str(
            r#"
            schema_version = 1

            [fonts]
            font_size = 15.0

            [platform_overrides.windows.window]
            title = "Migrated"
            "#,
            None,
            ConfigPlatform::Windows,
        )
        .expect("legacy config should migrate");

        assert_eq!(loaded.config.schema_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(loaded.config.font.size, 15.0);
        assert_eq!(loaded.config.window.title, "Migrated");
        assert!(loaded.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "schema_version" && diagnostic.message.contains("migrated")
        }));
    }

    #[test]
    fn future_schema_is_rejected_clearly() {
        let error = parse_str("schema_version = 999\n", None, ConfigPlatform::Unknown)
            .expect_err("future config cannot be interpreted safely");

        assert!(matches!(
            error,
            ConfigTomlError::UnsupportedSchema {
                found: 999,
                supported: CURRENT_CONFIG_SCHEMA_VERSION
            }
        ));
    }

    #[test]
    fn parse_error_reports_line_and_column() {
        let error = parse_str(
            r#"
            [window]
            rows = "not a number"
            "#,
            None,
            ConfigPlatform::Unknown,
        )
        .expect_err("bad type should fail");

        let ConfigTomlError::Parse { line, column, .. } = error else {
            panic!("expected parse error");
        };
        assert!(line.is_some());
        assert!(column.is_some());
    }

    #[test]
    fn unknown_and_deprecated_settings_are_diagnostics() {
        let loaded = parse_str(
            r#"
            shells = []

            [window]
            strange = true
            "#,
            None,
            ConfigPlatform::Unknown,
        )
        .expect("unknown settings should warn, not fail");

        assert!(
            loaded
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path == "window.strange")
        );
        assert!(
            loaded
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path == "shells")
        );
    }

    #[test]
    fn validation_errors_fail_load() {
        let error = parse_str(
            r#"
            [font]
            size = 2.0
            "#,
            None,
            ConfigPlatform::Unknown,
        )
        .expect_err("invalid config should fail");

        let ConfigTomlError::Validation { diagnostics } = error else {
            panic!("expected validation error");
        };
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path == "font.size")
        );
    }

    #[test]
    fn clipboard_config_paths_are_known_and_parsed() {
        let loaded = parse_str(
            r#"
            [clipboard]
            copy_on_select = false
            paste_protection = true
            bracketed_paste = true

            [clipboard.osc52]
            enabled = true
            allow_local = true
            allow_remote = false
            max_bytes = 4096
            confirm_remote_writes = true
            "#,
            None,
            ConfigPlatform::Unknown,
        )
        .expect("clipboard config should parse");

        assert!(!loaded.config.clipboard.copy_on_select);
        assert_eq!(loaded.config.clipboard.osc52.max_bytes, 4096);
        assert!(loaded.diagnostics.iter().all(|diagnostic| {
            diagnostic.path == "schema_version" && diagnostic.message.contains("migrated")
        }));
    }

    #[test]
    fn candidate_paths_are_platform_specific() {
        let mut env = BTreeMap::new();
        env.insert("APPDATA", "C:\\Users\\me\\AppData\\Roaming");
        env.insert("USERPROFILE", "C:\\Users\\me");
        let windows_paths = candidate_paths_from_env(ConfigPlatform::Windows, |key| {
            env.get(key).map(std::ffi::OsString::from)
        });

        assert!(windows_paths.iter().any(|path| {
            path.to_string_lossy()
                .replace('/', "\\")
                .ends_with("Panea\\config.toml")
        }));

        let mut env = BTreeMap::new();
        env.insert("XDG_CONFIG_HOME", "/home/me/.config");
        let linux_paths = candidate_paths_from_env(ConfigPlatform::LinuxWayland, |key| {
            env.get(key).map(std::ffi::OsString::from)
        });
        assert_eq!(
            linux_paths[0],
            PathBuf::from("/home/me/.config/panea/config.toml")
        );
    }

    #[test]
    fn semantic_visual_theme_and_collapse_settings_parse_portably() {
        let loaded = parse_str(
            r#"
            [visual_theme]
            badge_foreground = { red = 1, green = 2, blue = 3, alpha = 255 }

            [visual_theme.spacing]
            block_margin_px = 5

            [command_blocks]
            enabled = true
            style = "custom_theme"
            collapse_long_output = true
            collapse_after_lines = 80
            collapsed_preview_lines = 2
            "#,
            None,
            ConfigPlatform::Windows,
        )
        .expect("semantic visual config");

        assert_eq!(loaded.config.visual_theme.spacing.block_margin_px, 5);
        assert_eq!(loaded.config.visual_theme.badge_foreground.red, 1);
        assert_eq!(loaded.config.command_blocks.collapse_after_lines, 80);
        assert_eq!(loaded.config.command_blocks.collapsed_preview_lines, 2);
        assert!(
            !loaded
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("unknown setting"))
        );
    }

    #[test]
    fn default_config_and_schema_export() {
        let config = default_config_toml().expect("default config should serialize");
        assert!(config.contains("[window]"));

        let schema = schema_json().expect("schema should serialize");
        assert!(schema.contains("\"schema_version\""));
        assert!(schema.contains("font.family"));
        assert!(schema.contains("ssh_profiles.known_hosts_policy"));
        assert!(schema.contains("notifications.only_when_unfocused"));
    }

    #[test]
    fn notification_config_parses_with_portable_defaults() {
        let loaded = parse_str(
            r#"
            [notifications]
            enabled = true
            only_when_unfocused = false
            session_closed = false
            transport_errors = true
            "#,
            None,
            ConfigPlatform::Unknown,
        )
        .expect("notification config should parse");

        assert!(loaded.config.notifications.enabled);
        assert!(!loaded.config.notifications.only_when_unfocused);
        assert!(!loaded.config.notifications.session_closed);
        assert!(loaded.config.notifications.transport_errors);
        assert!(
            loaded
                .diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.message.contains("unknown setting"))
        );
    }

    #[test]
    fn ssh_profile_toml_parses_baseline_fields() {
        let loaded = parse_str(
            r#"
            [[ssh_profiles]]
            name = "prod"
            host = "example.com"
            port = 2222
            username = "deploy"
            auth_method = "public_key"
            identity_file = "~/.ssh/id_ed25519"
            known_hosts_policy = "require_known"
            remote_working_directory = "/srv/app"
            shell_integration = true
            agent_forwarding = false
            "#,
            None,
            ConfigPlatform::Unknown,
        )
        .expect("SSH profile should parse");

        let profile = &loaded.config.ssh_profiles[0];
        assert_eq!(profile.name, "prod");
        assert_eq!(profile.host, "example.com");
        assert_eq!(profile.port, 2222);
        assert_eq!(profile.username.as_deref(), Some("deploy"));
    }

    #[test]
    fn shipped_visual_examples_parse() {
        for (name, contents) in [
            (
                "plain-fast.toml",
                include_str!("../../assets/config-examples/plain-fast.toml"),
            ),
            (
                "balanced.toml",
                include_str!("../../assets/config-examples/balanced.toml"),
            ),
            (
                "command-blocks.toml",
                include_str!("../../assets/config-examples/command-blocks.toml"),
            ),
            (
                "minimal-aesthetic.toml",
                include_str!("../../assets/config-examples/minimal-aesthetic.toml"),
            ),
            (
                "heavy-visual-demo.toml",
                include_str!("../../assets/config-examples/heavy-visual-demo.toml"),
            ),
            (
                "foundational-customization.toml",
                include_str!("../../assets/config-examples/foundational-customization.toml"),
            ),
            (
                "custom-cursor.toml",
                include_str!("../../assets/config-examples/custom-cursor.toml"),
            ),
        ] {
            parse_str(contents, None, ConfigPlatform::Unknown)
                .unwrap_or_else(|error| panic!("{name} should parse: {error}"));
        }
    }

    #[test]
    fn named_profiles_expand_once_and_explicit_values_win() {
        let loaded = parse_str(
            r#"
            schema_version = 2

            [visual_theme]
            name = "minimal-aesthetic"

            [colors]
            background = { red = 1, green = 2, blue = 3, alpha = 255 }

            [cursor]
            thickness = 0.2

            [performance]
            profile = "battery_saver"
            max_animation_fps = 12
            "#,
            None,
            ConfigPlatform::Windows,
        )
        .expect("profiles should compile into AppConfig");

        assert_eq!(loaded.config.cursor.shape, config_core::CursorShape::Beam);
        assert_eq!(loaded.config.cursor.thickness, 0.2);
        assert_eq!(
            loaded.config.colors.background,
            config_core::RgbaColor::rgb(1, 2, 3)
        );
        assert_eq!(loaded.config.performance.frame_rate_limit, Some(30));
        assert_eq!(loaded.config.performance.max_animation_fps, 12);
    }

    #[test]
    fn recursive_startup_mux_layout_parses_without_unknown_diagnostics() {
        let loaded = parse_str(
            r#"
            [[shell_profiles]]
            name = "dev"

            [[ssh_profiles]]
            name = "prod"
            host = "example.test"
            known_hosts_policy = "require_known"

            [[mux.startup_workspaces]]
            name = "work"

            [[mux.startup_workspaces.tabs]]
            name = "mixed"

            [mux.startup_workspaces.tabs.layout]
            kind = "split"
            axis = "horizontal"
            ratio = 0.6

            [mux.startup_workspaces.tabs.layout.first]
            kind = "pane"
            profile = "dev"
            transport = "local"

            [mux.startup_workspaces.tabs.layout.second]
            kind = "pane"
            profile = "prod"
            transport = "ssh"
            "#,
            None,
            ConfigPlatform::Unknown,
        )
        .expect("startup mux config");

        assert_eq!(loaded.config.mux.startup_workspaces.len(), 1);
        assert!(
            !loaded
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.message.contains("unknown config setting") })
        );
    }

    #[test]
    fn watcher_reloads_valid_file_changes() {
        let path = temp_config_path("watcher-reloads-valid-file-changes");
        fs::write(&path, "[window]\ntitle = \"Initial\"\n").expect("write initial config");
        let options = ConfigLoadOptions {
            explicit_path: Some(path.clone()),
            platform: ConfigPlatform::Unknown,
        };
        let mut watcher = ConfigWatcher::new(options)
            .with_poll_interval(Duration::ZERO)
            .with_debounce(Duration::ZERO);

        fs::write(&path, "[window]\ntitle = \"Reloaded\"\n").expect("write changed config");

        let event = watcher.poll();
        let ConfigWatchEvent::Reloaded(loaded) = event else {
            panic!("expected reload, got {event:?}");
        };
        assert_eq!(loaded.config.window.title, "Reloaded");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn watcher_reports_invalid_file_without_repeating_until_change() {
        let path = temp_config_path("watcher-reports-invalid-file");
        fs::write(&path, "[window]\ntitle = \"Initial\"\n").expect("write initial config");
        let options = ConfigLoadOptions {
            explicit_path: Some(path.clone()),
            platform: ConfigPlatform::Unknown,
        };
        let mut watcher = ConfigWatcher::new(options)
            .with_poll_interval(Duration::ZERO)
            .with_debounce(Duration::ZERO);

        fs::write(&path, "[font]\nsize = 2.0\n").expect("write invalid config");
        let event = watcher.poll();
        assert!(matches!(
            event,
            ConfigWatchEvent::Failed {
                error: ConfigTomlError::Validation { .. },
                ..
            }
        ));
        assert!(matches!(watcher.poll(), ConfigWatchEvent::Unchanged));

        fs::write(&path, "[window]\ntitle = \"Recovered\"\n").expect("write recovered config");
        let event = watcher.poll();
        let ConfigWatchEvent::Reloaded(loaded) = event else {
            panic!("expected recovered reload, got {event:?}");
        };
        assert_eq!(loaded.config.window.title, "Recovered");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn custom_cursor_animation_and_image_config_is_portable() {
        let loaded = parse_str(
            r#"
            schema_version = 2

            [cursor]
            animations_enabled = true
            smooth_movement = true
            typing_pulse = true
            typing_stretch = true
            trail = true
            blink_easing = true
            short_lived_glow = true
            shadow = true

            [cursor.image]
            enabled = true
            path = "assets/cursor.gif"
            fps = 24
            warn_if_expensive = true
            "#,
            None,
            ConfigPlatform::Windows,
        )
        .expect("portable cursor config should parse");

        assert!(loaded.config.cursor.shadow);
        assert!(loaded.config.cursor.image.enabled);
        assert_eq!(loaded.config.cursor.image.path, "assets/cursor.gif");
        assert!(
            loaded.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            loaded.diagnostics
        );
    }

    #[test]
    fn desktop_drag_and_overlay_preferences_parse_without_unknown_keys() {
        let loaded = parse_str(
            r#"
            schema_version = 2

            [mux]
            drag_tabs = false
            drag_panes = true

            [diagnostics]
            performance_overlay = true
            performance_overlay_position = "bottom_left"
            performance_overlay_detail = "detailed"
            persist_performance_overlay = false
            "#,
            None,
            ConfigPlatform::LinuxWayland,
        )
        .expect("portable desktop UX config should parse");

        assert!(!loaded.config.mux.drag_tabs);
        assert!(loaded.config.mux.drag_panes);
        assert_eq!(
            loaded.config.diagnostics.performance_overlay_position,
            config_core::PerformanceOverlayPosition::BottomLeft
        );
        assert_eq!(
            loaded.config.diagnostics.performance_overlay_detail,
            config_core::PerformanceOverlayDetail::Detailed
        );
        assert!(!loaded.config.diagnostics.persist_performance_overlay);
        assert!(loaded.diagnostics.is_empty());
    }

    #[test]
    fn portable_vector_cursor_config_parses_without_unknown_keys() {
        let loaded = parse_str(
            r#"
            schema_version = 2
            [cursor.vector]
            enabled = true
            path = "assets/cursor.panea-cursor.json"
            "#,
            None,
            ConfigPlatform::Windows,
        )
        .expect("vector cursor config should parse");
        assert!(loaded.config.cursor.vector.enabled);
        assert_eq!(
            loaded.config.cursor.vector.path,
            "assets/cursor.panea-cursor.json"
        );
        assert!(loaded.diagnostics.is_empty());
    }

    #[test]
    fn portable_fullscreen_titlebar_config_parses_without_unknown_keys() {
        let loaded = parse_str(
            r#"
            schema_version = 2

            [window]
            mode = "borderless_fullscreen"

            [window.fullscreen_titlebar]
            enabled = true
            height = 38
            reveal_height = 4
            show_window_controls = true
            animation = "smooth"
            animation_duration_ms = 140
            hide_delay_ms = 80
            "#,
            None,
            ConfigPlatform::Windows,
        )
        .expect("portable fullscreen titlebar config should parse");

        assert!(loaded.config.window.fullscreen_titlebar.enabled);
        assert_eq!(loaded.config.window.fullscreen_titlebar.height, 38);
        assert_eq!(
            loaded.config.window.fullscreen_titlebar.animation,
            config_core::FullscreenChromeAnimation::Smooth
        );
        assert_eq!(
            loaded
                .config
                .window
                .fullscreen_titlebar
                .animation_duration_ms,
            140
        );
        assert_eq!(loaded.config.window.fullscreen_titlebar.hide_delay_ms, 80);
        assert!(loaded.diagnostics.is_empty());
    }

    #[test]
    fn fullscreen_titlebar_legacy_toml_uses_portable_motion_defaults() {
        let loaded = parse_str(
            r#"
            schema_version = 2
            [window.fullscreen_titlebar]
            enabled = true
            "#,
            None,
            ConfigPlatform::LinuxWayland,
        )
        .expect("legacy titlebar config should parse");

        assert_eq!(
            loaded.config.window.fullscreen_titlebar.animation,
            config_core::FullscreenChromeAnimation::Smooth
        );
        assert_eq!(
            loaded
                .config
                .window
                .fullscreen_titlebar
                .animation_duration_ms,
            120
        );
        assert_eq!(loaded.config.window.fullscreen_titlebar.hide_delay_ms, 120);
        assert!(loaded.diagnostics.is_empty());
    }

    fn temp_config_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("panea-{name}-{}-{nanos}.toml", std::process::id()))
    }
}
