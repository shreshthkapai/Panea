//! Static TOML configuration frontend.

pub const LAYER: &str = "config portability";

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use config_core::{
    AppConfig, ConfigDiagnostic, ConfigDiagnosticSeverity, ConfigPlatform, ValidationReport,
    export_schema,
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
        }
    }
}

impl Error for ConfigTomlError {}

pub fn load(options: ConfigLoadOptions) -> Result<LoadedConfig, ConfigTomlError> {
    if let Some(path) = options.explicit_path {
        return load_path(path, true, options.platform);
    }

    for path in candidate_paths_for_current_platform() {
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
    let value = contents
        .parse::<toml::Value>()
        .map_err(|error| parse_error(error, path.clone(), contents))?;

    let mut diagnostics = Vec::new();
    diagnostics.extend(detect_unknown_settings(&value));
    diagnostics.extend(detect_deprecated_settings(&value));

    let config = toml::from_str::<AppConfig>(contents)
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

fn value_at_path<'a>(value: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.as_table()?.get(part)?;
    }
    Some(current)
}

fn known_paths() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "window",
        "window.title",
        "window.columns",
        "window.rows",
        "window.initial_width",
        "window.initial_height",
        "window.padding_x",
        "window.padding_y",
        "window.opacity",
        "window.mode",
        "window.linux_backend",
        "window.decoration_strategy",
        "renderer",
        "renderer.backend",
        "renderer.vsync",
        "renderer.damage_tracking",
        "renderer.present_mode",
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
        "colors.selection_background",
        "colors.selection_background.red",
        "colors.selection_background.green",
        "colors.selection_background.blue",
        "colors.selection_background.alpha",
        "colors.palette",
        "scrollback",
        "scrollback.lines",
        "scrollback.preserve_on_resize",
        "cursor",
        "cursor.shape",
        "cursor.blink",
        "cursor.blink_interval_ms",
        "cursor.thickness",
        "cursor.animations_enabled",
        "command_blocks",
        "command_blocks.enabled",
        "command_blocks.show_duration",
        "command_blocks.show_exit_status",
        "prompt_decorations",
        "prompt_decorations.enabled",
        "prompt_decorations.show_current_directory",
        "prompt_decorations.show_remote_host",
        "keyboard",
        "keyboard.keybindings",
        "mouse",
        "mouse.bindings",
        "mouse.copy_on_select",
        "mouse.hide_cursor_when_typing",
        "paste",
        "paste.bracketed_paste",
        "paste.normalize_newlines",
        "paste.strip_control_characters",
        "default_shell_profile",
        "shell_profiles",
        "ssh_profiles",
        "mux",
        "mux.enabled",
        "mux.restore_sessions",
        "performance",
        "performance.profile",
        "performance.frame_rate_limit",
        "performance.glyph_cache_entries",
        "performance.max_frame_time_ms",
        "performance.expensive_effect_warnings",
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
        "colors.palette.",
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
    fn candidate_paths_are_platform_specific() {
        let mut env = BTreeMap::new();
        env.insert("APPDATA", "C:\\Users\\me\\AppData\\Roaming");
        env.insert("USERPROFILE", "C:\\Users\\me");
        let windows_paths = candidate_paths_from_env(ConfigPlatform::Windows, |key| {
            env.get(key).map(std::ffi::OsString::from)
        });

        assert!(
            windows_paths
                .iter()
                .any(|path| path.ends_with("Panea\\config.toml"))
        );

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
    fn default_config_and_schema_export() {
        let config = default_config_toml().expect("default config should serialize");
        assert!(config.contains("[window]"));

        let schema = schema_json().expect("schema should serialize");
        assert!(schema.contains("\"schema_version\""));
        assert!(schema.contains("font.family"));
    }
}
