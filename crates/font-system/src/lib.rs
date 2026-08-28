//! Font discovery, fallback, glyph rasterization, and cache policy.

pub const LAYER: &str = "render performance";

use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    error::Error,
    fmt, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
};

use ab_glyph::{Font, FontRef, GlyphId, GlyphImageFormat, PxScale, ScaleFont, point};
use image::imageops::FilterType;
use self_cell::self_cell;
use swash::{
    FontRef as SwashFontRef,
    scale::{Render as SwashRender, ScaleContext, Source, StrikeWith, image::Content},
    zeno::Format,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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
            ligatures: false,
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

/// System font discovery, with the scan deferred as long as possible.
///
/// `fontdb::Database::load_system_fonts` parses the name tables of every
/// installed font. On a cold file cache that is seconds of startup, and a
/// terminal needs three or four faces out of several hundred. So a catalog can
/// also be built from the handful of files that satisfied the last run, and it
/// promotes itself to a full scan the first time a query misses.
struct FontCatalog {
    database: RwLock<fontdb::Database>,
    fully_scanned: AtomicBool,
    resolved_files: Mutex<BTreeSet<PathBuf>>,
    file_bytes: Mutex<HashMap<FontDataKey, Arc<[u8]>>>,
    parsed_faces: Mutex<HashMap<ParsedFaceKey, Arc<OwnedFontFaces>>>,
}

impl fmt::Debug for FontCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FontCatalog")
            .field("database", &self.database)
            .field("file_bytes", &self.file_bytes)
            .field(
                "parsed_face_count",
                &self.parsed_faces.lock().map(|faces| faces.len()),
            )
            .finish_non_exhaustive()
    }
}

impl FontCatalog {
    fn discover() -> Self {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        Self::with_database(database, true)
    }

    /// Builds a catalog holding only the given font files.
    ///
    /// Files that no longer exist or no longer parse are skipped, which is what
    /// makes a stale cache harmless: the entries that survive still answer, and
    /// anything missing turns into a miss, and a miss triggers the full scan.
    fn from_font_files(paths: &[PathBuf]) -> Self {
        let mut database = fontdb::Database::new();
        for path in paths {
            let _ = database.load_font_file(path);
        }
        Self::with_database(database, false)
    }

    fn with_database(database: fontdb::Database, fully_scanned: bool) -> Self {
        Self {
            database: RwLock::new(database),
            fully_scanned: AtomicBool::new(fully_scanned),
            resolved_files: Mutex::new(BTreeSet::new()),
            file_bytes: Mutex::new(HashMap::new()),
            parsed_faces: Mutex::new(HashMap::new()),
        }
    }

    fn is_fully_scanned(&self) -> bool {
        self.fully_scanned.load(Ordering::Acquire)
    }

    /// Loads every installed font, once, however many queries race here.
    fn ensure_fully_scanned(&self) {
        if self.is_fully_scanned() {
            return;
        }
        let Ok(mut database) = self.database.write() else {
            return;
        };
        if self.is_fully_scanned() {
            return;
        }
        database.load_system_fonts();
        self.fully_scanned.store(true, Ordering::Release);
    }

    /// Answers a face query, scanning the system only if the query misses.
    ///
    /// `fontdb::query` returns its best match rather than nothing, so a database
    /// holding only last run's files would happily answer a bold request with a
    /// regular face and never scan for the real one. While the catalog is still
    /// partial, a match therefore has to genuinely satisfy the request; anything
    /// weaker counts as a miss and promotes to the full scan.
    fn with_face<T>(
        &self,
        query: &fontdb::Query<'_>,
        extract: impl Fn(&fontdb::FaceInfo) -> T,
    ) -> Option<T> {
        if let Some(found) = self.lookup(query, &extract, !self.is_fully_scanned()) {
            return Some(found);
        }
        if self.is_fully_scanned() {
            return None;
        }
        self.ensure_fully_scanned();
        self.lookup(query, &extract, false)
    }

    fn lookup<T>(
        &self,
        query: &fontdb::Query<'_>,
        extract: &impl Fn(&fontdb::FaceInfo) -> T,
        require_exact: bool,
    ) -> Option<T> {
        let database = self.database.read().ok()?;
        let id = database.query(query)?;
        let face = database.face(id)?;
        if require_exact && !face_satisfies(face, query) {
            return None;
        }
        Some(extract(face))
    }

    /// Remembers a file a face was resolved from, so the next run can start
        // from it instead of scanning.
    fn record_resolved_file(&self, path: &Path) {
        if let Ok(mut resolved) = self.resolved_files.lock() {
            resolved.insert(path.to_path_buf());
        }
    }

    fn resolved_files(&self) -> Vec<PathBuf> {
        self.resolved_files
            .lock()
            .map(|resolved| resolved.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// The first family name a loaded font file reports, for tests.
    #[cfg(test)]
    fn first_family_name(&self, path: &Path) -> Option<String> {
        let database = self.database.read().ok()?;
        database.faces().find_map(|face| {
            let matches = match &face.source {
                fontdb::Source::File(candidate) | fontdb::Source::SharedFile(candidate, _) => {
                    candidate == path
                }
                fontdb::Source::Binary(_) => false,
            };
            matches.then(|| face.families.first().map(|(name, _)| name.clone()))?
        })
    }

    /// The file backing a family    /// The file backing a family's regular face, for tests and diagnostics.
    #[cfg(test)]
    fn face_source_path(&self, family: &str) -> Option<PathBuf> {
        let families = family_query(family);
        let query = fontdb::Query {
            families: &families,
            ..fontdb::Query::default()
        };
        self.with_face(&query, |face| match &face.source {
            fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
                Some(path.clone())
            }
            fontdb::Source::Binary(_) => None,
        })
        .flatten()
    }

    fn bytes_for_face(&self, face: &ResolvedFace) -> Result<Arc<[u8]>, String> {
        let mut file_bytes = self
            .file_bytes
            .lock()
            .map_err(|_| "font byte cache lock was poisoned".to_owned())?;
        if let Some(bytes) = file_bytes.get(&face.data_key) {
            return Ok(Arc::clone(bytes));
        }

        let bytes: Arc<[u8]> = match (&face.path, &face.binary) {
            (Some(path), _) => Arc::from(fs::read(path).map_err(|error| error.to_string())?),
            (_, Some(bytes)) => Arc::from(bytes.as_ref().as_ref()),
            _ => return Err("resolved font face has no readable source".to_owned()),
        };
        file_bytes.insert(face.data_key.clone(), Arc::clone(&bytes));
        Ok(bytes)
    }

    /// Returns an already-parsed face without touching the filesystem.
    ///
    /// Used to decide whether a fallback can be finished inline or has to be
    /// handed to the loader thread.
    fn cached_parsed_faces(&self, face: &ResolvedFace) -> Option<Arc<OwnedFontFaces>> {
        let key = ParsedFaceKey {
            data: face.data_key.clone(),
            face_index: face.face_index,
        };
        self.parsed_faces.lock().ok()?.get(&key).cloned()
    }

    fn parsed_faces_for(&self, face: &ResolvedFace) -> Result<Arc<OwnedFontFaces>, String> {
        let key = ParsedFaceKey {
            data: face.data_key.clone(),
            face_index: face.face_index,
        };
        if let Some(parsed) = self
            .parsed_faces
            .lock()
            .map_err(|_| "parsed font face cache lock was poisoned".to_owned())?
            .get(&key)
            .cloned()
        {
            return Ok(parsed);
        }

        let parsed = Arc::new(build_parsed_faces(
            self.bytes_for_face(face)?,
            face.face_index,
        )?);
        let mut parsed_faces = self
            .parsed_faces
            .lock()
            .map_err(|_| "parsed font face cache lock was poisoned".to_owned())?;
        Ok(Arc::clone(parsed_faces.entry(key).or_insert(parsed)))
    }
}

/// In-flight fallback loads. Small: a terminal needs a handful of fallback
/// families at most, and a full queue simply falls back to loading inline.
const FONT_LOAD_QUEUE_DEPTH: usize = 8;

/// Callback used to nudge a host event loop once a fallback font has finished
/// loading, so the frame that fell back to tofu can be drawn again.
#[derive(Clone)]
pub struct FontLoadWaker(Arc<dyn Fn() + Send + Sync + 'static>);

impl FontLoadWaker {
    #[must_use]
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(wake))
    }

    fn wake(&self) {
        (self.0)();
    }
}

impl fmt::Debug for FontLoadWaker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FontLoadWaker(..)")
    }
}

struct FontLoadRequest {
    family: Arc<str>,
    face: ResolvedFace,
    bold: bool,
    italic: bool,
}

struct FontLoadResponse {
    key: (Arc<str>, bool, bool),
    font: Result<Arc<LoadedFont>, String>,
}

/// Reads and parses fallback faces off the UI thread.
///
/// A fallback family is only discovered when text needs it, and reading plus
/// parsing a CJK or emoji font is tens of megabytes of work — a visible stall if
/// it happens inside the frame that first prints such a character. Requests are
/// served on a worker; the frame in flight falls back to the primary face and is
/// redrawn when the real one arrives.
#[derive(Debug)]
struct FontLoader {
    requests: Option<SyncSender<FontLoadRequest>>,
    responses: Receiver<FontLoadResponse>,
    worker: Option<JoinHandle<()>>,
    waker: Arc<OnceLock<FontLoadWaker>>,
}

impl FontLoader {
    fn spawn(catalog: Arc<FontCatalog>) -> Self {
        let (request_tx, request_rx) = mpsc::sync_channel::<FontLoadRequest>(FONT_LOAD_QUEUE_DEPTH);
        let (response_tx, response_rx) = mpsc::channel();
        let waker: Arc<OnceLock<FontLoadWaker>> = Arc::new(OnceLock::new());
        let worker_waker = Arc::clone(&waker);

        let worker = thread::Builder::new()
            .name("panea-font-loader".to_owned())
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    let font = catalog.parsed_faces_for(&request.face).map(|faces| {
                        Arc::new(LoadedFont::new(
                            request.family.to_string(),
                            faces,
                            request.face.face_index,
                            request.face.source.clone(),
                            request.face.bold,
                            request.face.italic,
                        ))
                    });
                    let response = FontLoadResponse {
                        key: (request.family, request.bold, request.italic),
                        font,
                    };
                    if response_tx.send(response).is_err() {
                        break;
                    }
                    if let Some(waker) = worker_waker.get() {
                        waker.wake();
                    }
                }
            })
            .ok();

        Self {
            requests: worker.is_some().then_some(request_tx),
            responses: response_rx,
            worker,
            waker,
        }
    }

    /// Queues a load. Returns false when the worker is gone or its queue is
    /// full, in which case the caller falls back to loading inline.
    fn request(&self, request: FontLoadRequest) -> bool {
        self.requests
            .as_ref()
            .is_some_and(|requests| requests.try_send(request).is_ok())
    }
}

impl Drop for FontLoader {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub struct FontSystem {
    catalog: Arc<FontCatalog>,
    config: FontConfig,
    scale_factor: f32,
    primary: Option<Arc<LoadedFont>>,
    loaded: HashMap<u64, Arc<LoadedFont>>,
    attempted_faces: HashSet<(Arc<str>, bool, bool)>,
    /// Resolved family search order. Rebuilding it allocated roughly fifteen
    /// strings plus a set on every grapheme that needed font resolution.
    families: Arc<[Arc<str>]>,
    /// Common single-scalar graphemes resolve once per style. Multi-scalar
    /// graphemes still validate the complete sequence before choosing a face.
    character_fonts: HashMap<(char, bool, bool), u64>,
    /// Metrics keyed by the physical size and line height they were measured
    /// at; this is read every frame and on every mouse event.
    metrics: Option<((u32, u32), CellMetrics)>,
    loader: FontLoader,
    /// Fallback faces handed to the loader and not yet collected.
    pending_faces: HashSet<(Arc<str>, bool, bool)>,
    /// Bumped whenever a fallback arrives, so glyph and shaped-run caches keyed
    /// on the font generation are invalidated.
    load_generation: u64,
}

impl FontSystem {
    #[must_use]
    pub fn new(config: FontConfig) -> Self {
        Self::new_with_scale_factor(config, 1.0)
    }

    #[must_use]
    pub fn new_with_scale_factor(config: FontConfig, scale_factor: f64) -> Self {
        Self::with_catalog(config, scale_factor, Arc::new(FontCatalog::discover()))
    }

    fn with_catalog(config: FontConfig, scale_factor: f64, catalog: Arc<FontCatalog>) -> Self {
        let families = requested_families(&config).into();
        let catalog_for_loader = Arc::clone(&catalog);
        Self {
            catalog,
            config,
            scale_factor: normalized_scale_factor(scale_factor),
            primary: None,
            loaded: HashMap::new(),
            attempted_faces: HashSet::new(),
            families,
            character_fonts: HashMap::new(),
            metrics: None,
            loader: FontLoader::spawn(catalog_for_loader),
            pending_faces: HashSet::new(),
            load_generation: 0,
        }
    }

    /// The font files this view actually resolved a face from.
    ///
    /// Persisting these lets the next launch build its catalog from them and
    /// skip the system scan entirely.
    #[must_use]
    pub fn resolved_font_files(&self) -> Vec<PathBuf> {
        self.catalog.resolved_files()
    }

    /// Builds a view whose catalog starts from previously resolved font files.
    #[must_use]
    pub fn with_font_files(config: FontConfig, scale_factor: f64, files: &[PathBuf]) -> Self {
        Self::with_catalog(
            config,
            scale_factor,
            Arc::new(FontCatalog::from_font_files(files)),
        )
    }

    /// Builds a new configured font view while reusing the expensive system
    /// discovery result and any font files already loaded by the old view.
    #[must_use]
    pub fn reconfigured(&self, config: FontConfig, scale_factor: f64) -> Self {
        let scale_factor = normalized_scale_factor(scale_factor);
        let same_families = self.config.family == config.family
            && self.config.fallback_families == config.fallback_families;
        let same_metrics = same_families
            && self.config.size.to_bits() == config.size.to_bits()
            && self.config.line_height.to_bits() == config.line_height.to_bits()
            && self.scale_factor.to_bits() == scale_factor.to_bits();
        let mut reconfigured =
            Self::with_catalog(config, f64::from(scale_factor), Arc::clone(&self.catalog));
        if same_families {
            reconfigured.primary = self.primary.clone();
            reconfigured.loaded = self.loaded.clone();
            reconfigured.attempted_faces = self.attempted_faces.clone();
            reconfigured.families = Arc::clone(&self.families);
            reconfigured.character_fonts = self.character_fonts.clone();
        }
        if same_metrics {
            reconfigured.metrics = self.metrics;
        }
        reconfigured
    }

    pub fn resolve_fallback_chain(&self) -> FontFallbackChain {
        let descriptors = self
            .families
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
        // Fallbacks arrive after the frame that needed them, so the generation
        // has to change or caches would keep serving the tofu they recorded.
        self.load_generation.hash(&mut hasher);
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
            self.loaded.insert(font.id(), Arc::clone(&font));
            self.primary = Some(font);
        }

        Ok(self
            .primary
            .as_deref()
            .expect("primary font is initialized"))
    }

    pub fn cell_metrics(&mut self) -> Result<CellMetrics, FontError> {
        let size = self.physical_font_size();
        let line_height = self.config.line_height;
        // Measuring walks the primary face's tables; this is called every frame
        // and on every mouse event, so it is keyed and reused until the size or
        // scale factor actually changes.
        let key = (size.to_bits(), line_height.to_bits());
        if let Some((cached_key, metrics)) = self.metrics
            && cached_key == key
        {
            return Ok(metrics);
        }
        let metrics = self
            .primary_font()
            .map(|font| font.metrics(size, line_height))?;
        self.metrics = Some((key, metrics));
        Ok(metrics)
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
        let cell_width = self.cell_metrics()?.cell_width;
        let mut glyphs = Vec::new();
        let mut advance_width = 0.0;
        let mut families_used = Vec::new();
        for (font_id, segment, byte_offset) in segments {
            let font = self.loaded.get(&font_id).expect("resolved font is cached");
            if !families_used.contains(&font.family) {
                families_used.push(font.family.clone());
            }
            let mut shaped = font.shape(
                &segment,
                size,
                bold,
                italic,
                byte_offset as u32,
                self.config.ligatures,
            )?;
            fit_shaped_segment_to_terminal_cells(&mut shaped, &segment, cell_width);
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
            let face = self.resolve_face(&self.config.family, bold, italic);
            let resolved = face
                .as_ref()
                .is_some_and(|font| font.bold == bold && font.italic == italic);
            diagnostics.push(FontDiagnostic {
                family: self.config.family.clone(),
                role,
                resolved,
                source: face
                    .map(|font| font.source)
                    .unwrap_or(FontSource::Unresolved),
            });
        }
        diagnostics
    }

    fn load_primary(&mut self) -> Result<Arc<LoadedFont>, FontError> {
        let requested = Arc::clone(&self.families);
        for family in requested.iter() {
            if let Some(loaded) = self.load_family(family, false, false) {
                return loaded.map_err(|reason| FontError::FontLoadFailed {
                    family: family.to_string(),
                    reason,
                });
            }
        }

        Err(FontError::FontNotFound {
            requested: requested.iter().map(ToString::to_string).collect(),
        })
    }

    fn resolve_descriptor(&self, family: &str) -> FontDescriptor {
        let families = family_query(family);
        let query = fontdb::Query {
            families: &families,
            ..fontdb::Query::default()
        };
        let Some(source) = self
            .catalog
            .with_face(&query, |face| source_kind(&face.source))
        else {
            return FontDescriptor {
                family: family.to_owned(),
                source: FontSource::Unresolved,
            };
        };

        FontDescriptor {
            family: family.to_owned(),
            source,
        }
    }

    /// Resolves a family and style to a face without reading its bytes.
    fn resolve_face(&self, family: &str, bold: bool, italic: bool) -> Option<ResolvedFace> {
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
        let (face_index, face_source, face_weight, face_style, path, binary) =
            self.catalog.with_face(&query, |face| {
                let (path, binary) = match &face.source {
                    fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
                        (Some(path.clone()), None)
                    }
                    fontdb::Source::Binary(bytes) => (None, Some(Arc::clone(bytes))),
                };
                (
                    face.index,
                    source_kind(&face.source),
                    face.weight,
                    face.style,
                    path,
                    binary,
                )
            })?;
        if let Some(path) = path.as_deref() {
            self.catalog.record_resolved_file(path);
        }
        let data_key = match (&path, &binary) {
            (Some(path), _) => FontDataKey::File(path.clone()),
            (_, Some(bytes)) => {
                let slice = bytes.as_ref().as_ref();
                FontDataKey::Memory {
                    address: slice.as_ptr() as usize,
                    len: slice.len(),
                }
            }
            _ => return None,
        };
        Some(ResolvedFace {
            face_index,
            source: face_source,
            bold: face_weight >= fontdb::Weight::SEMIBOLD,
            italic: face_style != fontdb::Style::Normal,
            path,
            binary,
            data_key,
        })
    }

    /// Makes a fallback face available, off-thread when it needs I/O.
    ///
    /// Returns `Ok(None)` when the face is being loaded in the background: the
    /// caller moves on to the next family so the current frame is drawn with
    /// what is already resident instead of stalling on a multi-megabyte read.
    fn begin_family_load(
        &mut self,
        family: &Arc<str>,
        bold: bool,
        italic: bool,
    ) -> Result<Option<Arc<LoadedFont>>, FontError> {
        let Some(face) = self.resolve_face(family, bold, italic) else {
            return Ok(None);
        };

        // Already parsed by an earlier request (often another style of the same
        // file): finishing inline costs nothing.
        if let Some(faces) = self.catalog.cached_parsed_faces(&face) {
            return Ok(Some(Arc::new(LoadedFont::new(
                family.to_string(),
                faces,
                face.face_index,
                face.source.clone(),
                face.bold,
                face.italic,
            ))));
        }

        let key = (Arc::clone(family), bold, italic);
        if self.pending_faces.contains(&key) {
            return Ok(None);
        }
        let queued = self.loader.request(FontLoadRequest {
            family: Arc::clone(family),
            face: face.clone(),
            bold,
            italic,
        });
        if queued {
            self.pending_faces.insert(key);
            return Ok(None);
        }

        // No worker available (or its queue is full): fall back to loading here
        // rather than leaving the text unrenderable.
        self.load_family(family, bold, italic)
            .transpose()
            .map_err(|reason| FontError::FontLoadFailed {
                family: family.to_string(),
                reason,
            })
    }

    /// Collects fallback faces that finished loading in the background.
    ///
    /// Returns whether anything became available, in which case the caller
    /// should re-shape and redraw: the frames drawn while a face was in flight
    /// used the primary font in its place.
    pub fn poll_loaded_fonts(&mut self) -> bool {
        let mut changed = false;
        while let Ok(response) = self.loader.responses.try_recv() {
            self.pending_faces.remove(&response.key);
            match response.font {
                Ok(font) => {
                    self.loaded.entry(font.id()).or_insert(font);
                    changed = true;
                }
                Err(_) => {
                    // Leave it in `attempted_faces` so a broken file is not
                    // retried on every grapheme.
                }
            }
        }
        if changed {
            self.load_generation = self.load_generation.wrapping_add(1);
            // Characters that resolved to a substitute may now have a real face.
            self.character_fonts.clear();
        }
        changed
    }

    /// Registers the callback used to request a redraw when a fallback arrives.
    pub fn set_font_load_waker(&self, waker: FontLoadWaker) {
        let _ = self.loader.waker.set(waker);
    }

    /// Whether any fallback face is still being loaded.
    #[must_use]
    pub fn has_pending_font_loads(&self) -> bool {
        !self.pending_faces.is_empty()
    }

    fn load_family(
        &mut self,
        family: &str,
        bold: bool,
        italic: bool,
    ) -> Option<Result<Arc<LoadedFont>, String>> {
        let face = self.resolve_face(family, bold, italic)?;

        Some(
            self.catalog
                .parsed_faces_for(&face)
                .map(|faces| {
                    LoadedFont::new(
                        family.to_owned(),
                        faces,
                        face.face_index,
                        face.source.clone(),
                        face.bold,
                        face.italic,
                    )
                })
                .map(Arc::new),
        )
    }

    fn resolve_font_for_text(
        &mut self,
        text: &str,
        bold: bool,
        italic: bool,
    ) -> Result<u64, FontError> {
        let scalar = single_scalar(text);
        if let Some(ch) = scalar
            && let Some(font_id) = self.character_fonts.get(&(ch, bold, italic)).copied()
            && self.loaded.contains_key(&font_id)
        {
            return Ok(font_id);
        }

        let mut style_fallback = None;
        let families = Arc::clone(&self.families);
        for family in families.iter() {
            for font in self
                .loaded
                .values()
                .filter(|font| font.family == family.as_ref())
            {
                if font.supports_text(text) {
                    if font.is_bold() == bold && font.is_italic() == italic {
                        let id = font.id();
                        cache_scalar_font(&mut self.character_fonts, scalar, bold, italic, id);
                        return Ok(id);
                    }
                    style_fallback.get_or_insert(font.id());
                }
            }
            if !self
                .attempted_faces
                .insert((Arc::clone(family), bold, italic))
            {
                continue;
            }
            let Some(font) = self.begin_family_load(family, bold, italic)? else {
                continue;
            };
            if font.supports_text(text) {
                let id = font.id();
                self.loaded.entry(id).or_insert(font);
                let loaded = self.loaded.get(&id).expect("font was inserted");
                if loaded.is_bold() == bold && loaded.is_italic() == italic {
                    cache_scalar_font(&mut self.character_fonts, scalar, bold, italic, id);
                    return Ok(id);
                }
                style_fallback.get_or_insert(id);
            }
        }

        if let Some(id) = style_fallback {
            cache_scalar_font(&mut self.character_fonts, scalar, bold, italic, id);
            return Ok(id);
        }

        let id = self.primary_font()?.id();
        cache_scalar_font(&mut self.character_fonts, scalar, bold, italic, id);
        Ok(id)
    }
}

fn requested_families(config: &FontConfig) -> Vec<Arc<str>> {
    let mut requested: Vec<Arc<str>> = Vec::new();
    if !config.family.eq_ignore_ascii_case("monospace") {
        requested.push(Arc::from(config.family.as_str()));
    }
    requested.extend(
        config
            .fallback_families
            .iter()
            .map(|family| Arc::from(family.as_str())),
    );
    requested.extend([
        Arc::from("Cascadia Mono"),
        Arc::from("Consolas"),
        Arc::from("Menlo"),
        Arc::from("DejaVu Sans Mono"),
        Arc::from("Segoe UI Emoji"),
        Arc::from("Apple Color Emoji"),
        Arc::from("Noto Color Emoji"),
        Arc::from("Noto Emoji"),
        Arc::from("Microsoft YaHei UI"),
        Arc::from("Yu Gothic UI"),
        Arc::from("Hiragino Sans"),
        Arc::from("Noto Sans Mono CJK JP"),
        Arc::from("Noto Sans CJK JP"),
        Arc::from("monospace"),
    ]);
    let mut seen = HashSet::new();
    requested.retain(|family| seen.insert(family.to_ascii_lowercase()));
    requested
}

fn single_scalar(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    (chars.next().is_none() && !is_default_ignorable(ch)).then_some(ch)
}

fn cache_scalar_font(
    cache: &mut HashMap<(char, bool, bool), u64>,
    scalar: Option<char>,
    bold: bool,
    italic: bool,
    font_id: u64,
) {
    if let Some(ch) = scalar {
        cache.insert((ch, bold, italic), font_id);
    }
}

fn fit_shaped_segment_to_terminal_cells(glyphs: &mut [ShapedGlyph], text: &str, cell_width: f32) {
    let terminal_cells = UnicodeWidthStr::width(text).max(1) as f32;
    let target_advance = terminal_cells * cell_width;
    let shaped_advance = glyphs.iter().map(|glyph| glyph.x_advance).sum::<f32>();
    if shaped_advance.abs() <= f32::EPSILON {
        if let Some(glyph) = glyphs.last_mut() {
            glyph.x_advance = target_advance;
        }
        return;
    }
    let scale = target_advance / shaped_advance;
    for glyph in glyphs {
        glyph.x_advance *= scale;
        glyph.x_offset *= scale;
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

/// Whether a face actually satisfies a query, rather than merely being the
/// closest thing a small database happened to hold.
///
/// Generic families are never accepted from a partial database: there is no
/// name to check them against, so `monospace` would match whatever single font
/// was loaded.
fn face_satisfies(face: &fontdb::FaceInfo, query: &fontdb::Query<'_>) -> bool {
    let named = query.families.iter().any(|family| match family {
        fontdb::Family::Name(name) => face
            .families
            .iter()
            .any(|(candidate, _)| candidate.eq_ignore_ascii_case(name)),
        _ => false,
    });
    if !named {
        return false;
    }
    if query.weight >= fontdb::Weight::BOLD && face.weight < fontdb::Weight::SEMIBOLD {
        return false;
    }
    if query.style != fontdb::Style::Normal && face.style == fontdb::Style::Normal {
        return false;
    }
    true
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

struct ParsedFontFaces<'a> {
    ab_glyph: FontRef<'a>,
    rustybuzz: rustybuzz::Face<'a>,
}

self_cell!(
    struct OwnedFontFaces {
        owner: Arc<[u8]>,

        #[covariant]
        dependent: ParsedFontFaces,
    }
);

pub struct LoadedFont {
    id: u64,
    family: String,
    faces: Arc<OwnedFontFaces>,
    face_index: u32,
    source: FontSource,
    bold: bool,
    italic: bool,
}

impl fmt::Debug for LoadedFont {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedFont")
            .field("id", &self.id)
            .field("family", &self.family)
            .field("face_index", &self.face_index)
            .field("source", &self.source)
            .field("bold", &self.bold)
            .field("italic", &self.italic)
            .finish_non_exhaustive()
    }
}

impl LoadedFont {
    fn new(
        family: String,
        faces: Arc<OwnedFontFaces>,
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
            faces,
            face_index,
            source,
            bold,
            italic,
        }
    }

    fn ab_glyph(&self) -> &FontRef<'_> {
        &self.faces.borrow_dependent().ab_glyph
    }

    fn font_bytes(&self) -> &[u8] {
        self.faces.borrow_owner()
    }

    fn glyph_id(&self, ch: char) -> GlyphId {
        self.ab_glyph().glyph_id(ch)
    }

    fn units_per_em(&self) -> f32 {
        let font = self.ab_glyph();
        font.units_per_em()
            .unwrap_or_else(|| font.height_unscaled())
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
            .all(|ch| is_default_ignorable(ch) || self.glyph_id(ch) != GlyphId(0))
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
        let glyph_buffer =
            rustybuzz::shape(&self.faces.borrow_dependent().rustybuzz, &features, buffer);

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
        let font = self.ab_glyph();
        let scaled = font.as_scaled(self.ab_glyph_scale(size));
        let ascent = scaled.ascent();
        let descent = scaled.descent();
        let line_gap = scaled.line_gap();
        let cell_height = ((ascent - descent + line_gap) * line_height)
            .ceil()
            .max(1.0);
        let zero_width = terminal_cell_width(scaled.h_advance(font.glyph_id('0')));
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
        let font = self.ab_glyph();
        let scaled = font.as_scaled(self.ab_glyph_scale(size));
        let glyph_id = GlyphId(glyph_id);
        let advance_width = scaled.h_advance(glyph_id).max(0.0);
        let ascent = scaled.ascent();

        if let Some(font) = SwashFontRef::from_index(self.font_bytes(), self.face_index as usize) {
            let mut context = ScaleContext::new();
            let mut scaler = context.builder(font).size(size).hint(true).build();
            if let Some(image) = SwashRender::new(&[
                Source::ColorOutline(0),
                Source::ColorBitmap(StrikeWith::BestFit),
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

        if let Some(image) =
            font.glyph_raster_image2(glyph_id, size.round().clamp(1.0, u16::MAX as f32) as u16)
            && let Some(bitmap) = raster_image_to_bitmap(&image, advance_width, size, ascent)
        {
            return bitmap;
        }

        let glyph = glyph_id.with_scale_and_position(self.ab_glyph_scale(size), point(0.0, 0.0));

        let Some(outlined) = font.outline_glyph(glyph) else {
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
        let font = self.ab_glyph();
        let units_per_em = font
            .units_per_em()
            .unwrap_or_else(|| font.height_unscaled())
            .max(1.0);
        PxScale::from(pixels_per_em * font.height_unscaled().max(1.0) / units_per_em)
    }

    fn design_unit_scale(&self, pixels_per_em: f32) -> f32 {
        pixels_per_em / self.units_per_em().max(1.0)
    }

    fn decoration_metrics(&self, pixels_per_em: f32) -> (f32, f32, f32) {
        let fallback_stroke = (pixels_per_em / 14.0).max(1.0);
        let fallback_underline = -fallback_stroke;
        let fallback_strikeout = self
            .ab_glyph()
            .as_scaled(self.ab_glyph_scale(pixels_per_em))
            .ascent()
            * 0.35;
        let Some(font) = SwashFontRef::from_index(self.font_bytes(), self.face_index as usize)
        else {
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

fn terminal_cell_width(advance: f32) -> f32 {
    advance.round().max(1.0)
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

/// A face resolved from the database, before its bytes are loaded.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FontDataKey {
    File(PathBuf),
    Memory { address: usize, len: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ParsedFaceKey {
    data: FontDataKey,
    face_index: u32,
}

#[derive(Clone)]
struct ResolvedFace {
    face_index: u32,
    source: FontSource,
    bold: bool,
    italic: bool,
    path: Option<PathBuf>,
    binary: Option<Arc<dyn AsRef<[u8]> + Send + Sync>>,
    data_key: FontDataKey,
}

fn build_parsed_faces(bytes: Arc<[u8]>, face_index: u32) -> Result<OwnedFontFaces, String> {
    let faces = OwnedFontFaces::try_new(bytes, move |bytes| {
        let ab_glyph = FontRef::try_from_slice_and_index(bytes, face_index)
            .map_err(|error| error.to_string())?;
        let rustybuzz = rustybuzz::Face::from_slice(bytes, face_index)
            .ok_or_else(|| "OpenType shaping face could not be created".to_owned())?;
        Ok::<_, String>(ParsedFontFaces {
            ab_glyph,
            rustybuzz,
        })
    })?;
    Ok(faces)
}

#[derive(Debug)]
struct CachedGlyph {
    bitmap: Arc<GlyphBitmap>,
    /// Set on every hit and cleared when the sweep passes over the entry, so a
    /// glyph that is still in use survives one eviction round.
    referenced: bool,
}

/// Second-chance (CLOCK) cache. Strict insertion-order eviction let a burst of
/// unique glyphs — a screen of CJK, or a scroll through mixed scripts — evict
/// the ASCII set that every frame needs, which then had to be re-rasterized.
/// Hits stay O(1): they set a flag rather than reordering the queue.
#[derive(Debug)]
pub struct GlyphCache {
    capacity: usize,
    entries: HashMap<GlyphCacheKey, CachedGlyph>,
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
        self.get_or_insert_with_status(key, make).0
    }

    pub fn get_or_insert_with_status(
        &mut self,
        key: GlyphCacheKey,
        make: impl FnOnce() -> GlyphBitmap,
    ) -> (Arc<GlyphBitmap>, bool) {
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.referenced = true;
            return (Arc::clone(&entry.bitmap), true);
        }

        while self.entries.len() >= self.capacity {
            let Some(candidate) = self.order.pop_front() else {
                break;
            };
            match self.entries.get_mut(&candidate) {
                // Used since the last sweep: clear the flag and give it another
                // lap instead of evicting a glyph the current frame needs.
                Some(entry) if entry.referenced => {
                    entry.referenced = false;
                    self.order.push_back(candidate);
                }
                _ => {
                    self.entries.remove(&candidate);
                }
            }
        }

        let bitmap = Arc::new(make());
        self.entries.insert(
            key,
            CachedGlyph {
                bitmap: Arc::clone(&bitmap),
                referenced: false,
            },
        );
        self.order.push_back(key);
        (bitmap, false)
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
    fn glyph_cache_hits_survive_one_eviction_round_without_reordering() {
        let mut cache = GlyphCache::new(2);
        let key_a = GlyphCacheKey::new(1, u16::from(b'a'), 13.0, false, false);
        let key_b = GlyphCacheKey::new(1, u16::from(b'b'), 13.0, false, false);
        let key_c = GlyphCacheKey::new(1, u16::from(b'c'), 13.0, false, false);

        cache.get_or_insert_with(key_a, || GlyphBitmap::missing(8.0, 12));
        cache.get_or_insert_with(key_b, || GlyphBitmap::missing(8.0, 12));
        // A hit only flags the entry — the queue is never scanned or reordered.
        cache.get_or_insert_with(key_a, || panic!("cache hit must not rasterize"));
        cache.get_or_insert_with(key_c, || GlyphBitmap::missing(8.0, 12));

        assert_eq!(cache.len(), 2);
        assert!(
            cache.entries.contains_key(&key_a),
            "a glyph used since the last sweep must not be evicted ahead of an unused one"
        );
        assert!(!cache.entries.contains_key(&key_b));
        assert!(cache.entries.contains_key(&key_c));
    }

    #[test]
    fn a_unique_glyph_burst_does_not_evict_the_working_set() {
        // A screen of CJK used to evict the ASCII glyphs every frame needs,
        // which then had to be re-rasterized on the next frame.
        let mut cache = GlyphCache::new(8);
        let hot = (0..4)
            .map(|index| GlyphCacheKey::new(1, index, 13.0, false, false))
            .collect::<Vec<_>>();
        for key in &hot {
            cache.get_or_insert_with(*key, || GlyphBitmap::missing(8.0, 12));
        }

        for round in 0..8 {
            for key in &hot {
                cache.get_or_insert_with(*key, || panic!("working set must stay cached"));
            }
            for index in 0..4 {
                let unique = GlyphCacheKey::new(1, 100 + round * 4 + index, 13.0, false, false);
                cache.get_or_insert_with(unique, || GlyphBitmap::missing(8.0, 12));
            }
        }

        for key in &hot {
            assert!(
                cache.contains_key(*key),
                "repeatedly used glyphs must survive a burst of unique ones"
            );
        }
    }

    #[test]
    fn font_file_bytes_are_read_once_and_shared_across_faces() {
        let mut fonts = FontSystem::new(FontConfig::default());
        let families = Arc::clone(&fonts.families);
        let Some(family) = families.first().cloned() else {
            return;
        };
        let Some(Ok(first)) = fonts.load_family(&family, false, false) else {
            return;
        };
        if first.source == FontSource::Memory {
            return;
        }

        assert_eq!(fonts.catalog.file_bytes.lock().unwrap().len(), 1);
        let Some(Ok(second)) = fonts.load_family(&family, false, false) else {
            return;
        };

        assert!(
            Arc::ptr_eq(first.faces.borrow_owner(), second.faces.borrow_owner()),
            "loading a face again must share the cached file bytes instead of re-reading"
        );
        assert!(
            Arc::ptr_eq(&first.faces, &second.faces),
            "loading the same face again must reuse its parsed face cache"
        );
        assert_eq!(
            fonts.catalog.file_bytes.lock().unwrap().len(),
            1,
            "the same font file must not be held more than once"
        );
    }

    #[test]
    fn parsed_faces_borrow_the_shared_font_file_storage() {
        let mut fonts = FontSystem::new(FontConfig::default());
        let Ok(font) = fonts.primary_font() else {
            return;
        };

        let shared_bytes = font.faces.borrow_owner();
        let parsed = font.faces.borrow_dependent();

        assert_eq!(parsed.ab_glyph.glyph_id('0'), font.glyph_id('0'));
        assert_eq!(parsed.rustybuzz.units_per_em(), font.units_per_em() as i32);
        assert!(!shared_bytes.is_empty());
    }

    #[test]
    fn scalar_font_resolution_is_cached_by_character_and_style() {
        let mut fonts = FontSystem::new(FontConfig::default());
        let Ok(run) = fonts.shape_text("A", true, false) else {
            return;
        };
        let Some(glyph) = run.glyphs.first() else {
            return;
        };

        assert_eq!(
            fonts.character_fonts.get(&('A', true, false)),
            Some(&glyph.key.font_id)
        );
    }

    #[test]
    fn ligature_only_reload_reuses_loaded_faces_and_metrics() {
        let config = FontConfig::default();
        let mut fonts = FontSystem::new(config.clone());
        let Ok(first_metrics) = fonts.cell_metrics() else {
            return;
        };
        let first_font = Arc::clone(fonts.primary.as_ref().expect("primary font"));
        let mut next_config = config;
        next_config.ligatures = !next_config.ligatures;
        let mut reloaded = fonts.reconfigured(next_config, 1.0);

        assert!(Arc::ptr_eq(&fonts.catalog, &reloaded.catalog));
        assert!(Arc::ptr_eq(
            &first_font,
            reloaded.primary.as_ref().expect("reused primary font")
        ));
        assert_eq!(
            reloaded.metrics.map(|(_, metrics)| metrics),
            Some(first_metrics)
        );
        assert_eq!(reloaded.cell_metrics().unwrap(), first_metrics);
    }

    #[test]
    fn size_change_invalidates_reconfigured_metrics() {
        let mut fonts = FontSystem::new(FontConfig::default());
        if fonts.cell_metrics().is_err() {
            return;
        }
        let mut resized = FontConfig::default();
        resized.size += 1.0;

        let reloaded = fonts.reconfigured(resized, 1.0);

        assert!(reloaded.metrics.is_none());
    }

    #[test]
    fn terminal_cell_width_is_rounded_to_a_physical_pixel() {
        assert_eq!(terminal_cell_width(0.4), 1.0);
        assert_eq!(terminal_cell_width(7.49), 7.0);
        assert_eq!(terminal_cell_width(7.5), 8.0);
    }

    #[test]
    fn zero_advance_segment_still_consumes_its_terminal_cell() {
        let mut glyphs = vec![ShapedGlyph {
            key: GlyphCacheKey::new(1, 1, 13.0, false, false),
            cluster: 0,
            x_advance: 0.0,
            y_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
        }];

        fit_shaped_segment_to_terminal_cells(&mut glyphs, "\u{301}", 8.0);

        assert_eq!(glyphs[0].x_advance, 8.0);
    }

    #[test]
    fn ligatures_are_opt_in_by_default() {
        assert!(!FontConfig::default().ligatures);
    }

    #[test]
    fn cell_metrics_are_cached_until_the_scale_factor_changes() {
        let mut fonts = FontSystem::new(FontConfig::default());
        let Ok(first) = fonts.cell_metrics() else {
            return;
        };
        let Ok(second) = fonts.cell_metrics() else {
            return;
        };

        assert_eq!(first, second);
        assert!(
            fonts.metrics.is_some(),
            "metrics must be cached after a read"
        );

        assert!(fonts.set_scale_factor(2.0));
        let Ok(scaled) = fonts.cell_metrics() else {
            return;
        };
        assert!(
            scaled.font_size > first.font_size,
            "a scale factor change must invalidate the cached metrics"
        );
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
            let face = font.ab_glyph();
            let units_per_em = face.units_per_em().expect("units per em");
            let scale = PxScale::from(pixels_per_em * face.height_unscaled() / units_per_em);
            face.as_scaled(scale).ascent()
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
    fn a_partial_catalog_resolves_a_cached_face_without_scanning_the_system() {
        // Whatever this host resolves for the generic monospace family is a real
        // font file; feeding that one path back in must be enough to resolve it
        // again, with no system scan.
        let full = FontCatalog::discover();
        let Some(seed) = full.face_source_path("monospace") else {
            eprintln!("no resolvable monospace face on this host");
            return;
        };
        // A named family, because a generic one deliberately always promotes.
        let Some(family) = full.first_family_name(&seed) else {
            eprintln!("resolved face reports no family name");
            return;
        };

        let partial = FontCatalog::from_font_files(std::slice::from_ref(&seed));
        assert!(
            !partial.is_fully_scanned(),
            "a catalog built from cached paths must start unscanned"
        );
        let resolved = partial.face_source_path(&family);
        assert_eq!(
            resolved.as_ref(),
            Some(&seed),
            "the cached face must resolve from the partial database"
        );
        assert!(
            !partial.is_fully_scanned(),
            "resolving a cached face must not trigger a system scan"
        );
    }

    #[test]
    fn a_miss_on_a_partial_catalog_promotes_it_to_a_full_scan() {
        let partial = FontCatalog::from_font_files(&[]);
        assert!(!partial.is_fully_scanned());

        // An empty database cannot answer this, so the catalog has to scan.
        let resolved = partial.face_source_path("monospace");
        assert!(
            partial.is_fully_scanned(),
            "a miss must promote the catalog to a full system scan"
        );
        let full = FontCatalog::discover();
        assert_eq!(
            resolved,
            full.face_source_path("monospace"),
            "after promotion the partial catalog must answer like a full one"
        );
    }

    #[test]
    fn the_catalog_reports_only_the_font_files_it_actually_resolved() {
        let mut fonts = FontSystem::new(FontConfig::default());
        if fonts.primary_font().is_err() {
            eprintln!("no primary font on this host");
            return;
        }
        let used = fonts.resolved_font_files();
        assert!(
            !used.is_empty(),
            "resolving a primary font must record the file it came from"
        );
        assert!(
            used.len() < 64,
            "only the faces actually used should be recorded, got {}",
            used.len()
        );
        for path in &used {
            assert!(path.exists(), "recorded font file must exist: {path:?}");
        }
    }
    #[test]
    fn a_partial_catalog_will_not_answer_a_bold_request_with_a_regular_face() {
        // The failure this guards: fontdb returns its closest match rather than
        // nothing, so a catalog holding only a regular file would answer "give me
        // bold" with that regular face and never scan for a real bold one.
        let full = FontCatalog::discover();
        let Some(regular) = full.face_source_path("monospace") else {
            eprintln!("no resolvable monospace face on this host");
            return;
        };
        let Some(family) = full.first_family_name(&regular) else {
            eprintln!("resolved face reports no family name");
            return;
        };

        let partial = FontCatalog::from_font_files(std::slice::from_ref(&regular));
        let families = family_query(&family);
        let bold = fontdb::Query {
            families: &families,
            weight: fontdb::Weight::BOLD,
            ..fontdb::Query::default()
        };
        let resolved_weight = partial.with_face(&bold, |face| face.weight);
        assert!(
            partial.is_fully_scanned(),
            "a bold request the cached file cannot satisfy must promote to a full scan"
        );
        if let Some(weight) = resolved_weight {
            assert!(
                weight >= fontdb::Weight::SEMIBOLD
                    || full
                        .with_face(&bold, |face| face.weight)
                        .is_some_and(|full_weight| full_weight == weight),
                "after scanning, the answer must match what a full catalog gives"
            );
        }
    }

    #[test]
    fn a_partial_catalog_never_answers_a_generic_family_from_one_cached_file() {
        // "monospace" has no name to verify against, so a single cached file must
        // not be allowed to stand in for the platform's real monospace choice.
        let full = FontCatalog::discover();
        let Some(seed) = full.face_source_path("monospace") else {
            eprintln!("no resolvable monospace face on this host");
            return;
        };
        let partial = FontCatalog::from_font_files(std::slice::from_ref(&seed));
        let families = family_query("monospace");
        let query = fontdb::Query {
            families: &families,
            ..fontdb::Query::default()
        };
        let _ = partial.with_face(&query, |face| face.index);
        assert!(
            partial.is_fully_scanned(),
            "a generic family must promote a partial catalog to a full scan"
        );
    }
    #[test]
    fn monochrome_outline_rasterization_uses_the_unweighted_outline_mask() {
        let mut fonts = FontSystem::new(FontConfig::default());
        let size = fonts.physical_font_size();
        if fonts.primary_font().is_err() {
            return;
        }
        let font = Arc::clone(fonts.primary.as_ref().expect("primary font"));
        let glyph_id = font.glyph_id('h');
        let glyph = glyph_id.with_scale_and_position(font.ab_glyph_scale(size), point(0.0, 0.0));
        let Some(outlined) = font.ab_glyph().outline_glyph(glyph) else {
            return;
        };
        let bounds = outlined.px_bounds();
        let width = bounds.width().ceil().max(1.0) as u32;
        let height = bounds.height().ceil().max(1.0) as u32;
        let mut expected = vec![0; (width * height) as usize];
        outlined.draw(|x, y, coverage| {
            expected[(y * width + x) as usize] = (coverage * 255.0).round() as u8;
        });

        let actual = font.rasterize(glyph_id.0, size);

        assert_eq!(actual.format, GlyphBitmapFormat::Alpha);
        assert_eq!((actual.width, actual.height), (width, height));
        assert_eq!(actual.pixels, expected);
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
        let requested = &fonts.families;

        assert_eq!(requested.first().map(AsRef::as_ref), Some("Cascadia Mono"));
        assert_eq!(requested.last().map(AsRef::as_ref), Some("monospace"));
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

    #[test]
    fn a_fallback_face_loads_off_thread_and_reports_its_arrival() {
        let mut fonts = FontSystem::new(FontConfig::default());
        // The primary face must still be resolved synchronously; nothing can be
        // drawn without it.
        if fonts.cell_metrics().is_err() {
            return;
        }

        // Register the waker before anything is queued so its firing cannot be
        // missed by a load that finishes quickly.
        let waker_fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&waker_fired);
        fonts.set_font_load_waker(FontLoadWaker::new(move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }));

        // First sight of a script the primary face lacks must not block on a
        // multi-megabyte read: it hands the load to the worker and returns.
        let first = fonts.shape_text("日本語", false, false);
        assert!(
            first.is_ok(),
            "shaping must not fail while a fallback loads"
        );
        if !fonts.has_pending_font_loads() {
            // Either no fallback was needed on this host or it was already
            // cached; both are fine, there is nothing async left to observe.
            return;
        }

        let generation_before = fonts.generation_id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while fonts.has_pending_font_loads() && std::time::Instant::now() < deadline {
            if !fonts.poll_loaded_fonts() {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }

        assert!(
            !fonts.has_pending_font_loads(),
            "the loader must finish its queued work"
        );
        assert_ne!(
            fonts.generation_id(),
            generation_before,
            "an arrived fallback must change the generation so glyph caches invalidate"
        );
        assert!(
            waker_fired.load(std::sync::atomic::Ordering::SeqCst),
            "the host must be woken so the tofu frame is redrawn"
        );

        // The face is resident now, so the same text resolves to it.
        let resolved = fonts.shape_text("日本語", false, false).expect("reshape");
        assert!(!resolved.glyphs.is_empty());
    }

    /// Shapes until a fallback that had to be loaded in the background is
    /// resident, mirroring what a host does: draw the frame with what is
    /// available, then redraw when `poll_loaded_fonts` reports an arrival.
    fn shape_after_fallback_loads(fonts: &mut FontSystem, text: &str) -> ShapedRun {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let run = fonts.shape_text(text, false, false).expect("shape text");
            if !fonts.has_pending_font_loads() {
                return run;
            }
            if std::time::Instant::now() >= deadline {
                return run;
            }
            if !fonts.poll_loaded_fonts() {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_color_emoji_fallback_rasterizes_rgba() {
        let mut fonts = FontSystem::new(FontConfig::default());
        let run = shape_after_fallback_loads(&mut fonts, "😀");
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
