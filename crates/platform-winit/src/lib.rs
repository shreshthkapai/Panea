//! Desktop window and event integration boundary.

pub const LAYER: &str = "platform parity";

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        mpsc::{SyncSender, TrySendError, sync_channel},
    },
    thread,
    time::{Duration, Instant},
};

use arboard::Clipboard;
use notify_rust::Notification;
#[cfg(not(target_os = "macos"))]
use notify_rust::Urgency;
use platform_core::{
    ClipboardAvailability, ClipboardDiagnostic, ClipboardOperation, ClipboardProvider,
    CompositorInfo, DecorationMode, DecorationModeDiagnostic, DesktopPlatform, DpiBehavior,
    DpiInfo, ImeEvent, ImeSupport, InputEvent, KeyEvent, KeyModifiers, KeyState,
    LinuxWindowBackend, LinuxWindowBackendDiagnostic, MonitorInfo, MouseButton, MouseEvent,
    MouseEventKind, NotificationAvailability, NotificationBackend, NotificationDiagnostic,
    NotificationProvider, NotificationRequest, NotificationUrgency, PlatformCapabilities,
    PlatformFallback, PowerSource, PowerState, PowerStateDiagnostic, PowerStateProvider,
    ShellEnvironmentInfo, UrlOpenDiagnostic, UrlOpener, WindowAction, WindowChromeAction,
    WindowChromeActionDiagnostic, WindowChromeActionExecutor, WindowMode, WindowModeDiagnostic,
    execute_window_chrome_action,
};
use starship_battery::units::ratio::percent;
use starship_battery::{Manager as BatteryManager, State as BatteryState};
use winit::{
    dpi::LogicalSize,
    event::{ElementState, Ime, MouseScrollDelta, WindowEvent},
    event_loop::EventLoop,
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
    window::{Fullscreen, Icon, Window},
};

const POWER_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const NOTIFICATION_QUEUE_CAPACITY: usize = 16;
const INITIAL_ACTIVATION_KEY_GUARD: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowIcon {
    rgba: Arc<[u8]>,
    width: u32,
    height: u32,
}

impl WindowIcon {
    pub fn from_rgba(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, String> {
        let expected = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "window icon dimensions overflow".to_owned())?;
        if width == 0 || height == 0 {
            return Err("window icon dimensions must be non-zero".to_owned());
        }
        if rgba.len() != expected as usize {
            return Err(format!(
                "window icon contains {} RGBA bytes; expected {expected}",
                rgba.len()
            ));
        }
        Ok(Self {
            rgba: rgba.into(),
            width,
            height,
        })
    }

    fn to_winit(&self) -> Option<Icon> {
        Icon::from_rgba(self.rgba.to_vec(), self.width, self.height).ok()
    }
}

#[derive(Debug)]
pub struct DesktopNotificationProvider {
    enabled: bool,
    backend: NotificationBackend,
    sender: Option<SyncSender<NotificationRequest>>,
    diagnostic: Arc<Mutex<NotificationDiagnostic>>,
}

impl DesktopNotificationProvider {
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        let backend = notification_backend();
        let (availability, message) = if !enabled {
            (
                NotificationAvailability::Disabled,
                "native notifications are disabled by config".to_owned(),
            )
        } else if backend == NotificationBackend::Unsupported {
            (
                NotificationAvailability::Unavailable,
                "this platform build has no native notification backend".to_owned(),
            )
        } else {
            (
                NotificationAvailability::Available,
                format!(
                    "{} notification backend ready; delivery worker starts on first use",
                    notification_backend_name(backend)
                ),
            )
        };
        Self {
            enabled,
            backend,
            sender: None,
            diagnostic: Arc::new(Mutex::new(NotificationDiagnostic {
                backend,
                availability,
                message,
            })),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        *self = Self::new(enabled);
    }

    fn ensure_worker(&mut self) -> Result<(), NotificationDiagnostic> {
        if self.sender.is_some() {
            return Ok(());
        }
        if !self.enabled || self.backend == NotificationBackend::Unsupported {
            return Err(self.diagnostic());
        }
        let (sender, receiver) = sync_channel(NOTIFICATION_QUEUE_CAPACITY);
        let backend = self.backend;
        let diagnostic = Arc::clone(&self.diagnostic);
        thread::Builder::new()
            .name("panea-notifications".to_owned())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    let result = deliver_native_notification(&request);
                    let next = match result {
                        Ok(()) => NotificationDiagnostic {
                            backend,
                            availability: NotificationAvailability::Available,
                            message: format!(
                                "{} notification delivered",
                                notification_backend_name(backend)
                            ),
                        },
                        Err(message) => NotificationDiagnostic {
                            backend,
                            availability: NotificationAvailability::Unavailable,
                            message: format!(
                                "{} notification delivery failed: {message}",
                                notification_backend_name(backend)
                            ),
                        },
                    };
                    set_notification_diagnostic(&diagnostic, next);
                }
            })
            .map_err(|error| {
                let diagnostic = NotificationDiagnostic {
                    backend,
                    availability: NotificationAvailability::Unavailable,
                    message: format!("failed to start notification worker: {error}"),
                };
                set_notification_diagnostic(&self.diagnostic, diagnostic.clone());
                diagnostic
            })?;
        self.sender = Some(sender);
        Ok(())
    }
}

impl NotificationProvider for DesktopNotificationProvider {
    fn notify(&mut self, request: NotificationRequest) -> Result<(), NotificationDiagnostic> {
        if let Err(message) = request.validate() {
            return Err(NotificationDiagnostic {
                backend: self.backend,
                availability: NotificationAvailability::Unavailable,
                message: message.to_owned(),
            });
        }
        self.ensure_worker()?;
        let Some(sender) = self.sender.as_ref() else {
            return Err(self.diagnostic());
        };
        sender.try_send(request).map_err(|error| {
            let message = match error {
                TrySendError::Full(_) => {
                    "notification queue is full; newest notification was dropped".to_owned()
                }
                TrySendError::Disconnected(_) => {
                    self.sender = None;
                    "notification worker stopped unexpectedly".to_owned()
                }
            };
            let diagnostic = NotificationDiagnostic {
                backend: self.backend,
                availability: NotificationAvailability::Unavailable,
                message,
            };
            set_notification_diagnostic(&self.diagnostic, diagnostic.clone());
            diagnostic
        })
    }

    fn diagnostic(&self) -> NotificationDiagnostic {
        self.diagnostic
            .lock()
            .map(|diagnostic| diagnostic.clone())
            .unwrap_or_else(|_| NotificationDiagnostic {
                backend: self.backend,
                availability: NotificationAvailability::Unavailable,
                message: "notification diagnostic lock was poisoned".to_owned(),
            })
    }
}

fn set_notification_diagnostic(
    state: &Arc<Mutex<NotificationDiagnostic>>,
    diagnostic: NotificationDiagnostic,
) {
    if let Ok(mut state) = state.lock() {
        *state = diagnostic;
    }
}

fn deliver_native_notification(request: &NotificationRequest) -> Result<(), String> {
    let mut notification = Notification::new();
    notification
        .appname("Panea")
        .summary(&request.title)
        .body(&request.body);
    apply_notification_urgency(&mut notification, request.urgency);
    #[cfg(windows)]
    notification.app_id("Panea.Terminal");
    notification
        .show()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(not(target_os = "macos"))]
fn apply_notification_urgency(notification: &mut Notification, urgency: NotificationUrgency) {
    notification.urgency(match urgency {
        NotificationUrgency::Low => Urgency::Low,
        NotificationUrgency::Normal => Urgency::Normal,
        NotificationUrgency::Critical => Urgency::Critical,
    });
}

#[cfg(target_os = "macos")]
fn apply_notification_urgency(_notification: &mut Notification, _urgency: NotificationUrgency) {}

const fn notification_backend() -> NotificationBackend {
    if cfg!(windows) {
        NotificationBackend::WindowsToast
    } else if cfg!(target_os = "macos") {
        NotificationBackend::MacOsNotificationCenter
    } else if cfg!(target_os = "linux") {
        NotificationBackend::Freedesktop
    } else {
        NotificationBackend::Unsupported
    }
}

const fn notification_backend_name(backend: NotificationBackend) -> &'static str {
    match backend {
        NotificationBackend::WindowsToast => "Windows toast",
        NotificationBackend::MacOsNotificationCenter => "macOS Notification Center",
        NotificationBackend::Freedesktop => "freedesktop D-Bus",
        NotificationBackend::Unsupported => "unsupported",
    }
}

#[derive(Debug)]
pub struct DesktopPowerMonitor {
    enabled: bool,
    manager: Option<BatteryManager>,
    last: PowerStateDiagnostic,
    next_refresh: Instant,
}

impl DesktopPowerMonitor {
    #[must_use]
    pub fn new() -> Self {
        Self::with_enabled(true)
    }

    #[must_use]
    pub fn with_enabled(enabled: bool) -> Self {
        if !enabled {
            return Self {
                enabled: false,
                manager: None,
                last: PowerStateDiagnostic {
                    state: PowerState::UNKNOWN,
                    message: None,
                },
                next_refresh: Instant::now() + POWER_REFRESH_INTERVAL,
            };
        }
        let (manager, last) = match BatteryManager::new() {
            Ok(manager) => (
                Some(manager),
                PowerStateDiagnostic {
                    state: PowerState::UNKNOWN,
                    message: None,
                },
            ),
            Err(error) => (
                None,
                PowerStateDiagnostic {
                    state: PowerState::UNKNOWN,
                    message: Some(format!("power-state provider unavailable: {error}")),
                },
            ),
        };
        let mut monitor = Self {
            enabled: true,
            manager,
            last,
            next_refresh: Instant::now(),
        };
        monitor.refresh_if_due();
        monitor
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        *self = Self::with_enabled(enabled);
    }

    pub fn refresh_if_due(&mut self) -> bool {
        if !self.enabled {
            return false;
        }
        if Instant::now() < self.next_refresh {
            return false;
        }
        self.next_refresh = Instant::now() + POWER_REFRESH_INTERVAL;
        let next = self.read_power_state();
        let changed = next != self.last;
        self.last = next;
        changed
    }

    #[must_use]
    pub fn next_refresh_after(&self) -> Option<Duration> {
        self.enabled
            .then(|| self.next_refresh.saturating_duration_since(Instant::now()))
    }

    fn read_power_state(&self) -> PowerStateDiagnostic {
        let Some(manager) = self.manager.as_ref() else {
            return self.last.clone();
        };
        let batteries = match manager.batteries() {
            Ok(batteries) => batteries,
            Err(error) => {
                return PowerStateDiagnostic {
                    state: PowerState::UNKNOWN,
                    message: Some(format!("failed to enumerate batteries: {error}")),
                };
            }
        };

        let mut battery_count = 0usize;
        let mut on_battery = false;
        let mut charge_percent: Option<u8> = None;
        let mut provider_error = None;
        for battery in batteries {
            match battery {
                Ok(battery) => {
                    battery_count += 1;
                    on_battery |= matches!(
                        battery.state(),
                        BatteryState::Discharging | BatteryState::Empty
                    );
                    let charge = battery
                        .state_of_charge()
                        .get::<percent>()
                        .clamp(0.0, 100.0)
                        .round() as u8;
                    charge_percent = Some(charge_percent.map_or(charge, |value| value.min(charge)));
                }
                Err(error) => provider_error = Some(error.to_string()),
            }
        }

        let source = if on_battery {
            PowerSource::Battery
        } else if battery_count > 0 {
            PowerSource::Ac
        } else {
            PowerSource::Unknown
        };
        PowerStateDiagnostic {
            state: PowerState {
                source,
                battery_count,
                charge_percent,
            },
            message: provider_error.map(|error| format!("battery read failed: {error}")),
        }
    }
}

impl Default for DesktopPowerMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerStateProvider for DesktopPowerMonitor {
    fn power_state(&mut self) -> PowerStateDiagnostic {
        self.refresh_if_due();
        self.last.clone()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowSettings {
    pub title: String,
    pub initial_width: u32,
    pub initial_height: u32,
    pub visible_on_create: bool,
    pub mode: WindowMode,
    pub linux_backend: LinuxWindowBackend,
    pub decoration_mode: DecorationMode,
    pub opacity: f64,
    pub icon: Option<WindowIcon>,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            title: "Panea".to_owned(),
            initial_width: 960,
            initial_height: 560,
            visible_on_create: true,
            mode: WindowMode::Windowed,
            linux_backend: LinuxWindowBackend::Auto,
            decoration_mode: DecorationMode::Auto,
            opacity: 1.0,
            icon: None,
        }
    }
}

#[derive(Debug)]
pub struct DesktopWindow {
    window: Arc<Window>,
    diagnostics: DesktopWindowDiagnostics,
}

impl DesktopWindow {
    pub fn create(
        event_loop: &EventLoop<()>,
        settings: &WindowSettings,
    ) -> Result<Self, winit::error::OsError> {
        let decoration = resolve_decoration_mode(settings.decoration_mode, detected_platform());
        let decorations = window_mode_decorations_visible(settings.mode, decoration.effective);

        let attributes = Window::default_attributes()
            .with_title(settings.title.clone())
            .with_window_icon(settings.icon.as_ref().and_then(WindowIcon::to_winit))
            .with_inner_size(LogicalSize::new(
                settings.initial_width,
                settings.initial_height,
            ))
            .with_visible(settings.visible_on_create)
            .with_decorations(decorations)
            .with_transparent(settings.opacity < 1.0)
            .with_maximized(matches!(settings.mode, WindowMode::Maximized))
            .with_fullscreen(initial_winit_fullscreen(settings.mode));
        #[cfg(target_os = "windows")]
        let attributes = {
            use winit::platform::windows::WindowAttributesExtWindows;

            attributes.with_no_redirection_bitmap(no_redirection_bitmap_required(
                DesktopPlatform::Windows,
                settings.opacity,
            ))
        };
        #[allow(deprecated)]
        let window = event_loop.create_window(attributes)?;

        let window = Arc::new(window);
        window.set_ime_allowed(true);
        let window_mode =
            apply_window_mode_with_decoration(&window, settings.mode, decoration.effective);
        let linux = linux_backend_diagnostic(settings, &decoration);
        let monitors = monitor_infos(&window);
        let dpi = window_dpi_info(&window);

        Ok(Self {
            window,
            diagnostics: DesktopWindowDiagnostics {
                dpi,
                monitors,
                window_mode,
                decoration: DecorationModeDiagnostic {
                    requested: settings.decoration_mode,
                    effective: decoration.effective,
                    fallback: decoration.fallback.clone(),
                },
                linux,
            },
        })
    }

    #[must_use]
    pub fn window(&self) -> Arc<Window> {
        Arc::clone(&self.window)
    }

    #[must_use]
    pub fn diagnostics(&self) -> &DesktopWindowDiagnostics {
        &self.diagnostics
    }
}

fn initial_winit_fullscreen(mode: WindowMode) -> Option<Fullscreen> {
    matches!(
        mode,
        WindowMode::Fullscreen | WindowMode::BorderlessFullscreen | WindowMode::FramelessFullscreen
    )
    .then(|| Fullscreen::Borderless(None))
}

const fn no_redirection_bitmap_required(platform: DesktopPlatform, opacity: f64) -> bool {
    matches!(platform, DesktopPlatform::Windows) && opacity < 1.0
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesktopWindowDiagnostics {
    pub dpi: DpiInfo,
    pub monitors: Vec<MonitorInfo>,
    pub window_mode: WindowModeDiagnostic,
    pub decoration: DecorationModeDiagnostic,
    pub linux: Option<LinuxWindowBackendDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct InputTranslator {
    modifiers: KeyModifiers,
    cursor_position: (f64, f64),
    pressed_keys: HashSet<PhysicalKey>,
    suppressed_activation_keys: HashSet<PhysicalKey>,
    activation_guard_started_at: Instant,
    initial_focus_seen: bool,
}

impl InputTranslator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            modifiers: KeyModifiers::default(),
            cursor_position: (0.0, 0.0),
            pressed_keys: HashSet::new(),
            suppressed_activation_keys: HashSet::new(),
            activation_guard_started_at: Instant::now(),
            initial_focus_seen: false,
        }
    }

    #[must_use]
    pub fn modifiers(&self) -> KeyModifiers {
        self.modifiers
    }

    /// Starts a fresh launcher-to-window input handoff immediately before the
    /// application begins consuming native events. Expensive startup work may
    /// happen after this translator is constructed, so constructor time is not
    /// a reliable boundary for quarantining a held launcher key.
    pub fn arm_initial_focus_handoff(&mut self) {
        self.pressed_keys.clear();
        self.suppressed_activation_keys.clear();
        self.activation_guard_started_at = Instant::now();
        self.initial_focus_seen = false;
    }

    pub fn translate_window_event(&mut self, event: &WindowEvent) -> Vec<InputEvent> {
        match event {
            WindowEvent::CloseRequested => vec![InputEvent::CloseRequested],
            WindowEvent::Focused(focused) => {
                if *focused && !self.initial_focus_seen {
                    self.activation_guard_started_at = Instant::now();
                    self.initial_focus_seen = true;
                } else if !focused {
                    self.pressed_keys.clear();
                    self.suppressed_activation_keys.clear();
                    self.modifiers = KeyModifiers::default();
                }
                vec![InputEvent::Focused(*focused)]
            }
            WindowEvent::Resized(size) => vec![InputEvent::Resized {
                width: size.width,
                height: size.height,
            }],
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                vec![InputEvent::ScaleFactorChanged {
                    scale_factor: *scale_factor,
                }]
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers_from_winit(modifiers.state());
                Vec::new()
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if should_suppress_initial_activation_key(
                    &mut self.suppressed_activation_keys,
                    &event.physical_key,
                    event.state,
                    Instant::now().saturating_duration_since(self.activation_guard_started_at),
                ) {
                    return Vec::new();
                }
                if !should_forward_key_event(
                    &mut self.pressed_keys,
                    &event.physical_key,
                    event.state,
                    event.repeat,
                ) {
                    return Vec::new();
                }
                if matches!(event.logical_key, Key::Named(NamedKey::AltGraph)) {
                    self.modifiers.alt_graph = event.state == ElementState::Pressed;
                }
                if event.state != ElementState::Pressed {
                    return vec![InputEvent::Key(key_event_from_winit(event, self.modifiers))];
                }

                let key = key_event_from_winit(event, self.modifiers);
                let action = recovery_action(&key);
                if let Some(action) = action {
                    vec![InputEvent::WindowAction(action)]
                } else {
                    vec![InputEvent::Key(key)]
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = (position.x, position.y);
                vec![InputEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    x: position.x,
                    y: position.y,
                    modifiers: self.modifiers,
                })]
            }
            WindowEvent::MouseInput { state, button, .. } => {
                vec![InputEvent::Mouse(MouseEvent {
                    kind: match state {
                        ElementState::Pressed => MouseEventKind::Pressed(mouse_button(*button)),
                        ElementState::Released => MouseEventKind::Released(mouse_button(*button)),
                    },
                    x: self.cursor_position.0,
                    y: self.cursor_position.1,
                    modifiers: self.modifiers,
                })]
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (delta_x, delta_y) = wheel_delta(*delta);
                vec![InputEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::Wheel { delta_x, delta_y },
                    x: self.cursor_position.0,
                    y: self.cursor_position.1,
                    modifiers: self.modifiers,
                })]
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => vec![InputEvent::Ime(ImeEvent::Enabled)],
                Ime::Preedit(text, _) => {
                    vec![InputEvent::Ime(ImeEvent::Preedit { text: text.clone() })]
                }
                Ime::Commit(text) => ime_commit_text(text).map_or_else(Vec::new, |text| {
                    vec![InputEvent::Ime(ImeEvent::Commit { text })]
                }),
                Ime::Disabled => vec![InputEvent::Ime(ImeEvent::Disabled)],
            },
            _ => Vec::new(),
        }
    }
}

fn should_suppress_initial_activation_key(
    suppressed_keys: &mut HashSet<PhysicalKey>,
    key: &PhysicalKey,
    state: ElementState,
    since_initial_focus: Duration,
) -> bool {
    if suppressed_keys.contains(key) {
        if state == ElementState::Released {
            suppressed_keys.remove(key);
        }
        return true;
    }

    if state == ElementState::Pressed
        && since_initial_focus <= INITIAL_ACTIVATION_KEY_GUARD
        && is_window_activation_key(key)
    {
        suppressed_keys.insert(*key);
        return true;
    }

    false
}

fn is_window_activation_key(key: &PhysicalKey) -> bool {
    matches!(
        key,
        PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space)
    )
}

fn should_forward_key_event(
    pressed_keys: &mut HashSet<PhysicalKey>,
    key: &PhysicalKey,
    state: ElementState,
    repeat: bool,
) -> bool {
    match state {
        ElementState::Pressed => {
            if repeat && !pressed_keys.contains(key) {
                return false;
            }
            pressed_keys.insert(*key);
            true
        }
        ElementState::Released => {
            pressed_keys.remove(key);
            true
        }
    }
}

fn ime_commit_text(text: &str) -> Option<String> {
    (!text.is_empty() && text.chars().all(|character| !character.is_control()))
        .then(|| text.to_owned())
}

impl Default for InputTranslator {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ClipboardBridge {
    clipboard: Option<Clipboard>,
    last_diagnostic: ClipboardDiagnostic,
}

impl ClipboardBridge {
    #[must_use]
    pub fn new() -> Self {
        match Clipboard::new() {
            Ok(clipboard) => Self {
                clipboard: Some(clipboard),
                last_diagnostic: clipboard_diagnostic(
                    ClipboardOperation::Paste,
                    ClipboardAvailability::Available,
                    None,
                ),
            },
            Err(error) => Self {
                clipboard: None,
                last_diagnostic: clipboard_diagnostic(
                    ClipboardOperation::Paste,
                    ClipboardAvailability::Unavailable,
                    Some(error.to_string()),
                ),
            },
        }
    }

    pub fn copy_text(&mut self, text: &str) -> Result<(), ClipboardDiagnostic> {
        let Some(clipboard) = self.clipboard.as_mut() else {
            let diagnostic = clipboard_diagnostic(
                ClipboardOperation::Copy,
                ClipboardAvailability::Unavailable,
                Some("clipboard backend is unavailable".to_owned()),
            );
            self.last_diagnostic = diagnostic.clone();
            return Err(diagnostic);
        };

        clipboard.set_text(text.to_owned()).map_err(|error| {
            let diagnostic = clipboard_diagnostic(
                ClipboardOperation::Copy,
                ClipboardAvailability::Available,
                Some(error.to_string()),
            );
            self.last_diagnostic = diagnostic.clone();
            diagnostic
        })
    }

    pub fn paste_text(&mut self) -> Result<String, ClipboardDiagnostic> {
        let Some(clipboard) = self.clipboard.as_mut() else {
            let diagnostic = clipboard_diagnostic(
                ClipboardOperation::Paste,
                ClipboardAvailability::Unavailable,
                Some("clipboard backend is unavailable".to_owned()),
            );
            self.last_diagnostic = diagnostic.clone();
            return Err(diagnostic);
        };

        clipboard.get_text().map_err(|error| {
            let diagnostic = clipboard_diagnostic(
                ClipboardOperation::Paste,
                ClipboardAvailability::Available,
                Some(error.to_string()),
            );
            self.last_diagnostic = diagnostic.clone();
            diagnostic
        })
    }

    #[cfg(target_os = "linux")]
    pub fn copy_primary_text(&mut self, text: &str) -> Result<(), ClipboardDiagnostic> {
        use arboard::{LinuxClipboardKind, SetExtLinux};

        let Some(clipboard) = self.clipboard.as_mut() else {
            return Err(clipboard_diagnostic(
                ClipboardOperation::Copy,
                ClipboardAvailability::Unavailable,
                Some("clipboard backend is unavailable".to_owned()),
            ));
        };
        let result = clipboard
            .set()
            .clipboard(LinuxClipboardKind::Primary)
            .text(text.to_owned())
            .map_err(|error| {
                clipboard_diagnostic(
                    ClipboardOperation::Copy,
                    ClipboardAvailability::Unavailable,
                    Some(format!("Linux primary selection unavailable: {error}")),
                )
            });
        self.last_diagnostic = match &result {
            Ok(()) => clipboard_diagnostic(
                ClipboardOperation::Copy,
                ClipboardAvailability::Available,
                None,
            ),
            Err(diagnostic) => diagnostic.clone(),
        };
        result
    }

    #[cfg(not(target_os = "linux"))]
    pub fn copy_primary_text(&mut self, _text: &str) -> Result<(), ClipboardDiagnostic> {
        Err(clipboard_diagnostic(
            ClipboardOperation::Copy,
            ClipboardAvailability::Unavailable,
            Some("primary selection is available only on Linux".to_owned()),
        ))
    }

    #[cfg(target_os = "linux")]
    pub fn paste_primary_text(&mut self) -> Result<String, ClipboardDiagnostic> {
        use arboard::{GetExtLinux, LinuxClipboardKind};

        let Some(clipboard) = self.clipboard.as_mut() else {
            return Err(clipboard_diagnostic(
                ClipboardOperation::Paste,
                ClipboardAvailability::Unavailable,
                Some("clipboard backend is unavailable".to_owned()),
            ));
        };
        let result = clipboard
            .get()
            .clipboard(LinuxClipboardKind::Primary)
            .text()
            .map_err(|error| {
                clipboard_diagnostic(
                    ClipboardOperation::Paste,
                    ClipboardAvailability::Unavailable,
                    Some(format!("Linux primary selection unavailable: {error}")),
                )
            });
        self.last_diagnostic = match &result {
            Ok(_) => clipboard_diagnostic(
                ClipboardOperation::Paste,
                ClipboardAvailability::Available,
                None,
            ),
            Err(diagnostic) => diagnostic.clone(),
        };
        result
    }

    #[cfg(not(target_os = "linux"))]
    pub fn paste_primary_text(&mut self) -> Result<String, ClipboardDiagnostic> {
        Err(clipboard_diagnostic(
            ClipboardOperation::Paste,
            ClipboardAvailability::Unavailable,
            Some("primary selection is available only on Linux".to_owned()),
        ))
    }

    #[must_use]
    pub fn last_diagnostic(&self) -> &ClipboardDiagnostic {
        &self.last_diagnostic
    }
}

impl ClipboardProvider for ClipboardBridge {
    fn copy_text(&mut self, text: &str) -> Result<(), ClipboardDiagnostic> {
        Self::copy_text(self, text)
    }

    fn paste_text(&mut self) -> Result<String, ClipboardDiagnostic> {
        Self::paste_text(self)
    }

    fn last_diagnostic(&self) -> ClipboardDiagnostic {
        Self::last_diagnostic(self).clone()
    }

    fn copy_primary_text(&mut self, text: &str) -> Result<(), ClipboardDiagnostic> {
        Self::copy_primary_text(self, text)
    }

    fn paste_primary_text(&mut self) -> Result<String, ClipboardDiagnostic> {
        Self::paste_primary_text(self)
    }
}

#[derive(Debug, Default)]
pub struct DesktopUrlOpener;

impl DesktopUrlOpener {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl UrlOpener for DesktopUrlOpener {
    fn open_url(&mut self, url: &str) -> Result<(), UrlOpenDiagnostic> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(UrlOpenDiagnostic {
                url: url.to_owned(),
                message: Some("only http:// and https:// URL actions are allowed".to_owned()),
            });
        }
        webbrowser::open(url).map_err(|error| UrlOpenDiagnostic {
            url: url.to_owned(),
            message: Some(error.to_string()),
        })
    }
}

impl Default for ClipboardBridge {
    fn default() -> Self {
        Self::new()
    }
}

pub fn apply_window_mode(window: &Window, requested: WindowMode) -> WindowModeDiagnostic {
    apply_window_mode_with_decoration(window, requested, DecorationMode::Native)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WinitWindowChromeOperation {
    DragWindow,
    Minimize,
    LeaveFullscreen,
    RequestDesktopClose,
}

const fn window_chrome_operation(action: WindowChromeAction) -> WinitWindowChromeOperation {
    match action {
        WindowChromeAction::BeginDrag => WinitWindowChromeOperation::DragWindow,
        WindowChromeAction::Minimize => WinitWindowChromeOperation::Minimize,
        WindowChromeAction::LeaveFullscreen => WinitWindowChromeOperation::LeaveFullscreen,
        WindowChromeAction::Close => WinitWindowChromeOperation::RequestDesktopClose,
    }
}

struct WinitWindowChromeExecutor<'window> {
    window: &'window Window,
}

impl WindowChromeActionExecutor for WinitWindowChromeExecutor<'_> {
    fn try_apply_window_chrome_action(
        &mut self,
        action: WindowChromeAction,
    ) -> Result<(), PlatformFallback> {
        match window_chrome_operation(action) {
            WinitWindowChromeOperation::DragWindow => self
                .window
                .drag_window()
                .map_err(|error| window_chrome_action_fallback(action, &error.to_string())),
            WinitWindowChromeOperation::Minimize => {
                self.window.set_minimized(true);
                Ok(())
            }
            WinitWindowChromeOperation::LeaveFullscreen => {
                self.window.set_fullscreen(None);
                Ok(())
            }
            WinitWindowChromeOperation::RequestDesktopClose => Ok(()),
        }
    }
}

/// Applies a client-chrome action without exposing winit to the app's controller.
/// A successful `Close` diagnostic is an exit intent that the desktop event loop
/// must honor; this function never terminates the process itself.
#[must_use]
pub fn apply_window_chrome_action(
    window: &Window,
    action: WindowChromeAction,
) -> WindowChromeActionDiagnostic {
    execute_window_chrome_action(&mut WinitWindowChromeExecutor { window }, action)
}

fn window_chrome_action_fallback(action: WindowChromeAction, reason: &str) -> PlatformFallback {
    PlatformFallback {
        feature: "window_chrome_action".to_owned(),
        requested: action.as_str().to_owned(),
        effective: "unchanged".to_owned(),
        reason: reason.to_owned(),
    }
}

pub fn apply_window_mode_with_decoration(
    window: &Window,
    requested: WindowMode,
    decoration: DecorationMode,
) -> WindowModeDiagnostic {
    let mut effective = requested;
    let mut fallback = None;
    let decorated = window_mode_decorations_visible(requested, decoration);

    match requested {
        WindowMode::Windowed => {
            window.set_fullscreen(None);
            window.set_maximized(false);
            window.set_decorations(decorated);
        }
        WindowMode::Maximized => {
            window.set_fullscreen(None);
            window.set_decorations(decorated);
            window.set_maximized(true);
        }
        WindowMode::Fullscreen => {
            window.set_decorations(decorated);
            if let Some(video_mode) = preferred_video_mode(window) {
                window.set_fullscreen(Some(Fullscreen::Exclusive(video_mode)));
            } else {
                window.set_fullscreen(Some(Fullscreen::Borderless(window.current_monitor())));
                effective = WindowMode::BorderlessFullscreen;
                fallback = Some(PlatformFallback {
                    feature: "window_mode".to_owned(),
                    requested: "fullscreen".to_owned(),
                    effective: "borderless_fullscreen".to_owned(),
                    reason: "the active monitor did not expose an exclusive video mode".to_owned(),
                });
            }
        }
        WindowMode::BorderlessFullscreen => {
            window.set_maximized(false);
            // Remove the non-client frame before expanding a transparent
            // surface. Windows DWM can otherwise retain the old decorated
            // frame beneath the borderless fullscreen client.
            window.set_decorations(decorated);
            window.set_fullscreen(Some(Fullscreen::Borderless(window.current_monitor())));
        }
        WindowMode::FramelessWindowed => {
            window.set_fullscreen(None);
            window.set_maximized(false);
            window.set_decorations(false);
        }
        WindowMode::FramelessFullscreen => {
            window.set_maximized(false);
            window.set_decorations(false);
            window.set_fullscreen(Some(Fullscreen::Borderless(window.current_monitor())));
        }
    }

    WindowModeDiagnostic {
        requested,
        effective,
        fallback,
    }
}

const fn window_mode_decorations_visible(mode: WindowMode, decoration: DecorationMode) -> bool {
    !matches!(decoration, DecorationMode::None)
        && !matches!(
            mode,
            WindowMode::BorderlessFullscreen
                | WindowMode::FramelessWindowed
                | WindowMode::FramelessFullscreen
        )
}

fn preferred_video_mode(window: &Window) -> Option<winit::monitor::VideoModeHandle> {
    let size = window.inner_size();
    window.current_monitor()?.video_modes().max_by_key(|mode| {
        let mode_size = mode.size();
        let size_match = u8::from(mode_size == size);
        (size_match, mode.refresh_rate_millihertz(), mode.bit_depth())
    })
}

#[derive(Debug, Clone)]
struct DecorationResolution {
    effective: DecorationMode,
    fallback: Option<PlatformFallback>,
}

fn resolve_decoration_mode(
    requested: DecorationMode,
    platform: DesktopPlatform,
) -> DecorationResolution {
    let (effective, reason) = match requested {
        DecorationMode::Auto => (DecorationMode::Native, None),
        DecorationMode::Native | DecorationMode::None => (requested, None),
        DecorationMode::ServerSide if platform == DesktopPlatform::LinuxX11 => {
            (DecorationMode::Native, None)
        }
        DecorationMode::ClientSide if platform == DesktopPlatform::LinuxWayland => (
            DecorationMode::Native,
            Some(
                "winit negotiates Wayland decorations with the compositor; exact client-side selection is not guaranteed",
            ),
        ),
        DecorationMode::ServerSide
        | DecorationMode::ClientSide
        | DecorationMode::Custom
        | DecorationMode::FallbackDecorated => (
            DecorationMode::Native,
            Some("the active window backend cannot guarantee the requested decoration strategy"),
        ),
    };
    DecorationResolution {
        effective,
        fallback: reason.map(|reason| PlatformFallback {
            feature: "window_decorations".to_owned(),
            requested: format!("{requested:?}"),
            effective: format!("{effective:?}"),
            reason: reason.to_owned(),
        }),
    }
}

pub fn create_event_loop(
    requested: LinuxWindowBackend,
) -> Result<EventLoop<()>, winit::error::EventLoopError> {
    let mut builder = EventLoop::builder();
    #[cfg(target_os = "linux")]
    match requested {
        LinuxWindowBackend::Auto => {}
        LinuxWindowBackend::X11 => {
            use winit::platform::x11::EventLoopBuilderExtX11;
            builder.with_x11();
        }
        LinuxWindowBackend::Wayland => {
            use winit::platform::wayland::EventLoopBuilderExtWayland;
            builder.with_wayland();
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = requested;
    builder.build()
}

#[must_use]
pub fn platform_capabilities(_event_loop: &EventLoop<()>, window: &Window) -> PlatformCapabilities {
    PlatformCapabilities {
        platform: detected_platform(),
        window_modes_supported: vec![
            WindowMode::Windowed,
            WindowMode::Maximized,
            WindowMode::Fullscreen,
            WindowMode::BorderlessFullscreen,
            WindowMode::FramelessWindowed,
            WindowMode::FramelessFullscreen,
        ],
        window_chrome_actions_supported: window_chrome_actions_for(detected_platform()),
        decoration_modes_supported: vec![
            DecorationMode::Auto,
            DecorationMode::Native,
            DecorationMode::None,
            DecorationMode::FallbackDecorated,
        ],
        clipboard_capabilities: clipboard_capabilities(),
        gpu_backends_available: Vec::new(),
        ime_supported: ImeSupport::Basic,
        dpi_behavior: dpi_behavior_for_platform(),
        monitors: monitor_infos(window),
        compositor_info: compositor_info(),
        shell_environment_info: shell_environment_info(),
        fallbacks: Vec::new(),
    }
}

fn window_chrome_actions_for(platform: DesktopPlatform) -> Vec<WindowChromeAction> {
    if matches!(platform, DesktopPlatform::Unknown) {
        vec![WindowChromeAction::Close]
    } else {
        vec![
            WindowChromeAction::BeginDrag,
            WindowChromeAction::Minimize,
            WindowChromeAction::LeaveFullscreen,
            WindowChromeAction::Close,
        ]
    }
}

fn clipboard_capabilities() -> Vec<platform_core::ClipboardCapability> {
    let mut capabilities = vec![platform_core::ClipboardCapability::System];
    if cfg!(target_os = "linux") {
        capabilities.push(platform_core::ClipboardCapability::PrimarySelection);
    }
    capabilities
}

fn key_event_from_winit(event: &winit::event::KeyEvent, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        physical_key: Some(format!("{:?}", event.physical_key)),
        logical_key: match &event.logical_key {
            Key::Named(named) => format!("{named:?}"),
            Key::Character(text) => text.to_string(),
            Key::Unidentified(_) => "Unidentified".to_owned(),
            Key::Dead(dead) => format!("Dead({dead:?})"),
        },
        text: event.text.as_ref().map(ToString::to_string),
        state: match event.state {
            ElementState::Pressed => KeyState::Pressed,
            ElementState::Released => KeyState::Released,
        },
        modifiers,
        repeat: event.repeat,
    }
}

fn recovery_action(event: &KeyEvent) -> Option<WindowAction> {
    if !(event.modifiers.ctrl && event.modifiers.shift) {
        return None;
    }

    match event.logical_key.to_ascii_lowercase().as_str() {
        "f" => Some(WindowAction::ToggleFullscreen),
        "d" => Some(WindowAction::RestoreWindowDecorations),
        "m" => Some(WindowAction::ToggleFrameless),
        "w" => Some(WindowAction::CloseWindow),
        "p" => Some(WindowAction::OpenCommandPaletteLater),
        _ => None,
    }
}

fn modifiers_from_winit(modifiers: ModifiersState) -> KeyModifiers {
    KeyModifiers {
        shift: modifiers.shift_key(),
        ctrl: modifiers.control_key(),
        alt: modifiers.alt_key(),
        super_key: modifiers.super_key(),
        alt_graph: false,
    }
}

fn mouse_button(button: winit::event::MouseButton) -> MouseButton {
    match button {
        winit::event::MouseButton::Left => MouseButton::Left,
        winit::event::MouseButton::Middle => MouseButton::Middle,
        winit::event::MouseButton::Right => MouseButton::Right,
        winit::event::MouseButton::Back => MouseButton::Back,
        winit::event::MouseButton::Forward => MouseButton::Forward,
        winit::event::MouseButton::Other(value) => MouseButton::Other(value),
    }
}

fn wheel_delta(delta: MouseScrollDelta) -> (f64, f64) {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => (f64::from(x), f64::from(y)),
        MouseScrollDelta::PixelDelta(position) => (position.x, position.y),
    }
}

fn monitor_infos(window: &Window) -> Vec<MonitorInfo> {
    window.available_monitors().map(monitor_info).collect()
}

fn monitor_info(monitor: winit::monitor::MonitorHandle) -> MonitorInfo {
    let size = monitor.size();
    let position = monitor.position();
    let scale = monitor.scale_factor();
    MonitorInfo {
        name: monitor.name(),
        position_x: position.x,
        position_y: position.y,
        width: size.width,
        height: size.height,
        refresh_millihertz: monitor.refresh_rate_millihertz(),
        dpi: DpiInfo {
            scale_factor: scale,
            logical_width: (f64::from(size.width) / scale).round() as u32,
            logical_height: (f64::from(size.height) / scale).round() as u32,
            physical_width: size.width,
            physical_height: size.height,
        },
    }
}

fn window_dpi_info(window: &Window) -> DpiInfo {
    let scale = window.scale_factor();
    let physical = window.inner_size();
    DpiInfo {
        scale_factor: scale,
        logical_width: (f64::from(physical.width) / scale).round() as u32,
        logical_height: (f64::from(physical.height) / scale).round() as u32,
        physical_width: physical.width,
        physical_height: physical.height,
    }
}

fn linux_backend_diagnostic(
    settings: &WindowSettings,
    decoration: &DecorationResolution,
) -> Option<LinuxWindowBackendDiagnostic> {
    if !cfg!(target_os = "linux") {
        return None;
    }

    let backend_used = detected_platform();
    let backend_fallback = match (settings.linux_backend, backend_used) {
        (LinuxWindowBackend::X11, DesktopPlatform::LinuxWayland)
        | (LinuxWindowBackend::Wayland, DesktopPlatform::LinuxX11) => Some(PlatformFallback {
            feature: "linux_window_backend".to_owned(),
            requested: format!("{:?}", settings.linux_backend),
            effective: format!("{backend_used:?}"),
            reason: "winit selected the available backend from the current process environment"
                .to_owned(),
        }),
        _ => None,
    };
    let fallback = backend_fallback.or_else(|| decoration.fallback.clone());

    Some(LinuxWindowBackendDiagnostic {
        requested_backend: settings.linux_backend,
        backend_used,
        compositor: compositor_info(),
        decoration_requested: settings.decoration_mode,
        decoration_used: decoration.effective,
        fallback,
    })
}

fn detected_platform() -> DesktopPlatform {
    if cfg!(target_os = "windows") {
        DesktopPlatform::Windows
    } else if cfg!(target_os = "macos") {
        DesktopPlatform::MacOs
    } else if cfg!(target_os = "linux") {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            DesktopPlatform::LinuxWayland
        } else if std::env::var_os("DISPLAY").is_some() {
            DesktopPlatform::LinuxX11
        } else {
            DesktopPlatform::Unknown
        }
    } else {
        DesktopPlatform::Unknown
    }
}

fn dpi_behavior_for_platform() -> DpiBehavior {
    if cfg!(target_os = "windows") {
        DpiBehavior::PerMonitor
    } else if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
        DpiBehavior::FractionalScale
    } else {
        DpiBehavior::Unknown
    }
}

fn compositor_info() -> Option<CompositorInfo> {
    if !cfg!(target_os = "linux") {
        return None;
    }

    let name = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .ok();

    Some(CompositorInfo {
        name,
        version: None,
        protocol: detected_platform(),
        notes: Vec::new(),
    })
}

fn shell_environment_info() -> ShellEnvironmentInfo {
    ShellEnvironmentInfo {
        shell: std::env::var("SHELL")
            .or_else(|_| std::env::var("ComSpec"))
            .ok(),
        term: std::env::var("TERM").ok(),
        color_term: std::env::var("COLORTERM").ok(),
        current_working_directory: std::env::current_dir()
            .ok()
            .map(|directory| directory.display().to_string()),
    }
}

fn clipboard_diagnostic(
    operation: ClipboardOperation,
    availability: ClipboardAvailability,
    message: Option<String>,
) -> ClipboardDiagnostic {
    ClipboardDiagnostic {
        operation,
        availability,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_icon_rejects_invalid_rgba_and_accepts_exact_dimensions() {
        assert!(WindowIcon::from_rgba(vec![0; 15], 2, 2).is_err());
        assert!(WindowIcon::from_rgba(vec![0; 4], 0, 1).is_err());

        let icon = WindowIcon::from_rgba(vec![255; 16], 2, 2)
            .expect("four RGBA pixels should form a valid icon");
        assert!(icon.to_winit().is_some());
    }

    #[test]
    fn initial_window_activation_key_is_quarantined_until_release() {
        let enter = PhysicalKey::Code(KeyCode::Enter);
        let letter = PhysicalKey::Code(KeyCode::KeyA);
        let mut suppressed = HashSet::new();

        assert!(should_suppress_initial_activation_key(
            &mut suppressed,
            &enter,
            ElementState::Pressed,
            Duration::from_millis(100),
        ));
        assert!(should_suppress_initial_activation_key(
            &mut suppressed,
            &enter,
            ElementState::Pressed,
            Duration::from_secs(2),
        ));
        assert!(should_suppress_initial_activation_key(
            &mut suppressed,
            &enter,
            ElementState::Released,
            Duration::from_secs(2),
        ));
        assert!(suppressed.is_empty());

        assert!(!should_suppress_initial_activation_key(
            &mut suppressed,
            &letter,
            ElementState::Pressed,
            Duration::from_millis(100),
        ));
        assert!(!should_suppress_initial_activation_key(
            &mut suppressed,
            &enter,
            ElementState::Pressed,
            INITIAL_ACTIVATION_KEY_GUARD + Duration::from_millis(1),
        ));
    }

    #[test]
    fn initial_focus_handoff_is_rearmed_after_slow_startup() {
        let mut translator = InputTranslator::new();
        translator.activation_guard_started_at =
            Instant::now() - INITIAL_ACTIVATION_KEY_GUARD - Duration::from_secs(1);
        translator.initial_focus_seen = true;
        translator
            .pressed_keys
            .insert(PhysicalKey::Code(KeyCode::KeyA));

        translator.arm_initial_focus_handoff();

        assert!(!translator.initial_focus_seen);
        assert!(translator.pressed_keys.is_empty());
        assert!(translator.suppressed_activation_keys.is_empty());
        assert!(translator.activation_guard_started_at.elapsed() <= INITIAL_ACTIVATION_KEY_GUARD);
    }

    #[test]
    fn orphan_key_repeat_is_not_forwarded_as_terminal_input() {
        let key = PhysicalKey::Code(KeyCode::Enter);
        let mut pressed = HashSet::new();

        assert!(!should_forward_key_event(
            &mut pressed,
            &key,
            ElementState::Pressed,
            true,
        ));
        assert!(pressed.is_empty());

        assert!(should_forward_key_event(
            &mut pressed,
            &key,
            ElementState::Pressed,
            false,
        ));
        assert!(should_forward_key_event(
            &mut pressed,
            &key,
            ElementState::Pressed,
            true,
        ));
        assert!(should_forward_key_event(
            &mut pressed,
            &key,
            ElementState::Released,
            false,
        ));
        assert!(pressed.is_empty());
    }

    #[test]
    fn ime_commit_accepts_composed_text_but_not_command_controls() {
        assert_eq!(ime_commit_text("日本語").as_deref(), Some("日本語"));
        assert_eq!(ime_commit_text("e\u{301}").as_deref(), Some("e\u{301}"));
        assert_eq!(ime_commit_text("\r"), None);
        assert_eq!(ime_commit_text("\n"), None);
        assert_eq!(ime_commit_text(""), None);
    }

    #[test]
    fn disabled_power_monitor_does_not_schedule_provider_work() {
        let mut monitor = DesktopPowerMonitor::with_enabled(false);

        assert!(!monitor.refresh_if_due());
        assert_eq!(monitor.next_refresh_after(), None);
        assert_eq!(monitor.power_state().state, PowerState::UNKNOWN);
    }

    #[test]
    fn disabled_notification_provider_does_not_start_a_worker() {
        let mut provider = DesktopNotificationProvider::new(false);

        let diagnostic = provider
            .notify(NotificationRequest::new("Panea", "session exited"))
            .expect_err("disabled notifications must reject without queueing");
        assert_eq!(diagnostic.availability, NotificationAvailability::Disabled);
        assert!(provider.sender.is_none());
    }

    #[test]
    fn notification_backend_is_explicit_for_supported_desktops() {
        assert_eq!(
            notification_backend(),
            if cfg!(windows) {
                NotificationBackend::WindowsToast
            } else if cfg!(target_os = "macos") {
                NotificationBackend::MacOsNotificationCenter
            } else if cfg!(target_os = "linux") {
                NotificationBackend::Freedesktop
            } else {
                NotificationBackend::Unsupported
            }
        );
    }

    #[test]
    fn recovery_shortcuts_translate_to_window_actions() {
        let event = KeyEvent {
            physical_key: Some("KeyF".to_owned()),
            logical_key: "f".to_owned(),
            text: Some("f".to_owned()),
            state: KeyState::Pressed,
            modifiers: KeyModifiers {
                shift: true,
                ctrl: true,
                alt: false,
                super_key: false,
                alt_graph: false,
            },
            repeat: false,
        };

        assert_eq!(
            recovery_action(&event),
            Some(WindowAction::ToggleFullscreen)
        );
    }

    #[test]
    fn clipboard_unavailable_diagnostic_names_operation() {
        let diagnostic = clipboard_diagnostic(
            ClipboardOperation::Paste,
            ClipboardAvailability::Unavailable,
            Some("missing backend".to_owned()),
        );

        assert_eq!(diagnostic.operation, ClipboardOperation::Paste);
        assert_eq!(diagnostic.availability, ClipboardAvailability::Unavailable);
    }

    #[test]
    fn url_opener_rejects_non_web_schemes_before_platform_launch() {
        let mut opener = DesktopUrlOpener::new();
        let diagnostic = opener
            .open_url("file:///tmp/not-allowed")
            .expect_err("file URL must be rejected");
        assert!(diagnostic.message.unwrap().contains("http"));
    }

    #[test]
    fn primary_selection_capability_is_linux_only() {
        assert_eq!(
            clipboard_capabilities()
                .contains(&platform_core::ClipboardCapability::PrimarySelection),
            cfg!(target_os = "linux")
        );
    }

    #[test]
    fn borderless_fullscreen_suppresses_native_decorations_and_windowed_restores_them() {
        assert!(!window_mode_decorations_visible(
            WindowMode::BorderlessFullscreen,
            DecorationMode::Native,
        ));
        assert!(!window_mode_decorations_visible(
            WindowMode::FramelessFullscreen,
            DecorationMode::Native,
        ));
        assert!(window_mode_decorations_visible(
            WindowMode::Windowed,
            DecorationMode::Native,
        ));
    }

    #[test]
    fn fullscreen_modes_are_created_fullscreen_before_the_window_is_revealed() {
        assert!(initial_winit_fullscreen(WindowMode::Windowed).is_none());
        for mode in [
            WindowMode::Fullscreen,
            WindowMode::BorderlessFullscreen,
            WindowMode::FramelessFullscreen,
        ] {
            assert!(
                matches!(
                    initial_winit_fullscreen(mode),
                    Some(Fullscreen::Borderless(None))
                ),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn only_transparent_windows_disable_the_dwm_redirection_bitmap() {
        assert!(no_redirection_bitmap_required(
            DesktopPlatform::Windows,
            0.92,
        ));
        assert!(!no_redirection_bitmap_required(
            DesktopPlatform::Windows,
            1.0,
        ));
        assert!(!no_redirection_bitmap_required(
            DesktopPlatform::MacOs,
            0.92,
        ));
        assert!(!no_redirection_bitmap_required(
            DesktopPlatform::LinuxX11,
            0.92,
        ));
        assert!(!no_redirection_bitmap_required(
            DesktopPlatform::LinuxWayland,
            0.92,
        ));
    }

    #[test]
    fn unsupported_custom_decorations_report_an_explicit_fallback() {
        let resolution = resolve_decoration_mode(DecorationMode::Custom, DesktopPlatform::Windows);

        assert_eq!(resolution.effective, DecorationMode::Native);
        let fallback = resolution
            .fallback
            .expect("custom mode must report fallback");
        assert_eq!(fallback.feature, "window_decorations");
        assert!(fallback.reason.contains("cannot guarantee"));
    }

    #[test]
    fn wayland_client_side_request_reports_negotiation() {
        let resolution =
            resolve_decoration_mode(DecorationMode::ClientSide, DesktopPlatform::LinuxWayland);

        assert_eq!(resolution.effective, DecorationMode::Native);
        assert!(
            resolution
                .fallback
                .expect("Wayland negotiation must be visible")
                .reason
                .contains("compositor")
        );
    }

    #[test]
    fn window_chrome_action_mapping_is_total_and_close_stays_app_owned() {
        assert_eq!(
            window_chrome_operation(WindowChromeAction::BeginDrag),
            WinitWindowChromeOperation::DragWindow
        );
        assert_eq!(
            window_chrome_operation(WindowChromeAction::Minimize),
            WinitWindowChromeOperation::Minimize
        );
        assert_eq!(
            window_chrome_operation(WindowChromeAction::LeaveFullscreen),
            WinitWindowChromeOperation::LeaveFullscreen
        );
        assert_eq!(
            window_chrome_operation(WindowChromeAction::Close),
            WinitWindowChromeOperation::RequestDesktopClose
        );
    }

    #[test]
    fn window_chrome_action_capabilities_cover_every_desktop_backend() {
        let all_actions = vec![
            WindowChromeAction::BeginDrag,
            WindowChromeAction::Minimize,
            WindowChromeAction::LeaveFullscreen,
            WindowChromeAction::Close,
        ];
        for platform in [
            DesktopPlatform::Windows,
            DesktopPlatform::MacOs,
            DesktopPlatform::LinuxX11,
            DesktopPlatform::LinuxWayland,
        ] {
            assert_eq!(window_chrome_actions_for(platform), all_actions);
        }
        assert_eq!(
            window_chrome_actions_for(DesktopPlatform::Unknown),
            vec![WindowChromeAction::Close]
        );
    }

    #[test]
    fn rejected_window_chrome_action_names_the_backend_failure() {
        let fallback = window_chrome_action_fallback(
            WindowChromeAction::BeginDrag,
            "the compositor rejected interactive movement",
        );

        assert_eq!(fallback.feature, "window_chrome_action");
        assert_eq!(fallback.requested, "begin_drag");
        assert_eq!(fallback.effective, "unchanged");
        assert!(fallback.reason.contains("compositor"));
    }
}
