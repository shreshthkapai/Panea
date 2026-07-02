//! Font discovery, fallback, glyph rasterization, and cache policy.

pub const LAYER: &str = "render performance";

use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    fmt, fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
};

use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};

#[derive(Debug, Clone, PartialEq)]
pub struct FontConfig {
    pub family: String,
    pub fallback_families: Vec<String>,
    pub size: f32,
    pub line_height: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "monospace".to_owned(),
            fallback_families: Vec::new(),
            size: 13.0,
            line_height: 1.2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontDescriptor {
    pub family: String,
    pub source: FontSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphCacheKey {
    pub font_id: u64,
    pub ch: char,
    pub size_millipoints: u32,
    pub bold: bool,
    pub italic: bool,
}

impl GlyphCacheKey {
    #[must_use]
    pub fn new(font_id: u64, ch: char, size: f32, bold: bool, italic: bool) -> Self {
        Self {
            font_id,
            ch,
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
        }
    }
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
    primary: Option<LoadedFont>,
}

impl FontSystem {
    #[must_use]
    pub fn new(config: FontConfig) -> Self {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();

        Self {
            database,
            config,
            primary: None,
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

    pub fn primary_font(&mut self) -> Result<&LoadedFont, FontError> {
        if self.primary.is_none() {
            self.primary = Some(self.load_primary()?);
        }

        Ok(self.primary.as_ref().expect("primary font is initialized"))
    }

    pub fn cell_metrics(&mut self) -> Result<CellMetrics, FontError> {
        let size = self.config.size;
        let line_height = self.config.line_height;
        self.primary_font()
            .map(|font| font.metrics(size, line_height))
    }

    pub fn rasterize_glyph(&mut self, key: GlyphCacheKey) -> Result<GlyphBitmap, FontError> {
        let size = key.size_millipoints as f32 / 1000.0;
        self.primary_font().map(|font| font.rasterize(key.ch, size))
    }

    fn load_primary(&self) -> Result<LoadedFont, FontError> {
        let requested = self.requested_families();

        for family in &requested {
            if let Some(loaded) = self.load_family(family) {
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
        requested.push(self.config.family.clone());
        requested.extend(self.config.fallback_families.clone());
        requested.extend([
            "Cascadia Mono".to_owned(),
            "Consolas".to_owned(),
            "Menlo".to_owned(),
            "DejaVu Sans Mono".to_owned(),
            "monospace".to_owned(),
        ]);
        requested.dedup();
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

    fn load_family(&self, family: &str) -> Option<Result<LoadedFont, String>> {
        let families = family_query(family);
        let query = fontdb::Query {
            families: &families,
            ..fontdb::Query::default()
        };
        let id = self.database.query(&query)?;
        let face = self.database.face(id)?;
        let bytes = match &face.source {
            fontdb::Source::File(path) => fs::read(path).map_err(|err| err.to_string()),
            fontdb::Source::SharedFile(path, _) => fs::read(path).map_err(|err| err.to_string()),
            fontdb::Source::Binary(bytes) => Ok(bytes.as_ref().as_ref().to_vec()),
        };

        Some(bytes.and_then(|bytes| {
            let font = FontArc::try_from_vec(bytes).map_err(|err| err.to_string())?;
            Ok(LoadedFont::new(family.to_owned(), font))
        }))
    }
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
}

impl LoadedFont {
    fn new(family: String, font: FontArc) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        family.hash(&mut hasher);

        Self {
            id: hasher.finish(),
            family,
            font,
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
    pub fn metrics(&self, size: f32, line_height: f32) -> CellMetrics {
        let scaled = self.font.as_scaled(PxScale::from(size));
        let ascent = scaled.ascent();
        let descent = scaled.descent();
        let line_gap = scaled.line_gap();
        let cell_height = ((ascent - descent + line_gap) * line_height)
            .ceil()
            .max(1.0);
        let zero_width = scaled.h_advance(self.font.glyph_id('0')).ceil().max(1.0);

        CellMetrics {
            font_size: size,
            cell_width: zero_width,
            cell_height,
            ascent,
            descent,
            line_gap,
        }
    }

    #[must_use]
    pub fn rasterize(&self, ch: char, size: f32) -> GlyphBitmap {
        let scaled = self.font.as_scaled(PxScale::from(size));
        let glyph_id = self.font.glyph_id(ch);
        let advance_width = scaled.h_advance(glyph_id).ceil().max(1.0);
        let ascent = scaled.ascent();
        let glyph = glyph_id.with_scale_and_position(PxScale::from(size), point(0.0, ascent));

        let Some(outlined) = self.font.outline_glyph(glyph) else {
            return GlyphBitmap::missing(advance_width, (ascent - scaled.descent()).ceil() as u32);
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
        let key_a = GlyphCacheKey::new(1, 'a', 13.0, false, false);
        let key_b = GlyphCacheKey::new(1, 'b', 13.0, false, false);
        let key_c = GlyphCacheKey::new(1, 'c', 13.0, false, false);

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
}
