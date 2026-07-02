//! WGPU renderer implementation, glyph atlas policy, damage tracking, and frame scheduling.

pub const LAYER: &str = "render performance";

use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    fmt,
    sync::Arc,
};

use font_system::{CellMetrics, FontError, FontSystem, GlyphBitmap, GlyphCache, GlyphCacheKey};
use render_core::{
    CellPosition, CursorVisual, DamageRegion, FrameRequestReason, RenderCell, RenderCellStyle,
    RenderColor, RenderCursorShape, RenderGrid, RenderRect, RenderScene,
};
use winit::window::Window;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentMode {
    Vsync,
    Immediate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererOptions {
    pub present_mode: PresentMode,
    pub damage_tracking: bool,
}

impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            present_mode: PresentMode::Vsync,
            damage_tracking: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererError {
    SurfaceCreation(String),
    AdapterUnavailable,
    DeviceCreation(String),
    Surface(String),
    Font(String),
    EmptySurface,
}

impl fmt::Display for RendererError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SurfaceCreation(message) => {
                write!(f, "failed to create render surface: {message}")
            }
            Self::AdapterUnavailable => f.write_str("no compatible GPU adapter is available"),
            Self::DeviceCreation(message) => write!(f, "failed to create GPU device: {message}"),
            Self::Surface(message) => write!(f, "surface error: {message}"),
            Self::Font(message) => write!(f, "font error: {message}"),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasEntry {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    entries: HashMap<GlyphCacheKey, AtlasEntry>,
    lru: VecDeque<GlyphCacheKey>,
}

impl GlyphAtlas {
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width: width.max(1),
            height: height.max(1),
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    pub fn allocate(&mut self, key: GlyphCacheKey, bitmap: &GlyphBitmap) -> Option<AtlasEntry> {
        if let Some(entry) = self.entries.get(&key).copied() {
            self.touch(key);
            return Some(entry);
        }

        let width = bitmap.width.max(1);
        let height = bitmap.height.max(1);
        if width > self.width || height > self.height {
            return None;
        }

        if self.cursor_x + width > self.width {
            self.cursor_x = 0;
            self.cursor_y += self.row_height;
            self.row_height = 0;
        }

        if self.cursor_y + height > self.height {
            self.clear();
        }

        let entry = AtlasEntry {
            x: self.cursor_x,
            y: self.cursor_y,
            width,
            height,
        };
        self.cursor_x += width;
        self.row_height = self.row_height.max(height);
        self.entries.insert(key, entry);
        self.lru.push_back(key);
        Some(entry)
    }

    #[must_use]
    pub fn entry(&self, key: GlyphCacheKey) -> Option<AtlasEntry> {
        self.entries.get(&key).copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn clear(&mut self) {
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.row_height = 0;
        self.entries.clear();
        self.lru.clear();
    }

    fn touch(&mut self, key: GlyphCacheKey) {
        self.lru.retain(|entry| *entry != key);
        self.lru.push_back(key);
    }
}

#[derive(Debug, Default)]
pub struct DamageTracker {
    previous_cells: HashMap<CellPosition, CellFingerprint>,
    previous_cursor: Option<CursorVisual>,
    previous_size: Option<(u16, u16)>,
    force_full: bool,
}

impl DamageTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_full_redraw(&mut self) {
        self.force_full = true;
    }

    pub fn update(&mut self, scene: &RenderScene, metrics: CellMetrics) -> Vec<DamageRegion> {
        let size = (scene.grid.columns, scene.grid.rows);
        let mut regions = Vec::new();

        if self.force_full || self.previous_size != Some(size) {
            self.force_full = false;
            self.previous_size = Some(size);
            self.previous_cells = scene
                .grid
                .cells
                .iter()
                .map(|cell| (cell.position, CellFingerprint::from(cell)))
                .collect();
            self.previous_cursor = scene.cursor;
            return vec![grid_region(&scene.grid, metrics)];
        }

        for cell in &scene.grid.cells {
            let fingerprint = CellFingerprint::from(cell);
            if self.previous_cells.get(&cell.position) != Some(&fingerprint) {
                regions.push(cell_region(cell.position, metrics));
                self.previous_cells.insert(cell.position, fingerprint);
            }
        }

        if self.previous_cursor != scene.cursor {
            if let Some(cursor) = self.previous_cursor {
                regions.push(cell_region(cursor.position, metrics));
            }
            if let Some(cursor) = scene.cursor {
                regions.push(cell_region(cursor.position, metrics));
            }
            self.previous_cursor = scene.cursor;
        }

        merge_regions(regions)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CellFingerprint {
    text: String,
    foreground: RenderColor,
    background: RenderColor,
    style: RenderCellStyle,
}

impl From<&RenderCell> for CellFingerprint {
    fn from(value: &RenderCell) -> Self {
        Self {
            text: value.text.clone(),
            foreground: value.foreground,
            background: value.background,
            style: value.style,
        }
    }
}

fn grid_region(grid: &RenderGrid, metrics: CellMetrics) -> DamageRegion {
    RenderRect {
        x: 0,
        y: 0,
        width: (f32::from(grid.columns) * metrics.cell_width).ceil() as u32,
        height: (f32::from(grid.rows) * metrics.cell_height).ceil() as u32,
    }
}

fn cell_region(position: CellPosition, metrics: CellMetrics) -> DamageRegion {
    RenderRect {
        x: (f32::from(position.col) * metrics.cell_width).floor() as i32,
        y: (position.row.max(0) as f32 * metrics.cell_height).floor() as i32,
        width: metrics.cell_width.ceil() as u32,
        height: metrics.cell_height.ceil() as u32,
    }
}

fn merge_regions(regions: Vec<DamageRegion>) -> Vec<DamageRegion> {
    regions
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDecision {
    NoFrameNeeded,
    FrameNeeded(FrameRequestReason),
}

#[derive(Debug, Default)]
pub struct FrameScheduler {
    pending: Option<FrameRequestReason>,
}

impl FrameScheduler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn terminal_content_changed(&mut self) {
        self.request(FrameRequestReason::TerminalContentChanged);
    }

    pub fn cursor_blink_changed(&mut self) {
        self.request(FrameRequestReason::CursorBlink);
    }

    pub fn animation_changed(&mut self) {
        self.request(FrameRequestReason::Animation);
    }

    pub fn window_resized(&mut self) {
        self.request(FrameRequestReason::WindowResized);
    }

    pub fn selection_changed(&mut self) {
        self.request(FrameRequestReason::SelectionChanged);
    }

    pub fn request(&mut self, reason: FrameRequestReason) {
        self.pending = Some(reason);
    }

    #[must_use]
    pub fn next_frame(&mut self) -> FrameDecision {
        self.pending
            .take()
            .map_or(FrameDecision::NoFrameNeeded, FrameDecision::FrameNeeded)
    }
}

#[derive(Debug)]
pub struct CpuFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl CpuFrame {
    #[must_use]
    pub fn snapshot_hash(&self) -> u64 {
        let mut hash = 14_695_981_039_346_656_037_u64;
        for byte in &self.pixels {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
        hash
    }
}

#[derive(Debug)]
pub struct TerminalRasterizer {
    glyph_cache: GlyphCache,
    atlas: GlyphAtlas,
}

impl Default for TerminalRasterizer {
    fn default() -> Self {
        Self {
            glyph_cache: GlyphCache::new(4096),
            atlas: GlyphAtlas::new(2048, 2048),
        }
    }
}

impl TerminalRasterizer {
    #[must_use]
    pub fn new(glyph_capacity: usize, atlas_width: u32, atlas_height: u32) -> Self {
        Self {
            glyph_cache: GlyphCache::new(glyph_capacity),
            atlas: GlyphAtlas::new(atlas_width, atlas_height),
        }
    }

    pub fn rasterize(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<CpuFrame, RendererError> {
        let metrics = fonts.cell_metrics()?;
        let width = (f32::from(scene.grid.columns) * metrics.cell_width)
            .ceil()
            .max(1.0) as u32;
        let height = (f32::from(scene.grid.rows) * metrics.cell_height)
            .ceil()
            .max(1.0) as u32;
        let mut frame = CpuFrame {
            width,
            height,
            pixels: vec![0; (width * height * 4) as usize],
        };

        fill_rect(
            &mut frame,
            RenderRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            RenderColor::rgb(12, 12, 12),
        );

        for cell in &scene.grid.cells {
            self.draw_cell(&mut frame, cell, fonts, metrics)?;
        }

        for overlay in scene
            .search_highlights
            .iter()
            .chain(scene.semantic_overlays.iter())
        {
            blend_rect(&mut frame, overlay.bounds, overlay.color);
        }

        for selection in &scene.selections {
            for position in &selection.cells {
                fill_rect(&mut frame, cell_region(*position, metrics), selection.color);
            }
        }

        if let Some(cursor) = scene.cursor {
            draw_cursor(&mut frame, cursor, metrics);
        }

        Ok(frame)
    }

    fn draw_cell(
        &mut self,
        frame: &mut CpuFrame,
        cell: &RenderCell,
        fonts: &mut FontSystem,
        metrics: CellMetrics,
    ) -> Result<(), RendererError> {
        let rect = cell_region(cell.position, metrics);
        fill_rect(frame, rect, cell.background);

        let font_id = fonts.primary_font()?.id();
        let mut pen_x = rect.x;

        for ch in cell.text.chars() {
            if ch == ' ' {
                pen_x += metrics.cell_width.ceil() as i32;
                continue;
            }

            let key = GlyphCacheKey::new(
                font_id,
                ch,
                metrics.font_size,
                cell.style.bold,
                cell.style.italic,
            );
            let bitmap = self.glyph_cache.get_or_insert_with(key, || {
                fonts.rasterize_glyph(key).unwrap_or_else(|_| {
                    GlyphBitmap::missing(metrics.cell_width, metrics.cell_height as u32)
                })
            });
            let _ = self.atlas.allocate(key, bitmap.as_ref());
            draw_glyph(
                frame,
                pen_x + bitmap.offset_x,
                rect.y + bitmap.offset_y,
                bitmap.as_ref(),
                cell.foreground,
            );
            pen_x += bitmap.advance_width.ceil() as i32;
        }

        if cell.style.underline {
            let y = rect.y + rect.height as i32 - 2;
            fill_rect(
                frame,
                RenderRect {
                    y,
                    height: 1,
                    ..rect
                },
                cell.foreground,
            );
        }

        if cell.style.strikethrough {
            let y = rect.y + (rect.height / 2) as i32;
            fill_rect(
                frame,
                RenderRect {
                    y,
                    height: 1,
                    ..rect
                },
                cell.foreground,
            );
        }

        Ok(())
    }
}

fn fill_rect(frame: &mut CpuFrame, rect: RenderRect, color: RenderColor) {
    let x0 = rect.x.max(0) as u32;
    let y0 = rect.y.max(0) as u32;
    let x1 = (rect.x.max(0) as u32 + rect.width).min(frame.width);
    let y1 = (rect.y.max(0) as u32 + rect.height).min(frame.height);

    for y in y0..y1 {
        for x in x0..x1 {
            let index = ((y * frame.width + x) * 4) as usize;
            frame.pixels[index] = color.red;
            frame.pixels[index + 1] = color.green;
            frame.pixels[index + 2] = color.blue;
            frame.pixels[index + 3] = color.alpha;
        }
    }
}

fn blend_rect(frame: &mut CpuFrame, rect: RenderRect, color: RenderColor) {
    let x0 = rect.x.max(0) as u32;
    let y0 = rect.y.max(0) as u32;
    let x1 = (rect.x.max(0) as u32 + rect.width).min(frame.width);
    let y1 = (rect.y.max(0) as u32 + rect.height).min(frame.height);

    for y in y0..y1 {
        for x in x0..x1 {
            let index = ((y * frame.width + x) * 4) as usize;
            blend_pixel(&mut frame.pixels[index..index + 4], color, color.alpha);
        }
    }
}

fn draw_glyph(frame: &mut CpuFrame, x: i32, y: i32, bitmap: &GlyphBitmap, color: RenderColor) {
    for gy in 0..bitmap.height {
        for gx in 0..bitmap.width {
            let target_x = x + gx as i32;
            let target_y = y + gy as i32;
            if target_x < 0
                || target_y < 0
                || target_x >= frame.width as i32
                || target_y >= frame.height as i32
            {
                continue;
            }

            let alpha = bitmap.pixels[(gy * bitmap.width + gx) as usize];
            if alpha == 0 {
                continue;
            }

            let index = (((target_y as u32 * frame.width) + target_x as u32) * 4) as usize;
            blend_pixel(&mut frame.pixels[index..index + 4], color, alpha);
        }
    }
}

fn blend_pixel(pixel: &mut [u8], color: RenderColor, alpha: u8) {
    let alpha = u16::from(alpha);
    let inverse = 255 - alpha;
    pixel[0] = (((u16::from(color.red) * alpha) + (u16::from(pixel[0]) * inverse)) / 255) as u8;
    pixel[1] = (((u16::from(color.green) * alpha) + (u16::from(pixel[1]) * inverse)) / 255) as u8;
    pixel[2] = (((u16::from(color.blue) * alpha) + (u16::from(pixel[2]) * inverse)) / 255) as u8;
    pixel[3] = u8::MAX;
}

fn draw_cursor(frame: &mut CpuFrame, cursor: CursorVisual, metrics: CellMetrics) {
    if !cursor.visible {
        return;
    }

    let mut rect = cell_region(cursor.position, metrics);
    match cursor.shape {
        RenderCursorShape::Block | RenderCursorShape::Custom => {}
        RenderCursorShape::Beam => {
            rect.width = 2;
        }
        RenderCursorShape::Underline => {
            rect.y += rect.height as i32 - 2;
            rect.height = 2;
        }
    }
    fill_rect(frame, rect, cursor.color);
}

pub struct GpuTerminalRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    texture: Option<wgpu::Texture>,
    texture_view: Option<wgpu::TextureView>,
    bind_group: Option<wgpu::BindGroup>,
    rasterizer: TerminalRasterizer,
}

impl GpuTerminalRenderer {
    pub async fn new(window: Arc<Window>, options: RendererOptions) -> Result<Self, RendererError> {
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Err(RendererError::EmptySurface);
        }

        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window)
            .map_err(|err| RendererError::SurfaceCreation(err.to_string()))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or(RendererError::AdapterUnavailable)?;
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("panea-render-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .map_err(|err| RendererError::DeviceCreation(err.to_string()))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let present_mode = match options.present_mode {
            PresentMode::Vsync => wgpu::PresentMode::Fifo,
            PresentMode::Immediate => caps
                .present_modes
                .iter()
                .copied()
                .find(|mode| *mode == wgpu::PresentMode::Immediate)
                .unwrap_or(wgpu::PresentMode::Fifo),
        };
        let alpha_mode = caps.alpha_modes[0];
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width,
            height: size.height,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("panea-present-shader"),
            source: wgpu::ShaderSource::Wgsl(PRESENT_SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("panea-present-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("panea-present-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("panea-present-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("panea-present-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group_layout,
            sampler,
            texture: None,
            texture_view: None,
            bind_group: None,
            rasterizer: TerminalRasterizer::default(),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }

        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn render_scene(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<(), RendererError> {
        let frame = self.rasterizer.rasterize(scene, fonts)?;
        self.upload_frame(&frame);
        self.present()
    }

    fn upload_frame(&mut self, frame: &CpuFrame) {
        let needs_texture = self.texture.as_ref().is_none_or(|texture| {
            texture.width() != frame.width || texture.height() != frame.height
        });

        if needs_texture {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("panea-terminal-frame-texture"),
                size: wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("panea-present-bind-group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.texture = Some(texture);
            self.texture_view = Some(texture_view);
            self.bind_group = Some(bind_group);
        }

        let texture = self.texture.as_ref().expect("frame texture exists");
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * frame.width),
                rows_per_image: Some(frame.height),
            },
            wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn present(&mut self) -> Result<(), RendererError> {
        let output = match self.surface.get_current_texture() {
            Ok(output) => output,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
            Err(error) => return Err(RendererError::Surface(error.to_string())),
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("panea-present-encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("panea-present-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(
                0,
                self.bind_group.as_ref().expect("present bind group exists"),
                &[],
            );
            pass.draw(0..3, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
        output.present();
        Ok(())
    }
}

const PRESENT_SHADER: &str = r#"
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0)
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(2.0, 0.0)
    );

    var out: VertexOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

@group(0) @binding(0) var terminal_texture: texture_2d<f32>;
@group(0) @binding(1) var terminal_sampler: sampler;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(terminal_texture, terminal_sampler, in.uv);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> CellMetrics {
        CellMetrics {
            font_size: 13.0,
            cell_width: 8.0,
            cell_height: 16.0,
            ascent: 11.0,
            descent: -3.0,
            line_gap: 1.0,
        }
    }

    fn cell(row: i64, col: u16, text: &str) -> RenderCell {
        RenderCell {
            position: CellPosition { row, col },
            text: text.to_owned(),
            foreground: RenderColor::rgb(230, 230, 230),
            background: RenderColor::rgb(12, 12, 12),
            style: RenderCellStyle::default(),
        }
    }

    fn scene(cells: Vec<RenderCell>) -> RenderScene {
        RenderScene {
            grid: RenderGrid {
                columns: 4,
                rows: 2,
                cells,
            },
            cursor: Some(CursorVisual {
                position: CellPosition { row: 0, col: 0 },
                shape: RenderCursorShape::Block,
                color: RenderColor::rgb(255, 255, 255),
                visible: true,
            }),
            ..RenderScene::default()
        }
    }

    fn scene_without_cursor(cells: Vec<RenderCell>) -> RenderScene {
        RenderScene {
            cursor: None,
            ..scene(cells)
        }
    }

    #[test]
    fn atlas_allocates_and_clears_when_full() {
        let mut atlas = GlyphAtlas::new(8, 8);
        let key_a = GlyphCacheKey::new(1, 'a', 13.0, false, false);
        let key_b = GlyphCacheKey::new(1, 'b', 13.0, false, false);
        let bitmap = GlyphBitmap::missing(4.0, 4);

        assert!(atlas.allocate(key_a, &bitmap).is_some());
        assert!(atlas.allocate(key_b, &bitmap).is_some());
        assert_eq!(atlas.len(), 2);
    }

    #[test]
    fn damage_tracks_changed_cell_and_cursor() {
        let mut tracker = DamageTracker::new();
        let first = scene(vec![cell(0, 0, "a"), cell(0, 1, "b")]);
        let initial = tracker.update(&first, metrics());
        assert_eq!(initial.len(), 1);

        let mut second = scene(vec![cell(0, 0, "a"), cell(0, 1, "c")]);
        second.cursor = Some(CursorVisual {
            position: CellPosition { row: 1, col: 1 },
            shape: RenderCursorShape::Block,
            color: RenderColor::rgb(255, 255, 255),
            visible: true,
        });

        let damage = tracker.update(&second, metrics());
        assert!(damage.iter().any(|region| region.x == 8 && region.y == 0));
        assert!(damage.iter().any(|region| region.x == 0 && region.y == 0));
        assert!(damage.iter().any(|region| region.x == 8 && region.y == 16));
    }

    #[test]
    fn frame_scheduler_stays_idle_without_work() {
        let mut scheduler = FrameScheduler::new();
        assert_eq!(scheduler.next_frame(), FrameDecision::NoFrameNeeded);

        scheduler.terminal_content_changed();
        assert_eq!(
            scheduler.next_frame(),
            FrameDecision::FrameNeeded(FrameRequestReason::TerminalContentChanged)
        );
        assert_eq!(scheduler.next_frame(), FrameDecision::NoFrameNeeded);
    }

    #[test]
    fn cpu_snapshot_changes_when_content_changes() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut rasterizer = TerminalRasterizer::default();

        let Ok(first) =
            rasterizer.rasterize(&scene_without_cursor(vec![cell(0, 0, "a")]), &mut fonts)
        else {
            return;
        };
        let second = rasterizer
            .rasterize(&scene_without_cursor(vec![cell(0, 0, "b")]), &mut fonts)
            .expect("same resolved font should render second snapshot");

        assert_ne!(first.snapshot_hash(), second.snapshot_hash());
    }
}
