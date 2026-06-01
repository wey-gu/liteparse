use serde::Serialize;
use std::collections::HashMap;

#[doc(hidden)]
#[derive(Debug, Clone)]
pub enum PdfInput {
    /// Path to a PDF file on disk.
    Path(String),
    /// Raw PDF bytes (e.g. from a network response or in-memory buffer).
    Bytes(Vec<u8>),
}

/// Represents a single text item extracted from a PDF page,
/// including its content, position, size, rotation, and font metadata.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TextItem {
    pub text: String,
    /// Viewport-space coordinates (top-left origin, 72 DPI).
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Rotation in degrees (counter-clockwise, adjusted for page rotation).
    pub rotation: f32,
    pub font_name: Option<String>,
    pub font_size: Option<f32>,
    /// Font size * scale_y from the text matrix — accounts for CTM scaling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_ascent: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_descent: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_weight: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_flags: Option<i32>,
    /// Sum of glyph widths (using charcode-based lookup when possible).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_width: Option<f32>,
    /// Whether the font has buggy encoding (private-use codepoints, TT subset, etc.)
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub font_is_buggy: bool,
    /// Marked content ID from the PDF structure tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcid: Option<i32>,
    /// Fill color as ARGB hex string (e.g. "ff000000").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill_color: Option<String>,
    /// Stroke color as ARGB hex string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke_color: Option<String>,
    /// OCR confidence score (0.0–1.0). None for native PDF text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

#[doc(hidden)]
#[derive(Debug, Serialize)]
pub struct Page {
    pub page_number: usize,
    pub page_width: f32,
    pub page_height: f32,
    pub text_items: Vec<TextItem>,
    /// Vector graphics on the page, distilled from PDFium path objects.
    /// Not emitted in JSON/text outputs — consumed by the markdown layout pass.
    #[serde(skip)]
    pub graphics: Vec<GraphicPrimitive>,
}

/// Represents a fully parsed page with projected text layout.
#[derive(Debug, Serialize)]
pub struct ParsedPage {
    pub page_number: usize,
    pub page_width: f32,
    pub page_height: f32,
    pub text: String,
    pub text_items: Vec<TextItem>,
    /// Per-line structural metadata used by the markdown emitter. Not part of
    /// the JSON/text outputs (consumed internally) so it is `#[serde(skip)]`.
    #[serde(skip)]
    pub projected_lines: Vec<ProjectedLine>,
    /// Root of the XY-cut region tree for this page. Leaves correspond to the
    /// `region_path` on each `ProjectedLine`. Internal-only.
    #[serde(skip)]
    pub regions: Region,
    /// Vector graphics on the page (decomposed paths) used by the markdown
    /// emitter for ruled-table / HR / figure-cluster detection. Not part of
    /// the JSON/text output.
    #[serde(skip)]
    pub graphics: Vec<GraphicPrimitive>,
    /// Figure-region bounding rectangles derived from `graphics`. Pre-computed
    /// in `to_parsed_pages` so the XY-cut layout pass can treat them as
    /// obstacles, and reused downstream for figure classification.
    #[serde(skip)]
    pub figures: Vec<Rect>,
}

#[doc(hidden)]
#[derive(Debug, Clone, Default, Serialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Lightweight vector-graphic primitive derived from PDFium path objects.
/// Only the shapes useful to the markdown emitter (ruled tables, HRs, figure
/// clusters) are kept — bezier curves and complex paths are decomposed into
/// straight strokes, or dropped.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub enum GraphicPrimitive {
    /// A single straight line segment in viewport coords. Used for HR/table
    /// border detection.
    Stroke {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        color: Option<String>,
        width: f32,
    },
    /// An axis-aligned rectangle — typically a filled cell background, banner,
    /// or fully-stroked table border drawn as a single path.
    Rect {
        bbox: Rect,
        fill: Option<String>,
        stroke: Option<String>,
    },
}

impl GraphicPrimitive {
    /// Bbox of the primitive in viewport coords.
    pub fn bbox(&self) -> Rect {
        match self {
            GraphicPrimitive::Stroke { x1, y1, x2, y2, .. } => {
                let x = x1.min(*x2);
                let y = y1.min(*y2);
                Rect {
                    x,
                    y,
                    width: (x2 - x1).abs(),
                    height: (y2 - y1).abs(),
                }
            }
            GraphicPrimitive::Rect { bbox, .. } => bbox.clone(),
        }
    }
}

/// Per-line structural metadata derived during grid projection. Used by the
/// markdown emitter; not surfaced in JSON/text output.
#[doc(hidden)]
#[derive(Debug, Clone, Serialize)]
pub struct ProjectedLine {
    pub text: String,
    pub bbox: Rect,
    pub anchor: Anchor,
    pub indent_x: f32,
    pub dominant_font_size: f32,
    pub dominant_font_name: Option<String>,
    pub all_bold: bool,
    pub all_italic: bool,
    pub all_mono: bool,
    pub all_strike: bool,
    pub spans: Vec<TextItem>,
    /// Path from the page's region-tree root to the leaf containing this line.
    /// Equality means "same leaf"; prefix relationship means "one contains the
    /// other". Replaces the prior flat `column_id` scheme so nested layouts
    /// (banded splits with sub-columns) survive paragraph/table grouping.
    pub region_path: Vec<u16>,
    pub mcid: Option<i32>,
}

/// XY-cut region tree node. A page's root region recursively splits along H or
/// V axes until each leaf holds a coherent block of items.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct Region {
    pub bbox: Rect,
    pub kind: RegionKind,
}

#[doc(hidden)]
#[derive(Debug, Clone)]
pub enum RegionKind {
    Leaf {
        item_indices: Vec<usize>,
    },
    Split {
        axis: CutAxis,
        children: Vec<Region>,
    },
}

impl Default for RegionKind {
    fn default() -> Self {
        RegionKind::Leaf {
            item_indices: Vec::new(),
        }
    }
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutAxis {
    Horizontal,
    Vertical,
}

#[doc(hidden)]
#[derive(Debug, Serialize)]
pub enum Snap {
    Left,
    Right,
    Center,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Anchor {
    Left,
    Right,
    Center,
    /// Inline span that does not snap to a column edge — used by lines whose
    /// dominant items couldn't be classified as Left/Right/Center.
    Floating,
}

#[doc(hidden)]
#[derive(Debug, Serialize)]
pub struct ProjectedTextItem {
    pub item: TextItem,
    pub snap: Snap,
    pub anchor: Anchor,
    pub is_dup: bool,
    pub rendered: bool,
    pub num_spaces: usize,
    pub force_unsnapped: bool,
    pub is_margin_line_number: bool,
    pub rotated: bool,
    pub d: f32,
}

#[doc(hidden)]
pub type AnchorMap = HashMap<i32, Vec<(usize, usize)>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_item() -> TextItem {
        TextItem {
            text: "hi".into(),
            x: 1.0,
            y: 2.0,
            width: 10.0,
            height: 4.0,
            font_name: Some("Arial".into()),
            font_size: Some(12.0),
            ..Default::default()
        }
    }

    #[test]
    fn text_item_skips_none_fields() {
        let item = sample_item();
        let s = serde_json::to_string(&item).unwrap();
        assert!(!s.contains("font_height"));
        assert!(!s.contains("confidence"));
        assert!(!s.contains("font_is_buggy"));
        assert!(s.contains("\"text\":\"hi\""));
    }

    #[test]
    fn text_item_includes_buggy_flag_when_true() {
        let mut item = sample_item();
        item.font_is_buggy = true;
        let s = serde_json::to_string(&item).unwrap();
        assert!(s.contains("font_is_buggy"));
    }

    #[test]
    fn page_serializes() {
        let p = Page {
            page_number: 1,
            page_width: 100.0,
            page_height: 200.0,
            text_items: vec![sample_item()],
            graphics: vec![],
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"page_number\":1"));
    }

    #[test]
    fn anchor_map_basic() {
        let mut m: AnchorMap = HashMap::new();
        m.entry(5).or_default().push((1, 2));
        assert_eq!(m.get(&5).unwrap()[0], (1, 2));
    }
}
