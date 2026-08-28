// Portable config discovery, loading, watching, and doctor input.

struct LoadedDesktopConfig {
    config: AppConfig,
    diagnostics: Vec<ConfigDiagnostic>,
    source: String,
    asset_base_dir: Option<PathBuf>,
    watcher: Option<DesktopConfigWatcher>,
}

enum DesktopConfigWatcher {
    Toml(config_toml::ConfigWatcher),
    Programmable(config_lua::ProgrammableConfigWatcher),
}

enum DesktopConfigWatchEvent {
    Unchanged,
    Pending {
        path: Option<PathBuf>,
    },
    Reloaded {
        config: Box<AppConfig>,
        diagnostics: Vec<ConfigDiagnostic>,
    },
    Failed {
        path: Option<PathBuf>,
        error: String,
    },
}

impl DesktopConfigWatcher {
    fn poll(&mut self) -> DesktopConfigWatchEvent {
        match self {
            Self::Toml(watcher) => match watcher.poll() {
                config_toml::ConfigWatchEvent::Unchanged => DesktopConfigWatchEvent::Unchanged,
                config_toml::ConfigWatchEvent::Pending { path } => {
                    DesktopConfigWatchEvent::Pending { path }
                }
                config_toml::ConfigWatchEvent::Reloaded(loaded) => {
                    DesktopConfigWatchEvent::Reloaded {
                        config: Box::new(loaded.config),
                        diagnostics: loaded.diagnostics,
                    }
                }
                config_toml::ConfigWatchEvent::Failed { path, error } => {
                    DesktopConfigWatchEvent::Failed {
                        path,
                        error: error.to_string(),
                    }
                }
            },
            Self::Programmable(watcher) => match watcher.poll() {
                config_lua::ProgrammableConfigWatchEvent::Unchanged => {
                    DesktopConfigWatchEvent::Unchanged
                }
                config_lua::ProgrammableConfigWatchEvent::Pending { path } => {
                    DesktopConfigWatchEvent::Pending { path: Some(path) }
                }
                config_lua::ProgrammableConfigWatchEvent::Reloaded(loaded) => {
                    DesktopConfigWatchEvent::Reloaded {
                        config: Box::new(loaded.config),
                        diagnostics: loaded.diagnostics,
                    }
                }
                config_lua::ProgrammableConfigWatchEvent::Failed { path, error } => {
                    DesktopConfigWatchEvent::Failed {
                        path: Some(path),
                        error: error.to_string(),
                    }
                }
            },
        }
    }
}

/// How often the watcher thread asks its watcher to look.
///
/// The watcher applies its own poll interval and debounce on top of this, so
/// this only bounds how quickly a settled change is noticed.
const CONFIG_WATCH_TICK: Duration = Duration::from_millis(100);

/// Runs a config watcher on its own thread and delivers changes over a channel.
///
/// Polling from the UI thread had two problems: it did filesystem work on the
/// render thread, and it only ran when something else already woke the event
/// loop — `AboutToWait` does not fire for an idle window, so an edit went
/// unnoticed until the user moved the mouse. The thread wakes the event loop
/// itself, which makes reloads arrive on their own.
struct DesktopConfigWatchThread {
    events: Receiver<DesktopConfigWatchEvent>,
    worker: Option<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl DesktopConfigWatchThread {
    fn spawn(mut watcher: DesktopConfigWatcher, waker: TransportWakeHandle) -> Self {
        let (events_tx, events) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);

        let worker = thread::Builder::new()
            .name("panea-config-watch".to_owned())
            .spawn(move || {
                while !worker_shutdown.load(Ordering::Relaxed) {
                    match watcher.poll() {
                        DesktopConfigWatchEvent::Unchanged => {}
                        event => {
                            if events_tx.send(event).is_err() {
                                break;
                            }
                            waker.wake();
                        }
                    }
                    thread::sleep(CONFIG_WATCH_TICK);
                }
            })
            .ok();

        Self {
            events,
            worker,
            shutdown,
        }
    }

    fn poll(&mut self) -> DesktopConfigWatchEvent {
        self.events
            .try_recv()
            .unwrap_or(DesktopConfigWatchEvent::Unchanged)
    }
}

impl Drop for DesktopConfigWatchThread {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn load_desktop_config() -> Result<LoadedDesktopConfig, Box<dyn Error>> {
    let platform = config_core::ConfigPlatform::current();

    if let Some(path) = std::env::var_os("PANEA_CONFIG").map(PathBuf::from) {
        if config_lua::is_programmable_config_path(&path) {
            let loaded = config_lua::load_path(path.clone(), true, platform)?;
            return Ok(LoadedDesktopConfig {
                config: loaded.config,
                diagnostics: loaded.diagnostics,
                source: format!("explicit:{}", path.display()),
                asset_base_dir: path.parent().map(Path::to_path_buf),
                watcher: Some(DesktopConfigWatcher::Programmable(
                    config_lua::ProgrammableConfigWatcher::new(path, platform),
                )),
            });
        }

        let options = config_toml::ConfigLoadOptions {
            explicit_path: Some(path),
            platform,
        };
        let loaded = config_toml::load(options.clone())?;
        return Ok(LoadedDesktopConfig {
            source: config_source_text(&loaded.source),
            asset_base_dir: config_source_path(&loaded.source)
                .and_then(Path::parent)
                .map(Path::to_path_buf),
            config: loaded.config,
            diagnostics: loaded.diagnostics,
            watcher: Some(DesktopConfigWatcher::Toml(config_toml::ConfigWatcher::new(
                options,
            ))),
        });
    }

    let toml_exists = config_toml::candidate_paths_for_current_platform()
        .iter()
        .any(|path| path.exists());
    if toml_exists {
        let options = config_toml::ConfigLoadOptions {
            explicit_path: None,
            platform,
        };
        let loaded = config_toml::load(options.clone())?;
        return Ok(LoadedDesktopConfig {
            source: config_source_text(&loaded.source),
            asset_base_dir: config_source_path(&loaded.source)
                .and_then(Path::parent)
                .map(Path::to_path_buf),
            config: loaded.config,
            diagnostics: loaded.diagnostics,
            watcher: Some(DesktopConfigWatcher::Toml(config_toml::ConfigWatcher::new(
                options,
            ))),
        });
    }

    if let Some(path) = config_lua::candidate_paths_for_current_platform()
        .into_iter()
        .find(|path| path.exists())
    {
        let loaded = config_lua::load_path(path.clone(), false, platform)?;
        return Ok(LoadedDesktopConfig {
            config: loaded.config,
            diagnostics: loaded.diagnostics,
            source: path.display().to_string(),
            asset_base_dir: path.parent().map(Path::to_path_buf),
            watcher: Some(DesktopConfigWatcher::Programmable(
                config_lua::ProgrammableConfigWatcher::new(path, platform),
            )),
        });
    }

    let options = config_toml::ConfigLoadOptions {
        explicit_path: None,
        platform,
    };
    let loaded = config_toml::load(options.clone())?;
    Ok(LoadedDesktopConfig {
        source: config_source_text(&loaded.source),
        asset_base_dir: config_source_path(&loaded.source)
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        config: loaded.config,
        diagnostics: loaded.diagnostics,
        watcher: Some(DesktopConfigWatcher::Toml(config_toml::ConfigWatcher::new(
            options,
        ))),
    })
}

fn doctor_input() -> diagnostics::DoctorInput {
    match load_desktop_config() {
        Ok(loaded) => {
            let runtime = doctor_runtime_snapshot(&loaded.config, "loaded");
            diagnostics::DoctorInput {
                app_version: env!("CARGO_PKG_VERSION").to_owned(),
                config_source: loaded.source,
                config: loaded.config,
                config_diagnostics: loaded.diagnostics,
                platform: diagnostics::PlatformSnapshot::detect(),
                runtime,
                recent_errors: Vec::new(),
            }
        }
        Err(error) => {
            let config = AppConfig::default();
            let runtime = doctor_runtime_snapshot(&config, &error.to_string());
            diagnostics::DoctorInput {
                app_version: env!("CARGO_PKG_VERSION").to_owned(),
                config_source: "unloaded".to_owned(),
                config,
                config_diagnostics: Vec::new(),
                platform: diagnostics::PlatformSnapshot::detect(),
                runtime,
                recent_errors: vec![format!("config load failed: {error}")],
            }
        }
    }
}

fn config_source_text(source: &config_toml::ConfigSource) -> String {
    match source {
        config_toml::ConfigSource::Default => "default".to_owned(),
        config_toml::ConfigSource::File(path) => path.display().to_string(),
        config_toml::ConfigSource::ExplicitFile(path) => {
            format!("explicit:{}", path.display())
        }
    }
}

fn config_source_path(source: &config_toml::ConfigSource) -> Option<&Path> {
    match source {
        config_toml::ConfigSource::Default => None,
        config_toml::ConfigSource::File(path) | config_toml::ConfigSource::ExplicitFile(path) => {
            Some(path)
        }
    }
}

fn doctor_runtime_snapshot(
    config: &AppConfig,
    config_parse_status: &str,
) -> diagnostics::DoctorRuntimeSnapshot {
    let gpu_probe = pollster::block_on(render_wgpu::probe_gpu_adapter());
    let clipboard = ClipboardBridge::new();
    let clipboard_diagnostic = clipboard.last_diagnostic().clone();
    let notification_provider = DesktopNotificationProvider::new(config.notifications.enabled);
    let notification_diagnostic = notification_provider.diagnostic();
    let keychain = PlatformKeychainProvider::for_current_platform();
    let keychain_capability = keychain.capability();
    let performance_overlay_ui = PerformanceOverlayUiState::new(&config.diagnostics);

    diagnostics::DoctorRuntimeSnapshot {
        renderer_backend: gpu_probe.as_ref().map_or_else(
            || "wgpu adapter not detected".to_owned(),
            |probe| format!("wgpu {}", probe.backend),
        ),
        gpu_adapter: gpu_probe
            .as_ref()
            .map(|probe| format!("{} ({})", probe.adapter, probe.device_type)),
        gpu_features: gpu_probe
            .as_ref()
            .map_or_else(Vec::new, |probe| probe.features.clone()),
        window_backend: Some(window_backend_label(config)),
        x11_wayland_status: Some(x11_wayland_status()),
        dpi_scale: None,
        font_discovery: font_discovery_label(config),
        config_parse_status: config_parse_status.to_owned(),
        shell_integration_status: shell_integration_config_status(config),
        performance_overlay_status: performance_overlay_ui.diagnostic(),
        clipboard_provider: format!(
            "arboard system clipboard {:?}: {}",
            clipboard_diagnostic.availability,
            clipboard_diagnostic
                .message
                .as_deref()
                .unwrap_or("provider initialized")
        ),
        notification_provider: format!(
            "{:?} {:?}: {}",
            notification_diagnostic.backend,
            notification_diagnostic.availability,
            notification_diagnostic.message
        ),
        keychain_provider: format!(
            "{:?} available={} secure={} persistent={} ({})",
            keychain_capability.backend,
            keychain_capability.available,
            keychain_capability.secure_storage,
            keychain_capability.persistent,
            keychain_capability.message
        ),
        pty_backend: pty_backend_label(),
        ssh_provider_status: format!(
            "ssh2 transport; interactive host trust and credential prompts enabled; native keychain available={}",
            keychain_capability.available
        ),
        fullscreen_chrome: doctor_fullscreen_chrome_snapshot(config),
    }
}

fn doctor_fullscreen_chrome_snapshot(
    config: &AppConfig,
) -> diagnostics::FullscreenChromeRuntimeSnapshot {
    let configured = &config.window.fullscreen_titlebar;
    let retained_damage = if config.renderer.damage_tracking {
        diagnostics::FullscreenChromeRetainedDamage::Unverified
    } else {
        diagnostics::FullscreenChromeRetainedDamage::Disabled
    };
    let (effective_animation, fallback) = if !configured.enabled {
        (Some(configured.animation), None)
    } else if matches!(configured.animation, FullscreenChromeAnimation::Instant)
        || configured.animation_duration_ms == 0
    {
        (Some(FullscreenChromeAnimation::Instant), None)
    } else if !config.renderer.damage_tracking {
        (
            Some(FullscreenChromeAnimation::Instant),
            Some("smooth animation requires retained damage tracking".to_owned()),
        )
    } else {
        (
            None,
            Some("effective animation is resolved by the active window renderer".to_owned()),
        )
    };

    diagnostics::FullscreenChromeRuntimeSnapshot {
        effective_animation,
        retained_damage,
        fallback,
        metrics: None,
    }
}

fn shell_integration_config_status(config: &AppConfig) -> String {
    if !config.shell_integration.enabled
        || matches!(
            config.shell_integration.activation,
            ShellIntegrationActivationConfig::Disabled
        )
    {
        return "disabled by config".to_owned();
    }
    let remote_profiles = config
        .ssh_profiles
        .iter()
        .filter(|profile| profile.shell_integration)
        .count();
    if matches!(
        config.shell_integration.activation,
        ShellIntegrationActivationConfig::Heuristic
    ) {
        return format!(
            "heuristic low-confidence mode; runtime shell/cwd/exit metadata unavailable; remote_profiles={remote_profiles}"
        );
    }
    format!(
        "configured {:?}; no active session during doctor; remote_profiles={} remain inactive until markers are observed",
        config.shell_integration.activation, remote_profiles
    )
}

fn window_backend_label(config: &AppConfig) -> String {
    if cfg!(windows) {
        "winit/windows".to_owned()
    } else if cfg!(target_os = "macos") {
        "winit/macos".to_owned()
    } else if cfg!(target_os = "linux") {
        let detected = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            "wayland"
        } else if std::env::var_os("DISPLAY").is_some() {
            "x11"
        } else {
            "unavailable"
        };
        format!(
            "winit/linux requested={:?} detected={detected}",
            config.window.linux_backend
        )
    } else {
        "winit/unknown".to_owned()
    }
}

fn x11_wayland_status() -> String {
    if !cfg!(target_os = "linux") {
        return "n/a on this platform".to_owned();
    }

    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".to_owned());
    let wayland = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "unset".to_owned());
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| "unset".to_owned());
    format!("session={session} wayland_display={wayland} display={display}")
}

fn pty_backend_label() -> String {
    if cfg!(windows) {
        "portable-pty ConPTY".to_owned()
    } else if cfg!(unix) {
        "portable-pty Unix PTY".to_owned()
    } else {
        "portable-pty unknown backend".to_owned()
    }
}

fn font_discovery_label(config: &AppConfig) -> String {
    let fonts = FontSystem::new(font_config(&config.font));
    fonts
        .diagnostics()
        .into_iter()
        .map(|diagnostic| {
            format_font_diagnostic(
                diagnostic.role,
                &diagnostic.family,
                diagnostic.resolved,
                &diagnostic.source,
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn format_font_diagnostic(role: &str, family: &str, resolved: bool, source: &FontSource) -> String {
    let unresolved = matches!(source, FontSource::Unresolved);
    let source = match source {
        FontSource::File(path) => format!("file:{}", path.display()),
        FontSource::Memory => "memory".to_owned(),
        FontSource::Unresolved => "unresolved".to_owned(),
    };
    let fallback = if resolved || unresolved {
        ""
    } else {
        " (style fallback)"
    };
    format!("{role}:{family}={source}{fallback}")
}

/// Where the resolved-font-file cache lives, beside the other desktop state.
fn font_cache_path() -> PathBuf {
    mux_state_path()
        .parent()
        .map_or_else(std::env::temp_dir, Path::to_path_buf)
        .join("font-cache.txt")
}

/// A cheap signature of the font directories.
///
/// Installing or removing a font changes the containing directory\'s modified
/// time and entry count, so comparing this against the stored signature keeps a
/// newly installed face from being masked by a cache that predates it. Only one
/// level is walked: listing a directory is milliseconds, parsing every font in
/// it is seconds, and the latter is what this exists to avoid.
fn font_directory_signature() -> String {
    let mut parts: Vec<String> = Vec::new();
    for directory in system_font_directories() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        let mut count = 0usize;
        let mut newest = 0u64;
        for entry in entries.flatten() {
            count += 1;
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let stamp = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_secs());
            newest = newest.max(stamp);
            if metadata.is_dir()
                && let Ok(nested) = fs::read_dir(entry.path())
            {
                count += nested.flatten().count();
            }
        }
        parts.push(format!("{}|{count}|{newest}", directory.display()));
    }
    parts.sort();
    parts.join(";")
}

/// The directories the platform keeps installed fonts in.
fn system_font_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if cfg!(target_os = "windows") {
        if let Some(windir) = std::env::var_os("WINDIR") {
            directories.push(PathBuf::from(windir).join("Fonts"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            directories.push(
                PathBuf::from(local)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
        return directories;
    }
    if cfg!(target_os = "macos") {
        directories.push(PathBuf::from("/System/Library/Fonts"));
        directories.push(PathBuf::from("/Library/Fonts"));
        if let Some(home) = std::env::var_os("HOME") {
            directories.push(PathBuf::from(home).join("Library").join("Fonts"));
        }
        return directories;
    }
    directories.push(PathBuf::from("/usr/share/fonts"));
    directories.push(PathBuf::from("/usr/local/share/fonts"));
    if let Some(home) = std::env::var_os("HOME") {
        directories.push(PathBuf::from(&home).join(".local").join("share").join("fonts"));
        directories.push(PathBuf::from(home).join(".fonts"));
    }
    directories
}

/// Reads the cached font files, or nothing when the cache cannot be trusted.
///
/// The first line is the directory signature the cache was written under. A
/// mismatch, a missing file, or anything unparseable yields no paths, which
/// simply means the catalog performs its normal full scan.
fn cached_font_files(path: &Path, signature: &str) -> Vec<PathBuf> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = contents.lines();
    if lines.next() != Some(signature) {
        return Vec::new();
    }
    lines
        .filter(|line| !line.trim().is_empty())
        .map(PathBuf::from)
        .filter(|candidate| candidate.is_file())
        .collect()
}

/// Records the font files this run resolved, for the next launch to start from.
fn store_font_files(path: &Path, signature: &str, files: &[PathBuf]) {
    if files.is_empty() {
        return;
    }
    let mut contents = String::with_capacity(signature.len() + files.len() * 64);
    contents.push_str(signature);
    contents.push('\n');
    for file in files {
        contents.push_str(&file.to_string_lossy());
        contents.push('\n');
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, contents);
}
