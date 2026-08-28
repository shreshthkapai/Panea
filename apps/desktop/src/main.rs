use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    error::Error,
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use config_core::{
    AppConfig, ClipboardConfig, CommandBlockStyle, ConfigDiagnostic, ConfigDiagnosticSeverity,
    ConfigPlatform, DecorationStrategyConfig, FullscreenChromeAnimation, InputOutputGroupingStyle,
    LinuxBackendConfig, LogLevel, MuxLayoutConfig, MuxSplitAxisConfig, MuxTransportConfig,
    NotificationConfig, PasteConfig, PerformanceConfig, PerformanceOverlayDetail,
    PerformanceOverlayPosition, PerformanceProfile, PresentModePreference, PromptDecorationStyle,
    ReloadPlan, ReloadableSection, ShellIntegrationActivationConfig, ShellProfile,
    ShellProfileKind, SshAuthMethod, SshKnownHostsPolicy, SshProfile, WindowModeConfig,
};
use diagnostics::{PerformanceBudget, PerformanceOverlay};
use font_system::{
    CellMetrics, FontConfig as RuntimeFontConfig, FontLoadWaker, FontSource, FontSystem,
};
use fullscreen_chrome::{
    ChromeIntent, ChromeMotion, ChromePoint, ChromePointerButton, ChromeSettings, ChromeUpdate,
    FullscreenChromeController,
};
use mux::{
    LogicalRect, MuxAction, MuxModel, PaneExitDisposition, PaneId, PaneLayout, PaneRestore,
    RestoreSnapshot, SessionSpec, SessionStatus, SessionTransportKind, SplitAxis, SplitTree, TabId,
    TabRestore, TerminalGridSize, WindowRestore, WorkspaceRestore,
};
use platform_core::{
    DecorationMode, InputEvent, KeyEvent, KeyModifiers, KeyState, LinuxWindowBackend, MouseButton,
    MouseEvent, MouseEventKind, MouseScrollDelta, NotificationProvider, NotificationRequest,
    NotificationUrgency, PowerSource, PowerState, PowerStateProvider, UrlOpener, WindowAction,
    WindowChromeAction, WindowMode, win32_virtual_key,
};
use platform_winit::{
    ClipboardBridge, DesktopNotificationProvider, DesktopPowerMonitor, DesktopUrlOpener,
    DesktopWindow, InputTranslator, WindowSettings, apply_window_chrome_action,
    apply_window_mode_with_decoration, create_event_loop, platform_capabilities,
};
#[cfg(test)]
use render_core::RenderGrid;
use render_core::{
    CellPosition, CursorVisual, FrameRequestReason, OverlayKind, OverlayPrimitive, RenderCell,
    RenderCellStyle, RenderColor, RenderContentClip, RenderCursorShape, RenderDecoration,
    RenderInstrumentation, RenderItemRange, RenderOffset, RenderRect, RenderScene, SelectionVisual,
    WindowChromeControlKind, WindowChromeControlVisual, WindowChromeVisual,
};
use render_wgpu::{
    AnimatedCursorImageCache, AnimatedCursorImageRequest, AnimatedCursorImageRuntime,
    AnimatedCursorImageStatus, AnimationFramePacer, AnimationFramePacerDecision,
    CursorAnimationRuntime, CursorAnimationSettings, CursorBlinkRuntime, CursorOverlayFrame,
    CursorVectorCache, CursorVectorRequest, CursorVectorRuntime, CursorVectorStatus, DamageTracker,
    FrameDecision, FrameScheduler, GpuBackendPreference, GpuTerminalRenderer, PresentMode,
    RendererError, RendererOptions, RetainedDamageStatus,
};
use security::{
    HostKeyTrustAction, HostKeyTrustReason, HostKeyTrustRequest, HostTrustProvider,
    KeychainBackedSecretProvider, KeychainProvider, KeychainProviderCapability,
    SecretPromptProvider, SecretPromptResponse, SecretRequest, SecretString,
};
use security::{
    Osc52ClipboardDecision, Osc52ClipboardPolicy, Osc52ClipboardRequest as SecurityOsc52Request,
    Osc52ClipboardTarget, PlatformKeychainProvider, approve_osc52_clipboard_write,
    evaluate_osc52_clipboard_write,
};
use semantics::detect_url_hints;
use semantics::{
    BufferPosition, CommandStatus, IntegrationMode, RemoteMetadata, SemanticAction,
    SemanticActionResult, SemanticMetadata, SemanticRegionKind, SemanticSpan,
    SemanticTimelineStore, TerminalTextProvider,
};
use shell_integration::{
    HeuristicCommandDetector, IntegrationActivation, SemanticEscapeParser,
    ShellIntegrationActivationAction, ShellIntegrationActivationPlan, ShellIntegrationPolicy,
    ShellIntegrationRuntimeMode, ShellKind, detect_shell_kind, remote_install_plan,
};
#[cfg(test)]
use term_core::encode_terminal_key;
use term_core::{
    CellAttributes, ClipboardTarget, Color, ContentRevision, CursorShape, GridPosition, KeypadKey,
    Osc52ClipboardRequest, Selection, SelectionKind, TerminalAction, TerminalCore, TerminalKey,
    TerminalKeyEventType, TerminalKeyModifiers, TerminalMode, TerminalSize as CoreTerminalSize,
    WIN32_ENHANCED_KEY, WIN32_LEFT_ALT_PRESSED, WIN32_LEFT_CTRL_PRESSED, WIN32_RIGHT_ALT_PRESSED,
    WIN32_SHIFT_PRESSED, Win32InputRecord, encode_terminal_key_with_protocol,
};
use term_parser::TerminalEmulator;
use transport_core::{
    TerminalSize as TransportSize, TerminalTransport, TransportCommand, TransportEvent,
    TransportEventLoop, TransportOutput, TransportResult, TransportState, TransportWakeHandle,
};
use transport_pty::{LocalPtyTransport, LocalShellKind, LocalShellProfile};
use transport_ssh::{
    SshConnectionProfile, SshReconnectDecision, SshReconnectPolicy, SshReconnectRefusal,
    SshTransport,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use winit::{
    event::{Event, WindowEvent},
    event_loop::ControlFlow,
};

pub mod fullscreen_chrome;

pub fn main_entry() {
    if std::env::args().nth(1).as_deref() == Some("gui-smoke") {
        std::process::exit(run_gui_smoke_cli());
    }
    if let Some(code) = run_cli() {
        std::process::exit(code);
    }

    if let Err(error) = run(None) {
        eprintln!("panea desktop failed: {error}");
        std::process::exit(1);
    }
}

include!("cli.rs");
include!("config_load.rs");
include!("app_loop.rs");
include!("transport.rs");
include!("input_map.rs");
include!("clipboard.rs");
include!("mux_runtime.rs");
include!("pane_runtime.rs");
include!("scene.rs");
include!("semantic_overlays.rs");
include!("mouse_protocol.rs");

#[cfg(test)]
#[path = "../tests/unit/mod.rs"]
mod tests;
