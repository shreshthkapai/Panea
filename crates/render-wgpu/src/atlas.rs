// GPU glyph atlas allocation, cache entries, and eviction policy.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasEntry {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AtlasCacheKey {
    Glyph(GlyphCacheKey),
    PowerlineCap {
        codepoint: char,
        width: u32,
        height: u32,
    },
    PaneaLogo,
}

impl From<GlyphCacheKey> for AtlasCacheKey {
    fn from(value: GlyphCacheKey) -> Self {
        Self::Glyph(value)
    }
}

#[derive(Debug)]
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    entries: HashMap<AtlasCacheKey, AtlasEntry>,
    used_bytes: u64,
}

const GLYPH_ATLAS_PADDING: u32 = 1;

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
            used_bytes: 0,
        }
    }

    pub fn allocate(
        &mut self,
        key: impl Into<AtlasCacheKey>,
        bitmap: &GlyphBitmap,
    ) -> Option<AtlasEntry> {
        self.allocate_with_status(key, bitmap)
            .map(|(entry, _)| entry)
    }

    fn allocate_with_status(
        &mut self,
        key: impl Into<AtlasCacheKey>,
        bitmap: &GlyphBitmap,
    ) -> Option<(AtlasEntry, bool)> {
        let key = key.into();
        if let Some(entry) = self.entries.get(&key).copied() {
            return Some((entry, true));
        }

        let width = bitmap.width.max(1);
        let height = bitmap.height.max(1);
        let allocation_width = width.saturating_add(GLYPH_ATLAS_PADDING * 2);
        let allocation_height = height.saturating_add(GLYPH_ATLAS_PADDING * 2);
        if allocation_width > self.width || allocation_height > self.height {
            return None;
        }

        if self.cursor_x.saturating_add(allocation_width) > self.width {
            self.cursor_x = 0;
            self.cursor_y += self.row_height;
            self.row_height = 0;
        }

        if self.cursor_y.saturating_add(allocation_height) > self.height {
            return None;
        }

        let entry = AtlasEntry {
            x: self.cursor_x + GLYPH_ATLAS_PADDING,
            y: self.cursor_y + GLYPH_ATLAS_PADDING,
            width,
            height,
        };
        self.cursor_x += allocation_width;
        self.row_height = self.row_height.max(allocation_height);
        self.entries.insert(key, entry);
        self.used_bytes = self
            .used_bytes
            .saturating_add(u64::from(width) * u64::from(height) * 4);
        Some((entry, false))
    }

    #[must_use]
    pub fn entry(&self, key: impl Into<AtlasCacheKey>) -> Option<AtlasEntry> {
        self.entries.get(&key.into()).copied()
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
    pub const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    #[must_use]
    pub const fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    #[must_use]
    pub fn capacity_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * 4
    }

    fn clear(&mut self) {
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.row_height = 0;
        self.entries.clear();
        self.used_bytes = 0;
    }
}
