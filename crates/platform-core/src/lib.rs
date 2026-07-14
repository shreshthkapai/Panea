//! Platform capability and event contracts.

pub const LAYER: &str = "platform parity";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopPlatform {
    MacOs,
    LinuxX11,
    LinuxWayland,
    Windows,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    Windowed,
    Maximized,
    Fullscreen,
    BorderlessFullscreen,
    FramelessWindowed,
    FramelessFullscreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationMode {
    Auto,
    Native,
    ServerSide,
    ClientSide,
    Custom,
    None,
    FallbackDecorated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxWindowBackend {
    Auto,
    X11,
    Wayland,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    ToggleFullscreen,
    RestoreWindowDecorations,
    ToggleFrameless,
    CloseWindow,
    OpenCommandPaletteLater,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardCapability {
    System,
    PrimarySelection,
    Osc52,
}

pub type ClipboardMode = ClipboardCapability;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuBackend {
    Vulkan,
    Metal,
    Dx12,
    Gl,
    SoftwareFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImeSupport {
    Unsupported,
    Basic,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpiBehavior {
    IntegerScale,
    FractionalScale,
    PerMonitor,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DpiInfo {
    pub scale_factor: f64,
    pub logical_width: u32,
    pub logical_height: u32,
    pub physical_width: u32,
    pub physical_height: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub name: Option<String>,
    pub position_x: i32,
    pub position_y: i32,
    pub width: u32,
    pub height: u32,
    pub refresh_millihertz: Option<u32>,
    pub dpi: DpiInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
    pub alt_graph: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub physical_key: Option<String>,
    pub logical_key: String,
    pub text: Option<String>,
    pub state: KeyState,
    pub modifiers: KeyModifiers,
    pub repeat: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
    Other(u16),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEventKind {
    Pressed(MouseButton),
    Released(MouseButton),
    Moved,
    Wheel { delta_x: f64, delta_y: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    pub x: f64,
    pub y: f64,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImeEvent {
    Enabled,
    Disabled,
    Preedit { text: String },
    Commit { text: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Ime(ImeEvent),
    Focused(bool),
    Resized { width: u32, height: u32 },
    ScaleFactorChanged { scale_factor: f64 },
    CloseRequested,
    WindowAction(WindowAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositorInfo {
    pub name: Option<String>,
    pub version: Option<String>,
    pub protocol: DesktopPlatform,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellEnvironmentInfo {
    pub shell: Option<String>,
    pub term: Option<String>,
    pub color_term: Option<String>,
    pub current_working_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformFallback {
    pub feature: String,
    pub requested: String,
    pub effective: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardOperation {
    Copy,
    Paste,
    Osc52Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardAvailability {
    Available,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardDiagnostic {
    pub operation: ClipboardOperation,
    pub availability: ClipboardAvailability,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowModeDiagnostic {
    pub requested: WindowMode,
    pub effective: WindowMode,
    pub fallback: Option<PlatformFallback>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecorationModeDiagnostic {
    pub requested: DecorationMode,
    pub effective: DecorationMode,
    pub fallback: Option<PlatformFallback>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxWindowBackendDiagnostic {
    pub requested_backend: LinuxWindowBackend,
    pub backend_used: DesktopPlatform,
    pub compositor: Option<CompositorInfo>,
    pub decoration_requested: DecorationMode,
    pub decoration_used: DecorationMode,
    pub fallback: Option<PlatformFallback>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlatformCapabilities {
    pub platform: DesktopPlatform,
    pub window_modes_supported: Vec<WindowMode>,
    pub decoration_modes_supported: Vec<DecorationMode>,
    pub clipboard_capabilities: Vec<ClipboardCapability>,
    pub gpu_backends_available: Vec<GpuBackend>,
    pub ime_supported: ImeSupport,
    pub dpi_behavior: DpiBehavior,
    pub monitors: Vec<MonitorInfo>,
    pub compositor_info: Option<CompositorInfo>,
    pub shell_environment_info: ShellEnvironmentInfo,
    pub fallbacks: Vec<PlatformFallback>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformError {
    pub feature: String,
    pub message: String,
}

impl PlatformError {
    #[must_use]
    pub fn new(feature: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            feature: feature.into(),
            message: message.into(),
        }
    }
}

/// Clipboard contract exposed to app code without leaking a concrete OS backend.
pub trait ClipboardProvider {
    fn copy_text(&mut self, text: &str) -> Result<(), ClipboardDiagnostic>;

    fn paste_text(&mut self) -> Result<String, ClipboardDiagnostic>;

    fn last_diagnostic(&self) -> ClipboardDiagnostic;

    fn copy_primary_text(&mut self, _text: &str) -> Result<(), ClipboardDiagnostic> {
        Err(ClipboardDiagnostic {
            operation: ClipboardOperation::Copy,
            availability: ClipboardAvailability::Unavailable,
            message: Some("primary selection is unsupported by this platform provider".to_owned()),
        })
    }

    fn paste_primary_text(&mut self) -> Result<String, ClipboardDiagnostic> {
        Err(ClipboardDiagnostic {
            operation: ClipboardOperation::Paste,
            availability: ClipboardAvailability::Unavailable,
            message: Some("primary selection is unsupported by this platform provider".to_owned()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlOpenDiagnostic {
    pub url: String,
    pub message: Option<String>,
}

/// Opens validated URLs without exposing OS-specific launch mechanics to the app runtime.
pub trait UrlOpener {
    fn open_url(&mut self, url: &str) -> Result<(), UrlOpenDiagnostic>;
}

/// Window contract exposed to the application without exposing winit or OS APIs.
pub trait WindowProvider {
    fn capabilities(&self) -> PlatformCapabilities;

    fn set_window_mode(&mut self, mode: WindowMode) -> Result<WindowModeDiagnostic, PlatformError>;

    fn poll_input_events(&mut self) -> Vec<InputEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_core_has_no_crate_dependencies() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest.contains("[dependencies]"),
            "platform-core must report capability contracts without depending on renderers or transports"
        );
    }

    #[derive(Debug)]
    struct FakeClipboard {
        value: String,
        diagnostic: ClipboardDiagnostic,
    }

    impl Default for FakeClipboard {
        fn default() -> Self {
            Self {
                value: String::new(),
                diagnostic: ClipboardDiagnostic {
                    operation: ClipboardOperation::Copy,
                    availability: ClipboardAvailability::Available,
                    message: None,
                },
            }
        }
    }

    impl ClipboardProvider for FakeClipboard {
        fn copy_text(&mut self, text: &str) -> Result<(), ClipboardDiagnostic> {
            self.value = text.to_owned();
            self.diagnostic = ClipboardDiagnostic {
                operation: ClipboardOperation::Copy,
                availability: ClipboardAvailability::Available,
                message: None,
            };
            Ok(())
        }

        fn paste_text(&mut self) -> Result<String, ClipboardDiagnostic> {
            self.diagnostic = ClipboardDiagnostic {
                operation: ClipboardOperation::Paste,
                availability: ClipboardAvailability::Available,
                message: None,
            };
            Ok(self.value.clone())
        }

        fn last_diagnostic(&self) -> ClipboardDiagnostic {
            self.diagnostic.clone()
        }
    }

    #[test]
    fn clipboard_provider_contract_is_backend_neutral() {
        let mut clipboard = FakeClipboard::default();

        clipboard
            .copy_text("panea")
            .expect("fake clipboard copy should work");

        assert_eq!(clipboard.paste_text().as_deref(), Ok("panea"));
        assert_eq!(
            clipboard.last_diagnostic().availability,
            ClipboardAvailability::Available
        );
        assert_eq!(
            clipboard
                .paste_primary_text()
                .expect_err("fake provider has no primary selection")
                .availability,
            ClipboardAvailability::Unavailable
        );
    }
}
