//! Renderer-independent drawing contracts.

pub const LAYER: &str = "render performance";

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl RenderColor {
    #[must_use]
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: u8::MAX,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellPosition {
    pub row: i64,
    pub col: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderCell {
    pub position: CellPosition,
    pub text: String,
    pub foreground: RenderColor,
    pub background: RenderColor,
    pub style: RenderCellStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderCellStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderGrid {
    pub columns: u16,
    pub rows: u16,
    pub cells: Vec<RenderCell>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderCursorShape {
    Block,
    Beam,
    Underline,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorVisual {
    pub position: CellPosition,
    pub shape: RenderCursorShape,
    pub color: RenderColor,
    pub visible: bool,
}

pub type RenderCursor = CursorVisual;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub type DamageRegion = RenderRect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    Selection,
    SearchHighlight,
    Semantic,
    Decoration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayPrimitive {
    pub kind: OverlayKind,
    pub bounds: RenderRect,
    pub color: RenderColor,
    pub label: Option<String>,
}

pub type RenderOverlay = OverlayPrimitive;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionVisual {
    pub cells: Vec<CellPosition>,
    pub color: RenderColor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderDecoration {
    pub bounds: RenderRect,
    pub color: RenderColor,
    pub border_color: Option<RenderColor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationHandle {
    pub id: u64,
    pub affected_region: RenderRect,
    pub elapsed: Duration,
    pub remaining: Option<Duration>,
}

pub type RenderAnimation = AnimationHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRequestReason {
    TerminalContentChanged,
    CursorBlink,
    Animation,
    WindowResized,
    SelectionChanged,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRequest {
    pub reason: FrameRequestReason,
    pub damage: Option<DamageRegion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderScene {
    pub grid: RenderGrid,
    pub cursor: Option<CursorVisual>,
    pub selections: Vec<SelectionVisual>,
    pub search_highlights: Vec<OverlayPrimitive>,
    pub semantic_overlays: Vec<OverlayPrimitive>,
    pub decorations: Vec<RenderDecoration>,
    pub animations: Vec<AnimationHandle>,
    pub damage_regions: Vec<DamageRegion>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn render_core_has_no_crate_dependencies() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest.contains("[dependencies]"),
            "render-core must stay renderer-independent and avoid GPU API dependencies"
        );
    }
}
