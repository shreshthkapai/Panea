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
    Fullscreen,
    BorderlessFullscreen,
    Frameless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationMode {
    ServerSide,
    ClientSide,
    None,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Pressed(MouseButton),
    Released(MouseButton),
    Moved,
    Wheel { delta_x: i32, delta_y: i32 },
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

#[cfg(test)]
mod tests {
    #[test]
    fn platform_core_has_no_crate_dependencies() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest.contains("[dependencies]"),
            "platform-core must report capability contracts without depending on renderers or transports"
        );
    }
}
