//! Font discovery, fallback, glyph rasterization, and cache policy.

pub const LAYER: &str = "render performance";

use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error,
    fmt, fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};

use ab_glyph::{Font, FontArc, FontVec, GlyphId, GlyphImageFormat, PxScale, ScaleFont, point};
use image::imageops::FilterType;
use swash::{
    FontRef as SwashFontRef,
    scale::{Render as SwashRender, ScaleContext, Source, StrikeWith, image::Content},
    zeno::Format,
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, PartialEq)]
pub struct FontConfig {
    pub family: String,
    pub fallback_families: Vec<String>,
    pub size: f32,
    pub line_height: f32,
    pub ligatures: bool,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "monospace".to_owned(),
            fallback_families: Vec::new(),
            size: 13.0,
            line_height: 1.2,
            ligatures: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontDescriptor {
    pub family: String,
    pub source: FontSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FontSource {
    File(PathBuf),
    Memory,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FontFallbackChain {
    pub primary: FontDescriptor,
    pub fallbacks: Vec<FontDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    pub font_size: f32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
    /// Primary-font baseline measured down from the top of a terminal cell.
    pub baseline: f32,
    /// Underline top edge measured down from the top of a terminal cell.
    pub underline_position: f32,
    /// Strikeout top edge measured down from the top of a terminal cell.
    pub strikethrough_position: f32,
    pub decoration_thickness: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphCacheKey {
    pub font_id: u64,
    pub glyph_id: u16,
    pub size_millipoints: u32,
    pub bold: bool,
    pub italic: bool,
}

impl GlyphCacheKey {
    #[must_use]
    pub fn new(font_id: u64, glyph_id: u16, size: f32, bold: bool, italic: bool) -> Self {
        Self {
            font_id,
            glyph_id,
            size_millipoints: (size * 1000.0).round().max(1.0) as u32,
            bold,
            italic,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    pub offset_x: i32,
    pub offset_y: i32,
    pub advance_width: f32,
    pub pixels: Vec<u8>,
    pub format: GlyphBitmapFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphBitmapFormat {
    Alpha,
    Rgba,
}

impl GlyphBitmap {
    #[must_use]
    pub fn missing(advance_width: f32, height: u32) -> Self {
        let width = advance_width.ceil().max(1.0) as u32;
        let mut pixels = vec![0; (width * height) as usize];

        if width > 1 && height > 1 {
            for x in 0..width {
                pixels[x as usize] = 180;
                pixels[((height - 1) * width + x) as usize] = 180;
            }
            for y in 0..height {
                pixels[(y * width) as usize] = 180;
                pixels[(y * width + width - 1) as usize] = 180;
            }
        }

        Self {
            width,
            height,
            offset_x: 0,
            offset_y: 0,
            advance_width,
            pixels,
            format: GlyphBitmapFormat::Alpha,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    pub key: GlyphCacheKey,
    pub cluster: u32,
    pub x_advance: f32,
    pub y_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShapedRun {
    pub glyphs: Vec<ShapedGlyph>,
    pub advance_width: f32,
    pub families_used: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontDiagnostic {
    pub family: String,
    pub role: &'static str,
    pub resolved: bool,
    pub source: FontSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontError {
    FontNotFound { requested: Vec<String> },
    FontLoadFailed { family: String, reason: String },
}

impl fmt::Display for FontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FontNotFound { requested } => {
                write!(
                    f,
                    "no configured font family was found: {}",
                    requested.join(", ")
                )
            }
            Self::FontLoadFailed { family, reason } => {
                write!(f, "failed to load font '{family}': {reason}")
            }
        }
    }
}

impl Error for FontError {}

#[derive(Debug)]
pub struct FontSystem {
    database: fontdb::Database,
    config: FontConfig,
    scale_factor: f32,
    primary: Option<LoadedFont>,
    loaded: HashMap<u64, LoadedFont>,
    attempted_faces: HashSet<(String, bool, bool)>,
}

impl FontSystem {
    #[must_use]
    pub fn new(config: FontConfig) -> Self {
        Self::new_with_scale_factor(config, 1.0)
    }

    #[must_use]
    pub fn new_with_scale_factor(config: FontConfig, scale_factor: f64) -> Self {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();

        Self {
            database,
            config,
            scale_factor: normalized_scale_factor(scale_factor),
            primary: None,
            loaded: HashMap::new(),
            attempted_faces: HashSet::new(),
        }
    }

    pub fn resolve_fallback_chain(&self) -> FontFallbackChain {
        let requested = self.requested_families();
        let descriptors = requested
            .iter()
            .map(|family| self.resolve_descriptor(family))
            .collect::<Vec<_>>();

        let primary = descriptors
            .first()
            .cloned()
            .unwrap_or_else(|| FontDescriptor {
                family: self.config.family.clone(),
                source: FontSource::Unresolved,
            });

        FontFallbackChain {
            primary,
            fallbacks: descriptors.into_iter().skip(1).collect(),
        }
    }

    #[must_use]
    pub fn generation_id(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.config.family.hash(&mut hasher);
        self.config.fallback_families.hash(&mut hasher);
        self.config.size.to_bits().hash(&mut hasher);
        self.config.line_height.to_bits().hash(&mut hasher);
        self.config.ligatures.hash(&mut hasher);
        self.scale_factor.to_bits().hash(&mut hasher);
        hasher.finish()
    }

    pub fn set_scale_factor(&mut self, scale_factor: f64) -> bool {
        let scale_factor = normalized_scale_factor(scale_factor);
        if self.scale_factor.to_bits() == scale_factor.to_bits() {
            return false;
        }
        self.scale_factor = scale_factor;
        true
    }

    #[must_use]
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    #[must_use]
    pub fn physical_font_size(&self) -> f32 {
        points_to_physical_pixels(self.config.size, self.scale_factor)
    }

    pub fn primary_font(&mut self) -> Result<&LoadedFont, FontError> {
        if self.primary.is_none() {
            let font = self.load_primary()?;
            self.loaded.insert(font.id(), font.clone());
            self.primary = Some(font);
        }

        Ok(self.primary.as_ref().expect("primary font is initialized"))
    }

    pub fn cell_metrics(&mut self) -> Result<CellMetrics, FontError> {
        let size = self.physical_font_size();
        let line_height = self.config.line_height;
        self.primary_font()
            .map(|font| font.metrics(size, line_height))
    }

    pub fn rasterize_glyph(&mut self, key: GlyphCacheKey) -> Result<GlyphBitmap, FontError> {
        let size = key.size_millipoints as f32 / 1000.0;
        if !self.loaded.contains_key(&key.font_id) {
            let _ = self.primary_font()?;
        }
        self.loaded
            .get(&key.font_id)
            .map(|font| font.rasterize(key.glyph_id, size))
            .ok_or_else(|| FontError::FontLoadFailed {
                family: "resolved fallback".to_owned(),
                reason: format!("unknown font id {} in glyph cache key", key.font_id),
            })
    }

    pub fn shape_text(
        &mut self,
        text: &str,
        bold: bool,
        italic: bool,
    ) -> Result<ShapedRun, FontError> {
        if text.is_empty() {
            return Ok(ShapedRun {
                glyphs: Vec::new(),
                advance_width: 0.0,
                families_used: Vec::new(),
            });
        }

        let mut segments: Vec<(u64, String, usize)> = Vec::new();
        for (byte_index, grapheme) in text.grapheme_indices(true) {
            let font_id = self.resolve_font_for_text(grapheme, bold, italic)?;
            if let Some((last_id, segment, _)) = segments.last_mut()
                && *last_id == font_id
            {
                segment.push_str(grapheme);
            } else {
                segments.push((font_id, grapheme.to_owned(), byte_index));
            }
        }

        let size = self.physical_font_size();
        let mut glyphs = Vec::new();
        let mut advance_width = 0.0;
        let mut families_used = Vec::new();
        for (font_id, segment, byte_offset) in segments {
            let font = self.loaded.get(&font_id).expect("resolved font is cached");
            if !families_used.contains(&font.family) {
                families_used.push(font.family.clone());
            }
            let shaped = font.shape(
                &segment,
                size,
                bold,
                italic,
                byte_offset as u32,
                self.config.ligatures,
            )?;
            advance_width += shaped.iter().map(|glyph| glyph.x_advance).sum::<f32>();
            glyphs.extend(shaped);
        }

        Ok(ShapedRun {
            glyphs,
            advance_width,
            families_used,
        })
    }

    #[must_use]
    pub fn diagnostics(&self) -> Vec<FontDiagnostic> {
        let chain = self.resolve_fallback_chain();
        let mut diagnostics = std::iter::once(FontDiagnostic {
            family: chain.primary.family,
            role: "primary",
            resolved: chain.primary.source != FontSource::Unresolved,
            source: chain.primary.source,
        })
        .chain(
            chain
                .fallbacks
                .into_iter()
                .map(|descriptor| FontDiagnostic {
                    family: descriptor.family,
                    role: "fallback",
                    resolved: descriptor.source != FontSource::Unresolved,
                    source: descriptor.source,
                }),
        )
        .collect::<Vec<_>>();
        for (role, bold, italic) in [
            ("regular-face", false, false),
            ("bold-face", true, false),
            ("italic-face", false, true),
            ("bold-italic-face", true, true),
        ] {
            let loaded = self
                .load_family(&self.config.family, bold, italic)
                .and_then(Result::ok);
            let resolved = loaded
                .as_ref()
                .is_some_and(|font| font.is_bold() == bold && font.is_italic() == italic);
            diagnostics.push(FontDiagnostic {
                family: self.config.family.clone(),
                role,
                resolved,
                source: loaded
                    .map(|font| font.source)
                    .unwrap_or(FontSource::Unresolved),
            });
        }
        diagnostics
    }

    fn load_primary(&self) -> Result<LoadedFont, FontError> {
        let requested = self.requested_families();

        for family in &requested {
            if let Some(loaded) = self.load_family(family, false, false) {
                return loaded.map_err(|reason| FontError::FontLoadFailed {
                    family: family.clone(),
                    reason,
                });
            }
        }

        Err(FontError::FontNotFound { requested })
    }

    fn requested_families(&self) -> Vec<String> {
        let mut requested = Vec::new();
        if !self.config.family.eq_ignore_ascii_case("monospace") {
            requested.push(self.config.family.clone());
        }
        requested.extend(self.config.fallback_families.clone());
        requested.extend([
            "Cascadia Mono".to_owned(),
            "Consolas".to_owned(),
            "Menlo".to_owned(),
            "DejaVu Sans Mono".to_owned(),
            "Segoe UI Emoji".to_owned(),
            "Apple Color Emoji".to_owned(),
            "Noto Color Emoji".to_owned(),
            "Noto Emoji".to_owned(),
            "Microsoft YaHei UI".to_owned(),
            "Yu Gothic UI".to_owned(),
            "Hiragino Sans".to_owned(),
            "Noto Sans Mono CJK JP".to_owned(),
            "Noto Sans CJK JP".to_owned(),
            "monospace".to_owned(),
        ]);
        let mut seen = HashSet::new();
        requested.retain(|family| seen.insert(family.to_ascii_lowercase()));
        requested
    }

    fn resolve_descriptor(&self, family: &str) -> FontDescriptor {
        let families = family_query(family);
        let query = fontdb::Query {
            families: &families,
            ..fontdb::Query::default()
        };
        let Some(id) = self.database.query(&query) else {
            return FontDescriptor {
                family: family.to_owned(),
                source: FontSource::Unresolved,
            };
        };

        let Some(face) = self.database.face(id) else {
            return FontDescriptor {
                family: family.to_owned(),
                source: FontSource::Unresolved,
            };
        };

        FontDescriptor {
            family: family.to_owned(),
            source: source_kind(&face.source),
        }
    }

    fn load_family(
        &self,
        family: &str,
        bold: bool,
        italic: bool,
    ) -> Option<Result<LoadedFont, String>> {
        let families = family_query(family);
        let query = fontdb::Query {
            families: &families,
            weight: if bold {
                fontdb::Weight::BOLD
            } else {
                fontdb::Weight::NORMAL
            },
            style: if italic {
                fontdb::Style::Italic
            } else {
                fontdb::Style::Normal
            },
            ..fontdb::Query::default()
        };
        let id = self.database.query(&query)?;
        let face = self.database.face(id)?;
        let face_index = face.index;
        let source = source_kind(&face.source);
        let actual_bold = face.weight >= fontdb::Weight::SEMIBOLD;
        let actual_italic = face.style != fontdb::Style::Normal;
        let bytes = match &face.source {
            fontdb::Source::File(path) => fs::read(path).map_err(|err| err.to_string()),
            fontdb::Source::SharedFile(path, _) => fs::read(path).map_err(|err| err.to_string()),
            fontdb::Source::Binary(bytes) => Ok(bytes.as_ref().as_ref().to_vec()),
        };

        Some(bytes.and_then(|bytes| {
            let font = FontArc::new(
                FontVec::try_from_vec_and_index(bytes.clone(), face_index)
                    .map_err(|err| err.to_string())?,
            );
            Ok(LoadedFont::new(
                family.to_owned(),
                font,
                Arc::new(bytes),
                face_index,
                source,
                actual_bold,
                actual_italic,
            ))
        }))
    }

    fn resolve_font_for_text(
        &mut self,
        text: &str,
        bold: bool,
        italic: bool,
    ) -> Result<u64, FontError> {
        let mut style_fallback = None;
        for family in self.requested_families() {
            for font in self.loaded.values().filter(|font| font.family == family) {
                if font.supports_text(text) {
                    if font.is_bold() == bold && font.is_italic() == italic {
                        return Ok(font.id());
                    }
                    style_fallback.get_or_insert(font.id());
                }
            }
            if !self.attempted_faces.insert((family.clone(), bold, italic)) {
                continue;
            }
            let Some(result) = self.load_family(&family, bold, italic) else {
                continue;
            };
            let font = result.map_err(|reason| FontError::FontLoadFailed {
                family: family.clone(),
                reason,
            })?;
            if font.supports_text(text) {
                let id = font.id();
                self.loaded.entry(id).or_insert(font);
                let loaded = self.loaded.get(&id).expect("font was inserted");
                if loaded.is_bold() == bold && loaded.is_italic() == italic {
                    return Ok(id);
                }
                style_fallback.get_or_insert(id);
            }
        }

        if let Some(id) = style_fallback {
            return Ok(id);
        }

        let primary = self.primary_font()?.clone();
        let id = primary.id();
        self.loaded.entry(id).or_insert(primary);
        Ok(id)
    }
}

fn normalized_scale_factor(scale_factor: f64) -> f32 {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor.clamp(0.25, 8.0) as f32
    } else {
        1.0
    }
}

fn points_to_physical_pixels(points: f32, scale_factor: f32) -> f32 {
    const CSS_PIXELS_PER_INCH: f32 = 96.0;
    const POINTS_PER_INCH: f32 = 72.0;
    (points * CSS_PIXELS_PER_INCH / POINTS_PER_INCH * scale_factor).max(1.0)
}

fn family_query(family: &str) -> Vec<fontdb::Family<'_>> {
    if family.eq_ignore_ascii_case("monospace") {
        vec![fontdb::Family::Monospace]
    } else {
        vec![fontdb::Family::Name(family)]
    }
}

fn source_kind(source: &fontdb::Source) -> FontSource {
    match source {
        fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
            FontSource::File(path.clone())
        }
        fontdb::Source::Binary(_) => FontSource::Memory,
    }
}

#[derive(Debug, Clone)]
pub struct LoadedFont {
    id: u64,
    family: String,
    font: FontArc,
    bytes: Arc<Vec<u8>>,
    face_index: u32,
    source: FontSource,
    bold: bool,
    italic: bool,
}

impl LoadedFont {
    fn new(
        family: String,
        font: FontArc,
        bytes: Arc<Vec<u8>>,
        face_index: u32,
        source: FontSource,
        bold: bool,
        italic: bool,
    ) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        family.hash(&mut hasher);
        face_index.hash(&mut hasher);
        source.hash(&mut hasher);
        bold.hash(&mut hasher);
        italic.hash(&mut hasher);

        Self {
            id: hasher.finish(),
            family,
            font,
            bytes,
            face_index,
            source,
            bold,
            italic,
        }
    }

    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    #[must_use]
    pub fn family(&self) -> &str {
        &self.family
    }

    #[must_use]
    pub fn source(&self) -> &FontSource {
        &self.source
    }

    #[must_use]
    pub const fn is_bold(&self) -> bool {
        self.bold
    }

    #[must_use]
    pub const fn is_italic(&self) -> bool {
        self.italic
    }

    fn supports_text(&self, text: &str) -> bool {
        text.chars()
            .all(|ch| is_default_ignorable(ch) || self.font.glyph_id(ch) != GlyphId(0))
    }

    fn shape(
        &self,
        text: &str,
        size: f32,
        bold: bool,
        italic: bool,
        cluster_offset: u32,
        ligatures: bool,
    ) -> Result<Vec<ShapedGlyph>, FontError> {
        let Some(face) = rustybuzz::Face::from_slice(&self.bytes, self.face_index) else {
            return Err(FontError::FontLoadFailed {
                family: self.family.clone(),
                reason: "OpenType shaping face could not be created".to_owned(),
            });
        };
        // `size` is pixels per em. Rustybuzz reports positions in font design
        // units, so converting by units-per-em keeps shaping, Swash
        // rasterization, and typographic point sizes on one scale.
        let scale = self.design_unit_scale(size);
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.guess_segment_properties();
        let features = if ligatures {
            Vec::new()
        } else {
            ["liga=0", "clig=0", "calt=0"]
                .into_iter()
                .filter_map(|feature| rustybuzz::Feature::from_str(feature).ok())
                .collect::<Vec<_>>()
        };
        let glyph_buffer = rustybuzz::shape(&face, &features, buffer);

        Ok(glyph_buffer
            .glyph_infos()
            .iter()
            .zip(glyph_buffer.glyph_positions())
            .map(|(info, position)| ShapedGlyph {
                key: GlyphCacheKey::new(self.id, info.glyph_id as u16, size, bold, italic),
                cluster: cluster_offset.saturating_add(info.cluster),
                x_advance: position.x_advance as f32 * scale,
                y_advance: position.y_advance as f32 * scale,
                x_offset: position.x_offset as f32 * scale,
                y_offset: position.y_offset as f32 * scale,
            })
            .collect())
    }

    #[must_use]
    pub fn metrics(&self, size: f32, line_height: f32) -> CellMetrics {
        let scaled = self.font.as_scaled(self.ab_glyph_scale(size));
        let ascent = scaled.ascent();
        let descent = scaled.descent();
        let line_gap = scaled.line_gap();
        let cell_height = ((ascent - descent + line_gap) * line_height)
            .ceil()
            .max(1.0);
        let zero_width = scaled.h_advance(self.font.glyph_id('0')).max(1.0);
        let baseline = ((cell_height - (ascent - descent)) * 0.5 + ascent).clamp(0.0, cell_height);
        let (underline_offset, strikeout_offset, stroke_size) = self.decoration_metrics(size);
        let decoration_thickness = stroke_size.abs().max(1.0).min(cell_height);
        let maximum_decoration_y = (cell_height - decoration_thickness).max(0.0);
        let underline_position = (baseline - underline_offset).clamp(0.0, maximum_decoration_y);
        let strikethrough_position = (baseline - strikeout_offset).clamp(0.0, maximum_decoration_y);

        CellMetrics {
            font_size: size,
            cell_width: zero_width,
            cell_height,
            ascent,
            descent,
            line_gap,
            baseline,
            underline_position,
            strikethrough_position,
            decoration_thickness,
        }
    }

    #[must_use]
    pub fn rasterize(&self, glyph_id: u16, size: f32) -> GlyphBitmap {
        let scaled = self.font.as_scaled(self.ab_glyph_scale(size));
        let glyph_id = GlyphId(glyph_id);
        let advance_width = scaled.h_advance(glyph_id).max(0.0);
        let ascent = scaled.ascent();

        if let Some(font) = SwashFontRef::from_index(&self.bytes, self.face_index as usize) {
            let mut context = ScaleContext::new();
            let mut scaler = context.builder(font).size(size).hint(true).build();
            if let Some(image) = SwashRender::new(&[
                Source::ColorOutline(0),
                Source::ColorBitmap(StrikeWith::BestFit),
                Source::Outline,
            ])
            .format(Format::Alpha)
            .render(&mut scaler, glyph_id.0)
                && image.placement.width > 0
                && image.placement.height > 0
            {
                let format = match image.content {
                    Content::Color => GlyphBitmapFormat::Rgba,
                    Content::Mask | Content::SubpixelMask => GlyphBitmapFormat::Alpha,
                };
                let mut pixels = if image.content == Content::SubpixelMask {
                    image
                        .data
                        .chunks_exact(4)
                        .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]).max(pixel[3]))
                        .collect()
                } else {
                    image.data
                };
                if matches!(image.source, Source::ColorOutline(_)) {
                    unpremultiply_rgba(&mut pixels);
                }
                return GlyphBitmap {
                    width: image.placement.width,
                    height: image.placement.height,
                    offset_x: image.placement.left,
                    offset_y: -image.placement.top,
                    advance_width,
                    pixels,
                    format,
                };
            }
        }

        if let Some(image) = self
            .font
            .glyph_raster_image2(glyph_id, size.round().clamp(1.0, u16::MAX as f32) as u16)
            && let Some(bitmap) = raster_image_to_bitmap(&image, advance_width, size, ascent)
        {
            return bitmap;
        }

        let glyph = glyph_id.with_scale_and_position(self.ab_glyph_scale(size), point(0.0, 0.0));

        let Some(outlined) = self.font.outline_glyph(glyph) else {
            if glyph_id != GlyphId(0) {
                return GlyphBitmap {
                    width: 1,
                    height: 1,
                    offset_x: 0,
                    offset_y: -ascent.round() as i32,
                    advance_width,
                    pixels: vec![0],
                    format: GlyphBitmapFormat::Alpha,
                };
            }
            let mut missing =
                GlyphBitmap::missing(advance_width, (ascent - scaled.descent()).ceil() as u32);
            missing.offset_y = -ascent.round() as i32;
            return missing;
        };

        let bounds = outlined.px_bounds();
        let width = bounds.width().ceil().max(1.0) as u32;
        let height = bounds.height().ceil().max(1.0) as u32;
        let mut pixels = vec![0; (width * height) as usize];

        outlined.draw(|x, y, coverage| {
            let index = (y * width + x) as usize;
            pixels[index] = (coverage * 255.0).round() as u8;
        });

        GlyphBitmap {
            width,
            height,
            offset_x: bounds.min.x.floor() as i32,
            offset_y: bounds.min.y.floor() as i32,
            advance_width,
            pixels,
            format: GlyphBitmapFormat::Alpha,
        }
    }

    fn ab_glyph_scale(&self, pixels_per_em: f32) -> PxScale {
        let units_per_em = self
            .font
            .units_per_em()
            .unwrap_or_else(|| self.font.height_unscaled())
            .max(1.0);
        PxScale::from(pixels_per_em * self.font.height_unscaled().max(1.0) / units_per_em)
    }

    fn design_unit_scale(&self, pixels_per_em: f32) -> f32 {
        pixels_per_em
            / self
                .font
                .units_per_em()
                .unwrap_or_else(|| self.font.height_unscaled())
                .max(1.0)
    }

    fn decoration_metrics(&self, pixels_per_em: f32) -> (f32, f32, f32) {
        let fallback_stroke = (pixels_per_em / 14.0).max(1.0);
        let fallback_underline = -fallback_stroke;
        let fallback_strikeout = self
            .font
            .as_scaled(self.ab_glyph_scale(pixels_per_em))
            .ascent()
            * 0.35;
        let Some(font) = SwashFontRef::from_index(&self.bytes, self.face_index as usize) else {
            return (fallback_underline, fallback_strikeout, fallback_stroke);
        };
        let metrics = font
            .metrics(&[])
            .linear_scale(self.design_unit_scale(pixels_per_em));
        (
            if metrics.underline_offset == 0.0 {
                fallback_underline
            } else {
                metrics.underline_offset
            },
            if metrics.strikeout_offset == 0.0 {
                fallback_strikeout
            } else {
                metrics.strikeout_offset
            },
            if metrics.stroke_size == 0.0 {
                fallback_stroke
            } else {
                metrics.stroke_size
            },
        )
    }
}

fn is_default_ignorable(ch: char) -> bool {
    matches!(ch, '\u{200c}' | '\u{200d}' | '\u{fe0e}' | '\u{fe0f}')
        || ('\u{e0100}'..='\u{e01ef}').contains(&ch)
}

fn raster_image_to_bitmap(
    image: &ab_glyph::v2::GlyphImage<'_>,
    advance_width: f32,
    requested_size: f32,
    ascent: f32,
) -> Option<GlyphBitmap> {
    let strike_scale = requested_size / f32::from(image.pixels_per_em.max(1));
    let (width, height, pixels) = match &image.format {
        GlyphImageFormat::Png => {
            let decoded = image::load_from_memory(image.data).ok()?.to_rgba8();
            let width = (decoded.width() as f32 * strike_scale).round().max(1.0) as u32;
            let height = (decoded.height() as f32 * strike_scale).round().max(1.0) as u32;
            let resized = if decoded.width() == width && decoded.height() == height {
                decoded
            } else {
                image::imageops::resize(&decoded, width, height, FilterType::Triangle)
            };
            (width, height, resized.into_raw())
        }
        GlyphImageFormat::BitmapPremulBgra32 => {
            let mut rgba = Vec::with_capacity(image.data.len());
            for pixel in image.data.chunks_exact(4) {
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            }
            unpremultiply_rgba(&mut rgba);
            (u32::from(image.width), u32::from(image.height), rgba)
        }
        _ => return None,
    };

    Some(GlyphBitmap {
        width,
        height,
        offset_x: (image.origin.x * strike_scale).round() as i32,
        offset_y: (image.origin.y * strike_scale - ascent).round() as i32,
        advance_width,
        pixels,
        format: GlyphBitmapFormat::Rgba,
    })
}

fn unpremultiply_rgba(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * 255) / alpha).min(255) as u8;
        }
    }
}

#[derive(Debug)]
pub struct GlyphCache {
    capacity: usize,
    entries: HashMap<GlyphCacheKey, Arc<GlyphBitmap>>,
    order: VecDeque<GlyphCacheKey>,
}

impl GlyphCache {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn get_or_insert_with(
        &mut self,
        key: GlyphCacheKey,
        make: impl FnOnce() -> GlyphBitmap,
    ) -> Arc<GlyphBitmap> {
        if let Some(bitmap) = self.entries.get(&key).cloned() {
            self.touch(key);
            return bitmap;
        }

        while self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }

        let bitmap = Arc::new(make());
        self.entries.insert(key, Arc::clone(&bitmap));
        self.order.push_back(key);
        bitmap
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn contains_key(&self, key: GlyphCacheKey) -> bool {
        self.entries.contains_key(&key)
    }

    fn touch(&mut self, key: GlyphCacheKey) {
        self.order.retain(|entry| *entry != key);
        self.order.push_back(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_chain_preserves_configured_order() {
        let fonts = FontSystem::new(FontConfig {
            family: "Primary".to_owned(),
            fallback_families: vec!["Fallback".to_owned()],
            ..FontConfig::default()
        });

        let chain = fonts.resolve_fallback_chain();
        assert_eq!(chain.primary.family, "Primary");
        assert_eq!(chain.fallbacks[0].family, "Fallback");
    }

    #[test]
    fn glyph_cache_evicts_oldest_entry() {
        let mut cache = GlyphCache::new(2);
        let key_a = GlyphCacheKey::new(1, u16::from(b'a'), 13.0, false, false);
        let key_b = GlyphCacheKey::new(1, u16::from(b'b'), 13.0, false, false);
        let key_c = GlyphCacheKey::new(1, u16::from(b'c'), 13.0, false, false);

        cache.get_or_insert_with(key_a, || GlyphBitmap::missing(8.0, 12));
        cache.get_or_insert_with(key_b, || GlyphBitmap::missing(8.0, 12));
        cache.get_or_insert_with(key_c, || GlyphBitmap::missing(8.0, 12));

        assert_eq!(cache.len(), 2);
        assert!(!cache.entries.contains_key(&key_a));
        assert!(cache.entries.contains_key(&key_b));
        assert!(cache.entries.contains_key(&key_c));
    }

    #[test]
    fn default_font_metrics_are_positive_when_system_font_exists() {
        let mut fonts = FontSystem::new(FontConfig::default());
        let Ok(metrics) = fonts.cell_metrics() else {
            return;
        };

        assert!(metrics.cell_width > 0.0);
        assert!(metrics.cell_height > 0.0);
    }

    #[test]
    fn typographic_size_uses_pixels_per_em_for_font_metrics() {
        let mut fonts = FontSystem::new(FontConfig {
            size: 13.0,
            line_height: 1.0,
            ..FontConfig::default()
        });
        let pixels_per_em = fonts.physical_font_size();
        let expected_ascent = {
            let font = fonts.primary_font().expect("primary font");
            let units_per_em = font.font.units_per_em().expect("units per em");
            let scale = PxScale::from(pixels_per_em * font.font.height_unscaled() / units_per_em);
            font.font.as_scaled(scale).ascent()
        };
        let metrics = fonts.cell_metrics().expect("cell metrics");

        assert!(
            (metrics.ascent - expected_ascent).abs() <= f32::EPSILON,
            "font metrics must interpret configured points as pixels per em: actual={}, expected={expected_ascent}",
            metrics.ascent
        );
    }

    #[test]
    fn line_height_centers_primary_font_box_around_the_baseline() {
        for scale_factor in [1.0, 1.25, 1.5] {
            for line_height in [1.0, 1.2, 1.5] {
                let mut fonts = FontSystem::new_with_scale_factor(
                    FontConfig {
                        line_height,
                        ..FontConfig::default()
                    },
                    scale_factor,
                );
                let Ok(metrics) = fonts.cell_metrics() else {
                    return;
                };
                let leading_above = metrics.baseline - metrics.ascent;
                let leading_below = metrics.cell_height - (metrics.baseline - metrics.descent);

                assert!(
                    (leading_above - leading_below).abs() <= 0.51,
                    "line-height leading must be centered: scale={scale_factor}, line_height={line_height}, above={leading_above}, below={leading_below}"
                );
            }
        }
    }

    #[test]
    fn rasterized_glyph_offsets_are_relative_to_a_shared_baseline() {
        let mut fonts = FontSystem::new(FontConfig::default());
        let Ok(run) = fonts.shape_text("A", false, false) else {
            return;
        };
        let bitmap = fonts
            .rasterize_glyph(run.glyphs[0].key)
            .expect("rasterized glyph");

        assert!(
            bitmap.offset_y < 0,
            "an ordinary glyph top must be above the shared baseline, got {}",
            bitmap.offset_y
        );
    }

    #[test]
    fn point_sizes_scale_to_physical_pixels() {
        let config = FontConfig {
            size: 12.0,
            ..FontConfig::default()
        };
        let fonts = FontSystem::new_with_scale_factor(config, 1.5);

        assert_eq!(fonts.physical_font_size(), 24.0);
        assert_eq!(fonts.scale_factor(), 1.5);
    }

    #[test]
    fn scale_factor_changes_font_generation() {
        let mut fonts = FontSystem::new(FontConfig::default());
        let initial = fonts.generation_id();

        assert!(fonts.set_scale_factor(2.0));
        assert_ne!(fonts.generation_id(), initial);
        assert!(!fonts.set_scale_factor(2.0));
    }

    #[test]
    fn invalid_scale_factor_uses_one() {
        let fonts = FontSystem::new_with_scale_factor(FontConfig::default(), f64::NAN);

        assert_eq!(fonts.scale_factor(), 1.0);
    }

    #[test]
    fn generic_monospace_uses_portable_preferred_families_first() {
        let fonts = FontSystem::new(FontConfig::default());
        let requested = fonts.requested_families();

        assert_eq!(requested.first().map(String::as_str), Some("Cascadia Mono"));
        assert_eq!(requested.last().map(String::as_str), Some("monospace"));
    }

    #[test]
    fn shaping_preserves_clusters_and_rasterizes_selected_glyphs() {
        let mut fonts = FontSystem::new(FontConfig::default());
        let Ok(run) = fonts.shape_text("ffi e\u{301}", false, false) else {
            return;
        };
        assert!(!run.glyphs.is_empty());
        assert!(run.advance_width > 0.0);
        assert!(!run.families_used.is_empty());
        assert!(
            run.glyphs
                .windows(2)
                .all(|pair| pair[0].cluster <= pair[1].cluster)
        );
        for glyph in run.glyphs {
            let bitmap = fonts.rasterize_glyph(glyph.key).unwrap();
            assert!(bitmap.width > 0);
            assert!(bitmap.height > 0);
        }
    }

    #[test]
    fn primary_monospace_shape_advance_matches_cell_metrics() {
        let mut fonts = FontSystem::new_with_scale_factor(FontConfig::default(), 1.25);
        let metrics = fonts.cell_metrics().expect("cell metrics");
        let run = fonts
            .shape_text("panea-grid-cursor-check", false, false)
            .expect("shape text");
        let expected = metrics.cell_width * 23.0;

        assert!(
            (run.advance_width - expected).abs() <= 0.5,
            "terminal text advance drifted from the cell grid: shaped={}, expected={}, cell_width={}",
            run.advance_width,
            expected,
            metrics.cell_width
        );
    }

    #[test]
    fn fallback_selection_handles_cjk_and_emoji_without_losing_text() {
        let mut fonts = FontSystem::new(FontConfig {
            fallback_families: vec![
                "Segoe UI Emoji".to_owned(),
                "Apple Color Emoji".to_owned(),
                "Noto Color Emoji".to_owned(),
                "Noto Sans CJK JP".to_owned(),
            ],
            ..FontConfig::default()
        });
        let Ok(run) = fonts.shape_text("A界👍🏽👨‍👩‍👧‍👦", false, false)
        else {
            return;
        };
        assert!(!run.glyphs.is_empty());
        assert!(run.glyphs.iter().all(|glyph| glyph.key.font_id != 0));
    }

    #[test]
    fn fallback_glyph_bitmaps_use_the_primary_cell_baseline_contract() {
        let mut fonts = FontSystem::new(FontConfig {
            fallback_families: vec![
                "Segoe UI Emoji".to_owned(),
                "Apple Color Emoji".to_owned(),
                "Noto Color Emoji".to_owned(),
                "Noto Sans CJK JP".to_owned(),
            ],
            ..FontConfig::default()
        });
        let Ok(metrics) = fonts.cell_metrics() else {
            return;
        };
        let Ok(run) = fonts.shape_text("A\u{754c}\u{1f600}", false, false) else {
            return;
        };

        for glyph in run.glyphs {
            let bitmap = fonts
                .rasterize_glyph(glyph.key)
                .expect("rasterized fallback glyph");
            let top = metrics.baseline + bitmap.offset_y as f32;
            let bottom = top + bitmap.height as f32;
            assert!(
                top < metrics.cell_height && bottom > 0.0,
                "fallback glyph must intersect the primary terminal row: top={top}, bottom={bottom}, cell_height={}",
                metrics.cell_height
            );
        }
    }

    #[test]
    fn diagnostics_report_style_faces_and_unresolved_fallbacks() {
        let fonts = FontSystem::new(FontConfig {
            fallback_families: vec!["Panea Definitely Missing Font".to_owned()],
            ..FontConfig::default()
        });
        let diagnostics = fonts.diagnostics();
        assert!(diagnostics.iter().any(|item| item.role == "bold-face"));
        assert!(diagnostics.iter().any(|item| item.role == "italic-face"));
        assert!(
            diagnostics
                .iter()
                .any(|item| { item.family == "Panea Definitely Missing Font" && !item.resolved })
        );
    }

    #[test]
    fn color_outline_pixels_are_converted_to_straight_alpha() {
        let mut pixels = vec![64, 32, 16, 128, 0, 0, 0, 0];
        unpremultiply_rgba(&mut pixels);
        assert_eq!(&pixels[..4], &[127, 63, 31, 128]);
        assert_eq!(&pixels[4..], &[0, 0, 0, 0]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_color_emoji_fallback_rasterizes_rgba() {
        let mut fonts = FontSystem::new(FontConfig::default());
        let run = fonts.shape_text("😀", false, false).unwrap();
        assert!(
            run.families_used
                .iter()
                .any(|family| family == "Segoe UI Emoji")
        );
        assert!(run.glyphs.into_iter().any(|glyph| {
            fonts
                .rasterize_glyph(glyph.key)
                .is_ok_and(|bitmap| bitmap.format == GlyphBitmapFormat::Rgba)
        }));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_nerd_font_powerline_glyphs_resolve_and_rasterize() {
        let mut fonts = FontSystem::new(FontConfig {
            family: "CaskaydiaCove NF".to_owned(),
            ..FontConfig::default()
        });
        let configured_font_available =
            fonts.resolve_fallback_chain().primary.source != FontSource::Unresolved;
        let metrics = fonts.cell_metrics().expect("configured font metrics");
        for sample in ["\u{e0b6}", "\u{e62a}", "\u{e0b4}", "\u{e725}"] {
            let run = fonts
                .shape_text(sample, false, false)
                .expect("shape powerline glyph");
            assert_eq!(run.glyphs.len(), 1);
            if configured_font_available {
                assert!(
                    run.families_used
                        .iter()
                        .any(|family| family == "CaskaydiaCove NF")
                );
            }
            assert!((run.advance_width - metrics.cell_width).abs() < 0.01);

            let glyph = run.glyphs[0];
            let bitmap = fonts.rasterize_glyph(glyph.key).unwrap();
            assert!(bitmap.width > 0 && bitmap.height > 0);
            if configured_font_available {
                assert!(glyph.key.glyph_id != 0);
                assert!(bitmap.pixels.iter().any(|pixel| *pixel != 0));
            }
        }
    }
}
