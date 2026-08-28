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
pub enum WindowChromeAction {
    BeginDrag,
    Minimize,
    LeaveFullscreen,
    Close,
}

impl WindowChromeAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeginDrag => "begin_drag",
            Self::Minimize => "minimize",
            Self::LeaveFullscreen => "leave_fullscreen",
            Self::Close => "close",
        }
    }
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
pub enum PowerSource {
    Ac,
    Battery,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerState {
    pub source: PowerSource,
    pub battery_count: usize,
    pub charge_percent: Option<u8>,
}

impl PowerState {
    pub const UNKNOWN: Self = Self {
        source: PowerSource::Unknown,
        battery_count: 0,
        charge_percent: None,
    };

    #[must_use]
    pub const fn is_on_battery(self) -> bool {
        matches!(self.source, PowerSource::Battery)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerStateDiagnostic {
    pub state: PowerState,
    pub message: Option<String>,
}

/// Power-state contract. Providers are sampled outside render, input, and PTY hot paths.
pub trait PowerStateProvider {
    fn power_state(&mut self) -> PowerStateDiagnostic;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationBackend {
    WindowsToast,
    MacOsNotificationCenter,
    Freedesktop,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationAvailability {
    Available,
    Disabled,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotificationUrgency {
    Low,
    #[default]
    Normal,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationRequest {
    pub title: String,
    pub body: String,
    pub urgency: NotificationUrgency,
}

impl NotificationRequest {
    pub const MAX_TITLE_CHARS: usize = 256;
    pub const MAX_BODY_CHARS: usize = 4096;

    #[must_use]
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            urgency: NotificationUrgency::Normal,
        }
    }

    #[must_use]
    pub const fn with_urgency(mut self, urgency: NotificationUrgency) -> Self {
        self.urgency = urgency;
        self
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.title.trim().is_empty() {
            return Err("notification title cannot be empty");
        }
        if self.title.chars().any(char::is_control) {
            return Err("notification title cannot contain control characters");
        }
        if self
            .body
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
        {
            return Err("notification body contains an unsupported control character");
        }
        if self.title.chars().count() > Self::MAX_TITLE_CHARS {
            return Err("notification title exceeds the configured safety bound");
        }
        if self.body.chars().count() > Self::MAX_BODY_CHARS {
            return Err("notification body exceeds the configured safety bound");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationDiagnostic {
    pub backend: NotificationBackend,
    pub availability: NotificationAvailability,
    pub message: String,
}

/// Enqueues native notifications without exposing OS APIs to application code.
/// Implementations must not block the render, input, or terminal-I/O paths.
pub trait NotificationProvider {
    fn notify(&mut self, request: NotificationRequest) -> Result<(), NotificationDiagnostic>;

    fn diagnostic(&self) -> NotificationDiagnostic;
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
    pub logical_key_without_modifiers: String,
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
    Wheel(MouseScrollDelta),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseScrollDelta {
    Lines { x: f64, y: f64 },
    Pixels { x: f64, y: f64 },
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
    Preedit {
        text: String,
        cursor: Option<(usize, usize)>,
    },
    Commit {
        text: String,
    },
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
pub struct WindowChromeActionDiagnostic {
    pub action: WindowChromeAction,
    pub applied: bool,
    pub fallback: Option<PlatformFallback>,
}

/// Executes window-chrome intent without exposing a concrete window backend.
pub trait WindowChromeActionExecutor {
    fn try_apply_window_chrome_action(
        &mut self,
        action: WindowChromeAction,
    ) -> Result<(), PlatformFallback>;
}

#[must_use]
pub fn execute_window_chrome_action(
    executor: &mut impl WindowChromeActionExecutor,
    action: WindowChromeAction,
) -> WindowChromeActionDiagnostic {
    match executor.try_apply_window_chrome_action(action) {
        Ok(()) => WindowChromeActionDiagnostic {
            action,
            applied: true,
            fallback: None,
        },
        Err(fallback) => WindowChromeActionDiagnostic {
            action,
            applied: false,
            fallback: Some(fallback),
        },
    }
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
    pub window_chrome_actions_supported: Vec<WindowChromeAction>,
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

/// A Windows virtual key with the scan code and enhanced-key flag that go with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Win32VirtualKey {
    pub virtual_key: u16,
    pub scan_code: u16,
    /// True for keys the keyboard reports with an `0xE0` prefix.
    pub enhanced: bool,
}

/// Maps a physical key name to its Windows virtual key and PC set-1 scan code.
///
/// Names come from winit's `KeyCode`, either bare (`"KeyA"`) or in the debug
/// form the platform layer produces (`"Code(KeyA)"`). Returns `None` for keys
/// with no Windows equivalent, including `Unidentified`, where the caller falls
/// back to a character-only record.
#[must_use]
pub fn win32_virtual_key(physical_key: &str) -> Option<Win32VirtualKey> {
    let name = physical_key
        .strip_prefix("Code(")
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(physical_key);

    let (virtual_key, scan_code, enhanced) = match name {
        // Letters: VK is the ASCII uppercase code.
        "KeyA" => (0x41, 0x1e, false),
        "KeyB" => (0x42, 0x30, false),
        "KeyC" => (0x43, 0x2e, false),
        "KeyD" => (0x44, 0x20, false),
        "KeyE" => (0x45, 0x12, false),
        "KeyF" => (0x46, 0x21, false),
        "KeyG" => (0x47, 0x22, false),
        "KeyH" => (0x48, 0x23, false),
        "KeyI" => (0x49, 0x17, false),
        "KeyJ" => (0x4a, 0x24, false),
        "KeyK" => (0x4b, 0x25, false),
        "KeyL" => (0x4c, 0x26, false),
        "KeyM" => (0x4d, 0x32, false),
        "KeyN" => (0x4e, 0x31, false),
        "KeyO" => (0x4f, 0x18, false),
        "KeyP" => (0x50, 0x19, false),
        "KeyQ" => (0x51, 0x10, false),
        "KeyR" => (0x52, 0x13, false),
        "KeyS" => (0x53, 0x1f, false),
        "KeyT" => (0x54, 0x14, false),
        "KeyU" => (0x55, 0x16, false),
        "KeyV" => (0x56, 0x2f, false),
        "KeyW" => (0x57, 0x11, false),
        "KeyX" => (0x58, 0x2d, false),
        "KeyY" => (0x59, 0x15, false),
        "KeyZ" => (0x5a, 0x2c, false),
        // Digit row: VK is the ASCII digit. Shifted symbols share these keys,
        // which is why the character has to travel separately in the record.
        "Digit1" => (0x31, 0x02, false),
        "Digit2" => (0x32, 0x03, false),
        "Digit3" => (0x33, 0x04, false),
        "Digit4" => (0x34, 0x05, false),
        "Digit5" => (0x35, 0x06, false),
        "Digit6" => (0x36, 0x07, false),
        "Digit7" => (0x37, 0x08, false),
        "Digit8" => (0x38, 0x09, false),
        "Digit9" => (0x39, 0x0a, false),
        "Digit0" => (0x30, 0x0b, false),
        // OEM punctuation.
        "Minus" => (0xbd, 0x0c, false),
        "Equal" => (0xbb, 0x0d, false),
        "BracketLeft" => (0xdb, 0x1a, false),
        "BracketRight" => (0xdd, 0x1b, false),
        "Backslash" => (0xdc, 0x2b, false),
        "Semicolon" => (0xba, 0x27, false),
        "Quote" => (0xde, 0x28, false),
        "Backquote" => (0xc0, 0x29, false),
        "Comma" => (0xbc, 0x33, false),
        "Period" => (0xbe, 0x34, false),
        "Slash" => (0xbf, 0x35, false),
        "IntlBackslash" => (0xe2, 0x56, false),
        // Editing and whitespace.
        "Enter" => (0x0d, 0x1c, false),
        "Tab" => (0x09, 0x0f, false),
        "Space" => (0x20, 0x39, false),
        "Backspace" => (0x08, 0x0e, false),
        "Escape" => (0x1b, 0x01, false),
        // Navigation cluster: enhanced keys.
        "Insert" => (0x2d, 0x52, true),
        "Delete" => (0x2e, 0x53, true),
        "Home" => (0x24, 0x47, true),
        "End" => (0x23, 0x4f, true),
        "PageUp" => (0x21, 0x49, true),
        "PageDown" => (0x22, 0x51, true),
        "ArrowUp" => (0x26, 0x48, true),
        "ArrowDown" => (0x28, 0x50, true),
        "ArrowLeft" => (0x25, 0x4b, true),
        "ArrowRight" => (0x27, 0x4d, true),
        // Modifiers: the generic VK, with the side carried by the scan code and
        // the enhanced flag, matching what Windows reports.
        "ShiftLeft" => (0x10, 0x2a, false),
        "ShiftRight" => (0x10, 0x36, false),
        "ControlLeft" => (0x11, 0x1d, false),
        "ControlRight" => (0x11, 0x1d, true),
        "AltLeft" => (0x12, 0x38, false),
        "AltRight" => (0x12, 0x38, true),
        "SuperLeft" => (0x5b, 0x5b, true),
        "SuperRight" => (0x5c, 0x5c, true),
        "CapsLock" => (0x14, 0x3a, false),
        "NumLock" => (0x90, 0x45, true),
        "ScrollLock" => (0x91, 0x46, false),
        "ContextMenu" => (0x5d, 0x5d, true),
        "PrintScreen" => (0x2c, 0x37, true),
        "Pause" => (0x13, 0x45, false),
        // Function keys.
        "F1" => (0x70, 0x3b, false),
        "F2" => (0x71, 0x3c, false),
        "F3" => (0x72, 0x3d, false),
        "F4" => (0x73, 0x3e, false),
        "F5" => (0x74, 0x3f, false),
        "F6" => (0x75, 0x40, false),
        "F7" => (0x76, 0x41, false),
        "F8" => (0x77, 0x42, false),
        "F9" => (0x78, 0x43, false),
        "F10" => (0x79, 0x44, false),
        "F11" => (0x7a, 0x57, false),
        "F12" => (0x7b, 0x58, false),
        "F13" => (0x7c, 0x64, false),
        "F14" => (0x7d, 0x65, false),
        "F15" => (0x7e, 0x66, false),
        "F16" => (0x7f, 0x67, false),
        "F17" => (0x80, 0x68, false),
        "F18" => (0x81, 0x69, false),
        "F19" => (0x82, 0x6a, false),
        "F20" => (0x83, 0x6b, false),
        "F21" => (0x84, 0x6c, false),
        "F22" => (0x85, 0x6d, false),
        "F23" => (0x86, 0x6e, false),
        "F24" => (0x87, 0x76, false),
        // Numpad.
        "Numpad0" => (0x60, 0x52, false),
        "Numpad1" => (0x61, 0x4f, false),
        "Numpad2" => (0x62, 0x50, false),
        "Numpad3" => (0x63, 0x51, false),
        "Numpad4" => (0x64, 0x4b, false),
        "Numpad5" => (0x65, 0x4c, false),
        "Numpad6" => (0x66, 0x4d, false),
        "Numpad7" => (0x67, 0x47, false),
        "Numpad8" => (0x68, 0x48, false),
        "Numpad9" => (0x69, 0x49, false),
        "NumpadMultiply" => (0x6a, 0x37, false),
        "NumpadAdd" => (0x6b, 0x4e, false),
        "NumpadSubtract" => (0x6d, 0x4a, false),
        "NumpadDecimal" => (0x6e, 0x53, false),
        "NumpadDivide" => (0x6f, 0x35, true),
        "NumpadEnter" => (0x0d, 0x1c, true),
        _ => return None,
    };

    Some(Win32VirtualKey {
        virtual_key,
        scan_code,
        enhanced,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn win32_virtual_keys_cover_letters_digits_symbols_navigation_and_modifiers() {
        use super::{Win32VirtualKey, win32_virtual_key};
        let key = |name: &str| win32_virtual_key(name).unwrap_or_else(|| panic!("{name} must map"));

        // Accepts both the bare KeyCode name and the `Code(..)` debug form.
        assert_eq!(
            key("KeyA"),
            Win32VirtualKey {
                virtual_key: 0x41,
                scan_code: 0x1e,
                enhanced: false
            }
        );
        assert_eq!(key("Code(KeyA)"), key("KeyA"));
        assert_eq!(
            key("Digit7"),
            Win32VirtualKey {
                virtual_key: 0x37,
                scan_code: 0x08,
                enhanced: false
            }
        );
        assert_eq!(
            key("Digit0"),
            Win32VirtualKey {
                virtual_key: 0x30,
                scan_code: 0x0b,
                enhanced: false
            }
        );
        // OEM symbol keys carry their VK_OEM codes.
        assert_eq!(key("Minus").virtual_key, 0xbd);
        assert_eq!(key("Equal").virtual_key, 0xbb);
        assert_eq!(key("Semicolon").virtual_key, 0xba);
        assert_eq!(key("Quote").virtual_key, 0xde);
        assert_eq!(key("Backquote").virtual_key, 0xc0);
        assert_eq!(key("Backslash").virtual_key, 0xdc);
        // Navigation keys are enhanced (0xE0-prefixed) keys.
        assert_eq!(
            key("ArrowUp"),
            Win32VirtualKey {
                virtual_key: 0x26,
                scan_code: 0x48,
                enhanced: true
            }
        );
        assert!(key("Delete").enhanced && key("Home").enhanced && key("PageDown").enhanced);
        assert_eq!(
            key("Enter"),
            Win32VirtualKey {
                virtual_key: 0x0d,
                scan_code: 0x1c,
                enhanced: false
            }
        );
        assert!(key("NumpadEnter").enhanced);
        // Modifiers use the generic VK with left/right distinguished by scan code.
        assert_eq!(
            key("ShiftLeft"),
            Win32VirtualKey {
                virtual_key: 0x10,
                scan_code: 0x2a,
                enhanced: false
            }
        );
        assert_eq!(key("ShiftRight").scan_code, 0x36);
        assert!(key("ControlRight").enhanced && key("AltRight").enhanced);
        assert_eq!(key("F1").virtual_key, 0x70);
        assert_eq!(
            key("F12"),
            Win32VirtualKey {
                virtual_key: 0x7b,
                scan_code: 0x58,
                enhanced: false
            }
        );
        assert!(win32_virtual_key("Unidentified(0)").is_none());
    }

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

    #[test]
    fn power_state_contract_distinguishes_unknown_from_battery() {
        assert!(!PowerState::UNKNOWN.is_on_battery());
        assert!(
            PowerState {
                source: PowerSource::Battery,
                battery_count: 1,
                charge_percent: Some(42),
            }
            .is_on_battery()
        );
    }

    #[test]
    fn notification_contract_rejects_unbounded_or_empty_requests() {
        assert!(
            NotificationRequest::new("Panea", "session exited")
                .validate()
                .is_ok()
        );
        assert_eq!(
            NotificationRequest::new(" ", "body").validate(),
            Err("notification title cannot be empty")
        );
        assert!(
            NotificationRequest::new("Panea", "x".repeat(NotificationRequest::MAX_BODY_CHARS + 1))
                .validate()
                .is_err()
        );
        assert_eq!(
            NotificationRequest::new("Panea\nspoofed", "body").validate(),
            Err("notification title cannot contain control characters")
        );
        assert_eq!(
            NotificationRequest::new("Panea", "body\0spoofed").validate(),
            Err("notification body contains an unsupported control character")
        );
    }

    #[derive(Debug)]
    struct FakeWindowChromeExecutor {
        rejected: Option<WindowChromeAction>,
    }

    impl WindowChromeActionExecutor for FakeWindowChromeExecutor {
        fn try_apply_window_chrome_action(
            &mut self,
            action: WindowChromeAction,
        ) -> Result<(), PlatformFallback> {
            if self.rejected == Some(action) {
                Err(PlatformFallback {
                    feature: "window_chrome_action".to_owned(),
                    requested: action.as_str().to_owned(),
                    effective: "unchanged".to_owned(),
                    reason: "fake backend rejected the action".to_owned(),
                })
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn window_chrome_action_contract_never_silently_reports_failure() {
        let actions = [
            WindowChromeAction::BeginDrag,
            WindowChromeAction::Minimize,
            WindowChromeAction::LeaveFullscreen,
            WindowChromeAction::Close,
        ];

        for rejected in actions {
            let mut executor = FakeWindowChromeExecutor {
                rejected: Some(rejected),
            };
            for action in actions {
                let diagnostic = execute_window_chrome_action(&mut executor, action);
                assert_eq!(diagnostic.action, action);
                assert_ne!(
                    diagnostic.applied,
                    diagnostic.fallback.is_some(),
                    "{action:?} must be applied or carry an explicit fallback"
                );
                if action == rejected {
                    assert!(!diagnostic.applied);
                    assert_eq!(
                        diagnostic
                            .fallback
                            .as_ref()
                            .map(|value| value.requested.as_str()),
                        Some(action.as_str())
                    );
                } else {
                    assert!(diagnostic.applied);
                }
            }
        }
    }
}
