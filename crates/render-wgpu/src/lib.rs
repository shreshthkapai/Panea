//! WGPU renderer implementation, glyph atlas policy, damage tracking, and frame scheduling.

pub const LAYER: &str = "render performance";

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    hash::{Hash, Hasher},
    io::Cursor,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::Duration,
    time::Instant,
};

#[cfg(any(test, feature = "conformance"))]
use std::collections::BTreeMap;

use font_system::{
    CellMetrics, FontError, FontSystem, GlyphBitmap, GlyphBitmapFormat, GlyphCache, GlyphCacheKey,
    ShapedGlyph,
};
use hashbrown::{Equivalent, HashMap as HbHashMap};
use image::{AnimationDecoder, ImageDecoder};
use render_core::{
    AnimationHandle, AnimationKind, AnimationQuad, CellPosition, CursorImageAsset,
    CursorImageFrame, CursorImageVisual, CursorVectorAsset, CursorVectorPrimitive,
    CursorVectorVisual, CursorVisual, DamageRegion, FrameRequestReason, GpuTimingStatus,
    OverlayKind, OverlayPrimitive, RenderCell, RenderCellStyle, RenderColor, RenderCursorShape,
    RenderDecoration, RenderGrid, RenderInstrumentation, RenderRecoveryEvent, RenderRecoveryReason,
    RenderRecoveryStatus, RenderRect, RenderScene, RenderSurfaceStatus, RenderText,
    SelectionVisual, WindowChromeControlKind, WindowChromeControlVisual, WindowChromeVisual,
};
use serde::Deserialize;
use unicode_width::UnicodeWidthStr;
use winit::window::Window;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentMode {
    Auto,
    Fifo,
    Mailbox,
    Immediate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuBackendPreference {
    #[default]
    Auto,
    Vulkan,
    Metal,
    Dx12,
    Gl,
}

impl GpuBackendPreference {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vulkan => "vulkan",
            Self::Metal => "metal",
            Self::Dx12 => "dx12",
            Self::Gl => "gl",
        }
    }
}

fn instance_backends(preference: GpuBackendPreference) -> wgpu::Backends {
    match preference {
        GpuBackendPreference::Auto => wgpu::Backends::all(),
        GpuBackendPreference::Vulkan => wgpu::Backends::VULKAN,
        GpuBackendPreference::Metal => wgpu::Backends::METAL,
        GpuBackendPreference::Dx12 => wgpu::Backends::DX12,
        GpuBackendPreference::Gl => wgpu::Backends::GL,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeBackendFamily {
    Windows,
    Apple,
    Unix,
    Other,
}

fn native_backend_family() -> NativeBackendFamily {
    if cfg!(target_os = "windows") {
        NativeBackendFamily::Windows
    } else if cfg!(target_vendor = "apple") {
        NativeBackendFamily::Apple
    } else if cfg!(unix) {
        NativeBackendFamily::Unix
    } else {
        NativeBackendFamily::Other
    }
}

fn backend_candidates_for(
    family: NativeBackendFamily,
    requested: GpuBackendPreference,
    transparent: bool,
) -> Vec<GpuBackendPreference> {
    if requested != GpuBackendPreference::Auto {
        return vec![requested];
    }

    match (family, transparent) {
        (NativeBackendFamily::Windows, true) => vec![
            GpuBackendPreference::Vulkan,
            GpuBackendPreference::Dx12,
            GpuBackendPreference::Gl,
        ],
        (NativeBackendFamily::Windows, false) => vec![
            GpuBackendPreference::Dx12,
            GpuBackendPreference::Vulkan,
            GpuBackendPreference::Gl,
        ],
        (NativeBackendFamily::Apple, _) => vec![
            GpuBackendPreference::Metal,
            GpuBackendPreference::Vulkan,
            GpuBackendPreference::Gl,
        ],
        (NativeBackendFamily::Unix, _) => {
            vec![GpuBackendPreference::Vulkan, GpuBackendPreference::Gl]
        }
        (NativeBackendFamily::Other, _) => vec![GpuBackendPreference::Auto],
    }
}

const DESIRED_MAXIMUM_FRAME_LATENCY: u32 = 1;

fn select_present_mode(
    requested: PresentMode,
    available: &[wgpu::PresentMode],
) -> wgpu::PresentMode {
    let supports = |mode| available.contains(&mode);
    match requested {
        PresentMode::Mailbox if supports(wgpu::PresentMode::Mailbox) => wgpu::PresentMode::Mailbox,
        PresentMode::Immediate if supports(wgpu::PresentMode::Immediate) => {
            wgpu::PresentMode::Immediate
        }
        PresentMode::Immediate if supports(wgpu::PresentMode::Mailbox) => {
            wgpu::PresentMode::Mailbox
        }
        PresentMode::Auto | PresentMode::Fifo | PresentMode::Mailbox | PresentMode::Immediate => {
            wgpu::PresentMode::Fifo
        }
    }
}

fn select_composite_alpha_mode(
    transparent: bool,
    available: &[wgpu::CompositeAlphaMode],
) -> wgpu::CompositeAlphaMode {
    if transparent {
        available
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::CompositeAlphaMode::PreMultiplied)
            .or_else(|| {
                available
                    .iter()
                    .copied()
                    .find(|mode| *mode == wgpu::CompositeAlphaMode::PostMultiplied)
            })
            .unwrap_or(available[0])
    } else {
        available
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::CompositeAlphaMode::Opaque)
            .unwrap_or(available[0])
    }
}

fn blend_state_for_alpha_mode(alpha_mode: wgpu::CompositeAlphaMode) -> wgpu::BlendState {
    if alpha_mode == wgpu::CompositeAlphaMode::PreMultiplied {
        wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING
    } else {
        wgpu::BlendState::ALPHA_BLENDING
    }
}

#[cfg(test)]
fn adjust_text_coverage(coverage: f32, text_gamma_adjustment: f32) -> f32 {
    let gamma = text_gamma_adjustment.clamp(1.0, 2.0);
    coverage.clamp(0.0, 1.0).powf(1.0 / gamma)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetainedDamageStatus {
    Enabled,
    DisabledByConfig,
    Unsupported { reason: String },
    Unverified { reason: String },
}

impl fmt::Display for RetainedDamageStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Enabled => formatter.write_str("enabled"),
            Self::DisabledByConfig => formatter.write_str("disabled by configuration"),
            Self::Unsupported { reason } => write!(formatter, "unsupported: {reason}"),
            Self::Unverified { reason } => write!(formatter, "unverified: {reason}"),
        }
    }
}

fn retained_damage_status(requested: bool, surface_copy_supported: bool) -> RetainedDamageStatus {
    if !requested {
        RetainedDamageStatus::DisabledByConfig
    } else if surface_copy_supported {
        RetainedDamageStatus::Enabled
    } else {
        RetainedDamageStatus::Unsupported {
            reason: "the active WGPU surface cannot receive the retained frame texture".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RendererOptions {
    pub backend: GpuBackendPreference,
    pub present_mode: PresentMode,
    pub damage_tracking: bool,
    pub gpu_timestamps: bool,
    pub text_gamma_adjustment: f32,
    pub transparent: bool,
    pub glyph_cache_entries: usize,
    pub background: RenderColor,
}

impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            backend: GpuBackendPreference::Auto,
            present_mode: PresentMode::Auto,
            damage_tracking: false,
            gpu_timestamps: false,
            text_gamma_adjustment: 1.2,
            transparent: false,
            glyph_cache_entries: 8192,
            background: RenderColor::rgb(12, 12, 12),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RendererStartupTimings {
    pub instance_and_surface: Duration,
    pub adapter_request: Duration,
    pub device_request: Duration,
    pub surface_configuration: Duration,
    pub pipeline_creation: Duration,
    pub total: Duration,
}

impl RendererStartupTimings {
    #[must_use]
    pub fn accounted(self) -> Duration {
        [
            self.instance_and_surface,
            self.adapter_request,
            self.device_request,
            self.surface_configuration,
            self.pipeline_creation,
        ]
        .into_iter()
        .fold(Duration::ZERO, Duration::saturating_add)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererStartupDiagnostics {
    pub requested_backend: GpuBackendPreference,
    pub effective_backend: String,
    pub adapter: String,
    pub attempted_backends: Vec<GpuBackendPreference>,
    pub fallback_errors: Vec<String>,
    pub timings: RendererStartupTimings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuAdapterProbe {
    pub backend: String,
    pub adapter: String,
    pub device_type: String,
    pub features: Vec<String>,
}

#[must_use]
pub async fn probe_gpu_adapter() -> Option<GpuAdapterProbe> {
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await?;
    let info = adapter.get_info();
    let features = adapter.features();
    let feature_names = [
        (wgpu::Features::TIMESTAMP_QUERY, "timestamp_query"),
        (
            wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES,
            "texture_adapter_specific_format_features",
        ),
    ]
    .into_iter()
    .filter_map(|(feature, name)| features.contains(feature).then_some(name.to_owned()))
    .collect::<Vec<_>>();

    Some(GpuAdapterProbe {
        backend: format!("{:?}", info.backend),
        adapter: info.name,
        device_type: format!("{:?}", info.device_type),
        features: feature_names,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererError {
    SurfaceCreation(String),
    AdapterUnavailable {
        requested: GpuBackendPreference,
    },
    BackendSelection {
        requested: GpuBackendPreference,
        attempts: Vec<String>,
    },
    DeviceCreation(String),
    Surface(String),
    DeviceLost {
        reason: RenderRecoveryReason,
        message: String,
    },
    DeviceUnavailable(String),
    RecoveryFailed(String),
    Font(String),
    Asset(String),
    EmptySurface,
}

impl fmt::Display for RendererError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SurfaceCreation(message) => {
                write!(f, "failed to create render surface: {message}")
            }
            Self::AdapterUnavailable { requested } => write!(
                f,
                "no compatible GPU adapter is available for renderer backend '{}'",
                requested.as_str()
            ),
            Self::BackendSelection {
                requested,
                attempts,
            } => write!(
                f,
                "renderer backend '{}' failed after bounded attempts: {}",
                requested.as_str(),
                attempts.join("; ")
            ),
            Self::DeviceCreation(message) => write!(f, "failed to create GPU device: {message}"),
            Self::Surface(message) => write!(f, "surface error: {message}"),
            Self::DeviceLost { reason, message } => {
                write!(f, "GPU device lost ({reason:?}): {message}")
            }
            Self::DeviceUnavailable(message) => write!(f, "GPU device unavailable: {message}"),
            Self::RecoveryFailed(message) => write!(f, "GPU recovery failed: {message}"),
            Self::Font(message) => write!(f, "font error: {message}"),
            Self::Asset(message) => write!(f, "renderer asset error: {message}"),
            Self::EmptySurface => f.write_str("surface has zero width or height"),
        }
    }
}

impl Error for RendererError {}

impl From<FontError> for RendererError {
    fn from(value: FontError) -> Self {
        Self::Font(value.to_string())
    }
}

include!("atlas.rs");
include!("damage.rs");
include!("schedule.rs");
include!("cursor_fx.rs");
include!("cursor_assets.rs");
include!("planner.rs");

#[cfg(any(test, feature = "conformance"))]
include!("conformance.rs");

include!("gpu/buffers.rs");
include!("gpu/backend.rs");
include!("gpu/shaders.rs");

#[cfg(test)]
#[path = "../tests/unit/mod.rs"]
mod tests;
