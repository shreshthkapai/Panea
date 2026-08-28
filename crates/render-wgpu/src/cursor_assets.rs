// Off-thread animated image and static vector cursor assets.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimatedCursorImageRequest {
    pub path: PathBuf,
    pub fps: u16,
    pub max_size_kb: u32,
    pub warn_if_expensive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedCursorImage {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub frame_count: u16,
    pub fps: u16,
    pub size_kb: u32,
    pub warnings: Vec<String>,
    pub asset: Arc<CursorImageAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnimatedCursorImageStatus {
    Disabled,
    Loading { path: PathBuf },
    Ready(DecodedCursorImage),
    Failed { path: PathBuf, message: String },
}

#[derive(Debug, Default)]
pub struct AnimatedCursorImageCache {
    current: Option<AnimatedCursorImageStatus>,
    pending: Option<Receiver<AnimatedCursorImageStatus>>,
}

impl AnimatedCursorImageCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn disable(&mut self) {
        self.current = Some(AnimatedCursorImageStatus::Disabled);
        self.pending = None;
    }

    pub fn request(&mut self, request: AnimatedCursorImageRequest) {
        if request.path.as_os_str().is_empty() {
            self.current = Some(AnimatedCursorImageStatus::Failed {
                path: request.path,
                message: "cursor image path is empty".to_owned(),
            });
            self.pending = None;
            return;
        }

        if matches!(
            &self.current,
            Some(AnimatedCursorImageStatus::Ready(image)) if image.path == request.path && image.fps == request.fps
        ) {
            return;
        }

        let path = request.path.clone();
        let (sender, receiver) = mpsc::channel();
        self.current = Some(AnimatedCursorImageStatus::Loading { path: path.clone() });
        self.pending = Some(receiver);
        let spawn_result = thread::Builder::new()
            .name("panea-cursor-image-decode".to_owned())
            .spawn(move || {
                let status = decode_cursor_image_request(request);
                let _ = sender.send(status);
            });
        if let Err(error) = spawn_result {
            self.current = Some(AnimatedCursorImageStatus::Failed {
                path,
                message: format!("failed to start cursor image decoder: {error}"),
            });
            self.pending = None;
        }
    }

    pub fn poll(&mut self) -> AnimatedCursorImageStatus {
        if let Some(receiver) = &self.pending
            && let Ok(status) = receiver.try_recv()
        {
            self.current = Some(status);
            self.pending = None;
        }
        self.current
            .clone()
            .unwrap_or(AnimatedCursorImageStatus::Disabled)
    }
}

#[derive(Debug)]
pub struct AnimatedCursorImageRuntime {
    image: Option<DecodedCursorImage>,
    started_at: Instant,
    visible_last_frame: bool,
}

impl Default for AnimatedCursorImageRuntime {
    fn default() -> Self {
        Self {
            image: None,
            started_at: Instant::now(),
            visible_last_frame: false,
        }
    }
}

impl AnimatedCursorImageRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_image(&mut self, image: &DecodedCursorImage) -> bool {
        if self
            .image
            .as_ref()
            .is_some_and(|current| current.asset.id == image.asset.id && current.fps == image.fps)
        {
            return false;
        }
        self.image = Some(image.clone());
        self.started_at = Instant::now();
        true
    }

    pub fn clear(&mut self) -> bool {
        let changed = self.image.take().is_some();
        self.visible_last_frame = false;
        changed
    }

    pub fn populate_scene(&mut self, scene: &mut RenderScene, metrics: CellMetrics) {
        let (Some(image), Some(cursor)) = (&self.image, scene.cursor) else {
            self.visible_last_frame = false;
            scene.cursor_image = None;
            return;
        };
        let blink = scene
            .animations
            .iter()
            .find(|animation| animation.kind == AnimationKind::CursorBlinkEasing);
        if !cursor.visible && blink.is_none() {
            self.visible_last_frame = false;
            scene.cursor_image = None;
            return;
        }

        let elapsed = self.started_at.elapsed();
        let frame_count = image.asset.frames.len().max(1);
        let frame_index = if frame_count == 1 {
            0
        } else {
            let frame_micros = 1_000_000u128 / u128::from(image.fps.max(1));
            usize::try_from(elapsed.as_micros() / frame_micros).unwrap_or(usize::MAX) % frame_count
        };
        let cursor_region = scene
            .animations
            .iter()
            .find(|animation| animation.kind == AnimationKind::CursorSmoothMovement)
            .map_or_else(
                || cell_region(cursor.position, metrics),
                |animation| {
                    interpolate_region(
                        animation.start_region,
                        animation.end_region,
                        ease_out_cubic(animation_progress(*animation)),
                    )
                },
            );
        let opacity = blink.map_or(u8::MAX, |animation| {
            let progress = animation_progress(*animation);
            let alpha = if cursor.visible {
                progress
            } else {
                1.0 - progress
            };
            (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
        });
        scene.cursor_image = Some(CursorImageVisual {
            asset: Arc::clone(&image.asset),
            frame_index: u16::try_from(frame_index).unwrap_or(u16::MAX),
            bounds: fit_cursor_image_bounds(cursor_region, image.width, image.height),
            opacity,
        });
        self.visible_last_frame = true;
    }

    #[must_use]
    pub fn next_frame_after(&self) -> Option<Duration> {
        self.image.as_ref().and_then(|image| {
            (self.visible_last_frame && image.asset.frames.len() > 1)
                .then(|| Duration::from_micros(1_000_000 / u64::from(image.fps.max(1))))
        })
    }
}

fn fit_cursor_image_bounds(cell: RenderRect, image_width: u32, image_height: u32) -> RenderRect {
    let scale = (cell.width as f32 / image_width.max(1) as f32)
        .min(cell.height as f32 / image_height.max(1) as f32);
    let width = (image_width as f32 * scale).round().max(1.0) as u32;
    let height = (image_height as f32 * scale).round().max(1.0) as u32;
    RenderRect {
        x: cell.x + i32::try_from(cell.width.saturating_sub(width) / 2).unwrap_or(0),
        y: cell.y + i32::try_from(cell.height.saturating_sub(height) / 2).unwrap_or(0),
        width,
        height,
    }
}

fn decode_cursor_image_request(request: AnimatedCursorImageRequest) -> AnimatedCursorImageStatus {
    const MAX_DIMENSION: u32 = 512;
    const MAX_FRAMES: usize = 256;

    let path = request.path;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return AnimatedCursorImageStatus::Failed {
                path,
                message: format!("failed to read cursor image: {error}"),
            };
        }
    };

    let size_kb = u32::try_from(bytes.len().div_ceil(1024)).unwrap_or(u32::MAX);
    if size_kb > request.max_size_kb {
        return AnimatedCursorImageStatus::Failed {
            path,
            message: format!(
                "cursor image is {size_kb} KiB, above the configured {} KiB limit",
                request.max_size_kb
            ),
        };
    }

    let decoded_limit = usize::try_from(request.max_size_kb)
        .unwrap_or(usize::MAX)
        .saturating_mul(1024)
        .saturating_mul(16)
        .clamp(4 * 1024 * 1024, 64 * 1024 * 1024);
    let decoded = match decode_cursor_image_frames(&bytes, MAX_DIMENSION, MAX_FRAMES, decoded_limit)
    {
        Ok(decoded) => decoded,
        Err(message) => return AnimatedCursorImageStatus::Failed { path, message },
    };

    let mut warnings = Vec::new();
    if request.warn_if_expensive
        && size_kb > request.max_size_kb.saturating_mul(3).saturating_div(4)
    {
        warnings.push(format!(
            "cursor image {} KiB is close to the configured {} KiB limit",
            size_kb, request.max_size_kb
        ));
    }
    if request.warn_if_expensive && request.fps > 30 {
        warnings.push(format!(
            "cursor image FPS {} exceeds the low-cost 30 FPS range",
            request.fps
        ));
    }

    AnimatedCursorImageStatus::Ready(DecodedCursorImage {
        path,
        width: decoded.width,
        height: decoded.height,
        frame_count: u16::try_from(decoded.frames.len()).unwrap_or(u16::MAX),
        fps: request.fps,
        size_kb,
        warnings,
        asset: Arc::new(CursorImageAsset {
            id: cursor_image_asset_id(&bytes),
            width: decoded.width,
            height: decoded.height,
            frames: decoded.frames.into(),
        }),
    })
}

struct DecodedCursorFrames {
    width: u32,
    height: u32,
    frames: Vec<CursorImageFrame>,
}

fn decode_cursor_image_frames(
    bytes: &[u8],
    max_dimension: u32,
    max_frames: usize,
    max_decoded_bytes: usize,
) -> Result<DecodedCursorFrames, String> {
    let format = image::guess_format(bytes)
        .map_err(|_| "cursor image must be a valid GIF or PNG".to_owned())?;
    match format {
        image::ImageFormat::Gif => {
            let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
                .map_err(|error| format!("failed to decode GIF cursor: {error}"))?;
            let (width, height) = decoder.dimensions();
            validate_cursor_image_dimensions(width, height, max_dimension)?;
            let mut frames = Vec::new();
            let mut decoded_bytes = 0usize;
            for frame in decoder.into_frames().take(max_frames.saturating_add(1)) {
                if frames.len() == max_frames {
                    return Err(format!("cursor GIF exceeds the {max_frames}-frame limit"));
                }
                let frame =
                    frame.map_err(|error| format!("failed to decode GIF cursor frame: {error}"))?;
                let buffer = frame.into_buffer();
                if buffer.width() != width || buffer.height() != height {
                    return Err("cursor GIF frames must use one canvas size".to_owned());
                }
                let pixels = buffer.into_raw();
                decoded_bytes = decoded_bytes.saturating_add(pixels.len());
                if decoded_bytes > max_decoded_bytes {
                    return Err(format!(
                        "decoded cursor frames exceed the {} KiB memory budget",
                        max_decoded_bytes.div_ceil(1024)
                    ));
                }
                frames.push(CursorImageFrame {
                    pixels: pixels.into(),
                });
            }
            if frames.is_empty() {
                return Err("cursor GIF contains no frames".to_owned());
            }
            Ok(DecodedCursorFrames {
                width,
                height,
                frames,
            })
        }
        image::ImageFormat::Png => {
            let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
                .map_err(|error| format!("failed to decode PNG cursor: {error}"))?
                .to_rgba8();
            let (width, height) = image.dimensions();
            validate_cursor_image_dimensions(width, height, max_dimension)?;
            let pixels = image.into_raw();
            if pixels.len() > max_decoded_bytes {
                return Err(format!(
                    "decoded cursor image exceeds the {} KiB memory budget",
                    max_decoded_bytes.div_ceil(1024)
                ));
            }
            Ok(DecodedCursorFrames {
                width,
                height,
                frames: vec![CursorImageFrame {
                    pixels: pixels.into(),
                }],
            })
        }
        _ => Err("cursor image format is unsupported; use GIF or PNG".to_owned()),
    }
}

fn validate_cursor_image_dimensions(
    width: u32,
    height: u32,
    max_dimension: u32,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("cursor image dimensions must be non-zero".to_owned());
    }
    if width > max_dimension || height > max_dimension {
        return Err(format!(
            "cursor image dimensions {width}x{height} exceed the {max_dimension}px limit"
        ));
    }
    Ok(())
}

fn cursor_image_asset_id(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

pub const CURSOR_VECTOR_FORMAT_VERSION: u16 = 1;
pub const CURSOR_VECTOR_CANVAS_UNITS: u16 = 1000;
pub const CURSOR_VECTOR_MAX_PRIMITIVES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorVectorRequest {
    pub path: PathBuf,
    pub max_size_kb: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedCursorVector {
    pub path: PathBuf,
    pub size_kb: u32,
    pub asset: Arc<CursorVectorAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorVectorStatus {
    Disabled,
    Loading { path: PathBuf },
    Ready(DecodedCursorVector),
    Failed { path: PathBuf, message: String },
}

#[derive(Debug, Default)]
pub struct CursorVectorCache {
    current: Option<CursorVectorStatus>,
    pending: Option<Receiver<CursorVectorStatus>>,
}

impl CursorVectorCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn disable(&mut self) {
        self.current = Some(CursorVectorStatus::Disabled);
        self.pending = None;
    }

    pub fn request(&mut self, request: CursorVectorRequest) {
        if request.path.as_os_str().is_empty() {
            self.current = Some(CursorVectorStatus::Failed {
                path: request.path,
                message: "cursor vector path is empty".to_owned(),
            });
            self.pending = None;
            return;
        }
        if matches!(
            &self.current,
            Some(CursorVectorStatus::Ready(vector)) if vector.path == request.path
        ) {
            return;
        }

        let path = request.path.clone();
        let (sender, receiver) = mpsc::channel();
        self.current = Some(CursorVectorStatus::Loading { path: path.clone() });
        self.pending = Some(receiver);
        let spawn_result = thread::Builder::new()
            .name("panea-cursor-vector-decode".to_owned())
            .spawn(move || {
                let status = decode_cursor_vector_request(request);
                let _ = sender.send(status);
            });
        if let Err(error) = spawn_result {
            self.current = Some(CursorVectorStatus::Failed {
                path,
                message: format!("failed to start cursor vector decoder: {error}"),
            });
            self.pending = None;
        }
    }

    pub fn poll(&mut self) -> CursorVectorStatus {
        if let Some(receiver) = &self.pending
            && let Ok(status) = receiver.try_recv()
        {
            self.current = Some(status);
            self.pending = None;
        }
        self.current.clone().unwrap_or(CursorVectorStatus::Disabled)
    }
}

#[derive(Debug, Default)]
pub struct CursorVectorRuntime {
    vector: Option<DecodedCursorVector>,
}

impl CursorVectorRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_vector(&mut self, vector: &DecodedCursorVector) -> bool {
        if self
            .vector
            .as_ref()
            .is_some_and(|current| current.asset.id == vector.asset.id)
        {
            return false;
        }
        self.vector = Some(vector.clone());
        true
    }

    pub fn clear(&mut self) -> bool {
        self.vector.take().is_some()
    }

    pub fn populate_scene(&self, scene: &mut RenderScene, metrics: CellMetrics) {
        let (Some(vector), Some(cursor)) = (&self.vector, scene.cursor) else {
            scene.cursor_vector = None;
            return;
        };
        let blink = scene
            .animations
            .iter()
            .find(|animation| animation.kind == AnimationKind::CursorBlinkEasing);
        if !cursor.visible && blink.is_none() {
            scene.cursor_vector = None;
            return;
        }
        let bounds = scene
            .animations
            .iter()
            .find(|animation| animation.kind == AnimationKind::CursorSmoothMovement)
            .map_or_else(
                || cell_region(cursor.position, metrics),
                |animation| {
                    interpolate_region(
                        animation.start_region,
                        animation.end_region,
                        ease_out_cubic(animation_progress(*animation)),
                    )
                },
            );
        scene.cursor_vector = Some(CursorVectorVisual {
            asset: Arc::clone(&vector.asset),
            bounds,
            color: cursor.color,
            opacity: blink.map_or(u8::MAX, |animation| {
                let progress = animation_progress(*animation);
                let alpha = if cursor.visible {
                    progress
                } else {
                    1.0 - progress
                };
                (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
            }),
        });
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorVectorDocument {
    version: u16,
    primitives: Vec<CursorVectorDocumentPrimitive>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorVectorDocumentPrimitive {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    #[serde(default)]
    corner_radius: u16,
    color: Option<[u8; 4]>,
}

fn decode_cursor_vector_request(request: CursorVectorRequest) -> CursorVectorStatus {
    let path = request.path;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return CursorVectorStatus::Failed {
                path,
                message: format!("failed to read cursor vector: {error}"),
            };
        }
    };
    let size_kb = u32::try_from(bytes.len().div_ceil(1024)).unwrap_or(u32::MAX);
    if size_kb > request.max_size_kb {
        return CursorVectorStatus::Failed {
            path,
            message: format!(
                "cursor vector is {size_kb} KiB, above the configured {} KiB limit",
                request.max_size_kb
            ),
        };
    }
    match decode_cursor_vector(&bytes) {
        Ok(primitives) => CursorVectorStatus::Ready(DecodedCursorVector {
            path,
            size_kb,
            asset: Arc::new(CursorVectorAsset {
                id: cursor_image_asset_id(&bytes),
                primitives: primitives.into(),
            }),
        }),
        Err(message) => CursorVectorStatus::Failed { path, message },
    }
}

fn decode_cursor_vector(bytes: &[u8]) -> Result<Vec<CursorVectorPrimitive>, String> {
    let document: CursorVectorDocument = serde_json::from_slice(bytes)
        .map_err(|error| format!("cursor vector must be valid Panea JSON: {error}"))?;
    if document.version != CURSOR_VECTOR_FORMAT_VERSION {
        return Err(format!(
            "unsupported cursor vector version {}; expected {}",
            document.version, CURSOR_VECTOR_FORMAT_VERSION
        ));
    }
    if document.primitives.is_empty() {
        return Err("cursor vector must contain at least one primitive".to_owned());
    }
    if document.primitives.len() > CURSOR_VECTOR_MAX_PRIMITIVES {
        return Err(format!(
            "cursor vector exceeds the {CURSOR_VECTOR_MAX_PRIMITIVES}-primitive limit"
        ));
    }

    document
        .primitives
        .into_iter()
        .enumerate()
        .map(|(index, primitive)| {
            let right = primitive.x.saturating_add(primitive.width);
            let bottom = primitive.y.saturating_add(primitive.height);
            if primitive.width == 0
                || primitive.height == 0
                || right > CURSOR_VECTOR_CANVAS_UNITS
                || bottom > CURSOR_VECTOR_CANVAS_UNITS
                || primitive.corner_radius > CURSOR_VECTOR_CANVAS_UNITS / 2
            {
                return Err(format!(
                    "cursor vector primitive {index} is outside the 1000x1000 canvas or has invalid geometry"
                ));
            }
            Ok(CursorVectorPrimitive {
                x: primitive.x,
                y: primitive.y,
                width: primitive.width,
                height: primitive.height,
                corner_radius: primitive.corner_radius,
                color: primitive.color.map(|color| RenderColor {
                    red: color[0],
                    green: color[1],
                    blue: color[2],
                    alpha: color[3],
                }),
            })
        })
        .collect()
}

#[cfg(test)]
fn decode_cursor_image_header(bytes: &[u8]) -> Option<(u32, u32, u16)> {
    if bytes.len() >= 10 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        let width = u16::from_le_bytes([bytes[6], bytes[7]]).into();
        let height = u16::from_le_bytes([bytes[8], bytes[9]]).into();
        let frames = bytes
            .windows(2)
            .filter(|window| *window == [0x21, 0xF9])
            .count()
            .max(1);
        return Some((width, height, u16::try_from(frames).unwrap_or(u16::MAX)));
    }

    if bytes.len() >= 24 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some((width, height, 1));
    }

    None
}
