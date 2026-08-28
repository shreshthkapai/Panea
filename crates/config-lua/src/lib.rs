//! Controlled programmable configuration frontend.
//!
//! This crate intentionally does not embed a general-purpose Lua runtime yet.
//! Phase 21 exposes a deterministic `panea.*` API that compiles into
//! `config_core::AppConfig` before runtime hot paths see the config.

pub const LAYER: &str = "config portability";

use std::{
    collections::hash_map::DefaultHasher,
    error::Error,
    fmt, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use config_core::{
    AppConfig, ClipboardConfigPatch, ColorConfigPatch, CommandBlockStyle, CommandBlocksConfigPatch,
    ConfigDiagnostic, ConfigDiagnosticSeverity, ConfigPlatform, ConfigProvider,
    ConfigProviderError, CursorAnimationProfile, CursorConfigPatch, CursorShape,
    DecorationStrategyConfig, DiagnosticsConfigPatch, FontConfigPatch, FullscreenChromeAnimation,
    FullscreenTitlebarConfigPatch, InputOutputGroupingStyle, KeyBinding, LinuxBackendConfig,
    LoadedAppConfig, LogLevel, MouseBinding, NotificationConfigPatch, Osc52ClipboardConfigPatch,
    PerformanceConfigPatch, PerformanceOverlayDetail, PerformanceOverlayPosition,
    PerformanceProfile, PlatformOverride, PlatformOverrides, PresentModePreference,
    PromptDecorationStyle, PromptDecorationsConfigPatch, RendererBackendPreference,
    RendererConfigPatch, RgbaColor, ShellIntegrationActivationConfig, ShellIntegrationConfigPatch,
    ShellProfile, ShellProfileKind, SshProfile, WindowConfigPatch, WindowModeConfig,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgrammableConfigLoadOptions {
    pub explicit_path: Option<PathBuf>,
    pub platform: ConfigPlatform,
}

impl Default for ProgrammableConfigLoadOptions {
    fn default() -> Self {
        Self {
            explicit_path: std::env::var_os("PANEA_CONFIG").map(PathBuf::from),
            platform: ConfigPlatform::current(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedProgrammableConfig {
    pub config: AppConfig,
    pub diagnostics: Vec<ConfigDiagnostic>,
    pub source: ProgrammableConfigSource,
    pub executed_actions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgrammableConfigSource {
    File(PathBuf),
    ExplicitFile(PathBuf),
    Inline,
}

#[derive(Debug, Clone)]
pub struct ProgrammableConfigWatcher {
    path: PathBuf,
    platform: ConfigPlatform,
    poll_interval: Duration,
    debounce: Duration,
    last_poll: Option<Instant>,
    last_seen: Option<ProgrammableFingerprint>,
    pending: Option<(Option<ProgrammableFingerprint>, Instant)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProgrammableConfigWatchEvent {
    Unchanged,
    Pending {
        path: PathBuf,
    },
    Reloaded(Box<LoadedProgrammableConfig>),
    Failed {
        path: PathBuf,
        error: ProgrammableConfigError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgrammableFingerprint {
    modified: Option<SystemTime>,
    len: u64,
    content_hash: u64,
}

impl ProgrammableConfigWatcher {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, platform: ConfigPlatform) -> Self {
        let path = path.into();
        Self {
            last_seen: programmable_fingerprint(&path).ok(),
            path,
            platform,
            poll_interval: Duration::from_millis(500),
            debounce: Duration::from_millis(150),
            last_poll: None,
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

    pub fn poll(&mut self) -> ProgrammableConfigWatchEvent {
        let now = Instant::now();
        if self
            .last_poll
            .is_some_and(|last| now.duration_since(last) < self.poll_interval)
        {
            return ProgrammableConfigWatchEvent::Unchanged;
        }
        self.last_poll = Some(now);
        let fingerprint = programmable_fingerprint(&self.path).ok();
        if fingerprint == self.last_seen {
            self.pending = None;
            return ProgrammableConfigWatchEvent::Unchanged;
        }
        if let Some((pending, first_seen)) = &self.pending
            && *pending == fingerprint
        {
            if now.duration_since(*first_seen) < self.debounce {
                return ProgrammableConfigWatchEvent::Pending {
                    path: self.path.clone(),
                };
            }
        } else {
            self.pending = Some((fingerprint.clone(), now));
            return ProgrammableConfigWatchEvent::Pending {
                path: self.path.clone(),
            };
        }
        self.pending = None;
        self.last_seen = fingerprint;
        match load_path(self.path.clone(), true, self.platform) {
            Ok(loaded) => ProgrammableConfigWatchEvent::Reloaded(Box::new(loaded)),
            Err(error) => ProgrammableConfigWatchEvent::Failed {
                path: self.path.clone(),
                error,
            },
        }
    }
}

fn programmable_fingerprint(path: &Path) -> Result<ProgrammableFingerprint, std::io::Error> {
    let metadata = fs::metadata(path)?;
    let contents = fs::read(path)?;
    let mut hasher = DefaultHasher::new();
    contents.hash(&mut hasher);
    Ok(ProgrammableFingerprint {
        modified: metadata.modified().ok(),
        len: metadata.len(),
        content_hash: hasher.finish(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgrammableConfigError {
    Io {
        path: PathBuf,
        message: String,
    },
    Parse {
        path: Option<PathBuf>,
        line: usize,
        message: String,
    },
    Validation {
        diagnostics: Vec<ConfigDiagnostic>,
    },
    NotFound,
}

impl fmt::Display for ProgrammableConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => {
                write!(
                    f,
                    "failed to read programmable config '{}': {message}",
                    path.display()
                )
            }
            Self::Parse {
                path,
                line,
                message,
            } => {
                if let Some(path) = path {
                    write!(
                        f,
                        "failed to parse programmable config '{}' at line {line}: {message}",
                        path.display()
                    )
                } else {
                    write!(
                        f,
                        "failed to parse programmable config at line {line}: {message}"
                    )
                }
            }
            Self::Validation { diagnostics } => write!(
                f,
                "programmable config validation failed with {} diagnostic(s)",
                diagnostics.len()
            ),
            Self::NotFound => write!(f, "programmable config file not found"),
        }
    }
}

impl Error for ProgrammableConfigError {}

#[derive(Debug, Clone)]
pub struct ProgrammableConfigProvider {
    options: ProgrammableConfigLoadOptions,
}

impl ProgrammableConfigProvider {
    #[must_use]
    pub fn new(options: ProgrammableConfigLoadOptions) -> Self {
        Self { options }
    }
}

impl ConfigProvider for ProgrammableConfigProvider {
    fn load_config(&self) -> Result<LoadedAppConfig, ConfigProviderError> {
        let loaded = load(self.options.clone()).map_err(|error| {
            ConfigProviderError::new(
                self.options
                    .explicit_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                error.to_string(),
            )
        })?;

        Ok(LoadedAppConfig {
            config: loaded.config,
            diagnostics: loaded.diagnostics,
            source: match loaded.source {
                ProgrammableConfigSource::File(path)
                | ProgrammableConfigSource::ExplicitFile(path) => path.display().to_string(),
                ProgrammableConfigSource::Inline => "inline programmable config".to_owned(),
            },
        })
    }
}

#[must_use]
pub fn is_programmable_config_path(path: impl AsRef<Path>) -> bool {
    matches!(
        path.as_ref()
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("panea" | "panea.lua" | "lua")
    )
}

pub fn load(
    options: ProgrammableConfigLoadOptions,
) -> Result<LoadedProgrammableConfig, ProgrammableConfigError> {
    if let Some(path) = options.explicit_path {
        return load_path(path, true, options.platform);
    }

    for path in candidate_paths_for_current_platform() {
        if path.exists() {
            return load_path(path, false, options.platform);
        }
    }

    Err(ProgrammableConfigError::NotFound)
}

pub fn load_path(
    path: impl Into<PathBuf>,
    explicit: bool,
    platform: ConfigPlatform,
) -> Result<LoadedProgrammableConfig, ProgrammableConfigError> {
    let path = path.into();
    let contents = fs::read_to_string(&path).map_err(|error| ProgrammableConfigError::Io {
        path: path.clone(),
        message: error.to_string(),
    })?;
    let mut loaded = parse_str(&contents, Some(path.clone()), platform)?;
    loaded.source = if explicit {
        ProgrammableConfigSource::ExplicitFile(path)
    } else {
        ProgrammableConfigSource::File(path)
    };
    Ok(loaded)
}

pub fn parse_str(
    contents: &str,
    path: Option<PathBuf>,
    platform: ConfigPlatform,
) -> Result<LoadedProgrammableConfig, ProgrammableConfigError> {
    let mut state = ProgramState::new(platform);

    for (index, line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let stripped = strip_comment(line);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }

        state
            .apply_line(line)
            .map_err(|message| ProgrammableConfigError::Parse {
                path: path.clone(),
                line: line_number,
                message,
            })?;
    }

    if state.active_stack.len() != 1 {
        return Err(ProgrammableConfigError::Parse {
            path,
            line: contents.lines().count().max(1),
            message: "unterminated panea.when_platform block".to_owned(),
        });
    }

    let validation = state.config.validate();
    let has_errors = validation.has_errors();
    let mut diagnostics = state.diagnostics;
    if state.executed_actions > 0 {
        diagnostics.push(ConfigDiagnostic {
            severity: ConfigDiagnosticSeverity::Warning,
            path: "program".to_owned(),
            message: format!(
                "programmable config compiled {} action(s) before runtime from {}",
                state.executed_actions,
                path.as_ref().map_or_else(
                    || "inline source".to_owned(),
                    |path| path.display().to_string()
                )
            ),
        });
    }
    diagnostics.extend(validation.diagnostics);
    if has_errors {
        return Err(ProgrammableConfigError::Validation { diagnostics });
    }

    Ok(LoadedProgrammableConfig {
        config: state.config.resolved_for_platform(platform),
        diagnostics,
        source: ProgrammableConfigSource::Inline,
        executed_actions: state.executed_actions,
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
                paths.push(PathBuf::from(appdata).join("Panea").join("config.panea"));
            }
            if let Some(user_profile) = env("USERPROFILE") {
                paths.push(
                    PathBuf::from(user_profile)
                        .join(".config")
                        .join("panea")
                        .join("config.panea"),
                );
            }
        }
        ConfigPlatform::MacOs
        | ConfigPlatform::Linux
        | ConfigPlatform::LinuxX11
        | ConfigPlatform::LinuxWayland
        | ConfigPlatform::Unknown => {
            if let Some(config_home) = env("XDG_CONFIG_HOME") {
                paths.push(
                    PathBuf::from(config_home)
                        .join("panea")
                        .join("config.panea"),
                );
            }
            if let Some(home) = env("HOME") {
                paths.push(
                    PathBuf::from(home)
                        .join(".config")
                        .join("panea")
                        .join("config.panea"),
                );
            }
        }
    }
    paths
}

struct ProgramState {
    config: AppConfig,
    platform: ConfigPlatform,
    active_stack: Vec<bool>,
    diagnostics: Vec<ConfigDiagnostic>,
    executed_actions: usize,
}

impl ProgramState {
    fn new(platform: ConfigPlatform) -> Self {
        Self {
            config: AppConfig::default(),
            platform,
            active_stack: vec![true],
            diagnostics: Vec::new(),
            executed_actions: 0,
        }
    }

    fn apply_line(&mut self, line: &str) -> Result<(), String> {
        let Some((function, args)) = parse_call(line)? else {
            return Err("expected a panea.* function call".to_owned());
        };

        match function {
            "when_platform" | "if_platform" => {
                let platform = parse_platform(expect_string_arg(&args, 0, function)?)?;
                let parent_active = self.active_stack.last().copied().unwrap_or(true);
                self.active_stack
                    .push(parent_active && platform_matches(self.platform, platform));
                return Ok(());
            }
            "end" => {
                if self.active_stack.len() == 1 {
                    return Err("panea.end() without a matching panea.when_platform()".to_owned());
                }
                self.active_stack.pop();
                return Ok(());
            }
            _ => {}
        }

        if !self.active_stack.last().copied().unwrap_or(true) {
            return Ok(());
        }

        match function {
            "set" => {
                let key = expect_string_arg(&args, 0, function)?;
                let value = args
                    .get(1)
                    .ok_or_else(|| "panea.set requires key and value".to_owned())?;
                set_config_value(&mut self.config, key, value)?;
            }
            "platform_set" => {
                let platform = parse_platform(expect_string_arg(&args, 0, function)?)?;
                let key = expect_string_arg(&args, 1, function)?;
                let value = args.get(2).ok_or_else(|| {
                    "panea.platform_set requires platform, key, and value".to_owned()
                })?;
                set_platform_override_value(
                    &mut self.config.platform_overrides,
                    platform,
                    key,
                    value,
                )?;
            }
            "theme" => {
                let name = expect_string_arg(&args, 0, function)?;
                let background = parse_color(expect_string_arg(&args, 1, function)?)?;
                let foreground = parse_color(expect_string_arg(&args, 2, function)?)?;
                let accent = parse_color(expect_string_arg(&args, 3, function)?)?;
                self.config.visual_theme.name = name.to_owned();
                self.config.colors.background = background;
                self.config.colors.foreground = foreground;
                self.config.colors.cursor = accent;
                self.config.colors.selection_background = RgbaColor {
                    red: accent.red,
                    green: accent.green,
                    blue: accent.blue,
                    alpha: 96,
                };
                self.config.visual_theme.success_color = accent;
            }
            "key" => {
                let keys = expect_string_arg(&args, 0, function)?;
                let action = expect_string_arg(&args, 1, function)?;
                self.config
                    .keyboard
                    .keybindings
                    .push(KeyBinding::new(keys, action));
            }
            "mouse" => {
                let gesture = expect_string_arg(&args, 0, function)?;
                let action = expect_string_arg(&args, 1, function)?;
                self.config
                    .mouse
                    .bindings
                    .push(MouseBinding::new(gesture, action));
            }
            "cursor_mode" => {
                let mode = expect_string_arg(&args, 0, function)?;
                let shape = parse_cursor_shape(expect_string_arg(&args, 1, function)?)?;
                self.config
                    .cursor
                    .mode_specific_styles
                    .insert(mode.to_owned(), shape);
            }
            "shell_profile" => {
                let name = expect_string_arg(&args, 0, function)?;
                let kind = parse_shell_profile_kind(expect_string_arg(&args, 1, function)?)?;
                let program = expect_string_arg(&args, 2, function)?;
                let shell_args = args
                    .get(3)
                    .map(value_as_string_array)
                    .transpose()?
                    .unwrap_or_default();
                self.config.shell_profiles.push(ShellProfile {
                    name: name.to_owned(),
                    kind,
                    program: program.to_owned(),
                    args: shell_args,
                    ..ShellProfile::default()
                });
            }
            "ssh_profile" => {
                let name = expect_string_arg(&args, 0, function)?;
                let host = expect_string_arg(&args, 1, function)?;
                let username = args.get(2).map(value_as_string).transpose()?;
                self.config.ssh_profiles.push(SshProfile {
                    name: name.to_owned(),
                    host: host.to_owned(),
                    username,
                    ..SshProfile::default()
                });
            }
            other => {
                return Err(format!(
                    "unsupported programmable config API panea.{other}; allowed APIs are set, platform_set, theme, key, mouse, cursor_mode, shell_profile, ssh_profile, when_platform, end"
                ));
            }
        }

        self.executed_actions += 1;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
enum ConfigValue {
    String(String),
    Number(f64),
    Bool(bool),
    StringArray(Vec<String>),
}

fn parse_call(line: &str) -> Result<Option<(&str, Vec<ConfigValue>)>, String> {
    let Some(rest) = line.strip_prefix("panea.") else {
        return Ok(None);
    };
    let Some(open) = rest.find('(') else {
        return Err("missing '(' in panea.* call".to_owned());
    };
    let function = &rest[..open];
    if function.is_empty() {
        return Err("missing panea API name".to_owned());
    }
    if !rest.ends_with(')') {
        return Err("panea.* calls must end with ')'".to_owned());
    }
    let inner = &rest[open + 1..rest.len() - 1];
    let args = if inner.trim().is_empty() {
        Vec::new()
    } else {
        split_args(inner)?
            .iter()
            .map(|arg| parse_value(arg.trim()))
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(Some((function, args)))
}

fn split_args(input: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut bracket_depth = 0_u32;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                current.push(ch);
                escaped = true;
            }
            '"' => {
                current.push(ch);
                in_string = !in_string;
            }
            '[' if !in_string => {
                bracket_depth += 1;
                current.push(ch);
            }
            ']' if !in_string => {
                bracket_depth = bracket_depth
                    .checked_sub(1)
                    .ok_or_else(|| "unexpected ']' in argument list".to_owned())?;
                current.push(ch);
            }
            ',' if !in_string && bracket_depth == 0 => {
                args.push(current.trim().to_owned());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if in_string {
        return Err("unterminated string literal".to_owned());
    }
    if bracket_depth != 0 {
        return Err("unterminated array literal".to_owned());
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_owned());
    }
    Ok(args)
}

fn parse_value(input: &str) -> Result<ConfigValue, String> {
    if input.starts_with('"') {
        return unquote(input).map(ConfigValue::String);
    }
    if input == "true" {
        return Ok(ConfigValue::Bool(true));
    }
    if input == "false" {
        return Ok(ConfigValue::Bool(false));
    }
    if input.starts_with('[') {
        if !input.ends_with(']') {
            return Err("array literal must end with ']'".to_owned());
        }
        let inner = &input[1..input.len() - 1];
        if inner.trim().is_empty() {
            return Ok(ConfigValue::StringArray(Vec::new()));
        }
        let values = split_args(inner)?
            .iter()
            .map(|value| match parse_value(value)? {
                ConfigValue::String(value) => Ok(value),
                _ => Err("only arrays of strings are supported".to_owned()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(ConfigValue::StringArray(values));
    }
    input
        .parse::<f64>()
        .map(ConfigValue::Number)
        .map_err(|_| format!("unsupported value literal: {input}"))
}

fn strip_comment(line: &str) -> String {
    let mut out = String::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                out.push(ch);
                escaped = true;
            }
            '"' => {
                out.push(ch);
                in_string = !in_string;
            }
            '-' if !in_string && chars.peek() == Some(&'-') => break,
            '#' if !in_string => break,
            _ => out.push(ch),
        }
    }
    out
}

fn unquote(input: &str) -> Result<String, String> {
    if !input.starts_with('"') || !input.ends_with('"') {
        return Err("expected quoted string".to_owned());
    }
    let mut out = String::new();
    let mut escaped = false;
    for ch in input[1..input.len() - 1].chars() {
        if escaped {
            out.push(match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                other => other,
            });
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else {
            out.push(ch);
        }
    }
    if escaped {
        return Err("dangling escape in string literal".to_owned());
    }
    Ok(out)
}

fn expect_string_arg<'a>(
    args: &'a [ConfigValue],
    index: usize,
    function: &str,
) -> Result<&'a str, String> {
    args.get(index)
        .ok_or_else(|| format!("panea.{function} is missing argument {}", index + 1))
        .and_then(value_as_string_ref)
}

fn value_as_string_ref(value: &ConfigValue) -> Result<&str, String> {
    match value {
        ConfigValue::String(value) => Ok(value),
        _ => Err("expected string value".to_owned()),
    }
}

fn value_as_string(value: &ConfigValue) -> Result<String, String> {
    value_as_string_ref(value).map(str::to_owned)
}

fn value_as_bool(value: &ConfigValue) -> Result<bool, String> {
    match value {
        ConfigValue::Bool(value) => Ok(*value),
        _ => Err("expected boolean value".to_owned()),
    }
}

fn value_as_f64(value: &ConfigValue) -> Result<f64, String> {
    match value {
        ConfigValue::Number(value) => Ok(*value),
        _ => Err("expected numeric value".to_owned()),
    }
}

fn value_as_u16(value: &ConfigValue) -> Result<u16, String> {
    let number = value_as_f64(value)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > f64::from(u16::MAX)
    {
        return Err("expected unsigned 16-bit integer".to_owned());
    }
    Ok(number as u16)
}

fn value_as_u32(value: &ConfigValue) -> Result<u32, String> {
    let number = value_as_f64(value)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > f64::from(u32::MAX)
    {
        return Err("expected unsigned 32-bit integer".to_owned());
    }
    Ok(number as u32)
}

fn value_as_usize(value: &ConfigValue) -> Result<usize, String> {
    let number = value_as_f64(value)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err("expected unsigned integer".to_owned());
    }
    Ok(number as usize)
}

fn value_as_string_array(value: &ConfigValue) -> Result<Vec<String>, String> {
    match value {
        ConfigValue::StringArray(values) => Ok(values.clone()),
        _ => Err("expected string array".to_owned()),
    }
}

fn value_as_color(value: &ConfigValue) -> Result<RgbaColor, String> {
    parse_color(value_as_string_ref(value)?)
}

fn parse_color(input: &str) -> Result<RgbaColor, String> {
    let hex = input
        .strip_prefix('#')
        .ok_or_else(|| "colors must use #rrggbb or #rrggbbaa".to_owned())?;
    if hex.len() != 6 && hex.len() != 8 {
        return Err("colors must use #rrggbb or #rrggbbaa".to_owned());
    }
    let red = parse_hex_byte(&hex[0..2])?;
    let green = parse_hex_byte(&hex[2..4])?;
    let blue = parse_hex_byte(&hex[4..6])?;
    let alpha = if hex.len() == 8 {
        parse_hex_byte(&hex[6..8])?
    } else {
        u8::MAX
    };
    Ok(RgbaColor {
        red,
        green,
        blue,
        alpha,
    })
}

fn parse_hex_byte(input: &str) -> Result<u8, String> {
    u8::from_str_radix(input, 16).map_err(|_| format!("invalid color byte '{input}'"))
}

fn set_config_value(config: &mut AppConfig, path: &str, value: &ConfigValue) -> Result<(), String> {
    match path {
        "window.title" => config.window.title = value_as_string(value)?,
        "window.columns" => config.window.columns = value_as_u16(value)?,
        "window.rows" => config.window.rows = value_as_u16(value)?,
        "window.initial_width" => config.window.initial_width = value_as_u32(value)?,
        "window.initial_height" => config.window.initial_height = value_as_u32(value)?,
        "window.padding_x" => config.window.padding_x = value_as_u16(value)?,
        "window.padding_y" => config.window.padding_y = value_as_u16(value)?,
        "window.margin_x" => config.window.margin_x = value_as_u16(value)?,
        "window.margin_y" => config.window.margin_y = value_as_u16(value)?,
        "window.opacity" => config.window.opacity = value_as_f64(value)?,
        "window.mode" => config.window.mode = parse_window_mode(value_as_string_ref(value)?)?,
        "window.linux_backend" => {
            config.window.linux_backend = parse_linux_backend(value_as_string_ref(value)?)?;
        }
        "window.decoration_strategy" => {
            config.window.decoration_strategy =
                parse_decoration_strategy(value_as_string_ref(value)?)?;
        }
        "window.fullscreen_titlebar.enabled" => {
            config.window.fullscreen_titlebar.enabled = value_as_bool(value)?;
        }
        "window.fullscreen_titlebar.height" => {
            config.window.fullscreen_titlebar.height = value_as_u16(value)?;
        }
        "window.fullscreen_titlebar.reveal_height" => {
            config.window.fullscreen_titlebar.reveal_height = value_as_u16(value)?;
        }
        "window.fullscreen_titlebar.show_window_controls" => {
            config.window.fullscreen_titlebar.show_window_controls = value_as_bool(value)?;
        }
        "window.fullscreen_titlebar.animation" => {
            config.window.fullscreen_titlebar.animation =
                parse_fullscreen_chrome_animation(value_as_string_ref(value)?)?;
        }
        "window.fullscreen_titlebar.animation_duration_ms" => {
            config.window.fullscreen_titlebar.animation_duration_ms = value_as_u16(value)?;
        }
        "window.fullscreen_titlebar.hide_delay_ms" => {
            config.window.fullscreen_titlebar.hide_delay_ms = value_as_u16(value)?;
        }
        "renderer.backend" => {
            config.renderer.backend = parse_renderer_backend(value_as_string_ref(value)?)?;
        }
        "renderer.vsync" => config.renderer.vsync = value_as_bool(value)?,
        "renderer.damage_tracking" => config.renderer.damage_tracking = value_as_bool(value)?,
        "renderer.present_mode" => {
            config.renderer.present_mode = parse_present_mode(value_as_string_ref(value)?)?;
        }
        "renderer.gpu_timestamps" => config.renderer.gpu_timestamps = value_as_bool(value)?,
        "renderer.text_gamma_adjustment" => {
            config.renderer.text_gamma_adjustment = value_as_f64(value)? as f32;
        }
        "font.family" => config.font.family = value_as_string(value)?,
        "font.size" => config.font.size = value_as_f64(value)?,
        "font.line_height" => config.font.line_height = value_as_f64(value)?,
        "font.fallback_families" => config.font.fallback_families = value_as_string_array(value)?,
        "font.ligatures" => config.font.ligatures = value_as_bool(value)?,
        "colors.foreground" => config.colors.foreground = value_as_color(value)?,
        "colors.background" => config.colors.background = value_as_color(value)?,
        "colors.cursor" => config.colors.cursor = value_as_color(value)?,
        "colors.cursor_text" => config.colors.cursor_text = Some(value_as_color(value)?),
        "colors.selection_foreground" => {
            config.colors.selection_foreground = Some(value_as_color(value)?);
        }
        "colors.selection_background" => {
            config.colors.selection_background = value_as_color(value)?
        }
        "visual_theme.name" => {
            let name = value_as_string(value)?;
            if !config.apply_visual_profile(&name) {
                config.visual_theme.name = name;
            }
        }
        "visual_theme.cursor_profile" => {
            config.visual_theme.cursor_profile = value_as_string(value)?;
        }
        "visual_theme.prompt_decoration_profile" => {
            config.visual_theme.prompt_decoration_profile = value_as_string(value)?;
        }
        "visual_theme.command_block_profile" => {
            config.visual_theme.command_block_profile = value_as_string(value)?;
        }
        "visual_theme.animation_profile" => {
            config.visual_theme.animation_profile = value_as_string(value)?;
        }
        "visual_theme.grouping_style" => {
            config.visual_theme.grouping_style = parse_grouping_style(value_as_string_ref(value)?)?;
        }
        "visual_theme.success_color" => config.visual_theme.success_color = value_as_color(value)?,
        "visual_theme.error_color" => config.visual_theme.error_color = value_as_color(value)?,
        "cursor.shape" => config.cursor.shape = parse_cursor_shape(value_as_string_ref(value)?)?,
        "cursor.blink" => config.cursor.blink = value_as_bool(value)?,
        "cursor.blink_interval_ms" => config.cursor.blink_interval_ms = value_as_u16(value)?,
        "cursor.thickness" => config.cursor.thickness = value_as_f64(value)?,
        "cursor.corner_radius" => config.cursor.corner_radius = value_as_f64(value)?,
        "cursor.color" => config.cursor.color = Some(value_as_color(value)?),
        "cursor.inactive_shape" => {
            config.cursor.inactive_shape = parse_cursor_shape(value_as_string_ref(value)?)?;
        }
        "cursor.inactive_color" => config.cursor.inactive_color = Some(value_as_color(value)?),
        "cursor.animation" => {
            config.cursor.animation =
                Some(parse_cursor_animation_profile(value_as_string_ref(value)?)?);
        }
        "cursor.animations_enabled" => config.cursor.animations_enabled = value_as_bool(value)?,
        "cursor.smooth_movement" => config.cursor.smooth_movement = value_as_bool(value)?,
        "cursor.typing_pulse" => config.cursor.typing_pulse = value_as_bool(value)?,
        "cursor.typing_stretch" => config.cursor.typing_stretch = value_as_bool(value)?,
        "cursor.trail" => config.cursor.trail = value_as_bool(value)?,
        "cursor.blink_easing" => config.cursor.blink_easing = value_as_bool(value)?,
        "cursor.short_lived_glow" => config.cursor.short_lived_glow = value_as_bool(value)?,
        "cursor.image.enabled" => config.cursor.image.enabled = value_as_bool(value)?,
        "cursor.image.path" => config.cursor.image.path = value_as_string(value)?,
        "cursor.image.fps" => config.cursor.image.fps = value_as_u16(value)?,
        "cursor.image.warn_if_expensive" => {
            config.cursor.image.warn_if_expensive = value_as_bool(value)?;
        }
        "cursor.vector.enabled" => config.cursor.vector.enabled = value_as_bool(value)?,
        "cursor.vector.path" => config.cursor.vector.path = value_as_string(value)?,
        "command_blocks.enabled" => config.command_blocks.enabled = value_as_bool(value)?,
        "command_blocks.style" => {
            config.command_blocks.style = parse_command_block_style(value_as_string_ref(value)?)?;
        }
        "command_blocks.show_duration" => {
            config.command_blocks.show_duration = value_as_bool(value)?
        }
        "command_blocks.show_exit_status" => {
            config.command_blocks.show_exit_status = value_as_bool(value)?;
        }
        "command_blocks.show_current_directory" => {
            config.command_blocks.show_current_directory = value_as_bool(value)?;
        }
        "command_blocks.show_shell_host" => {
            config.command_blocks.show_shell_host = value_as_bool(value)?;
        }
        "command_blocks.allow_in_alternate_screen" => {
            config.command_blocks.allow_in_alternate_screen = value_as_bool(value)?;
        }
        "command_blocks.collapse_long_output" => {
            config.command_blocks.collapse_long_output = value_as_bool(value)?;
        }
        "prompt_decorations.enabled" => config.prompt_decorations.enabled = value_as_bool(value)?,
        "prompt_decorations.style" => {
            config.prompt_decorations.style =
                parse_prompt_decoration_style(value_as_string_ref(value)?)?;
        }
        "shell_integration.enabled" => config.shell_integration.enabled = value_as_bool(value)?,
        "shell_integration.activation" => {
            config.shell_integration.activation =
                parse_shell_activation(value_as_string_ref(value)?)?;
        }
        "shell_integration.auto_install" => {
            config.shell_integration.auto_install = value_as_bool(value)?;
        }
        "shell_integration.enabled_shells" => {
            config.shell_integration.enabled_shells = value_as_string_array(value)?;
        }
        "clipboard.enabled" => config.clipboard.enabled = value_as_bool(value)?,
        "clipboard.copy_on_select" => config.clipboard.copy_on_select = value_as_bool(value)?,
        "clipboard.paste_protection" => config.clipboard.paste_protection = value_as_bool(value)?,
        "clipboard.bracketed_paste" => config.clipboard.bracketed_paste = value_as_bool(value)?,
        "clipboard.middle_click_paste" => {
            config.clipboard.middle_click_paste = value_as_bool(value)?
        }
        "clipboard.prefer_primary_selection_on_linux" => {
            config.clipboard.prefer_primary_selection_on_linux = value_as_bool(value)?;
        }
        "clipboard.osc52.enabled" => config.clipboard.osc52.enabled = value_as_bool(value)?,
        "clipboard.osc52.allow_local" => config.clipboard.osc52.allow_local = value_as_bool(value)?,
        "clipboard.osc52.allow_remote" => {
            config.clipboard.osc52.allow_remote = value_as_bool(value)?;
        }
        "clipboard.osc52.max_bytes" => config.clipboard.osc52.max_bytes = value_as_usize(value)?,
        "clipboard.osc52.confirm_remote_writes" => {
            config.clipboard.osc52.confirm_remote_writes = value_as_bool(value)?;
        }
        "notifications.enabled" => config.notifications.enabled = value_as_bool(value)?,
        "notifications.only_when_unfocused" => {
            config.notifications.only_when_unfocused = value_as_bool(value)?;
        }
        "notifications.session_closed" => {
            config.notifications.session_closed = value_as_bool(value)?;
        }
        "notifications.transport_errors" => {
            config.notifications.transport_errors = value_as_bool(value)?;
        }
        "paste.bracketed_paste" => config.paste.bracketed_paste = value_as_bool(value)?,
        "paste.normalize_newlines" => config.paste.normalize_newlines = value_as_bool(value)?,
        "paste.strip_control_characters" => {
            config.paste.strip_control_characters = value_as_bool(value)?;
        }
        "default_shell_profile" => config.default_shell_profile = Some(value_as_string(value)?),
        "mux.enabled" => config.mux.enabled = value_as_bool(value)?,
        "mux.restore_sessions" => config.mux.restore_sessions = value_as_bool(value)?,
        "mux.default_workspace" => config.mux.default_workspace = value_as_string(value)?,
        "mux.show_tab_bar" => config.mux.show_tab_bar = value_as_bool(value)?,
        "mux.drag_tabs" => config.mux.drag_tabs = value_as_bool(value)?,
        "mux.drag_panes" => config.mux.drag_panes = value_as_bool(value)?,
        "mux.tab_title_format" => config.mux.tab_title_format = value_as_string(value)?,
        "mux.status_format" => config.mux.status_format = value_as_string(value)?,
        "mux.pane_resize_step" => config.mux.pane_resize_step = value_as_f64(value)?,
        "mux.remember_working_directory" => {
            config.mux.remember_working_directory = value_as_bool(value)?;
        }
        "performance.profile" => {
            let profile = parse_performance_profile(value_as_string_ref(value)?)?;
            config.performance.apply_profile(profile);
        }
        "performance.frame_rate_limit" => {
            config.performance.frame_rate_limit = Some(value_as_u16(value)?);
        }
        "performance.glyph_cache_entries" => {
            config.performance.glyph_cache_entries = value_as_usize(value)?;
        }
        "performance.max_frame_time_ms" => {
            config.performance.max_frame_time_ms = value_as_u16(value)?;
        }
        "performance.expensive_effect_warnings" => {
            config.performance.expensive_effect_warnings = value_as_bool(value)?;
        }
        "performance.max_animation_fps" => {
            config.performance.max_animation_fps = value_as_u16(value)?;
        }
        "performance.max_cursor_asset_size_kb" => {
            config.performance.max_cursor_asset_size_kb = value_as_u32(value)?;
        }
        "performance.max_active_animations" => {
            config.performance.max_active_animations = value_as_u16(value)?;
        }
        "performance.max_animated_region_pixels" => {
            config.performance.max_animated_region_pixels = value_as_u32(value)?;
        }
        "performance.disable_expensive_effects_on_battery" => {
            config.performance.disable_expensive_effects_on_battery = value_as_bool(value)?;
        }
        "diagnostics.enabled" => config.diagnostics.enabled = value_as_bool(value)?,
        "diagnostics.performance_overlay" => {
            config.diagnostics.performance_overlay = value_as_bool(value)?;
        }
        "diagnostics.performance_overlay_position" => {
            config.diagnostics.performance_overlay_position =
                parse_performance_overlay_position(value_as_string_ref(value)?)?;
        }
        "diagnostics.performance_overlay_detail" => {
            config.diagnostics.performance_overlay_detail =
                parse_performance_overlay_detail(value_as_string_ref(value)?)?;
        }
        "diagnostics.persist_performance_overlay" => {
            config.diagnostics.persist_performance_overlay = value_as_bool(value)?;
        }
        "diagnostics.capability_report" => {
            config.diagnostics.capability_report = value_as_bool(value)?;
        }
        "diagnostics.log_level" => {
            config.diagnostics.log_level = parse_log_level(value_as_string_ref(value)?)?;
        }
        other => return Err(format!("unsupported config key '{other}'")),
    }
    Ok(())
}

fn set_platform_override_value(
    overrides: &mut PlatformOverrides,
    platform: ConfigPlatform,
    path: &str,
    value: &ConfigValue,
) -> Result<(), String> {
    let entry = platform_override_mut(overrides, platform)?;
    match path {
        "default_shell_profile" => entry.default_shell_profile = Some(value_as_string(value)?),
        "window.title" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .title = Some(value_as_string(value)?);
        }
        "window.padding_x" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .padding_x = Some(value_as_u16(value)?);
        }
        "window.padding_y" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .padding_y = Some(value_as_u16(value)?);
        }
        "window.margin_x" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .margin_x = Some(value_as_u16(value)?);
        }
        "window.margin_y" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .margin_y = Some(value_as_u16(value)?);
        }
        "window.opacity" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .opacity = Some(value_as_f64(value)?);
        }
        "window.mode" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .mode = Some(parse_window_mode(value_as_string_ref(value)?)?);
        }
        "window.linux_backend" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .linux_backend = Some(parse_linux_backend(value_as_string_ref(value)?)?);
        }
        "window.decoration_strategy" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .decoration_strategy =
                Some(parse_decoration_strategy(value_as_string_ref(value)?)?);
        }
        "window.fullscreen_titlebar.enabled" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .fullscreen_titlebar
                .get_or_insert_with(FullscreenTitlebarConfigPatch::default)
                .enabled = Some(value_as_bool(value)?);
        }
        "window.fullscreen_titlebar.height" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .fullscreen_titlebar
                .get_or_insert_with(FullscreenTitlebarConfigPatch::default)
                .height = Some(value_as_u16(value)?);
        }
        "window.fullscreen_titlebar.reveal_height" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .fullscreen_titlebar
                .get_or_insert_with(FullscreenTitlebarConfigPatch::default)
                .reveal_height = Some(value_as_u16(value)?);
        }
        "window.fullscreen_titlebar.show_window_controls" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .fullscreen_titlebar
                .get_or_insert_with(FullscreenTitlebarConfigPatch::default)
                .show_window_controls = Some(value_as_bool(value)?);
        }
        "window.fullscreen_titlebar.animation" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .fullscreen_titlebar
                .get_or_insert_with(FullscreenTitlebarConfigPatch::default)
                .animation = Some(parse_fullscreen_chrome_animation(value_as_string_ref(
                value,
            )?)?);
        }
        "window.fullscreen_titlebar.animation_duration_ms" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .fullscreen_titlebar
                .get_or_insert_with(FullscreenTitlebarConfigPatch::default)
                .animation_duration_ms = Some(value_as_u16(value)?);
        }
        "window.fullscreen_titlebar.hide_delay_ms" => {
            entry
                .window
                .get_or_insert_with(WindowConfigPatch::default)
                .fullscreen_titlebar
                .get_or_insert_with(FullscreenTitlebarConfigPatch::default)
                .hide_delay_ms = Some(value_as_u16(value)?);
        }
        "renderer.backend" => {
            entry
                .renderer
                .get_or_insert_with(RendererConfigPatch::default)
                .backend = Some(parse_renderer_backend(value_as_string_ref(value)?)?);
        }
        "font.family" => {
            entry
                .font
                .get_or_insert_with(FontConfigPatch::default)
                .family = Some(value_as_string(value)?);
        }
        "font.size" => {
            entry.font.get_or_insert_with(FontConfigPatch::default).size =
                Some(value_as_f64(value)?);
        }
        "font.fallback_families" => {
            entry
                .font
                .get_or_insert_with(FontConfigPatch::default)
                .fallback_families = Some(value_as_string_array(value)?);
        }
        "colors.foreground" => {
            entry
                .colors
                .get_or_insert_with(ColorConfigPatch::default)
                .foreground = Some(value_as_color(value)?);
        }
        "colors.background" => {
            entry
                .colors
                .get_or_insert_with(ColorConfigPatch::default)
                .background = Some(value_as_color(value)?);
        }
        "colors.cursor" => {
            entry
                .colors
                .get_or_insert_with(ColorConfigPatch::default)
                .cursor = Some(value_as_color(value)?);
        }
        "cursor.shape" => {
            entry
                .cursor
                .get_or_insert_with(CursorConfigPatch::default)
                .shape = Some(parse_cursor_shape(value_as_string_ref(value)?)?);
        }
        "cursor.animation" => {
            entry
                .cursor
                .get_or_insert_with(CursorConfigPatch::default)
                .animation = Some(parse_cursor_animation_profile(value_as_string_ref(value)?)?);
        }
        "cursor.animations_enabled" => {
            entry
                .cursor
                .get_or_insert_with(CursorConfigPatch::default)
                .animations_enabled = Some(value_as_bool(value)?);
        }
        "cursor.inactive_shape" => {
            entry
                .cursor
                .get_or_insert_with(CursorConfigPatch::default)
                .inactive_shape = Some(parse_cursor_shape(value_as_string_ref(value)?)?);
        }
        "command_blocks.enabled" => {
            entry
                .command_blocks
                .get_or_insert_with(CommandBlocksConfigPatch::default)
                .enabled = Some(value_as_bool(value)?);
        }
        "command_blocks.style" => {
            entry
                .command_blocks
                .get_or_insert_with(CommandBlocksConfigPatch::default)
                .style = Some(parse_command_block_style(value_as_string_ref(value)?)?);
        }
        "prompt_decorations.enabled" => {
            entry
                .prompt_decorations
                .get_or_insert_with(PromptDecorationsConfigPatch::default)
                .enabled = Some(value_as_bool(value)?);
        }
        "prompt_decorations.style" => {
            entry
                .prompt_decorations
                .get_or_insert_with(PromptDecorationsConfigPatch::default)
                .style = Some(parse_prompt_decoration_style(value_as_string_ref(value)?)?);
        }
        "shell_integration.activation" => {
            entry
                .shell_integration
                .get_or_insert_with(ShellIntegrationConfigPatch::default)
                .activation = Some(parse_shell_activation(value_as_string_ref(value)?)?);
        }
        "clipboard.osc52.allow_remote" => {
            let clipboard = entry
                .clipboard
                .get_or_insert_with(ClipboardConfigPatch::default);
            clipboard
                .osc52
                .get_or_insert_with(Osc52ClipboardConfigPatch::default)
                .allow_remote = Some(value_as_bool(value)?);
        }
        "notifications.enabled" => {
            entry
                .notifications
                .get_or_insert_with(NotificationConfigPatch::default)
                .enabled = Some(value_as_bool(value)?);
        }
        "notifications.only_when_unfocused" => {
            entry
                .notifications
                .get_or_insert_with(NotificationConfigPatch::default)
                .only_when_unfocused = Some(value_as_bool(value)?);
        }
        "notifications.session_closed" => {
            entry
                .notifications
                .get_or_insert_with(NotificationConfigPatch::default)
                .session_closed = Some(value_as_bool(value)?);
        }
        "notifications.transport_errors" => {
            entry
                .notifications
                .get_or_insert_with(NotificationConfigPatch::default)
                .transport_errors = Some(value_as_bool(value)?);
        }
        "performance.profile" => {
            entry
                .performance
                .get_or_insert_with(PerformanceConfigPatch::default)
                .profile = Some(parse_performance_profile(value_as_string_ref(value)?)?);
        }
        "performance.max_animation_fps" => {
            entry
                .performance
                .get_or_insert_with(PerformanceConfigPatch::default)
                .max_animation_fps = Some(value_as_u16(value)?);
        }
        "diagnostics.performance_overlay" => {
            entry
                .diagnostics
                .get_or_insert_with(DiagnosticsConfigPatch::default)
                .performance_overlay = Some(value_as_bool(value)?);
        }
        "diagnostics.performance_overlay_position" => {
            entry
                .diagnostics
                .get_or_insert_with(DiagnosticsConfigPatch::default)
                .performance_overlay_position = Some(parse_performance_overlay_position(
                value_as_string_ref(value)?,
            )?);
        }
        "diagnostics.performance_overlay_detail" => {
            entry
                .diagnostics
                .get_or_insert_with(DiagnosticsConfigPatch::default)
                .performance_overlay_detail = Some(parse_performance_overlay_detail(
                value_as_string_ref(value)?,
            )?);
        }
        "diagnostics.persist_performance_overlay" => {
            entry
                .diagnostics
                .get_or_insert_with(DiagnosticsConfigPatch::default)
                .persist_performance_overlay = Some(value_as_bool(value)?);
        }
        other => return Err(format!("unsupported platform override key '{other}'")),
    }
    Ok(())
}

fn platform_override_mut(
    overrides: &mut PlatformOverrides,
    platform: ConfigPlatform,
) -> Result<&mut PlatformOverride, String> {
    match platform {
        ConfigPlatform::MacOs => Ok(overrides
            .macos
            .get_or_insert_with(PlatformOverride::default)),
        ConfigPlatform::Linux => Ok(overrides
            .linux
            .get_or_insert_with(PlatformOverride::default)),
        ConfigPlatform::LinuxX11 => Ok(overrides
            .linux_x11
            .get_or_insert_with(PlatformOverride::default)),
        ConfigPlatform::LinuxWayland => Ok(overrides
            .linux_wayland
            .get_or_insert_with(PlatformOverride::default)),
        ConfigPlatform::Windows => Ok(overrides
            .windows
            .get_or_insert_with(PlatformOverride::default)),
        ConfigPlatform::Unknown => Err("platform overrides cannot target unknown".to_owned()),
    }
}

fn parse_platform(value: &str) -> Result<ConfigPlatform, String> {
    match normalized(value).as_str() {
        "macos" | "mac" | "darwin" => Ok(ConfigPlatform::MacOs),
        "linux" => Ok(ConfigPlatform::Linux),
        "linux_x11" | "x11" => Ok(ConfigPlatform::LinuxX11),
        "linux_wayland" | "wayland" => Ok(ConfigPlatform::LinuxWayland),
        "windows" | "win" => Ok(ConfigPlatform::Windows),
        other => Err(format!("unknown platform '{other}'")),
    }
}

fn platform_matches(current: ConfigPlatform, requested: ConfigPlatform) -> bool {
    current == requested
        || (requested == ConfigPlatform::Linux
            && matches!(
                current,
                ConfigPlatform::LinuxX11 | ConfigPlatform::LinuxWayland
            ))
}

fn parse_window_mode(value: &str) -> Result<WindowModeConfig, String> {
    match normalized(value).as_str() {
        "windowed" => Ok(WindowModeConfig::Windowed),
        "maximized" => Ok(WindowModeConfig::Maximized),
        "fullscreen" => Ok(WindowModeConfig::Fullscreen),
        "borderless_fullscreen" => Ok(WindowModeConfig::BorderlessFullscreen),
        "frameless_windowed" => Ok(WindowModeConfig::FramelessWindowed),
        "frameless_fullscreen" => Ok(WindowModeConfig::FramelessFullscreen),
        other => Err(format!("unknown window mode '{other}'")),
    }
}

fn parse_fullscreen_chrome_animation(value: &str) -> Result<FullscreenChromeAnimation, String> {
    match value {
        "instant" => Ok(FullscreenChromeAnimation::Instant),
        "smooth" => Ok(FullscreenChromeAnimation::Smooth),
        _ => Err(format!("unsupported fullscreen chrome animation '{value}'")),
    }
}

fn parse_linux_backend(value: &str) -> Result<LinuxBackendConfig, String> {
    match normalized(value).as_str() {
        "auto" => Ok(LinuxBackendConfig::Auto),
        "x11" => Ok(LinuxBackendConfig::X11),
        "wayland" => Ok(LinuxBackendConfig::Wayland),
        other => Err(format!("unknown Linux backend '{other}'")),
    }
}

fn parse_decoration_strategy(value: &str) -> Result<DecorationStrategyConfig, String> {
    match normalized(value).as_str() {
        "auto" => Ok(DecorationStrategyConfig::Auto),
        "native" => Ok(DecorationStrategyConfig::Native),
        "client_side" => Ok(DecorationStrategyConfig::ClientSide),
        "custom" => Ok(DecorationStrategyConfig::Custom),
        "none" => Ok(DecorationStrategyConfig::None),
        "fallback_decorated" => Ok(DecorationStrategyConfig::FallbackDecorated),
        other => Err(format!("unknown decoration strategy '{other}'")),
    }
}

fn parse_renderer_backend(value: &str) -> Result<RendererBackendPreference, String> {
    match normalized(value).as_str() {
        "auto" => Ok(RendererBackendPreference::Auto),
        "vulkan" => Ok(RendererBackendPreference::Vulkan),
        "metal" => Ok(RendererBackendPreference::Metal),
        "dx12" => Ok(RendererBackendPreference::Dx12),
        "gl" => Ok(RendererBackendPreference::Gl),
        other => Err(format!("unknown renderer backend '{other}'")),
    }
}

fn parse_present_mode(value: &str) -> Result<PresentModePreference, String> {
    match normalized(value).as_str() {
        "auto" => Ok(PresentModePreference::Auto),
        "fifo" => Ok(PresentModePreference::Fifo),
        "mailbox" => Ok(PresentModePreference::Mailbox),
        "immediate" => Ok(PresentModePreference::Immediate),
        other => Err(format!("unknown present mode '{other}'")),
    }
}

fn parse_grouping_style(value: &str) -> Result<InputOutputGroupingStyle, String> {
    match normalized(value).as_str() {
        "traditional" => Ok(InputOutputGroupingStyle::Traditional),
        "subtle_separators" => Ok(InputOutputGroupingStyle::SubtleSeparators),
        "command_cards" => Ok(InputOutputGroupingStyle::CommandCards),
        "input_output_split" => Ok(InputOutputGroupingStyle::InputOutputSplit),
        "minimal_headers" => Ok(InputOutputGroupingStyle::MinimalHeaders),
        "custom_theme" => Ok(InputOutputGroupingStyle::CustomTheme),
        other => Err(format!("unknown grouping style '{other}'")),
    }
}

fn parse_cursor_shape(value: &str) -> Result<CursorShape, String> {
    match normalized(value).as_str() {
        "block" => Ok(CursorShape::Block),
        "beam" => Ok(CursorShape::Beam),
        "underline" => Ok(CursorShape::Underline),
        "hollow_block" => Ok(CursorShape::HollowBlock),
        "custom" => Ok(CursorShape::Custom),
        "custom_static_shape" => Ok(CursorShape::CustomStaticShape),
        other => Err(format!("unknown cursor shape '{other}'")),
    }
}

fn parse_cursor_animation_profile(value: &str) -> Result<CursorAnimationProfile, String> {
    match normalized(value).as_str() {
        "static" | "none" | "off" => Ok(CursorAnimationProfile::Static),
        "panea" => Ok(CursorAnimationProfile::Panea),
        "custom" => Ok(CursorAnimationProfile::Custom),
        other => Err(format!("unknown cursor animation profile '{other}'")),
    }
}

fn parse_command_block_style(value: &str) -> Result<CommandBlockStyle, String> {
    match normalized(value).as_str() {
        "subtle" => Ok(CommandBlockStyle::Subtle),
        "card" => Ok(CommandBlockStyle::Card),
        "split" => Ok(CommandBlockStyle::Split),
        "minimal_header" => Ok(CommandBlockStyle::MinimalHeader),
        "custom_theme" => Ok(CommandBlockStyle::CustomTheme),
        other => Err(format!("unknown command block style '{other}'")),
    }
}

fn parse_prompt_decoration_style(value: &str) -> Result<PromptDecorationStyle, String> {
    match normalized(value).as_str() {
        "minimal_separator" => Ok(PromptDecorationStyle::MinimalSeparator),
        "rounded_box" => Ok(PromptDecorationStyle::RoundedBox),
        "pill_header" => Ok(PromptDecorationStyle::PillHeader),
        other => Err(format!("unknown prompt decoration style '{other}'")),
    }
}

fn parse_shell_activation(value: &str) -> Result<ShellIntegrationActivationConfig, String> {
    match normalized(value).as_str() {
        "full" => Ok(ShellIntegrationActivationConfig::Full),
        "auto" | "auto_detect" => Ok(ShellIntegrationActivationConfig::AutoDetect),
        "manual" => Ok(ShellIntegrationActivationConfig::Manual),
        "heuristic" => Ok(ShellIntegrationActivationConfig::Heuristic),
        "disabled" | "off" => Ok(ShellIntegrationActivationConfig::Disabled),
        other => Err(format!("unknown shell integration activation '{other}'")),
    }
}

fn parse_performance_profile(value: &str) -> Result<PerformanceProfile, String> {
    match normalized(value).as_str() {
        "maximum_performance" => Ok(PerformanceProfile::MaximumPerformance),
        "balanced" => Ok(PerformanceProfile::Balanced),
        "visual" => Ok(PerformanceProfile::Visual),
        "battery_saver" | "battery_conscious" => Ok(PerformanceProfile::BatterySaver),
        other => Err(format!("unknown performance profile '{other}'")),
    }
}

fn parse_performance_overlay_position(value: &str) -> Result<PerformanceOverlayPosition, String> {
    match normalized(value).as_str() {
        "top_left" => Ok(PerformanceOverlayPosition::TopLeft),
        "top_right" => Ok(PerformanceOverlayPosition::TopRight),
        "bottom_left" => Ok(PerformanceOverlayPosition::BottomLeft),
        "bottom_right" => Ok(PerformanceOverlayPosition::BottomRight),
        other => Err(format!("unknown performance overlay position '{other}'")),
    }
}

fn parse_performance_overlay_detail(value: &str) -> Result<PerformanceOverlayDetail, String> {
    match normalized(value).as_str() {
        "compact" => Ok(PerformanceOverlayDetail::Compact),
        "detailed" => Ok(PerformanceOverlayDetail::Detailed),
        other => Err(format!("unknown performance overlay detail '{other}'")),
    }
}

fn parse_log_level(value: &str) -> Result<LogLevel, String> {
    match normalized(value).as_str() {
        "error" => Ok(LogLevel::Error),
        "warn" | "warning" => Ok(LogLevel::Warn),
        "info" => Ok(LogLevel::Info),
        "debug" => Ok(LogLevel::Debug),
        "trace" => Ok(LogLevel::Trace),
        other => Err(format!("unknown log level '{other}'")),
    }
}

fn parse_shell_profile_kind(value: &str) -> Result<ShellProfileKind, String> {
    match normalized(value).as_str() {
        "default" => Ok(ShellProfileKind::Default),
        "powershell" | "pwsh" => Ok(ShellProfileKind::PowerShell),
        "cmd" => Ok(ShellProfileKind::Cmd),
        "wsl" => Ok(ShellProfileKind::Wsl),
        "custom" => Ok(ShellProfileKind::Custom),
        other => Err(format!("unknown shell profile kind '{other}'")),
    }
}

fn normalized(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_program(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "panea-{name}-{}-{}.panea",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn programmable_config_compiles_into_app_config() {
        let loaded = parse_str(
            r##"
            panea.theme("generated-night", "#101820", "#f4f7fb", "#4dd4ac")
            panea.set("font.size", 15)
            panea.set("font.fallback_families", ["Noto Color Emoji", "Segoe UI Emoji"])
            panea.set("cursor.animation", "panea")
            panea.set("command_blocks.enabled", true)
            panea.set("command_blocks.style", "card")
            panea.set("mux.default_workspace", "dev")
            panea.set("mux.tab_title_format", "{index}:{title}")
            panea.key("Ctrl+Alt+T", "new_tab")
            panea.cursor_mode("insert", "beam")
            panea.shell_profile("dev-pwsh", "powershell", "pwsh", ["-NoLogo"])
            panea.ssh_profile("prod", "example.com", "deploy")
            "##,
            None,
            ConfigPlatform::Windows,
        )
        .expect("program should compile");

        assert_eq!(loaded.config.visual_theme.name, "generated-night");
        assert_eq!(loaded.config.font.size, 15.0);
        assert_eq!(
            loaded.config.cursor.animation,
            Some(config_core::CursorAnimationProfile::Panea)
        );
        assert_eq!(loaded.config.command_blocks.style, CommandBlockStyle::Card);
        assert_eq!(loaded.config.mux.default_workspace, "dev");
        assert_eq!(
            loaded.config.keyboard.keybindings.last().unwrap().action,
            "new_tab"
        );
        assert_eq!(
            loaded.config.cursor.mode_specific_styles.get("insert"),
            Some(&CursorShape::Beam)
        );
        assert_eq!(loaded.config.shell_profiles[0].name, "dev-pwsh");
        assert_eq!(
            loaded.config.ssh_profiles[0].username.as_deref(),
            Some("deploy")
        );
        assert!(loaded.executed_actions >= 9);
    }

    #[test]
    fn platform_conditionals_are_deterministic() {
        let loaded = parse_str(
            r#"
            panea.set("window.title", "Base")
            panea.when_platform("windows")
            panea.set("window.title", "Windows")
            panea.end()
            panea.when_platform("linux")
            panea.set("window.title", "Linux")
            panea.end()
            "#,
            None,
            ConfigPlatform::Windows,
        )
        .expect("program should compile");

        assert_eq!(loaded.config.window.title, "Windows");
    }

    #[test]
    fn platform_set_emits_portable_overrides() {
        let loaded = parse_str(
            r#"
            panea.platform_set("windows", "font.family", "Cascadia Mono")
            panea.platform_set("windows", "cursor.animation", "panea")
            panea.platform_set("linux_wayland", "window.linux_backend", "wayland")
            "#,
            None,
            ConfigPlatform::LinuxWayland,
        )
        .expect("program should compile");

        assert_eq!(
            loaded.config.window.linux_backend,
            LinuxBackendConfig::Wayland
        );
        assert_eq!(
            loaded
                .config
                .platform_overrides
                .windows
                .as_ref()
                .unwrap()
                .font
                .as_ref()
                .unwrap()
                .family
                .as_deref(),
            Some("Cascadia Mono")
        );
        assert_eq!(
            loaded
                .config
                .platform_overrides
                .windows
                .as_ref()
                .unwrap()
                .cursor
                .as_ref()
                .unwrap()
                .animation,
            Some(config_core::CursorAnimationProfile::Panea)
        );
    }

    #[test]
    fn bad_config_reports_line_number() {
        let error = parse_str(
            r#"
            panea.set("font.size", 2)
            "#,
            None,
            ConfigPlatform::Unknown,
        )
        .expect_err("validation should fail");

        match error {
            ProgrammableConfigError::Validation { diagnostics } => {
                assert!(
                    diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.path == "font.size")
                );
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_api_fails_clearly() {
        let error = parse_str(
            r#"
            panea.os.exec("bad")
            "#,
            None,
            ConfigPlatform::Unknown,
        )
        .expect_err("unsupported API should fail");

        assert!(
            error
                .to_string()
                .contains("unsupported programmable config API")
        );
    }

    #[test]
    fn reload_plan_uses_compiled_configs_only() {
        let current = parse_str(
            r#"panea.set("window.title", "A")"#,
            None,
            ConfigPlatform::Unknown,
        )
        .expect("current config");
        let next = parse_str(
            r#"
            panea.set("window.title", "B")
            panea.set("renderer.backend", "dx12")
            "#,
            None,
            ConfigPlatform::Unknown,
        )
        .expect("next config");

        let plan = current.config.reload_plan_from(&next.config);

        assert!(plan.requires_restart());
        assert!(
            plan.restart_required
                .iter()
                .any(|change| change.path == "renderer.backend")
        );
    }

    #[test]
    fn config_provider_returns_loaded_app_config() {
        let provider = ProgrammableConfigProvider::new(ProgrammableConfigLoadOptions {
            explicit_path: None,
            platform: ConfigPlatform::Unknown,
        });

        let error = provider.load_config().expect_err("no discovered config");
        assert!(error.message.contains("not found"));
    }

    #[test]
    fn shipped_programmable_example_compiles() {
        let loaded = parse_str(
            include_str!("../../assets/config-examples/advanced.panea"),
            None,
            ConfigPlatform::Windows,
        )
        .expect("advanced programmable example should compile");

        assert_eq!(loaded.config.visual_theme.name, "generated-night");
        assert_eq!(
            loaded.config.font.family, "Cascadia Mono",
            "windows platform override should resolve"
        );
        assert!(loaded.config.command_blocks.enabled);
    }

    #[test]
    fn programmable_notifications_compile_into_app_config() {
        let loaded = parse_str(
            r#"
            panea.set("notifications.enabled", true)
            panea.set("notifications.only_when_unfocused", false)
            panea.platform_set("windows", "notifications.transport_errors", false)
            "#,
            None,
            ConfigPlatform::Windows,
        )
        .expect("notification config should compile");

        assert!(loaded.config.notifications.enabled);
        assert!(!loaded.config.notifications.only_when_unfocused);
        assert!(!loaded.config.notifications.transport_errors);
    }

    #[test]
    fn programmable_desktop_ux_compiles_into_app_config() {
        let loaded = parse_str(
            r#"
            panea.set("mux.drag_tabs", false)
            panea.set("mux.drag_panes", true)
            panea.set("diagnostics.performance_overlay_position", "bottom_right")
            panea.set("diagnostics.performance_overlay_detail", "detailed")
            panea.set("diagnostics.persist_performance_overlay", false)
            "#,
            None,
            ConfigPlatform::MacOs,
        )
        .expect("desktop UX programmable config should compile");

        assert!(!loaded.config.mux.drag_tabs);
        assert!(loaded.config.mux.drag_panes);
        assert_eq!(
            loaded.config.diagnostics.performance_overlay_position,
            PerformanceOverlayPosition::BottomRight
        );
        assert_eq!(
            loaded.config.diagnostics.performance_overlay_detail,
            PerformanceOverlayDetail::Detailed
        );
        assert!(!loaded.config.diagnostics.persist_performance_overlay);
    }

    #[test]
    fn programmable_watcher_reloads_and_reports_invalid_edits() {
        let path = temp_program("programmable-watcher");
        fs::write(&path, r#"panea.set("window.title", "A")"#).expect("write initial");
        let mut watcher = ProgrammableConfigWatcher::new(&path, ConfigPlatform::Unknown)
            .with_poll_interval(Duration::ZERO)
            .with_debounce(Duration::ZERO);

        fs::write(&path, r#"panea.set("window.title", "B")"#).expect("write update");
        assert!(matches!(
            watcher.poll(),
            ProgrammableConfigWatchEvent::Pending { .. }
        ));
        let ProgrammableConfigWatchEvent::Reloaded(loaded) = watcher.poll() else {
            panic!("expected compiled programmable reload");
        };
        assert_eq!(loaded.config.window.title, "B");

        fs::write(&path, "panea.os.exec(\"bad\")").expect("write invalid update");
        assert!(matches!(
            watcher.poll(),
            ProgrammableConfigWatchEvent::Pending { .. }
        ));
        assert!(matches!(
            watcher.poll(),
            ProgrammableConfigWatchEvent::Failed { .. }
        ));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn programmable_config_compiles_vector_cursor_settings() {
        let loaded = parse_str(
            r#"
            panea.set("cursor.vector.enabled", true)
            panea.set("cursor.vector.path", "assets/cursor.panea-cursor.json")
            "#,
            None,
            ConfigPlatform::LinuxX11,
        )
        .expect("vector cursor programmable config should compile");
        assert!(loaded.config.cursor.vector.enabled);
        assert_eq!(
            loaded.config.cursor.vector.path,
            "assets/cursor.panea-cursor.json"
        );
    }

    #[test]
    fn programmable_config_compiles_fullscreen_titlebar_settings() {
        let loaded = parse_str(
            r#"
            panea.set("window.fullscreen_titlebar.enabled", true)
            panea.set("window.fullscreen_titlebar.height", 40)
            panea.set("window.fullscreen_titlebar.animation", "smooth")
            panea.set("window.fullscreen_titlebar.animation_duration_ms", 140)
            panea.set("window.fullscreen_titlebar.hide_delay_ms", 80)
            panea.platform_set("windows", "window.fullscreen_titlebar.reveal_height", 5)
            panea.platform_set("windows", "window.fullscreen_titlebar.animation", "instant")
            "#,
            None,
            ConfigPlatform::Windows,
        )
        .expect("fullscreen titlebar programmable config should compile");

        assert!(loaded.config.window.fullscreen_titlebar.enabled);
        assert_eq!(loaded.config.window.fullscreen_titlebar.height, 40);
        assert_eq!(loaded.config.window.fullscreen_titlebar.reveal_height, 5);
        assert_eq!(
            loaded.config.window.fullscreen_titlebar.animation,
            config_core::FullscreenChromeAnimation::Instant
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
    }
}
