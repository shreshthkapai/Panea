//! Desktop window and event integration boundary.

pub const LAYER: &str = "platform parity";

use std::sync::Arc;

use arboard::Clipboard;
use platform_core::{
    ClipboardAvailability, ClipboardDiagnostic, ClipboardOperation, ClipboardProvider,
    CompositorInfo, DecorationMode, DesktopPlatform, DpiBehavior, DpiInfo, ImeEvent, ImeSupport,
    InputEvent, KeyEvent, KeyModifiers, KeyState, LinuxWindowBackend, LinuxWindowBackendDiagnostic,
    MonitorInfo, MouseButton, MouseEvent, MouseEventKind, PlatformCapabilities, PlatformFallback,
    ShellEnvironmentInfo, UrlOpenDiagnostic, UrlOpener, WindowAction, WindowMode,
    WindowModeDiagnostic,
};
use winit::{
    dpi::LogicalSize,
    event::{ElementState, Ime, MouseScrollDelta, WindowEvent},
    event_loop::EventLoopWindowTarget,
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Fullscreen, Window, WindowBuilder},
};

#[derive(Debug, Clone, PartialEq)]
pub struct WindowSettings {
    pub title: String,
    pub initial_width: u32,
    pub initial_height: u32,
    pub mode: WindowMode,
    pub linux_backend: LinuxWindowBackend,
    pub decoration_mode: DecorationMode,
    pub opacity: f64,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            title: "Panea".to_owned(),
            initial_width: 960,
            initial_height: 560,
            mode: WindowMode::Windowed,
            linux_backend: LinuxWindowBackend::Auto,
            decoration_mode: DecorationMode::Auto,
            opacity: 1.0,
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
        event_loop: &EventLoopWindowTarget<()>,
        settings: &WindowSettings,
    ) -> Result<Self, winit::error::OsError> {
        let decorations = !matches!(
            settings.mode,
            WindowMode::FramelessWindowed | WindowMode::FramelessFullscreen
        ) && !matches!(settings.decoration_mode, DecorationMode::None);

        let window = WindowBuilder::new()
            .with_title(settings.title.clone())
            .with_inner_size(LogicalSize::new(
                settings.initial_width,
                settings.initial_height,
            ))
            .with_decorations(decorations)
            .with_transparent(settings.opacity < 1.0)
            .with_maximized(matches!(settings.mode, WindowMode::Maximized))
            .build(event_loop)?;

        let window = Arc::new(window);
        let window_mode = apply_window_mode(&window, settings.mode);
        let linux = linux_backend_diagnostic(settings);
        let monitors = monitor_infos(event_loop);
        let dpi = window_dpi_info(&window);

        Ok(Self {
            window,
            diagnostics: DesktopWindowDiagnostics {
                dpi,
                monitors,
                window_mode,
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

#[derive(Debug, Clone, PartialEq)]
pub struct DesktopWindowDiagnostics {
    pub dpi: DpiInfo,
    pub monitors: Vec<MonitorInfo>,
    pub window_mode: WindowModeDiagnostic,
    pub linux: Option<LinuxWindowBackendDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct InputTranslator {
    modifiers: KeyModifiers,
    cursor_position: (f64, f64),
}

impl InputTranslator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            modifiers: KeyModifiers::default(),
            cursor_position: (0.0, 0.0),
        }
    }

    #[must_use]
    pub fn modifiers(&self) -> KeyModifiers {
        self.modifiers
    }

    pub fn translate_window_event(&mut self, event: &WindowEvent) -> Vec<InputEvent> {
        match event {
            WindowEvent::CloseRequested => vec![InputEvent::CloseRequested],
            WindowEvent::Focused(focused) => vec![InputEvent::Focused(*focused)],
            WindowEvent::Resized(size) => vec![InputEvent::Resized {
                width: size.width,
                height: size.height,
            }],
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers_from_winit(modifiers.state());
                Vec::new()
            }
            WindowEvent::KeyboardInput { event, .. } => {
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
                Ime::Commit(text) => vec![InputEvent::Ime(ImeEvent::Commit { text: text.clone() })],
                Ime::Disabled => vec![InputEvent::Ime(ImeEvent::Disabled)],
            },
            _ => Vec::new(),
        }
    }
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
    let mut effective = requested;
    let mut fallback = None;

    match requested {
        WindowMode::Windowed => {
            window.set_decorations(true);
            window.set_fullscreen(None);
            window.set_maximized(false);
        }
        WindowMode::Maximized => {
            window.set_decorations(true);
            window.set_fullscreen(None);
            window.set_maximized(true);
        }
        WindowMode::Fullscreen => {
            window.set_decorations(true);
            window.set_fullscreen(Some(Fullscreen::Borderless(window.current_monitor())));
            effective = WindowMode::BorderlessFullscreen;
            fallback = Some(PlatformFallback {
                feature: "window_mode".to_owned(),
                requested: "fullscreen".to_owned(),
                effective: "borderless_fullscreen".to_owned(),
                reason: "exclusive fullscreen requires backend-specific video-mode selection"
                    .to_owned(),
            });
        }
        WindowMode::BorderlessFullscreen => {
            window.set_decorations(true);
            window.set_fullscreen(Some(Fullscreen::Borderless(window.current_monitor())));
        }
        WindowMode::FramelessWindowed => {
            window.set_fullscreen(None);
            window.set_decorations(false);
        }
        WindowMode::FramelessFullscreen => {
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

#[must_use]
pub fn platform_capabilities(
    event_loop: &EventLoopWindowTarget<()>,
    _window: &Window,
) -> PlatformCapabilities {
    PlatformCapabilities {
        platform: detected_platform(),
        window_modes_supported: vec![
            WindowMode::Windowed,
            WindowMode::Maximized,
            WindowMode::BorderlessFullscreen,
            WindowMode::FramelessWindowed,
            WindowMode::FramelessFullscreen,
        ],
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
        monitors: monitor_infos(event_loop),
        compositor_info: compositor_info(),
        shell_environment_info: shell_environment_info(),
        fallbacks: Vec::new(),
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

fn monitor_infos(event_loop: &EventLoopWindowTarget<()>) -> Vec<MonitorInfo> {
    event_loop.available_monitors().map(monitor_info).collect()
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

fn linux_backend_diagnostic(settings: &WindowSettings) -> Option<LinuxWindowBackendDiagnostic> {
    if !cfg!(target_os = "linux") {
        return None;
    }

    let backend_used = detected_platform();
    let decoration_used = match settings.decoration_mode {
        DecorationMode::Auto | DecorationMode::Native => DecorationMode::Native,
        other => other,
    };
    let fallback = match (settings.linux_backend, backend_used) {
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

    Some(LinuxWindowBackendDiagnostic {
        requested_backend: settings.linux_backend,
        backend_used,
        compositor: compositor_info(),
        decoration_requested: settings.decoration_mode,
        decoration_used,
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
}
