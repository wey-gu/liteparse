//! Block classification for the markdown emitter.
//!
//! Consumes `ProjectedLine` entries from each `ParsedPage` and groups them into
//! a sequence of `Block`s: headings, paragraphs, and (for now) raw lines that
//! don't fit a recognized shape. Tables, lists, and code blocks land in later
//! build-order steps.

use crate::projection::{is_bold_item, is_italic_item, is_mono_item};
use crate::types::{Anchor, GraphicPrimitive, ParsedPage, ProjectedLine, TextItem};

/// Maximum stroke thickness (or |y1-y2|) for a stroke to count as a candidate HR.
/// Thicker shapes are filled rects, not rules.
const HR_MAX_THICKNESS_PT: f32 = 2.0;

/// Minimum fraction of page width a horizontal stroke must span to count as an HR.
/// Shorter strokes are typically table borders, list bullets, or inline marks.
const HR_MIN_WIDTH_FRACTION: f32 = 0.3;

/// Vertical tolerance (points) for treating a stroke as "underlining" the
/// nearest text line. Strokes within this band of a text line's baseline are
/// dropped — they're underlines, not rules.
const HR_UNDERLINE_PROXIMITY_PT: f32 = 3.0;

/// Detect horizontal rules from a page's vector graphics.
///
/// Returns the y-coordinates (viewport space) of accepted HRs, sorted ascending.
/// An HR is a roughly horizontal stroke that spans at least
/// `HR_MIN_WIDTH_FRACTION` of the page width, is thinner than
/// `HR_MAX_THICKNESS_PT`, and does not sit on the baseline of any text line
/// (which would make it an underline).
pub fn detect_horizontal_rules(page: &ParsedPage) -> Vec<f32> {
    if page.graphics.is_empty() || page.page_width <= 0.0 {
        return Vec::new();
    }
    let min_width = page.page_width * HR_MIN_WIDTH_FRACTION;
    let mut ys: Vec<f32> = Vec::new();

    for g in &page.graphics {
        let GraphicPrimitive::Stroke {
            x1,
            y1,
            x2,
            y2,
            width,
            ..
        } = g
        else {
            continue;
        };
        let (x1, y1, x2, y2, width) = (*x1, *y1, *x2, *y2, *width);
        let dy = (y1 - y2).abs();
        let dx = (x1 - x2).abs();
        if dy > HR_MAX_THICKNESS_PT || width > HR_MAX_THICKNESS_PT {
            continue;
        }
        if dx < min_width {
            continue;
        }
        let y = (y1 + y2) * 0.5;
        let xmin = x1.min(x2);
        let xmax = x1.max(x2);

        // Drop if this stroke sits on a text-line baseline — it's an underline,
        // not a divider.
        let is_underline = page.projected_lines.iter().any(|line| {
            let bottom = line.bbox.y + line.bbox.height;
            (y - bottom).abs() < HR_UNDERLINE_PROXIMITY_PT
                && xmin >= line.bbox.x - 2.0
                && xmax <= line.bbox.x + line.bbox.width + 2.0
        });
        if is_underline {
            continue;
        }
        ys.push(y);
    }

    // Sort + dedup near-duplicates (some PDFs draw the same rule twice).
    ys.sort_by(|a, b| a.total_cmp(b));
    ys.dedup_by(|a, b| (*a - *b).abs() < 1.0);
    ys
}

/// Coarse block representation. Intentionally minimal — extended as later
/// build-order steps land (tables, figures).
#[derive(Debug, Clone)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
        bold: bool,
        italic: bool,
    },
    ListItem {
        ordered: bool,
        marker: String,
        level: u8,
        text: String,
        bold: bool,
        italic: bool,
    },
    /// Fenced code block — content rendered between triple-backtick fences.
    /// Each entry in `lines` is one source line; preserved as-is (only trailing
    /// whitespace stripped) so indentation survives.
    CodeBlock {
        lines: Vec<String>,
    },
    /// Confident table emitted as a markdown pipe table. `header` is `None`
    /// when the first row didn't qualify (e.g. wasn't bold and the table mode
    /// can't otherwise distinguish it).
    Table {
        header: Option<Vec<String>>,
        rows: Vec<Vec<String>>,
    },
    /// Tabular-looking region we couldn't classify confidently — rendered
    /// verbatim inside a fenced block to preserve visual structure for the
    /// downstream LLM. Strictly better than emitting a mangled pipe table.
    GridFallback {
        lines: Vec<String>,
    },
    /// A horizontal rule detected from a long thin horizontal stroke in the
    /// page's vector graphics (e.g. divider line between sections).
    HorizontalRule,
    Blank,
}

/// Minimum cells per row for a region to qualify as a table.
const TABLE_MIN_COLUMNS: usize = 3;

/// Minimum consecutive rows for a region to qualify as a table.
const TABLE_MIN_ROWS: usize = 2;

/// Gap between adjacent spans (in multiples of dominant font size) above which
/// we treat the gap as a cell boundary.
const TABLE_CELL_GAP_FONT_MULTIPLIER: f32 = 1.0;

/// Tolerance (points) for matching a cell's start-x to an existing column
/// track when extending a candidate table run.
const TABLE_TRACK_TOLERANCE_PT: f32 = 6.0;

/// Maximum vertical gap between consecutive table rows, expressed in multiples
/// of the line height. Looser than the paragraph rule because table rows often
/// have more vertical padding than prose lines.
const TABLE_ROW_GAP_MULTIPLIER: f32 = 2.5;

/// Maximum coefficient-of-variation for row spacing within a confident table
/// (rejecting irregular spacing that's more likely prose or a footer block).
const TABLE_ROW_SPACING_MAX_CV: f32 = 0.5;

/// One cell within a tabular row: contributing spans aggregated to text and
/// its leftmost x position, used to align cells across rows into column
/// "tracks".
#[derive(Debug, Clone)]
struct TableCell {
    start_x: f32,
    /// Right edge of the cell (x of the last span's right). Used by
    /// `recover_merged_cell` to detect cells that straddle two column tracks
    /// when the projection merged two adjacent words into one span.
    end_x: f32,
    text: String,
    bold: bool,
}

/// A contiguous tabular run: line indices `[start, end)` plus the detected
/// rows. Used so the line-classifier can skip the consumed range and so
/// fallback rendering can reach back for the original projected text.
#[derive(Debug, Clone)]
struct TableRun {
    start: usize,
    end: usize,
    block: Block,
}

/// Split a `ProjectedLine`'s spans into cells. A gap larger than
/// `TABLE_CELL_GAP_FONT_MULTIPLIER × font_size` between adjacent spans starts
/// a new cell; otherwise spans join into the same cell with a single space.
fn split_cells(line: &ProjectedLine) -> Vec<TableCell> {
    // Skip whitespace-only spans before computing gaps — leading/trailing
    // empty items would otherwise add spurious cell boundaries.
    let mut spans: Vec<&TextItem> = line
        .spans
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .collect();
    spans.sort_by(|a, b| a.x.total_cmp(&b.x));
    if spans.is_empty() {
        return Vec::new();
    }
    let font_size = if line.dominant_font_size > 0.0 {
        line.dominant_font_size
    } else {
        line.bbox.height.max(1.0)
    };
    let gap_threshold = font_size * TABLE_CELL_GAP_FONT_MULTIPLIER;

    let mut cells: Vec<TableCell> = Vec::new();
    let mut current_text = String::new();
    let mut current_start = spans[0].x;
    let mut current_bold_chars: usize = 0;
    let mut current_total_chars: usize = 0;
    let mut prev_right = spans[0].x;

    for (i, span) in spans.iter().enumerate() {
        let gap = span.x - prev_right;
        let break_cell = i > 0 && gap > gap_threshold;
        if break_cell {
            let bold = current_total_chars > 0 && current_bold_chars * 2 > current_total_chars;
            cells.push(TableCell {
                start_x: current_start,
                end_x: prev_right,
                text: collapse_whitespace(current_text.trim()),
                bold,
            });
            current_text.clear();
            current_start = span.x;
            current_bold_chars = 0;
            current_total_chars = 0;
        }
        if !current_text.is_empty() && !current_text.ends_with(' ') {
            current_text.push(' ');
        }
        current_text.push_str(&span.text);
        let n = span.text.chars().count();
        current_total_chars += n;
        if is_bold_span(span) {
            current_bold_chars += n;
        }
        prev_right = span.x + span.width.max(0.0);
    }
    if !current_text.trim().is_empty() {
        let bold = current_total_chars > 0 && current_bold_chars * 2 > current_total_chars;
        cells.push(TableCell {
            start_x: current_start,
            end_x: prev_right,
            text: collapse_whitespace(current_text.trim()),
            bold,
        });
    }
    cells
}

/// When a candidate row has fewer cells than the established column count,
/// look for cells whose x-range straddles multiple column tracks (likely two
/// or more adjacent words that PDFium merged into a single text run) and
/// split each on internal whitespace at the boundaries nearest to the
/// straddled tracks.
///
/// Returns the patched cells if every short cell could be cleanly split to
/// recover `tracks.len()` cells total; otherwise `None`.
fn recover_merged_cell(mut cells: Vec<TableCell>, tracks: &[f32]) -> Option<Vec<TableCell>> {
    let target = tracks.len();
    if cells.len() >= target {
        return None;
    }
    // Repeatedly find the cell that straddles the most tracks (≥2) and split
    // it. Each iteration strictly grows `cells.len()`, so termination is
    // guaranteed; if no cell straddles ≥2 tracks before we hit the target,
    // recovery fails.
    while cells.len() < target {
        let mut best_i: Option<usize> = None;
        let mut best_count: usize = 1;
        let mut best_contained: Vec<f32> = Vec::new();
        for (i, cell) in cells.iter().enumerate() {
            let contained: Vec<f32> = tracks
                .iter()
                .copied()
                .filter(|t| {
                    *t >= cell.start_x - TABLE_TRACK_TOLERANCE_PT
                        && *t <= cell.end_x + TABLE_TRACK_TOLERANCE_PT
                })
                .collect();
            if contained.len() > best_count {
                best_count = contained.len();
                best_i = Some(i);
                best_contained = contained;
            }
        }
        let Some(i) = best_i else {
            return None;
        };
        let cell = cells[i].clone();
        let chars: Vec<char> = cell.text.trim().chars().collect();
        let n = chars.len();
        if n == 0 || best_contained.len() < 2 {
            return None;
        }
        let text_width = (cell.end_x - cell.start_x).max(1.0);
        // For each track after the first, pick the whitespace boundary in
        // `chars` whose linearly-interpolated x is closest to the track.
        let mut split_indices: Vec<usize> = Vec::new();
        for t in best_contained.iter().skip(1) {
            let mut best: Option<(usize, f32)> = None;
            for (k, ch) in chars.iter().enumerate() {
                if !ch.is_whitespace() {
                    continue;
                }
                if split_indices.contains(&k) {
                    continue;
                }
                let frac = k as f32 / n as f32;
                let x = cell.start_x + frac * text_width;
                let d = (x - t).abs();
                if best.as_ref().is_none_or(|b| d < b.1) {
                    best = Some((k, d));
                }
            }
            let (k, _) = best?;
            split_indices.push(k);
        }
        split_indices.sort();
        // Build the split pieces.
        let mut pieces: Vec<String> = Vec::new();
        let mut prev = 0usize;
        for k in &split_indices {
            let piece: String = chars[prev..*k]
                .iter()
                .collect::<String>()
                .trim()
                .to_string();
            if piece.is_empty() {
                return None;
            }
            pieces.push(piece);
            prev = *k;
        }
        let last: String = chars[prev..].iter().collect::<String>().trim().to_string();
        if last.is_empty() {
            return None;
        }
        pieces.push(last);
        if pieces.len() != best_contained.len() {
            return None;
        }
        // Synthesize new TableCells aligned with each track.
        let mut new_cells: Vec<TableCell> = Vec::with_capacity(pieces.len());
        for (p, piece) in pieces.iter().enumerate() {
            let start_x = if p == 0 {
                cell.start_x
            } else {
                best_contained[p]
            };
            let end_x = if p + 1 < best_contained.len() {
                (best_contained[p + 1] - 1.0).max(start_x)
            } else {
                cell.end_x
            };
            new_cells.push(TableCell {
                start_x,
                end_x,
                text: piece.clone(),
                bold: cell.bold,
            });
        }
        cells.remove(i);
        for (offset, c) in new_cells.into_iter().enumerate() {
            cells.insert(i + offset, c);
        }
    }
    if cells.len() == target {
        Some(cells)
    } else {
        None
    }
}

/// Lightweight bold detection on a raw `TextItem`. Delegates to
/// `projection::is_bold_item` so per-span and per-line logic stay in sync.
fn is_bold_span(s: &TextItem) -> bool {
    is_bold_item(s)
}

/// Per-span style flags used by the inline-emphasis renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct SpanStyle {
    bold: bool,
    italic: bool,
    mono: bool,
}

impl SpanStyle {
    fn from_item(item: &TextItem) -> Self {
        SpanStyle {
            bold: is_bold_item(item),
            italic: is_italic_item(item),
            mono: is_mono_item(item),
        }
    }

    fn is_plain(self) -> bool {
        !self.bold && !self.italic && !self.mono
    }
}

/// Escape characters that would otherwise be interpreted as markdown emphasis.
/// Deliberately narrow: only `*`, `_`, and backslash. Aggressive escaping
/// (`#`, `[`, backticks, etc.) breaks more output than it saves in practice —
/// pymupdf4llm takes the same conservative stance.
fn escape_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '*' | '_' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Wrap `inner` with the markdown markers for `style`. Mono wins over bold/italic:
/// inline code (`` `…` ``) doesn't compose with emphasis in CommonMark, so when
/// a span is mono we drop the `**/*` wrap. Bold + italic → `***…***`.
fn apply_style(inner: &str, style: SpanStyle) -> String {
    if style.mono {
        // Use backticks; if inner already contains backticks, switch to a
        // longer fence (pair of backticks plus a space buffer) per CommonMark.
        if inner.contains('`') {
            return format!("`` {} ``", inner);
        }
        return format!("`{}`", inner);
    }
    match (style.bold, style.italic) {
        (true, true) => format!("***{}***", inner),
        (true, false) => format!("**{}**", inner),
        (false, true) => format!("*{}*", inner),
        (false, false) => inner.to_string(),
    }
}

/// Render a `ProjectedLine` to markdown with per-span emphasis. Adjacent
/// same-style spans are merged into a single emphasis run; whitespace between
/// spans is preserved as one space (the underlying projection output already
/// has the right inter-word spacing baked into span text).
///
/// Per-line shortcut: when every non-whitespace span shares the same style,
/// emit one outer wrap around the collapsed line text instead of run-by-run
/// (matches pymupdf4llm; avoids `**foo** **bar** **baz**` noise on uniformly
/// styled lines).
///
/// Falls back to `collapse_whitespace(line.text)` when the line has no usable
/// spans (e.g. OCR-only lines where TextItem styling isn't populated).
fn render_line_inline(line: &ProjectedLine) -> String {
    let spans: Vec<&TextItem> = line
        .spans
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .collect();
    if spans.is_empty() {
        return collapse_whitespace(&line.text);
    }

    // Sort spans by x so we render in visual reading order regardless of
    // extraction order. Stable so equal-x spans keep their original sequence.
    let mut spans = spans;
    spans.sort_by(|a, b| a.x.total_cmp(&b.x));

    let styles: Vec<SpanStyle> = spans.iter().map(|s| SpanStyle::from_item(s)).collect();

    // Per-line shortcut.
    let uniform = styles.iter().all(|s| *s == styles[0]);
    if uniform {
        let joined = collapse_whitespace(&line.text);
        if joined.is_empty() {
            return joined;
        }
        let escaped = escape_inline(&joined);
        if styles[0].is_plain() {
            return escaped;
        }
        return apply_style(&escaped, styles[0]);
    }

    // Group consecutive spans by style. Within a group, span texts join with
    // a single space (we lose intra-group spacing precision; acceptable).
    let mut out = String::new();
    let mut i = 0;
    while i < spans.len() {
        let style = styles[i];
        let mut j = i + 1;
        while j < spans.len() && styles[j] == style {
            j += 1;
        }
        let mut group_text = String::new();
        for k in i..j {
            if !group_text.is_empty() && !group_text.ends_with(' ') {
                group_text.push(' ');
            }
            group_text.push_str(spans[k].text.trim());
        }
        let group_text = collapse_whitespace(&group_text);
        let escaped = escape_inline(&group_text);
        let rendered = if style.is_plain() {
            escaped
        } else {
            apply_style(&escaped, style)
        };
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(&rendered);
        i = j;
    }
    out
}

/// Render the text portion of a list item with per-span emphasis. The marker
/// itself isn't included in the output (the renderer handles it separately).
///
/// When the line is uniformly styled we wrap the marker-stripped `rest` with
/// the line's style — this avoids the awkward emphasis-marker mismatch we'd
/// hit if we naively stripped a leading bullet out of an already-wrapped
/// rendered line (`**• item**` → `** item**`).
///
/// When the line is mixed-style we render the full line via the inline pipeline
/// and then best-effort-strip the marker prefix (with optional emphasis wrap
/// around it). On any failure we fall back to plain escaped `rest`.
fn render_list_item_text(line: &ProjectedLine, marker: &str, rest: &str) -> String {
    if let Some(style) = line_uniform_style(line) {
        let plain = collapse_whitespace(rest);
        let escaped = escape_inline(&plain);
        return if style.is_plain() {
            escaped
        } else {
            apply_style(&escaped, style)
        };
    }
    let full = render_line_inline(line);
    if let Some(stripped) = strip_leading_marker_from_inline(&full, marker) {
        return stripped;
    }
    escape_inline(&collapse_whitespace(rest))
}

/// Try to strip a leading list marker (optionally wrapped in emphasis markers)
/// off `s`. Recognizes `***MARK*** `, `**MARK** `, `*MARK* `, `` `MARK` ``,
/// and bare `MARK `. Returns the suffix on a match.
fn strip_leading_marker_from_inline(s: &str, marker: &str) -> Option<String> {
    for wrap in ["***", "**", "*", "`"] {
        let prefix = format!("{wrap}{marker}{wrap} ");
        if let Some(rest) = s.strip_prefix(&prefix) {
            return Some(rest.to_string());
        }
    }
    let prefix = format!("{marker} ");
    s.strip_prefix(&prefix).map(|r| r.to_string())
}

/// Append an inline-rendered continuation line to an existing list-item body.
/// De-hyphenates against the raw text boundary (mirrors the paragraph rule)
/// and falls back to a space join otherwise.
fn append_inline_continuation(prev_text: &mut String, next_raw: &str, next_inline: &str) {
    let next_raw = collapse_whitespace(next_raw);
    if next_raw.is_empty() {
        return;
    }
    if prev_text.is_empty() {
        prev_text.push_str(next_inline);
        return;
    }
    let dehyphenate = prev_text.ends_with('-')
        && next_raw
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase());
    if dehyphenate {
        prev_text.pop();
        prev_text.push_str(next_inline);
    } else {
        prev_text.push(' ');
        prev_text.push_str(next_inline);
    }
}

/// Vertical-gap check for table rows. Looser than paragraph continuation
/// because table rows often have extra padding between them.
fn table_rows_adjacent(prev: &ProjectedLine, cur: &ProjectedLine) -> bool {
    // Intentionally don't require region_path equality. Indented sub-group
    // rows (e.g. an indented "MEMORYBANK" row in a grouped academic results
    // table) sometimes land in a different XY-cut leaf than the rest of the
    // table — but the column-track alignment and y-gap checks below are
    // strong enough signals on their own to keep us from spuriously
    // bridging unrelated regions.
    let prev_bottom = prev.bbox.y + prev.bbox.height;
    let gap = cur.bbox.y - prev_bottom;
    let line_height = prev.bbox.height.max(cur.bbox.height).max(1.0);
    gap >= -line_height && gap <= line_height * TABLE_ROW_GAP_MULTIPLIER
}

/// Coefficient of variation (std-dev / mean) of inter-row vertical gaps.
/// Returns 0.0 for runs with <2 gaps (nothing to compare). Used to reject
/// runs whose row spacing is too irregular to be a real table.
fn row_spacing_cv(rows: &[(usize, &ProjectedLine, Vec<TableCell>)]) -> f32 {
    if rows.len() < 3 {
        return 0.0;
    }
    let gaps: Vec<f32> = rows
        .windows(2)
        .map(|w| (w[1].1.bbox.y - w[0].1.bbox.y).abs())
        .collect();
    let mean = gaps.iter().sum::<f32>() / gaps.len() as f32;
    if mean <= 0.0 {
        return f32::INFINITY;
    }
    let var = gaps.iter().map(|g| (g - mean).powi(2)).sum::<f32>() / gaps.len() as f32;
    var.sqrt() / mean
}

/// Try to extend a candidate table starting at `start_idx`. On success returns
/// a `TableRun` with `Block::Table` or `Block::GridFallback`; on failure
/// returns `None` (and the caller should fall through to per-line
/// classification).
fn try_detect_table(lines: &[ProjectedLine], start_idx: usize) -> Option<TableRun> {
    let first_cells = split_cells(&lines[start_idx]);
    if first_cells.len() < TABLE_MIN_COLUMNS {
        return None;
    }

    let mut rows: Vec<(usize, &ProjectedLine, Vec<TableCell>)> =
        vec![(start_idx, &lines[start_idx], first_cells.clone())];
    let column_count = first_cells.len();
    let tracks: Vec<f32> = first_cells.iter().map(|c| c.start_x).collect();

    let mut j = start_idx + 1;
    while j < lines.len() {
        if !table_rows_adjacent(rows.last().unwrap().1, &lines[j]) {
            break;
        }
        let mut cells = split_cells(&lines[j]);
        if cells.len() < column_count && cells.len() >= TABLE_MIN_COLUMNS {
            // PDFium occasionally merges two (or more) adjacent words into one
            // text run when inter-word kerning is tighter than the gap
            // threshold — common in tightly-set numeric tables (e.g. the
            // "MEMORYBANK 5.00 4.77" case on page 6 of the AMEM paper).
            // Recover by splitting straddling cells on internal whitespace.
            if let Some(patched) = recover_merged_cell(cells.clone(), &tracks) {
                cells = patched;
            }
        }
        if cells.len() != column_count {
            break;
        }
        // Allow at most one column track to drift out of tolerance, which lets
        // grouped row-labels in academic tables (e.g. an indented "MEMORYBANK"
        // row whose label column shifts right by ~30pt while the numeric
        // columns stay aligned) stay in the same run. Without this slack a
        // single indented label fragments a 6-row table into three 2-row chunks.
        let misaligned = cells
            .iter()
            .zip(tracks.iter())
            .filter(|(c, t)| (c.start_x - **t).abs() > TABLE_TRACK_TOLERANCE_PT)
            .count();
        if misaligned > 1 {
            break;
        }
        rows.push((j, &lines[j], cells));
        j += 1;
    }

    if rows.len() < TABLE_MIN_ROWS {
        return None;
    }

    let cv = row_spacing_cv(&rows);
    let end = j;

    if cv > TABLE_ROW_SPACING_MAX_CV {
        // Suggestive layout but the row cadence is too irregular to trust as a
        // clean table — surface as a fenced fallback so the structure is at
        // least preserved.
        let raw: Vec<String> = rows
            .iter()
            .map(|(_, line, _)| line.text.trim_end().to_string())
            .collect();
        return Some(TableRun {
            start: start_idx,
            end,
            block: Block::GridFallback { lines: raw },
        });
    }

    // Promote the first row to header iff every cell in it is bold (matches
    // pymupdf4llm's "bold-or-filled" heuristic; fills require fork data).
    let first_row = &rows[0].2;
    let header_qualifies = first_row.iter().all(|c| c.bold);
    let header = if header_qualifies {
        Some(first_row.iter().map(|c| c.text.clone()).collect())
    } else {
        None
    };
    let row_start = if header.is_some() { 1 } else { 0 };
    let body_rows: Vec<Vec<String>> = rows[row_start..]
        .iter()
        .map(|(_, _, cells)| cells.iter().map(|c| c.text.clone()).collect())
        .collect();
    if header.is_none() && body_rows.len() < TABLE_MIN_ROWS {
        return None;
    }

    Some(TableRun {
        start: start_idx,
        end,
        block: Block::Table {
            header,
            rows: body_rows,
        },
    })
}

/// Scan `lines` once and return all detected tabular regions (sorted by
/// `start`). Caller uses these as cut-points so the per-line classifier never
/// sees lines inside a table.
fn detect_tables(lines: &[ProjectedLine]) -> Vec<TableRun> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(run) = try_detect_table(lines, i) {
            i = run.end;
            out.push(run);
        } else {
            i += 1;
        }
    }
    out
}

// ── Ruled-grid table detection ─────────────────────────────────────────────
//
// Detect tables drawn with explicit horizontal + vertical rules (the "Strong"
// mode in MARKDOWN_PLAN.md). Strokes are clustered into H/V grid lines, then
// union-find groups crossing lines into table regions. For each region the
// distinct row/column boundaries form a cell grid; text lines are assigned to
// cells by centroid containment.
//
// Ruled tables are detected before the borderless `detect_tables`. The caller
// merges the two outputs; overlapping ranges defer to the ruled run because
// path-based geometry is a strictly stronger signal than text alignment alone.

/// Horizontal segment in viewport coords (top-left origin). `y` is the rule's
/// y-position; `x_min..x_max` is its horizontal span. Endpoints of multiple
/// short segments sharing a y get unioned into one wider segment during
/// clustering.
#[derive(Debug, Clone, Copy)]
struct HSeg {
    x_min: f32,
    x_max: f32,
    y: f32,
}

#[derive(Debug, Clone, Copy)]
struct VSeg {
    y_min: f32,
    y_max: f32,
    x: f32,
}

/// Strokes are considered "axis-aligned" when the perpendicular delta is at
/// most this many points. Generous to absorb antialiased near-pixel strokes.
const TABLE_AXIS_TOLERANCE_PT: f32 = 1.0;

/// Two H lines (or two V lines) are merged into one grid line when their
/// perpendicular coords are within this many points. Slightly looser than the
/// axis tolerance because rules drawn at the same row can have ±1pt jitter
/// from different stroke widths.
const TABLE_GRID_CLUSTER_PT: f32 = 2.0;

/// Slack added when checking whether a V line "crosses" an H line. Helps
/// when rules don't quite reach the corner because the PDF drew them as
/// individual segments with small gaps.
const TABLE_CROSS_TOLERANCE_PT: f32 = 3.0;

/// Reject ruled-table candidates whose empty-cell fraction exceeds this.
const TABLE_MAX_EMPTY_CELL_FRACTION: f32 = 0.30;

/// Reject candidates whose grid covers nearly the whole page — almost always
/// a page border, not a real table.
const TABLE_MAX_PAGE_COVERAGE: f32 = 0.95;

/// Extract horizontal and vertical line segments from a page's graphics. Each
/// `Stroke` becomes one HSeg or VSeg depending on orientation; each stroked
/// `Rect` contributes its four edges (cell-border rects, table frames).
fn extract_h_v_segments(graphics: &[GraphicPrimitive]) -> (Vec<HSeg>, Vec<VSeg>) {
    let mut hs = Vec::new();
    let mut vs = Vec::new();
    for g in graphics {
        match g {
            GraphicPrimitive::Stroke { x1, y1, x2, y2, .. } => {
                let (x1, y1, x2, y2) = (*x1, *y1, *x2, *y2);
                let dy = (y1 - y2).abs();
                let dx = (x1 - x2).abs();
                if dy <= TABLE_AXIS_TOLERANCE_PT && dx > 1.0 {
                    hs.push(HSeg {
                        x_min: x1.min(x2),
                        x_max: x1.max(x2),
                        y: (y1 + y2) * 0.5,
                    });
                } else if dx <= TABLE_AXIS_TOLERANCE_PT && dy > 1.0 {
                    vs.push(VSeg {
                        y_min: y1.min(y2),
                        y_max: y1.max(y2),
                        x: (x1 + x2) * 0.5,
                    });
                }
            }
            GraphicPrimitive::Rect { bbox, stroke, .. } => {
                if stroke.is_none() {
                    continue;
                }
                let top = bbox.y;
                let bottom = bbox.y + bbox.height;
                let left = bbox.x;
                let right = bbox.x + bbox.width;
                if bbox.width > 1.0 {
                    hs.push(HSeg {
                        x_min: left,
                        x_max: right,
                        y: top,
                    });
                    hs.push(HSeg {
                        x_min: left,
                        x_max: right,
                        y: bottom,
                    });
                }
                if bbox.height > 1.0 {
                    vs.push(VSeg {
                        y_min: top,
                        y_max: bottom,
                        x: left,
                    });
                    vs.push(VSeg {
                        y_min: top,
                        y_max: bottom,
                        x: right,
                    });
                }
            }
        }
    }
    (hs, vs)
}

/// Cluster H segments sharing a y-coordinate (within `TABLE_GRID_CLUSTER_PT`)
/// into a single wider grid line whose x-extent is the union of the inputs.
fn cluster_h_segments(mut segs: Vec<HSeg>) -> Vec<HSeg> {
    if segs.is_empty() {
        return segs;
    }
    segs.sort_by(|a, b| a.y.total_cmp(&b.y));
    let mut out: Vec<HSeg> = Vec::with_capacity(segs.len());
    for seg in segs {
        if let Some(last) = out.last_mut()
            && (last.y - seg.y).abs() <= TABLE_GRID_CLUSTER_PT
        {
            last.x_min = last.x_min.min(seg.x_min);
            last.x_max = last.x_max.max(seg.x_max);
            continue;
        }
        out.push(seg);
    }
    out
}

fn cluster_v_segments(mut segs: Vec<VSeg>) -> Vec<VSeg> {
    if segs.is_empty() {
        return segs;
    }
    segs.sort_by(|a, b| a.x.total_cmp(&b.x));
    let mut out: Vec<VSeg> = Vec::with_capacity(segs.len());
    for seg in segs {
        if let Some(last) = out.last_mut()
            && (last.x - seg.x).abs() <= TABLE_GRID_CLUSTER_PT
        {
            last.y_min = last.y_min.min(seg.y_min);
            last.y_max = last.y_max.max(seg.y_max);
            continue;
        }
        out.push(seg);
    }
    out
}

/// Union-find root with path compression.
fn uf_find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn uf_union(parent: &mut [usize], a: usize, b: usize) {
    let ra = uf_find(parent, a);
    let rb = uf_find(parent, b);
    if ra != rb {
        parent[ra] = rb;
    }
}

/// Group H/V grid lines that cross each other into connected components.
/// Each component is a candidate ruled table — typically one component per
/// distinct table on the page. Returns `(h_indices, v_indices)` per component,
/// dropping components without ≥2 H and ≥2 V lines (a single L-shape doesn't
/// make a table).
fn find_grid_components(hs: &[HSeg], vs: &[VSeg]) -> Vec<(Vec<usize>, Vec<usize>)> {
    let n_h = hs.len();
    let n_v = vs.len();
    if n_h < 2 || n_v < 2 {
        return Vec::new();
    }
    let n = n_h + n_v;
    let mut parent: Vec<usize> = (0..n).collect();
    let mut connected = vec![false; n];

    let tol = TABLE_CROSS_TOLERANCE_PT;
    for (i, h) in hs.iter().enumerate() {
        for (j, v) in vs.iter().enumerate() {
            let v_crosses_h_x = v.x >= h.x_min - tol && v.x <= h.x_max + tol;
            let h_crosses_v_y = h.y >= v.y_min - tol && h.y <= v.y_max + tol;
            if v_crosses_h_x && h_crosses_v_y {
                uf_union(&mut parent, i, n_h + j);
                connected[i] = true;
                connected[n_h + j] = true;
            }
        }
    }

    use std::collections::HashMap;
    let mut groups: HashMap<usize, (Vec<usize>, Vec<usize>)> = HashMap::new();
    for i in 0..n_h {
        if !connected[i] {
            continue;
        }
        let r = uf_find(&mut parent, i);
        groups.entry(r).or_default().0.push(i);
    }
    for j in 0..n_v {
        if !connected[n_h + j] {
            continue;
        }
        let r = uf_find(&mut parent, n_h + j);
        groups.entry(r).or_default().1.push(j);
    }
    groups
        .into_values()
        .filter(|(h_idx, v_idx)| h_idx.len() >= 2 && v_idx.len() >= 2)
        .collect()
}

/// Build a `TableRun` for one ruled-grid component. Returns `None` if the
/// resulting grid is too small (< 2 cols or < 2 rows), covers nearly the
/// whole page (likely the page border), or is mostly empty cells.
fn build_ruled_table(
    hs: &[HSeg],
    vs: &[VSeg],
    h_indices: &[usize],
    v_indices: &[usize],
    lines: &[ProjectedLine],
    page_width: f32,
    page_height: f32,
) -> Option<TableRun> {
    // Distinct row y-coords (cluster again — multiple H lines may share a y).
    let mut ys: Vec<f32> = h_indices.iter().map(|&i| hs[i].y).collect();
    ys.sort_by(|a, b| a.total_cmp(b));
    dedup_close(&mut ys, TABLE_GRID_CLUSTER_PT);

    let mut xs: Vec<f32> = v_indices.iter().map(|&i| vs[i].x).collect();
    xs.sort_by(|a, b| a.total_cmp(b));
    dedup_close(&mut xs, TABLE_GRID_CLUSTER_PT);

    // Need ≥2 cells per axis → ≥3 boundary lines.
    if ys.len() < 3 || xs.len() < 3 {
        return None;
    }

    let n_rows = ys.len() - 1;
    let n_cols = xs.len() - 1;
    let bbox = crate::types::Rect {
        x: xs[0],
        y: ys[0],
        width: xs[n_cols] - xs[0],
        height: ys[n_rows] - ys[0],
    };

    // Reject page-border-as-table.
    if page_width > 0.0 && page_height > 0.0 {
        let coverage = (bbox.width / page_width) * (bbox.height / page_height);
        if coverage > TABLE_MAX_PAGE_COVERAGE {
            return None;
        }
    }

    // Assign each text line to its cell by centroid.
    let mut cells: Vec<Vec<String>> = vec![vec![String::new(); n_cols]; n_rows];
    let mut cell_is_bold: Vec<Vec<bool>> = vec![vec![true; n_cols]; n_rows];
    let mut cell_has_text: Vec<Vec<bool>> = vec![vec![false; n_cols]; n_rows];
    let mut consumed_indices: Vec<usize> = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        let cx = line.bbox.x + line.bbox.width * 0.5;
        let cy = line.bbox.y + line.bbox.height * 0.5;
        if cy < ys[0] || cy > ys[n_rows] || cx < xs[0] || cx > xs[n_cols] {
            continue;
        }
        let row = match find_bucket(&ys, cy) {
            Some(r) => r,
            None => continue,
        };
        let col = match find_bucket(&xs, cx) {
            Some(c) => c,
            None => continue,
        };
        let txt = line.text.trim();
        if txt.is_empty() {
            continue;
        }
        if !cells[row][col].is_empty() {
            cells[row][col].push(' ');
        }
        cells[row][col].push_str(txt);
        cell_has_text[row][col] = true;
        if !line.all_bold {
            cell_is_bold[row][col] = false;
        }
        consumed_indices.push(idx);
    }

    if consumed_indices.is_empty() {
        return None;
    }

    let total = n_rows * n_cols;
    let empty_count = cell_has_text
        .iter()
        .flatten()
        .filter(|filled| !**filled)
        .count();
    if (empty_count as f32) / (total as f32) > TABLE_MAX_EMPTY_CELL_FRACTION {
        return None;
    }

    // Header = first row iff every non-empty cell in it is bold.
    let header_qualifies = cell_has_text[0]
        .iter()
        .zip(cell_is_bold[0].iter())
        .all(|(has, bold)| !has || *bold)
        && cell_has_text[0].iter().any(|has| *has);
    let header = if header_qualifies {
        Some(cells[0].clone())
    } else {
        None
    };
    let body_start = if header.is_some() { 1 } else { 0 };
    let body_rows: Vec<Vec<String>> = cells[body_start..].to_vec();
    if body_rows.is_empty() {
        return None;
    }

    // Line index span this table covers.
    let start = *consumed_indices.iter().min().unwrap();
    let end = *consumed_indices.iter().max().unwrap() + 1;

    Some(TableRun {
        start,
        end,
        block: Block::Table {
            header,
            rows: body_rows,
        },
    })
}

/// In-place dedup of a sorted Vec, collapsing entries within `tol` to the
/// first of each cluster.
fn dedup_close(v: &mut Vec<f32>, tol: f32) {
    if v.len() < 2 {
        return;
    }
    let mut out: Vec<f32> = Vec::with_capacity(v.len());
    for x in v.iter().copied() {
        if let Some(&last) = out.last()
            && (x - last).abs() <= tol
        {
            continue;
        }
        out.push(x);
    }
    *v = out;
}

/// Find the bucket index `i` such that `boundaries[i] <= val < boundaries[i+1]`.
/// Returns `None` if `val` is outside the boundaries.
fn find_bucket(boundaries: &[f32], val: f32) -> Option<usize> {
    if boundaries.len() < 2 || val < boundaries[0] || val > *boundaries.last().unwrap() {
        return None;
    }
    for (i, w) in boundaries.windows(2).enumerate() {
        if val >= w[0] && val <= w[1] {
            return Some(i);
        }
    }
    None
}

/// Detect ruled-grid tables on a page from its vector graphics. Returns runs
/// in document order (sorted by `start`).
fn detect_ruled_tables(
    lines: &[ProjectedLine],
    graphics: &[GraphicPrimitive],
    page_width: f32,
    page_height: f32,
) -> Vec<TableRun> {
    let (hs, vs) = extract_h_v_segments(graphics);
    let hs = cluster_h_segments(hs);
    let vs = cluster_v_segments(vs);
    if hs.len() < 2 || vs.len() < 2 {
        return Vec::new();
    }
    let components = find_grid_components(&hs, &vs);
    let mut out = Vec::new();
    for (h_idx, v_idx) in components {
        if let Some(run) =
            build_ruled_table(&hs, &vs, &h_idx, &v_idx, lines, page_width, page_height)
        {
            out.push(run);
        }
    }
    out.sort_by_key(|r| r.start);
    out
}

/// Merge ruled-grid runs with borderless runs into a single sorted list. When
/// ranges overlap the ruled run wins (path-based geometry is strictly stronger
/// than text-alignment heuristics) — overlapping borderless runs are dropped.
fn merge_table_runs(mut ruled: Vec<TableRun>, borderless: Vec<TableRun>) -> Vec<TableRun> {
    for b in borderless {
        let overlaps = ruled
            .iter()
            .any(|r| !(b.end <= r.start || b.start >= r.end));
        if !overlaps {
            ruled.push(b);
        }
    }
    ruled.sort_by_key(|r| r.start);
    ruled
}

/// Escape `|` and `\n` inside a markdown table cell so the pipe-table grammar
/// stays valid. Newlines should be impossible inside a single cell (we built
/// cells from spans on the same projected line) but guard anyway.
fn escape_table_cell(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\n', " ")
}

/// Tolerance in points for "is this size larger than the body size".
const HEADING_SIZE_EPSILON: f32 = 0.5;

/// Cap on heading levels (matches plan: H1..H6).
const MAX_HEADING_LEVELS: usize = 6;

/// Multiplier on line height used as the paragraph-break threshold.
const PARAGRAPH_GAP_MULTIPLIER: f32 = 1.5;

/// Tolerance for treating two font sizes as "the same" when grouping
/// paragraph lines. Generous because we sometimes derive the "size" from
/// `bbox.height`, which varies a few points line-to-line based on whether
/// the glyphs include descenders (`y`, `g`, `p`).
const FONT_SIZE_PARAGRAPH_TOLERANCE: f32 = 2.5;

/// Tighter tolerance for matching against the heading-size map. Keeps the
/// heading detector strict so descender-induced height jitter doesn't promote
/// regular body lines to headings.
const FONT_SIZE_HEADING_TOLERANCE: f32 = 0.6;

/// Tolerance in points for treating two indent positions as "the same column".
const INDENT_TOLERANCE: f32 = 6.0;

/// Fraction of page height treated as the "top band" for header detection.
/// Most running headers sit within the top 8–12% of a page; 12% gives some
/// slack for two-line headers without sweeping in body text.
const HEADER_BAND_FRACTION: f32 = 0.12;

/// Fraction of page height treated as the "bottom band" for footer detection.
const FOOTER_BAND_FRACTION: f32 = 0.12;

/// Fraction of pages on which a normalized line must appear (in the same
/// band) to be classified as a running header/footer.
const HEADER_FOOTER_MIN_FRACTION: f32 = 0.5;

/// Absolute floor on header/footer matches — single-page docs can't have a
/// "running" header by definition, and a single match on a 2-page doc is too
/// weak to act on.
const HEADER_FOOTER_MIN_PAGES: usize = 2;

/// Roughly one indent step in PDF points. Used to bucket list items into
/// nesting levels relative to the first item of the list.
const LIST_INDENT_STEP_PT: f32 = 12.0;

/// Maximum characters in a "bold body-size heading" candidate. Section
/// headings like "Abstract", "1 Introduction", "2.1 Related Work" are short;
/// a bold body-size line longer than this is almost always a bold *sentence*
/// inside a paragraph, not a heading.
const BOLD_HEADING_MAX_CHARS: usize = 80;

/// Returns true if `line` looks like a section heading rendered in body-size
/// bold text (a very common style for academic / technical PDFs where every
/// "real" heading uses the same font size as body, distinguished only by
/// weight). Requires:
///   - uniformly bold across all spans
///   - short (≤ `BOLD_HEADING_MAX_CHARS`)
///   - paragraph-break gap above (or first line on the page)
///   - paragraph-break gap below (or last line on the page)
fn looks_like_bold_heading(
    line: &ProjectedLine,
    prev: Option<&ProjectedLine>,
    next: Option<&ProjectedLine>,
) -> bool {
    let text = line.text.trim();
    if text.is_empty() || text.chars().count() > BOLD_HEADING_MAX_CHARS {
        return false;
    }
    let style = match line_uniform_style(line) {
        Some(s) => s,
        None => return false,
    };
    if !style.bold || style.mono {
        return false;
    }
    // Reject bold-uniform lines dominated by digits / punctuation — these are
    // almost always cells inside a tabular layout the table detector didn't
    // pick up (results tables, scoreboards, math display). A real section
    // heading is mostly letters: "1 Introduction" passes (~92% alpha across
    // non-whitespace chars), "47.5 14" doesn't (0%), "BLEU-1 25.87" doesn't.
    let mut alpha = 0usize;
    let mut total = 0usize;
    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }
        total += 1;
        if c.is_alphabetic() {
            alpha += 1;
        }
    }
    if total == 0 || (alpha as f32) / (total as f32) < 0.5 {
        return false;
    }
    // Reject when the line itself looks tabular: ≥3 cells separated by font-size
    // gaps. A bold body-size line with that many cell tracks is almost always a
    // table header row, not a section heading. Without this guard, multi-line
    // table headers ("Model Method ... | F1 BLEU-1 F1 BLEU-1 ...") get promoted
    // to H3 instead of being absorbed by the table detector.
    // Tabular shape rejection. Two passes because the projection sometimes
    // collapses a wide multi-column line into a single span with column-
    // padding spaces — span-based detection misses those.
    if split_cells(line).len() >= TABLE_MIN_COLUMNS {
        return false;
    }
    // Text-based fallback: ≥3 tokens separated by runs of 2+ spaces (the
    // projection inserts alignment padding between cells) → table header row
    // collapsed into one span, not a section heading.
    let multi_space_tokens = text
        .split("  ")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .count();
    if multi_space_tokens >= TABLE_MIN_COLUMNS {
        return false;
    }
    let gap_above_ok = match prev {
        None => true,
        Some(p) => !continues_paragraph(p, line),
    };
    if !gap_above_ok {
        return false;
    }
    let gap_below_ok = match next {
        None => true,
        Some(n) => !continues_paragraph(line, n),
    };
    gap_below_ok
}

/// Characters recognized as bullet markers when followed by whitespace.
/// Limited to glyphs that are unlikely to appear at line-start in normal prose.
const BULLET_CHARS: &[char] = &['•', '·', '◦', '▪', '▸', '▶', '●', '○', '■', '□'];

/// Detect a list marker at the start of `text`. Returns `(ordered, marker_str,
/// remainder)` when matched; otherwise `None`.
///
/// Recognizes:
/// - Unicode bullet characters (`BULLET_CHARS`) followed by whitespace.
/// - Decimal-prefix markers like `1.` / `1)` / `12.` / `12)` followed by
///   whitespace — kept strict (digits only) so things like footnote callers
///   (`1` alone) and section refs (`A.1`) don't match.
fn parse_list_marker(text: &str) -> Option<(bool, String, &str)> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    let mut chars = trimmed.chars();
    let first = chars.next()?;

    // Unicode bullet
    if BULLET_CHARS.contains(&first) {
        let rest = chars.as_str();
        if let Some(rest_trim) = rest.strip_prefix(|c: char| c.is_whitespace()) {
            return Some((false, first.to_string(), rest_trim.trim_start()));
        }
    }

    // Decimal: 1. / 1) / 12. / 12)
    if first.is_ascii_digit() {
        let mut digit_end = 1;
        for c in trimmed[1..].chars() {
            if c.is_ascii_digit() {
                digit_end += c.len_utf8();
            } else {
                break;
            }
        }
        // Cap to keep us from matching page-number-like prefixes
        if digit_end <= 3 {
            let after_digits = &trimmed[digit_end..];
            let mut after_iter = after_digits.chars();
            if let Some(punct) = after_iter.next()
                && (punct == '.' || punct == ')')
            {
                let after_punct = after_iter.as_str();
                if let Some(rest_trim) = after_punct.strip_prefix(|c: char| c.is_whitespace()) {
                    let marker = format!("{}{}", &trimmed[..digit_end], punct);
                    return Some((true, marker, rest_trim.trim_start()));
                }
            }
        }
    }

    None
}

/// Returns true if any span on the line is rotated more than ~5° off
/// horizontal — used to exclude sidebar / margin-stamp text (arXiv banners,
/// watermarks, vertical legends) from the body-size and heading-size
/// histograms so it doesn't compete with normal-flow text for heading slots.
fn is_rotated_line(line: &ProjectedLine) -> bool {
    line.spans.iter().any(|s| {
        let r = s.rotation.abs() % 360.0;
        // Anything more than ~5° off the horizontal axes is "rotated" for
        // our purposes. 0° and 180° are both horizontal text.
        !(r < 5.0 || (175.0..=185.0).contains(&r) || (355.0..=360.0).contains(&r))
    })
}

/// Compute the body font size as the char-weighted mode across all lines in
/// all pages. Rotated lines are excluded so a long rotated sidebar can't
/// pull the body estimate. Falls back to `0.0` when no font-size info is
/// available.
pub fn compute_body_size(pages: &[ParsedPage]) -> f32 {
    use std::collections::HashMap;
    let mut weights: HashMap<u32, (f32, usize)> = HashMap::new();
    for page in pages {
        for line in &page.projected_lines {
            if is_rotated_line(line) {
                continue;
            }
            let size = line.dominant_font_size;
            if size <= 0.0 {
                continue;
            }
            let chars = line.text.chars().count().max(1);
            let key = (size * 100.0).round() as u32;
            let entry = weights.entry(key).or_insert((size, 0));
            entry.1 += chars;
        }
    }
    weights
        .values()
        .max_by_key(|(_, n)| *n)
        .map(|(s, _)| *s)
        .unwrap_or(0.0)
}

/// Minimum total non-whitespace characters across all occurrences at a font
/// size for it to qualify as a heading level. Calibrated against `paper.pdf`:
/// the 30pt chart-legend tokens ("A-mem"×2 + "Base"×2 = 18-20 chars) need to
/// fail this filter, while the 14.35pt title ("A-MEM: Agentic Memory for LLM
/// Agents" = 31 chars on a single line) needs to pass. 25 is the gap. Smaller
/// single-word headings like a lone "Summary" on a short doc still survive
/// because they share their font size with other (larger) headings in the
/// histogram entry.
const MIN_HEADING_TOTAL_CHARS: usize = 25;

/// Maximum average characters per line for a size to qualify as a heading.
/// A "size larger than body" with very long lines is almost always a
/// large-print body block (callouts, footnotes-as-display, intro paragraph),
/// not a real heading.
const MAX_HEADING_AVG_LINE_CHARS: f32 = 200.0;

/// Minimum fraction of non-whitespace chars at a size that must be alphabetic
/// for it to qualify as a heading. Drops sizes dominated by digits (graph
/// axes, results tables, math display) which otherwise pollute the top
/// heading slots.
const MIN_HEADING_ALPHA_RATIO: f32 = 0.5;

/// Build a heading-size → level map: sizes strictly larger than `body_size`,
/// filtered to those with at least `MIN_HEADING_LINES` distinct occurrences
/// (drops one-off equation/figure-label artifacts), sorted descending, mapped
/// to levels 1..=MAX_HEADING_LEVELS.
pub fn build_heading_map(pages: &[ParsedPage], body_size: f32) -> Vec<(f32, u8)> {
    use std::collections::HashMap;
    // (size_key → (size, line_count, total_chars, alpha_chars))
    let mut sizes: HashMap<u32, (f32, usize, usize, usize)> = HashMap::new();
    for page in pages {
        for line in &page.projected_lines {
            if is_rotated_line(line) {
                continue;
            }
            let size = line.dominant_font_size;
            if size > body_size + HEADING_SIZE_EPSILON {
                let key = (size * 100.0).round() as u32;
                let entry = sizes.entry(key).or_insert((size, 0, 0, 0));
                entry.1 += 1;
                for c in line.text.chars() {
                    if c.is_whitespace() {
                        continue;
                    }
                    entry.2 += 1;
                    if c.is_alphabetic() {
                        entry.3 += 1;
                    }
                }
            }
        }
    }
    let all: Vec<(f32, usize, usize, usize)> = sizes.into_values().collect();
    // Always apply quality filters: total-char floor, average-line cap, and
    // alpha-ratio floor. The total-char floor (rather than a line-count one)
    // lets one-off titles survive — a 31-char title on a single line passes —
    // while still rejecting chart-legend tokens like "A-mem" + "Base" that
    // total fewer chars across 4 occurrences than a single heading line does.
    let mut kept: Vec<f32> = all
        .iter()
        .filter(|(_, lines, chars, alpha)| {
            let alpha_ratio = if *chars == 0 {
                0.0
            } else {
                (*alpha as f32) / (*chars as f32)
            };
            *chars >= MIN_HEADING_TOTAL_CHARS
                && (*chars as f32 / (*lines).max(1) as f32) <= MAX_HEADING_AVG_LINE_CHARS
                && alpha_ratio >= MIN_HEADING_ALPHA_RATIO
        })
        .map(|(s, _, _, _)| *s)
        .collect();
    kept.sort_by(|a, b| b.total_cmp(a));
    kept.truncate(MAX_HEADING_LEVELS);
    kept.into_iter()
        .enumerate()
        .map(|(i, s)| (s, (i + 1) as u8))
        .collect()
}

fn heading_level_for(size: f32, heading_map: &[(f32, u8)]) -> Option<u8> {
    for (s, level) in heading_map {
        if (size - *s).abs() < FONT_SIZE_HEADING_TOLERANCE {
            return Some(*level);
        }
    }
    None
}

/// Collapse runs of whitespace into single spaces. The projected text from
/// `projection.rs` pads with column-alignment spaces (e.g. `for    instance`)
/// which look fine as a layout grid but are wrong for prose.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Append `next` to `accum` as the continuation of a paragraph. De-hyphenates
/// when `accum` ends with `-` and `next` starts with an ASCII lowercase letter;
/// otherwise joins with a single space.
#[cfg(test)]
fn append_paragraph_line(accum: &mut String, next: &str) {
    let next = collapse_whitespace(next);
    if next.is_empty() {
        return;
    }
    if accum.is_empty() {
        accum.push_str(&next);
        return;
    }
    let dehyphenate =
        accum.ends_with('-') && next.chars().next().is_some_and(|c| c.is_ascii_lowercase());
    if dehyphenate {
        accum.pop(); // drop the '-'
        accum.push_str(&next);
    } else {
        accum.push(' ');
        accum.push_str(&next);
    }
}

/// Decide whether `cur` continues the paragraph started by `prev`.
fn continues_paragraph(prev: &ProjectedLine, cur: &ProjectedLine) -> bool {
    // Anchor only signals a paragraph break when one of the lines is clearly
    // centered while the other isn't — justified prose routinely alternates
    // between Left / Right / Floating dominant anchors as line widths flex,
    // and treating those as paragraph breaks shreds normal text.
    let centered_mismatch = (prev.anchor == Anchor::Center) ^ (cur.anchor == Anchor::Center);
    if centered_mismatch {
        return false;
    }
    if (prev.dominant_font_size - cur.dominant_font_size).abs() > FONT_SIZE_PARAGRAPH_TOLERANCE {
        return false;
    }
    if prev.region_path != cur.region_path {
        // Cross-region continuation: the same paragraph can wrap from the
        // bottom of one column into the top of the next. Only bridge regions
        // when the previous line clearly breaks mid-sentence (no terminal
        // punctuation) AND the next line starts with a lowercase letter — a
        // strict signal that catches the column-wrap case while rejecting
        // unrelated paragraphs that happen to sit in adjacent leaves.
        let prev_trim = prev.text.trim_end();
        let ends_open = !prev_trim.ends_with(|c: char| {
            matches!(
                c,
                '.' | '!' | '?' | ':' | ';' | '”' | '"' | ')' | ']' | '。' | '』' | '」'
            )
        });
        let starts_lower = cur
            .text
            .trim_start()
            .chars()
            .next()
            .is_some_and(|c| c.is_lowercase());
        return ends_open && starts_lower;
    }
    if (prev.indent_x - cur.indent_x).abs() > INDENT_TOLERANCE && cur.anchor == Anchor::Left {
        // Indent change on a left-aligned block usually means a new paragraph
        // (block-quote, list, indented passage, etc.). Allow first-line indent
        // by checking only when the *next* line shifts right relative to prev.
        if cur.indent_x > prev.indent_x + INDENT_TOLERANCE {
            return false;
        }
    }
    // Vertical gap check.
    let prev_bottom = prev.bbox.y + prev.bbox.height;
    let gap = cur.bbox.y - prev_bottom;
    let line_height = prev.bbox.height.max(cur.bbox.height).max(1.0);
    gap <= line_height * PARAGRAPH_GAP_MULTIPLIER
}

/// Paragraph accumulator state. We track two parallel representations of the
/// running paragraph text:
///
/// - `raw` — plain text (no emphasis markers). Used for the paragraph-uniform
///   shortcut: if every contributing line had the same uniform style, we wrap
///   the whole paragraph once with `wrap_emphasis(raw, …)` to avoid the
///   `**foo** **bar** **baz**` per-line noise pymupdf4llm warns about.
/// - `inline` — per-line markdown with emphasis baked in via
///   `render_line_inline`. Used when the paragraph contains mid-line emphasis
///   shifts or lines with differing uniform styles.
///
/// `uniform` is `Some((bold, italic))` while every line so far has been a
/// uniformly-styled line sharing the same (bold, italic) flags, and `None` as
/// soon as that invariant breaks.
struct ParaAccum {
    raw: String,
    inline: String,
    last: ProjectedLine,
    uniform: Option<(bool, bool)>,
}

/// Returns the shared `SpanStyle` of `line` when every non-whitespace span on
/// the line has the same style; `None` when spans disagree. Mono is folded
/// into the style for the purpose of "uniform" — a fully-mono line is
/// uniform-mono. Used by the paragraph-level optimization to decide whether
/// to wrap once around the whole paragraph or fall back to per-line inline.
fn line_uniform_style(line: &ProjectedLine) -> Option<SpanStyle> {
    let mut iter = line
        .spans
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .map(SpanStyle::from_item);
    let first = iter.next()?;
    for s in iter {
        if s != first {
            return None;
        }
    }
    Some(first)
}

/// Append `next_line` to a paragraph accumulator. Maintains both the `raw` and
/// `inline` text representations and updates the running `uniform` flag.
/// De-hyphenation runs on the `raw` boundary; the `inline` boundary mirrors it
/// when the trailing char is still a literal `-` (i.e. the hyphen sits outside
/// any emphasis wrap — the common case).
fn append_to_paragraph(accum: &mut ParaAccum, next_line: &ProjectedLine) {
    let next_raw = collapse_whitespace(next_line.text.trim());
    if next_raw.is_empty() {
        return;
    }
    let next_inline = render_line_inline(next_line);
    let next_uniform = line_uniform_style(next_line);

    if accum.raw.is_empty() {
        accum.raw.push_str(&next_raw);
        accum.inline.push_str(&next_inline);
        accum.uniform = next_uniform.map(|s| (s.bold, s.italic));
        accum.last = next_line.clone();
        return;
    }

    let dehyphenate = accum.raw.ends_with('-')
        && next_raw
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase());
    if dehyphenate {
        accum.raw.pop();
        accum.raw.push_str(&next_raw);
        if accum.inline.ends_with('-') {
            accum.inline.pop();
            accum.inline.push_str(&next_inline);
        } else {
            // Hyphen sits inside an emphasis wrap — give up on stripping it
            // from `inline` cleanly. Join with a space; raw is still correct
            // for the uniform-paragraph case.
            accum.inline.push(' ');
            accum.inline.push_str(&next_inline);
        }
    } else {
        accum.raw.push(' ');
        accum.raw.push_str(&next_raw);
        accum.inline.push(' ');
        accum.inline.push_str(&next_inline);
    }

    // Maintain the running uniform-style flag.
    accum.uniform = match (accum.uniform, next_uniform) {
        (Some(cur), Some(s)) if cur == (s.bold, s.italic) => Some(cur),
        _ => None,
    };
    accum.last = next_line.clone();
}

/// Resolve a `ParaAccum` to a `Block::Paragraph`. When the paragraph was
/// uniformly styled across all lines, return the raw text with block-level
/// `bold`/`italic` flags set so the renderer wraps it once. Otherwise return
/// the per-line inline-styled text with the flags cleared.
fn paragraph_from_accum(accum: ParaAccum) -> Block {
    match accum.uniform {
        Some((bold, italic)) if bold || italic => Block::Paragraph {
            text: escape_inline(&accum.raw),
            bold,
            italic,
        },
        Some(_) => Block::Paragraph {
            // Uniformly plain — no emphasis markers anywhere, so the raw text
            // (with markdown specials escaped) is the right rendering.
            text: escape_inline(&accum.raw),
            bold: false,
            italic: false,
        },
        None => Block::Paragraph {
            text: accum.inline,
            bold: false,
            italic: false,
        },
    }
}

/// Normalize a line for cross-page header/footer matching. Lowercases,
/// collapses whitespace, and replaces every run of ASCII digits with `#` so
/// `Page 1 of 6` and `Page 2 of 6` collapse to the same key.
fn normalize_for_repetition(s: &str) -> String {
    let collapsed = collapse_whitespace(s).to_lowercase();
    let mut out = String::with_capacity(collapsed.len());
    let mut in_digits = false;
    for c in collapsed.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('#');
                in_digits = true;
            }
        } else {
            out.push(c);
            in_digits = false;
        }
    }
    out
}

/// Cross-page repetition detector. Returns the set of normalized strings that
/// appear in the top or bottom band of ≥ `HEADER_FOOTER_MIN_FRACTION` of
/// pages (capped below by `HEADER_FOOTER_MIN_PAGES`). The caller uses this to
/// filter `ProjectedLine`s before classification.
///
/// "Same band" means a line whose top is within `HEADER_BAND_FRACTION` of the
/// page top (header) or whose bottom is within `FOOTER_BAND_FRACTION` of the
/// page bottom (footer). Header and footer bands are tracked separately so a
/// company name that appears as both a header and a body-section title on
/// different pages isn't stripped from the body.
pub fn compute_header_footer_set(pages: &[ParsedPage]) -> std::collections::HashSet<String> {
    use std::collections::{HashMap, HashSet};
    let mut set: HashSet<String> = HashSet::new();
    if pages.len() < HEADER_FOOTER_MIN_PAGES {
        return set;
    }
    // Two counters keyed by `(band, normalized_text)` — band is `'h'` or `'f'`.
    let mut counts: HashMap<(char, String), HashSet<usize>> = HashMap::new();
    for page in pages {
        let header_cutoff = page.page_height * HEADER_BAND_FRACTION;
        let footer_cutoff = page.page_height * (1.0 - FOOTER_BAND_FRACTION);
        for line in &page.projected_lines {
            let text = line.text.trim();
            if text.is_empty() {
                continue;
            }
            let norm = normalize_for_repetition(text);
            if norm.is_empty() {
                continue;
            }
            // Header band: top of line within the top band.
            if line.bbox.y <= header_cutoff {
                counts
                    .entry(('h', norm.clone()))
                    .or_default()
                    .insert(page.page_number);
            }
            // Footer band: bottom of line at or below the footer cutoff.
            let line_bottom = line.bbox.y + line.bbox.height;
            if line_bottom >= footer_cutoff {
                counts
                    .entry(('f', norm))
                    .or_default()
                    .insert(page.page_number);
            }
        }
    }
    let threshold = (pages.len() as f32 * HEADER_FOOTER_MIN_FRACTION)
        .ceil()
        .max(HEADER_FOOTER_MIN_PAGES as f32) as usize;
    for ((_, norm), pages_seen) in counts {
        if pages_seen.len() >= threshold {
            set.insert(norm);
        }
    }
    set
}

/// Returns true if `line` (located on `page`) matches the running
/// header/footer set: the line sits in the top or bottom band AND its
/// normalized text is in `header_footer`.
fn is_header_or_footer(
    line: &ProjectedLine,
    page: &ParsedPage,
    header_footer: &std::collections::HashSet<String>,
) -> bool {
    if header_footer.is_empty() {
        return false;
    }
    let header_cutoff = page.page_height * HEADER_BAND_FRACTION;
    let footer_cutoff = page.page_height * (1.0 - FOOTER_BAND_FRACTION);
    let in_band = line.bbox.y <= header_cutoff || line.bbox.y + line.bbox.height >= footer_cutoff;
    if !in_band {
        return false;
    }
    let norm = normalize_for_repetition(line.text.trim());
    header_footer.contains(&norm)
}

/// Classify a single page's `ProjectedLine`s into blocks.
pub fn classify_page(page: &ParsedPage, heading_map: &[(f32, u8)]) -> Vec<Block> {
    classify_page_with_filters(page, heading_map, &std::collections::HashSet::new())
}

/// Same as `classify_page` but also strips lines matching a precomputed
/// running header/footer set. Use this when emitting a whole document so
/// repeating chrome (titles, page numbers) doesn't show up in every page.
pub fn classify_page_with_filters(
    page: &ParsedPage,
    heading_map: &[(f32, u8)],
    header_footer: &std::collections::HashSet<String>,
) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut paragraph: Option<ParaAccum> = None;
    // Active fenced code block being accumulated (consecutive `all_mono` lines).
    let mut code: Option<Vec<String>> = None;
    // Tracks the "level 0" indent of the current contiguous list run so we can
    // bucket deeper items into nesting levels. Reset whenever a non-list block
    // breaks the run.
    let mut list_base_indent: Option<f32> = None;
    // Index into `blocks` of the most recent ListItem in the current run — used
    // to merge wrapped continuation lines into the same item.
    let mut last_list_item_idx: Option<usize> = None;
    // Most recent ProjectedLine appended to the active list item, for
    // gap/font-size checks on continuation lines.
    let mut last_list_line: Option<ProjectedLine> = None;

    let flush_paragraph = |blocks: &mut Vec<Block>, p: Option<ParaAccum>| {
        if let Some(acc) = p
            && !acc.raw.trim().is_empty()
        {
            blocks.push(paragraph_from_accum(acc));
        }
    };
    let flush_code = |blocks: &mut Vec<Block>, c: Option<Vec<String>>| {
        if let Some(lines) = c
            && !lines.is_empty()
        {
            blocks.push(Block::CodeBlock { lines });
        }
    };

    let debug = std::env::var("LITEPARSE_DEBUG_MD").is_ok();

    // Strip running header/footer lines up-front so they don't leak into
    // table detection (a repeating two-column footer would otherwise look
    // like a 2-row table) or paragraph grouping.
    let filtered_owned: Vec<ProjectedLine> = if header_footer.is_empty() {
        Vec::new()
    } else {
        page.projected_lines
            .iter()
            .filter(|l| !is_header_or_footer(l, page, header_footer))
            .cloned()
            .collect()
    };
    let lines: &[ProjectedLine] = if header_footer.is_empty() {
        &page.projected_lines
    } else {
        &filtered_owned
    };

    // Pre-pass: detect tabular regions so the per-line classifier below can
    // skip over them. Tables take priority over heading / paragraph / list
    // classification because a row of bold short cells would otherwise be
    // misread as a heading or list item.
    //
    // Two detectors run in sequence: ruled-grid (path-based, strongest signal)
    // and the borderless column-alignment fallback. Where ranges overlap, the
    // ruled output wins.
    let ruled_runs = detect_ruled_tables(lines, &page.graphics, page.page_width, page.page_height);
    let borderless_runs = detect_tables(lines);
    let table_runs = merge_table_runs(ruled_runs, borderless_runs);

    // Suppress HRs that fall inside a detected table's y-range — they're the
    // table's own row dividers, not document-level section breaks. Build the
    // y-extents once before we move table_runs into the iterator.
    // Extend each table's HR-suppression band upward to cover any
    // header/sub-header rows we didn't absorb into the table. The expansion
    // is a few row heights — large enough to catch a 2–3 line bold/italic
    // header sitting just above the table, small enough not to swallow a
    // real section divider belonging to a different block. The downward
    // edge gets a small slack to catch HRs drawn flush with the last row.
    const TABLE_HR_SUPPRESS_HEADROOM_ROWS: f32 = 4.0;
    let table_y_extents: Vec<(f32, f32)> = table_runs
        .iter()
        .map(|run| {
            let top_line = &lines[run.start];
            let row_h = top_line.bbox.height.max(8.0);
            let top = top_line.bbox.y - row_h * TABLE_HR_SUPPRESS_HEADROOM_ROWS;
            let last = &lines[run.end.saturating_sub(1).max(run.start)];
            let bot = last.bbox.y + last.bbox.height;
            (top, bot)
        })
        .collect();

    let mut table_iter = table_runs.into_iter().peekable();

    // Pre-pass: detect horizontal rules from vector graphics so they can be
    // emitted in document order between surrounding text lines.
    let hr_ys: Vec<f32> = detect_horizontal_rules(page)
        .into_iter()
        .filter(|y| {
            !table_y_extents
                .iter()
                .any(|(top, bot)| *y >= *top - 2.0 && *y <= *bot + 2.0)
        })
        .collect();
    let mut hr_iter = hr_ys.into_iter().peekable();

    // Emit any HRs whose y is at or above `before_y`. Flushes the active
    // paragraph/code/list state first so the rule lands as its own block.
    let emit_hrs_before = |blocks: &mut Vec<Block>,
                           paragraph: &mut Option<ParaAccum>,
                           code: &mut Option<Vec<String>>,
                           list_base: &mut Option<f32>,
                           last_item: &mut Option<usize>,
                           last_line: &mut Option<ProjectedLine>,
                           hr_iter: &mut std::iter::Peekable<std::vec::IntoIter<f32>>,
                           before_y: f32| {
        while let Some(&hy) = hr_iter.peek() {
            if hy > before_y {
                break;
            }
            hr_iter.next();
            flush_paragraph(blocks, paragraph.take());
            flush_code(blocks, code.take());
            *list_base = None;
            *last_item = None;
            *last_line = None;
            blocks.push(Block::HorizontalRule);
        }
    };

    let mut idx = 0;
    while idx < lines.len() {
        if let Some(run) = table_iter.peek()
            && run.start == idx
        {
            // Flush any HRs above this table's top edge first.
            let table_top = lines[run.start].bbox.y;
            emit_hrs_before(
                &mut blocks,
                &mut paragraph,
                &mut code,
                &mut list_base_indent,
                &mut last_list_item_idx,
                &mut last_list_line,
                &mut hr_iter,
                table_top,
            );
            flush_paragraph(&mut blocks, paragraph.take());
            flush_code(&mut blocks, code.take());
            list_base_indent = None;
            last_list_item_idx = None;
            last_list_line = None;
            let run = table_iter.next().unwrap();
            blocks.push(run.block);
            idx = run.end;
            continue;
        }
        let line = &lines[idx];
        // Emit any HRs that fall above this line.
        emit_hrs_before(
            &mut blocks,
            &mut paragraph,
            &mut code,
            &mut list_base_indent,
            &mut last_list_item_idx,
            &mut last_list_line,
            &mut hr_iter,
            line.bbox.y,
        );
        idx += 1;
        let text = line.text.trim();
        if text.is_empty() {
            continue;
        }
        // Skip rotated text (vertical sidebars, arXiv banners, watermarks).
        // Including it would either inject a paragraph of disconnected
        // characters or be misclassified as a heading.
        if is_rotated_line(line) {
            continue;
        }
        if debug {
            eprintln!(
                "[md] y={:.1} h={:.1} size={:.2} anchor={:?} indent={:.1} text={:?}",
                line.bbox.y,
                line.bbox.height,
                line.dominant_font_size,
                line.anchor,
                line.indent_x,
                text
            );
        }

        // Code block detection runs first so a mono heading-shaped line
        // (rare but plausible — e.g., a code identifier in a large mono font)
        // still becomes code. Mono content also wouldn't make a useful
        // heading.
        if line.all_mono {
            flush_paragraph(&mut blocks, paragraph.take());
            list_base_indent = None;
            last_list_item_idx = None;
            last_list_line = None;
            code.get_or_insert_with(Vec::new)
                .push(line.text.trim_end().to_string());
            continue;
        }
        // Any non-mono line ends the current code block (if any).
        flush_code(&mut blocks, code.take());

        if let Some(level) = heading_level_for(line.dominant_font_size, heading_map) {
            flush_paragraph(&mut blocks, paragraph.take());
            list_base_indent = None;
            last_list_item_idx = None;
            last_list_line = None;
            blocks.push(Block::Heading {
                level,
                text: collapse_whitespace(text),
            });
            continue;
        }

        // List item?
        if let Some((ordered, marker, rest)) = parse_list_marker(text) {
            flush_paragraph(&mut blocks, paragraph.take());
            let base = *list_base_indent.get_or_insert(line.indent_x);
            let level = (((line.indent_x - base) / LIST_INDENT_STEP_PT)
                .round()
                .max(0.0)) as u8;
            last_list_item_idx = Some(blocks.len());
            last_list_line = Some(line.clone());
            // Render the list-item text via the inline pipeline so per-span
            // emphasis surfaces. We strip the marker from `rest` (already
            // done by `parse_list_marker`), but emphasis lives on `line.spans`,
            // which still contain the marker span — render the line and then
            // peel the marker off the front of the rendered string.
            let item_text = render_list_item_text(line, &marker, rest);
            blocks.push(Block::ListItem {
                ordered,
                marker,
                level,
                text: item_text,
                bold: false,
                italic: false,
            });
            continue;
        }

        // Continuation of a list item: same gap/font rules as paragraphs.
        // Footnote-style continuations often left-flush below the marker's
        // hanging indent, so we don't require indent ≥ marker indent.
        if let Some(idx) = last_list_item_idx
            && let Some(prev_line) = last_list_line.as_ref()
            && continues_paragraph(prev_line, line)
            && let Some(Block::ListItem {
                text: prev_text, ..
            }) = blocks.get_mut(idx)
        {
            // De-hyphenate against the prior rendered text, then append the
            // inline-styled continuation.
            let cont_inline = render_line_inline(line);
            append_inline_continuation(prev_text, text, &cont_inline);
            last_list_line = Some(line.clone());
            continue;
        }

        // Bold body-size heading. Section headings in academic / technical
        // PDFs are routinely body-sized + bold (e.g. "Abstract",
        // "1 Introduction"); without this rule they look just like a bold
        // first sentence of a paragraph. Runs after list-marker detection so
        // bold bullet items stay as list items.
        let prev_for_gap = paragraph
            .as_ref()
            .map(|p| &p.last)
            .or(last_list_line.as_ref());
        let next_for_gap = lines.get(idx);
        if looks_like_bold_heading(line, prev_for_gap, next_for_gap) {
            flush_paragraph(&mut blocks, paragraph.take());
            list_base_indent = None;
            last_list_item_idx = None;
            last_list_line = None;
            // Level: one deeper than the deepest size-based level we already
            // have. With an empty heading_map this lands on H1; with a full
            // 6-level map it caps at H6.
            let level = (heading_map.len() as u8 + 1).clamp(1, MAX_HEADING_LEVELS as u8);
            blocks.push(Block::Heading {
                level,
                text: collapse_whitespace(text),
            });
            continue;
        }

        match paragraph.as_mut() {
            Some(acc) if continues_paragraph(&acc.last, line) => {
                append_to_paragraph(acc, line);
            }
            _ => {
                flush_paragraph(&mut blocks, paragraph.take());
                list_base_indent = None;
                last_list_item_idx = None;
                last_list_line = None;
                let inline = render_line_inline(line);
                let raw = collapse_whitespace(text);
                let uniform = line_uniform_style(line).map(|s| (s.bold, s.italic));
                paragraph = Some(ParaAccum {
                    raw,
                    inline,
                    last: line.clone(),
                    uniform,
                });
            }
        }
    }

    flush_paragraph(&mut blocks, paragraph.take());
    flush_code(&mut blocks, code.take());
    // Flush any trailing HRs that sat below the last text line.
    emit_hrs_before(
        &mut blocks,
        &mut paragraph,
        &mut code,
        &mut list_base_indent,
        &mut last_list_item_idx,
        &mut last_list_line,
        &mut hr_iter,
        f32::INFINITY,
    );
    blocks
}

/// Wrap `text` in markdown emphasis markers based on `bold`/`italic`. Both →
/// `***text***`. Headings deliberately skip this (the `#` is the emphasis).
fn wrap_emphasis(text: &str, bold: bool, italic: bool) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    match (bold, italic) {
        (true, true) => format!("***{text}***"),
        (true, false) => format!("**{text}**"),
        (false, true) => format!("*{text}*"),
        (false, false) => text.to_string(),
    }
}

/// Render a list of blocks to a markdown string.
pub fn render_blocks(blocks: &[Block]) -> String {
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            // Consecutive list items render as a tight list (single newline).
            // Everything else gets a blank line between blocks.
            let tight = matches!(block, Block::ListItem { .. })
                && matches!(blocks[i - 1], Block::ListItem { .. });
            if tight {
                out.push('\n');
            } else {
                out.push_str("\n\n");
            }
        }
        match block {
            Block::Heading { level, text } => {
                let level = (*level).clamp(1, 6) as usize;
                out.push_str(&"#".repeat(level));
                out.push(' ');
                out.push_str(text);
            }
            Block::Paragraph { text, bold, italic } => {
                out.push_str(&wrap_emphasis(text, *bold, *italic));
            }
            Block::ListItem {
                ordered,
                marker,
                level,
                text,
                bold,
                italic,
            } => {
                let indent = "  ".repeat((*level).min(6) as usize);
                out.push_str(&indent);
                if *ordered {
                    // Preserve the original marker (e.g. `138.` for footnotes
                    // or `iii)` for roman numerals) so semantic numbering
                    // survives the round-trip. CommonMark requires `1.` /
                    // `1)` style but most LLM consumers tolerate alt forms,
                    // and the alternative — renumbering as `1.` — drops info.
                    out.push_str(marker);
                    out.push(' ');
                } else {
                    out.push_str("- ");
                }
                out.push_str(&wrap_emphasis(text, *bold, *italic));
            }
            Block::Table { header, rows } => {
                let column_count = header
                    .as_ref()
                    .map(|h| h.len())
                    .or_else(|| rows.first().map(|r| r.len()))
                    .unwrap_or(0);
                if column_count == 0 {
                    continue;
                }
                if let Some(h) = header {
                    out.push_str("| ");
                    for (i, cell) in h.iter().enumerate() {
                        if i > 0 {
                            out.push_str(" | ");
                        }
                        out.push_str(&escape_table_cell(cell));
                    }
                    out.push_str(" |\n");
                } else {
                    // CommonMark/GFM requires a header row before the
                    // separator; synthesize a blank header so renderers that
                    // refuse header-less tables still display the body.
                    out.push_str("|");
                    for _ in 0..column_count {
                        out.push_str("   |");
                    }
                    out.push('\n');
                }
                out.push('|');
                for _ in 0..column_count {
                    out.push_str("---|");
                }
                for row in rows {
                    out.push_str("\n| ");
                    for (i, cell) in row.iter().enumerate() {
                        if i > 0 {
                            out.push_str(" | ");
                        }
                        out.push_str(&escape_table_cell(cell));
                    }
                    out.push_str(" |");
                }
            }
            Block::GridFallback { lines } => {
                out.push_str("```text\n");
                for line in lines {
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str("```");
            }
            Block::CodeBlock { lines } => {
                // Pick a fence that doesn't appear inside the body. Standard
                // triple-backtick handles ~all real-world code; bump to ~~~ if
                // the body itself contains ``` (rare).
                let fence = if lines.iter().any(|l| l.contains("```")) {
                    "~~~"
                } else {
                    "```"
                };
                out.push_str(fence);
                out.push('\n');
                for line in lines {
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str(fence);
            }
            Block::HorizontalRule => {
                out.push_str("---");
            }
            Block::Blank => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Rect, TextItem};

    fn line(text: &str, x: f32, y: f32, h: f32, size: f32) -> ProjectedLine {
        ProjectedLine {
            text: text.into(),
            bbox: Rect {
                x,
                y,
                width: text.chars().count() as f32 * (size * 0.5),
                height: h,
            },
            anchor: Anchor::Left,
            indent_x: x,
            dominant_font_size: size,
            dominant_font_name: Some("Arial".into()),
            all_bold: false,
            all_italic: false,
            all_mono: false,
            all_strike: false,
            spans: vec![TextItem::default()],
            region_path: Vec::new(),
            mcid: None,
        }
    }

    fn page(lines: Vec<ProjectedLine>) -> ParsedPage {
        ParsedPage {
            page_number: 1,
            page_width: 612.0,
            page_height: 792.0,
            text: String::new(),
            text_items: vec![],
            projected_lines: lines,
            regions: crate::types::Region::default(),
            graphics: vec![],
            figures: vec![],
        }
    }

    #[test]
    fn body_size_picks_most_common() {
        let pages = vec![page(vec![
            line("Title", 50.0, 50.0, 18.0, 18.0),
            line("body line one", 50.0, 80.0, 10.0, 10.0),
            line("body line two", 50.0, 92.0, 10.0, 10.0),
            line("body line three", 50.0, 104.0, 10.0, 10.0),
        ])];
        let body = compute_body_size(&pages);
        assert!((body - 10.0).abs() < 0.01, "body size = {body}");
    }

    #[test]
    fn heading_map_descending_levels() {
        // Heading text needs to clear `MIN_HEADING_TOTAL_CHARS` (25) so the
        // size qualifies as a real heading rather than chart-legend noise.
        let pages = vec![page(vec![
            line("The largest heading on the page", 50.0, 50.0, 24.0, 24.0),
            line("A smaller heading right below it", 50.0, 80.0, 18.0, 18.0),
            // Several lines of body so it beats the heading text in the
            // char-weighted body-size mode.
            line(
                "body text line one with plenty of content",
                50.0,
                110.0,
                10.0,
                10.0,
            ),
            line(
                "body text line two with plenty of content",
                50.0,
                122.0,
                10.0,
                10.0,
            ),
            line(
                "body text line three with even more content",
                50.0,
                134.0,
                10.0,
                10.0,
            ),
            line(
                "body text line four with even more content",
                50.0,
                146.0,
                10.0,
                10.0,
            ),
        ])];
        let body = compute_body_size(&pages);
        let map = build_heading_map(&pages, body);
        assert_eq!(map.len(), 2);
        assert_eq!(map[0].1, 1);
        assert_eq!(map[1].1, 2);
        assert!(map[0].0 > map[1].0);
    }

    #[test]
    fn classify_emits_heading_and_paragraph() {
        let p = page(vec![
            line("Title of the document goes here", 50.0, 50.0, 18.0, 18.0),
            line("First sentence of the para-", 50.0, 80.0, 10.0, 10.0),
            line("graph continues here.", 50.0, 92.0, 10.0, 10.0),
            line("Another paragraph.", 50.0, 130.0, 10.0, 10.0),
        ]);
        let pages = vec![p];
        let body = compute_body_size(&pages);
        let map = build_heading_map(&pages, body);
        let blocks = classify_page(&pages[0], &map);
        assert_eq!(blocks.len(), 3);
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 1);
                assert_eq!(text, "Title of the document goes here");
            }
            other => panic!("expected heading, got {other:?}"),
        }
        match &blocks[1] {
            Block::Paragraph { text: t, .. } => {
                assert!(t.contains("paragraph continues"), "got: {t}");
                assert!(!t.contains("para-"), "de-hyphenation failed: {t}");
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
        match &blocks[2] {
            Block::Paragraph { text: t, .. } => assert_eq!(t, "Another paragraph."),
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn paragraph_break_on_big_gap() {
        let p = page(vec![
            line("Line A.", 50.0, 80.0, 10.0, 10.0),
            line("Line B.", 50.0, 200.0, 10.0, 10.0),
        ]);
        let pages = vec![p];
        let body = compute_body_size(&pages);
        let map = build_heading_map(&pages, body);
        let blocks = classify_page(&pages[0], &map);
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn render_blocks_formats_markdown() {
        let blocks = vec![
            Block::Heading {
                level: 1,
                text: "Title".into(),
            },
            Block::Paragraph {
                text: "A paragraph.".into(),
                bold: false,
                italic: false,
            },
            Block::Heading {
                level: 2,
                text: "Sub".into(),
            },
        ];
        let s = render_blocks(&blocks);
        assert_eq!(s, "# Title\n\nA paragraph.\n\n## Sub");
    }

    #[test]
    fn parse_list_marker_bullets() {
        let (ordered, marker, rest) = parse_list_marker("• item one").unwrap();
        assert!(!ordered);
        assert_eq!(marker, "•");
        assert_eq!(rest, "item one");
    }

    #[test]
    fn parse_list_marker_decimal() {
        let (ordered, marker, rest) = parse_list_marker("1. first").unwrap();
        assert!(ordered);
        assert_eq!(marker, "1.");
        assert_eq!(rest, "first");

        let (ordered, marker, rest) = parse_list_marker("12) twelfth").unwrap();
        assert!(ordered);
        assert_eq!(marker, "12)");
        assert_eq!(rest, "twelfth");
    }

    #[test]
    fn parse_list_marker_rejects_prose() {
        assert!(parse_list_marker("This sentence.").is_none());
        // Bare digit with no terminator → not a list
        assert!(parse_list_marker("2023 was a year").is_none());
        // Footnote caller / page number style — no whitespace after
        assert!(parse_list_marker("1.5x growth").is_none());
    }

    #[test]
    fn classify_emits_list_items() {
        let p = page(vec![
            line("Intro line.", 50.0, 50.0, 10.0, 10.0),
            line("• first bullet", 60.0, 80.0, 10.0, 10.0),
            line("• second bullet", 60.0, 92.0, 10.0, 10.0),
            line("◦ nested item", 72.0, 104.0, 10.0, 10.0),
            line("• back to top", 60.0, 116.0, 10.0, 10.0),
        ]);
        let pages = vec![p];
        let body = compute_body_size(&pages);
        let map = build_heading_map(&pages, body);
        let blocks = classify_page(&pages[0], &map);
        let list_items: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b, Block::ListItem { .. }))
            .collect();
        assert_eq!(list_items.len(), 4);
        if let Block::ListItem { level, text, .. } = list_items[0] {
            assert_eq!(*level, 0);
            assert_eq!(text, "first bullet");
        } else {
            panic!();
        }
        // The "- nested item" line is indented +12pt from the base bullet.
        if let Block::ListItem { level, .. } = list_items[2] {
            assert_eq!(*level, 1);
        } else {
            panic!();
        }
    }

    #[test]
    fn render_lists_are_tight() {
        let blocks = vec![
            Block::Paragraph {
                text: "Intro.".into(),
                bold: false,
                italic: false,
            },
            Block::ListItem {
                ordered: false,
                marker: "•".into(),
                level: 0,
                text: "a".into(),
                bold: false,
                italic: false,
            },
            Block::ListItem {
                ordered: false,
                marker: "•".into(),
                level: 0,
                text: "b".into(),
                bold: false,
                italic: false,
            },
            Block::Paragraph {
                text: "Outro.".into(),
                bold: false,
                italic: false,
            },
        ];
        let s = render_blocks(&blocks);
        assert_eq!(s, "Intro.\n\n- a\n- b\n\nOutro.");

        // Ordered: original marker preserved
        let s = render_blocks(&[
            Block::ListItem {
                ordered: true,
                marker: "138.".into(),
                level: 0,
                text: "footnote".into(),
                bold: false,
                italic: false,
            },
            Block::ListItem {
                ordered: true,
                marker: "139.".into(),
                level: 0,
                text: "next footnote".into(),
                bold: false,
                italic: false,
            },
        ]);
        assert_eq!(s, "138. footnote\n139. next footnote");
    }

    fn mono_line(text: &str, y: f32) -> ProjectedLine {
        let mut l = line(text, 50.0, y, 10.0, 10.0);
        l.all_mono = true;
        l
    }

    #[test]
    fn classify_emits_code_block() {
        let p = page(vec![
            line("Intro line.", 50.0, 50.0, 10.0, 10.0),
            mono_line("    let x = 1;", 80.0),
            mono_line("    let y = x + 2;", 92.0),
            line("After the code.", 50.0, 120.0, 10.0, 10.0),
        ]);
        let pages = vec![p];
        let body = compute_body_size(&pages);
        let map = build_heading_map(&pages, body);
        let blocks = classify_page(&pages[0], &map);
        // Expect: Paragraph("Intro line."), CodeBlock(2 lines), Paragraph("After...")
        assert_eq!(blocks.len(), 3);
        match &blocks[1] {
            Block::CodeBlock { lines } => {
                assert_eq!(lines.len(), 2);
                assert!(lines[0].contains("let x = 1;"));
                assert!(lines[1].contains("let y = x + 2;"));
            }
            other => panic!("expected code block, got {other:?}"),
        }
        let s = render_blocks(&blocks);
        assert!(s.contains("```\n    let x = 1;"));
        assert!(s.ends_with("After the code."));
    }

    #[test]
    fn classify_marks_paragraph_bold_when_all_lines_bold() {
        let mut a = line("Bold line one.", 50.0, 50.0, 10.0, 10.0);
        let mut b = line("bold continuation.", 50.0, 62.0, 10.0, 10.0);
        // Mark the underlying spans as bold so per-span style detection sees
        // it — the new inline pipeline reads from `spans`, not the per-line
        // `all_bold` shortcut flag.
        let bold_span = TextItem {
            text: "x".into(),
            font_name: Some("Arial-Bold".into()),
            ..Default::default()
        };
        a.spans = vec![bold_span.clone()];
        b.spans = vec![bold_span];
        a.all_bold = true;
        b.all_bold = true;
        let p = page(vec![a, b]);
        let pages = vec![p];
        let body = compute_body_size(&pages);
        let map = build_heading_map(&pages, body);
        let blocks = classify_page(&pages[0], &map);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Paragraph { bold, italic, .. } => {
                assert!(*bold);
                assert!(!*italic);
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
        let s = render_blocks(&blocks);
        assert!(s.starts_with("**") && s.ends_with("**"), "got: {s}");
    }

    #[test]
    fn render_emphasis_combinations() {
        assert_eq!(wrap_emphasis("hi", false, false), "hi");
        assert_eq!(wrap_emphasis("hi", true, false), "**hi**");
        assert_eq!(wrap_emphasis("hi", false, true), "*hi*");
        assert_eq!(wrap_emphasis("hi", true, true), "***hi***");
    }

    #[test]
    fn code_block_escapes_internal_fence() {
        let blocks = vec![Block::CodeBlock {
            lines: vec!["body containing ``` backticks".into()],
        }];
        let s = render_blocks(&blocks);
        assert!(s.starts_with("~~~\n"));
        assert!(s.ends_with("~~~"));
    }

    /// Build a line whose spans are placed at explicit x positions — used to
    /// drive the table detector, which relies on per-span x for cell splitting.
    fn line_with_spans(cells: &[(&str, f32)], y: f32, size: f32) -> ProjectedLine {
        let spans: Vec<TextItem> = cells
            .iter()
            .map(|(t, x)| TextItem {
                text: (*t).into(),
                x: *x,
                y,
                width: t.chars().count() as f32 * size * 0.5,
                height: size,
                font_size: Some(size),
                font_name: Some("Arial".into()),
                ..Default::default()
            })
            .collect();
        let min_x = spans.iter().map(|s| s.x).fold(f32::INFINITY, f32::min);
        let max_x = spans
            .iter()
            .map(|s| s.x + s.width)
            .fold(f32::NEG_INFINITY, f32::max);
        ProjectedLine {
            text: cells
                .iter()
                .map(|(t, _)| *t)
                .collect::<Vec<_>>()
                .join("   "),
            bbox: Rect {
                x: min_x,
                y,
                width: (max_x - min_x).max(0.0),
                height: size,
            },
            anchor: Anchor::Left,
            indent_x: min_x,
            dominant_font_size: size,
            dominant_font_name: Some("Arial".into()),
            all_bold: false,
            all_italic: false,
            all_mono: false,
            all_strike: false,
            spans,
            region_path: Vec::new(),
            mcid: None,
        }
    }

    #[test]
    fn split_cells_splits_on_wide_gaps() {
        let l = line_with_spans(&[("A", 50.0), ("B", 150.0), ("C", 250.0)], 100.0, 10.0);
        let cells = split_cells(&l);
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].text, "A");
        assert_eq!(cells[1].text, "B");
        assert_eq!(cells[2].text, "C");
    }

    #[test]
    fn recover_merged_cell_splits_off_by_one() {
        // Mimics the page-6 case: row 0 establishes 3 tracks at 50/150/250.
        // Row 1's projection merges "MEMORYBANK" + "5.00" into one span at
        // x=50 width=110, so split_cells yields 2 cells while the table
        // expects 3. Recovery must split on whitespace at the missing track.
        let row = vec![
            TableCell {
                start_x: 50.0,
                end_x: 160.0,
                text: "MEMORYBANK 5.00".into(),
                bold: false,
            },
            TableCell {
                start_x: 250.0,
                end_x: 280.0,
                text: "4.77".into(),
                bold: false,
            },
        ];
        let tracks = vec![50.0, 150.0, 250.0];
        let out = recover_merged_cell(row, &tracks).expect("recovery should succeed");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].text, "MEMORYBANK");
        assert_eq!(out[1].text, "5.00");
        assert_eq!(out[2].text, "4.77");
    }

    #[test]
    fn recover_merged_cell_splits_off_by_two() {
        // Three merged tokens in one cell: "MEMORYBANK 13.18 10.03" straddles
        // tracks at 50/150/250 and the row has only 2 cells, off by 2.
        let row = vec![
            TableCell {
                start_x: 50.0,
                end_x: 260.0,
                text: "MEMORYBANK 13.18 10.03".into(),
                bold: false,
            },
            TableCell {
                start_x: 350.0,
                end_x: 380.0,
                text: "7.61".into(),
                bold: false,
            },
        ];
        let tracks = vec![50.0, 150.0, 250.0, 350.0];
        let out = recover_merged_cell(row, &tracks).expect("recovery should succeed");
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].text, "MEMORYBANK");
        assert_eq!(out[1].text, "13.18");
        assert_eq!(out[2].text, "10.03");
        assert_eq!(out[3].text, "7.61");
    }

    #[test]
    fn recover_merged_cell_bails_without_enough_whitespace() {
        // A cell that straddles two tracks but has no internal whitespace
        // (e.g. a hyphenated token) can't be safely split — return None.
        let row = vec![TableCell {
            start_x: 50.0,
            end_x: 200.0,
            text: "ABC-DEF-GHI".into(),
            bold: false,
        }];
        let tracks = vec![50.0, 150.0];
        assert!(recover_merged_cell(row, &tracks).is_none());
    }

    #[test]
    fn split_cells_keeps_close_spans_together() {
        // Two spans 2pt apart at 10pt font (gap < font_size) → same cell.
        let l = line_with_spans(&[("Hello", 50.0), ("world", 80.0)], 100.0, 10.0);
        let cells = split_cells(&l);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].text, "Hello world");
    }

    #[test]
    fn detects_simple_borderless_table() {
        let lines = vec![
            line_with_spans(
                &[("Name", 50.0), ("Age", 150.0), ("City", 250.0)],
                100.0,
                10.0,
            ),
            line_with_spans(
                &[("Alice", 50.0), ("30", 150.0), ("NYC", 250.0)],
                115.0,
                10.0,
            ),
            line_with_spans(&[("Bob", 50.0), ("25", 150.0), ("LA", 250.0)], 130.0, 10.0),
        ];
        let p = page(lines);
        let pages = vec![p];
        let body = compute_body_size(&pages);
        let map = build_heading_map(&pages, body);
        let blocks = classify_page(&pages[0], &map);
        assert_eq!(blocks.len(), 1, "got: {blocks:?}");
        match &blocks[0] {
            Block::Table { header, rows } => {
                // Header isn't bold so no header row promoted.
                assert!(header.is_none());
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0][0], "Name");
                assert_eq!(rows[1][2], "NYC");
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn rejects_table_when_row_count_too_low() {
        let lines = vec![line_with_spans(
            &[("A", 50.0), ("B", 150.0), ("C", 250.0)],
            100.0,
            10.0,
        )];
        let runs = detect_tables(&lines);
        assert!(runs.is_empty());
    }

    #[test]
    fn rejects_table_when_column_count_too_low() {
        let lines = vec![
            line_with_spans(&[("A", 50.0), ("B", 200.0)], 100.0, 10.0),
            line_with_spans(&[("C", 50.0), ("D", 200.0)], 115.0, 10.0),
        ];
        let runs = detect_tables(&lines);
        assert!(runs.is_empty());
    }

    #[test]
    fn renders_table_to_pipe_format() {
        let blocks = vec![Block::Table {
            header: Some(vec!["a".into(), "b".into()]),
            rows: vec![vec!["1".into(), "2".into()], vec!["3".into(), "4".into()]],
        }];
        let s = render_blocks(&blocks);
        assert_eq!(s, "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |");
    }

    #[test]
    fn render_table_without_header_synthesizes_blank_header() {
        let blocks = vec![Block::Table {
            header: None,
            rows: vec![vec!["1".into(), "2".into()]],
        }];
        let s = render_blocks(&blocks);
        // GFM/CommonMark needs a header row before the separator; we emit a
        // blank one so renderers don't choke.
        assert!(s.contains("|---|---|"));
        assert!(s.ends_with("| 1 | 2 |"));
    }

    #[test]
    fn normalize_collapses_digits_and_case() {
        assert_eq!(normalize_for_repetition("Page 1 of 6"), "page # of #");
        assert_eq!(normalize_for_repetition("PAGE 12 OF 6"), "page # of #");
        assert_eq!(normalize_for_repetition("Confidential"), "confidential");
    }

    fn header_footer_page(n: usize, header: &str, footer: &str, body: &str) -> ParsedPage {
        // Page height 100 → header band ≤12pt, footer band ≥88pt.
        let mut lines = vec![
            line(header, 50.0, 5.0, 8.0, 8.0),
            line(body, 50.0, 50.0, 10.0, 10.0),
            line(footer, 50.0, 92.0, 6.0, 6.0),
        ];
        for l in &mut lines {
            l.region_path = Vec::new();
        }
        ParsedPage {
            page_number: n,
            page_width: 612.0,
            page_height: 100.0,
            text: String::new(),
            text_items: vec![],
            projected_lines: lines,
            regions: crate::types::Region::default(),
            graphics: vec![],
            figures: vec![],
        }
    }

    #[test]
    fn detects_repeating_header_and_footer() {
        let pages = vec![
            header_footer_page(1, "Acme Confidential", "Page 1 of 3", "Body one."),
            header_footer_page(2, "Acme Confidential", "Page 2 of 3", "Body two."),
            header_footer_page(3, "Acme Confidential", "Page 3 of 3", "Body three."),
        ];
        let set = compute_header_footer_set(&pages);
        assert!(set.contains("acme confidential"));
        assert!(set.contains("page # of #"));
    }

    #[test]
    fn skips_repetition_check_on_single_page() {
        let pages = vec![header_footer_page(1, "Solo", "footer", "body")];
        let set = compute_header_footer_set(&pages);
        assert!(set.is_empty());
    }

    #[test]
    fn body_text_not_classified_as_header() {
        // Same text in the body of every page should NOT be stripped — only
        // text within the top/bottom band qualifies.
        let mut pages = Vec::new();
        for n in 1..=3 {
            let mut p = header_footer_page(n, "unique header", "unique footer", "shared body text");
            // Move "shared body text" out of header/footer bands (already at y=50 in mid-page).
            // No-op — just illustrating intent.
            p.projected_lines[0].text = format!("unique header {n}");
            p.projected_lines[2].text = format!("unique footer {n}");
            pages.push(p);
        }
        let set = compute_header_footer_set(&pages);
        // "shared body text" sits mid-page — never matched.
        assert!(!set.contains("shared body text"));
    }

    #[test]
    fn full_format_strips_header_footer() {
        let pages = vec![
            header_footer_page(1, "Acme Confidential", "Page 1 of 2", "First page body."),
            header_footer_page(2, "Acme Confidential", "Page 2 of 2", "Second page body."),
        ];
        let body = compute_body_size(&pages);
        let map = build_heading_map(&pages, body);
        let set = compute_header_footer_set(&pages);
        let blocks = classify_page_with_filters(&pages[0], &map, &set);
        let s = render_blocks(&blocks);
        assert!(!s.contains("Acme Confidential"), "got: {s}");
        assert!(!s.contains("Page 1 of 2"), "got: {s}");
        assert!(s.contains("First page body."));
    }

    #[test]
    fn escapes_pipe_inside_cell() {
        assert_eq!(escape_table_cell("a|b"), "a\\|b");
    }

    #[test]
    fn dehyphenation_only_for_lowercase_followups() {
        let mut s = String::from("co-");
        append_paragraph_line(&mut s, "operate");
        assert_eq!(s, "cooperate");

        let mut s = String::from("Vitamin-");
        append_paragraph_line(&mut s, "A");
        assert_eq!(s, "Vitamin- A");
    }

    /// Build a line whose spans carry explicit per-span font metadata. Lets us
    /// exercise the mid-line emphasis pipeline without needing real PDF input.
    fn styled_line(spans: &[(&str, f32, Option<&str>)], y: f32, size: f32) -> ProjectedLine {
        let items: Vec<TextItem> = spans
            .iter()
            .map(|(t, x, font)| TextItem {
                text: (*t).into(),
                x: *x,
                y,
                width: t.chars().count() as f32 * size * 0.5,
                height: size,
                font_size: Some(size),
                font_name: font.map(String::from),
                ..Default::default()
            })
            .collect();
        let joined: String = spans
            .iter()
            .map(|(t, _, _)| *t)
            .collect::<Vec<_>>()
            .join(" ");
        let min_x = items.iter().map(|s| s.x).fold(f32::INFINITY, f32::min);
        let max_x = items
            .iter()
            .map(|s| s.x + s.width)
            .fold(f32::NEG_INFINITY, f32::max);
        ProjectedLine {
            text: joined,
            bbox: Rect {
                x: min_x,
                y,
                width: (max_x - min_x).max(0.0),
                height: size,
            },
            anchor: Anchor::Left,
            indent_x: min_x,
            dominant_font_size: size,
            dominant_font_name: Some("Arial".into()),
            all_bold: false,
            all_italic: false,
            all_mono: false,
            all_strike: false,
            spans: items,
            region_path: Vec::new(),
            mcid: None,
        }
    }

    #[test]
    fn render_line_inline_mid_line_bold() {
        // Plain span, then bold span: should produce a mid-line `**bold**` run.
        let l = styled_line(
            &[
                ("regular text with", 50.0, Some("Arial")),
                ("bold word", 200.0, Some("Arial-Bold")),
            ],
            100.0,
            10.0,
        );
        let out = render_line_inline(&l);
        assert!(out.contains("regular text with"), "got: {out}");
        assert!(out.contains("**bold word**"), "got: {out}");
        assert!(
            !out.starts_with("**"),
            "mid-line shouldn't open with bold: {out}"
        );
    }

    #[test]
    fn render_line_inline_uniform_bold_uses_shortcut() {
        // All spans bold → single outer wrap, no per-span noise.
        let l = styled_line(
            &[
                ("first", 50.0, Some("Arial-Bold")),
                ("second", 100.0, Some("Arial-Bold")),
            ],
            100.0,
            10.0,
        );
        let out = render_line_inline(&l);
        assert!(out.starts_with("**") && out.ends_with("**"), "got: {out}");
        // Only one bold run, not two — the shortcut should kick in.
        assert_eq!(out.matches("**").count(), 2, "got: {out}");
    }

    #[test]
    fn render_line_inline_escapes_emphasis_chars() {
        let l = styled_line(&[("5*4=20", 50.0, Some("Arial"))], 100.0, 10.0);
        let out = render_line_inline(&l);
        assert_eq!(out, "5\\*4=20");
    }

    #[test]
    fn render_line_inline_italic_then_bold() {
        let l = styled_line(
            &[
                ("italic", 50.0, Some("Arial-Italic")),
                ("plain", 100.0, Some("Arial")),
                ("bold", 150.0, Some("Arial-Bold")),
            ],
            100.0,
            10.0,
        );
        let out = render_line_inline(&l);
        assert!(out.contains("*italic*"), "got: {out}");
        assert!(out.contains("plain"), "got: {out}");
        assert!(out.contains("**bold**"), "got: {out}");
    }

    #[test]
    fn render_line_inline_mono_span() {
        let l = styled_line(
            &[
                ("call", 50.0, Some("Arial")),
                ("foo()", 100.0, Some("Courier")),
                ("on it", 150.0, Some("Arial")),
            ],
            100.0,
            10.0,
        );
        let out = render_line_inline(&l);
        assert!(out.contains("`foo()`"), "got: {out}");
        // Plain spans stay unwrapped.
        assert!(out.contains("call"));
        assert!(out.contains("on it"));
    }

    #[test]
    fn classify_paragraph_with_mid_line_bold() {
        // First line has a bold word mid-line → not uniformly styled; paragraph
        // should emit baked-in `**bold**` inside the text and `bold=false` at
        // the block level.
        let a = styled_line(
            &[
                ("a sentence with a", 50.0, Some("Arial")),
                ("bold", 180.0, Some("Arial-Bold")),
                ("word in it.", 230.0, Some("Arial")),
            ],
            50.0,
            10.0,
        );
        let p = page(vec![a]);
        let pages = vec![p];
        let body = compute_body_size(&pages);
        let map = build_heading_map(&pages, body);
        let blocks = classify_page(&pages[0], &map);
        assert_eq!(blocks.len(), 1, "got: {blocks:?}");
        match &blocks[0] {
            Block::Paragraph { text, bold, italic } => {
                assert!(!*bold, "mixed-style paragraph shouldn't set block bold");
                assert!(!*italic);
                assert!(text.contains("**bold**"), "got: {text}");
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn classify_list_item_strips_marker_under_emphasis() {
        // Whole bullet line is bold (marker + text). Rendered text should be
        // wrapped, with the marker dropped (the renderer prints it).
        let l = styled_line(
            &[
                ("•", 60.0, Some("Arial-Bold")),
                ("important item", 80.0, Some("Arial-Bold")),
            ],
            50.0,
            10.0,
        );
        let p = page(vec![l]);
        let pages = vec![p];
        let body = compute_body_size(&pages);
        let map = build_heading_map(&pages, body);
        let blocks = classify_page(&pages[0], &map);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::ListItem { text, .. } => {
                assert_eq!(text, "**important item**");
            }
            other => panic!("expected list item, got {other:?}"),
        }
    }

    fn stroke(x1: f32, y1: f32, x2: f32, y2: f32, width: f32) -> GraphicPrimitive {
        GraphicPrimitive::Stroke {
            x1,
            y1,
            x2,
            y2,
            width,
            color: None,
        }
    }

    fn page_with_graphics(
        lines: Vec<ProjectedLine>,
        graphics: Vec<GraphicPrimitive>,
    ) -> ParsedPage {
        let mut p = page(lines);
        p.graphics = graphics;
        p
    }

    #[test]
    fn hr_long_thin_horizontal_stroke_detected() {
        // 400pt wide stroke on a 612pt page → comfortably above 30% threshold.
        let p = page_with_graphics(vec![], vec![stroke(50.0, 200.0, 450.0, 200.5, 0.5)]);
        let ys = detect_horizontal_rules(&p);
        assert_eq!(ys, vec![200.25]);
    }

    #[test]
    fn hr_short_stroke_rejected() {
        // 50pt wide — table border or list bullet, not an HR.
        let p = page_with_graphics(vec![], vec![stroke(50.0, 200.0, 100.0, 200.0, 0.5)]);
        assert!(detect_horizontal_rules(&p).is_empty());
    }

    #[test]
    fn hr_vertical_stroke_rejected() {
        let p = page_with_graphics(vec![], vec![stroke(50.0, 50.0, 50.0, 500.0, 0.5)]);
        assert!(detect_horizontal_rules(&p).is_empty());
    }

    #[test]
    fn hr_thick_stroke_rejected() {
        // 4pt-thick stroke → a filled bar, not a rule.
        let p = page_with_graphics(vec![], vec![stroke(50.0, 200.0, 450.0, 200.0, 4.0)]);
        assert!(detect_horizontal_rules(&p).is_empty());
    }

    #[test]
    fn hr_underline_at_text_baseline_dropped() {
        // Text line at y=100 height=10 → bottom at y=110. Stroke at y=111 within
        // the line's horizontal extent → underline, not an HR.
        let text_line = line(
            "Some underlined heading text on the page",
            50.0,
            100.0,
            10.0,
            10.0,
        );
        let bottom = text_line.bbox.y + text_line.bbox.height;
        let p = page_with_graphics(
            vec![text_line.clone()],
            vec![stroke(
                50.0,
                bottom + 1.0,
                50.0 + text_line.bbox.width,
                bottom + 1.0,
                0.5,
            )],
        );
        assert!(detect_horizontal_rules(&p).is_empty());
    }

    /// Helper: build the four borders of a rectangle as four strokes.
    fn rect_borders(x: f32, y: f32, w: f32, h: f32) -> Vec<GraphicPrimitive> {
        vec![
            stroke(x, y, x + w, y, 0.5),         // top
            stroke(x, y + h, x + w, y + h, 0.5), // bottom
            stroke(x, y, x, y + h, 0.5),         // left
            stroke(x + w, y, x + w, y + h, 0.5), // right
        ]
    }

    #[test]
    fn ruled_table_2x2_detected() {
        // 2 rows × 2 cols grid: 3 H lines (y=100,140,180), 3 V lines (x=50,150,250)
        // Cell text dropped in the centroid of each cell.
        let mut graphics = Vec::new();
        for y in [100.0_f32, 140.0, 180.0] {
            graphics.push(stroke(50.0, y, 250.0, y, 0.5));
        }
        for x in [50.0_f32, 150.0, 250.0] {
            graphics.push(stroke(x, 100.0, x, 180.0, 0.5));
        }

        // Text lines: one per cell, centered.
        let lines = vec![
            line("a", 90.0, 115.0, 10.0, 10.0),  // row 0, col 0
            line("b", 190.0, 115.0, 10.0, 10.0), // row 0, col 1
            line("c", 90.0, 155.0, 10.0, 10.0),  // row 1, col 0
            line("d", 190.0, 155.0, 10.0, 10.0), // row 1, col 1
        ];

        let runs = detect_ruled_tables(&lines, &graphics, 612.0, 792.0);
        assert_eq!(runs.len(), 1, "expected 1 ruled table, got {runs:?}");
        match &runs[0].block {
            Block::Table { header, rows } => {
                assert!(header.is_none(), "no bold first row → no header");
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0], vec!["a", "b"]);
                assert_eq!(rows[1], vec!["c", "d"]);
            }
            other => panic!("expected Block::Table, got {other:?}"),
        }
    }

    #[test]
    fn ruled_table_rect_borders_detected() {
        // Same 2×2 table but drawn as 4 individual cell rects (each cell is a
        // stroked rectangle). Each rect contributes 4 strokes via
        // extract_h_v_segments.
        let mut graphics = Vec::new();
        graphics.extend(rect_borders(50.0, 100.0, 100.0, 40.0)); // r0 c0
        graphics.extend(rect_borders(150.0, 100.0, 100.0, 40.0)); // r0 c1
        graphics.extend(rect_borders(50.0, 140.0, 100.0, 40.0)); // r1 c0
        graphics.extend(rect_borders(150.0, 140.0, 100.0, 40.0)); // r1 c1

        let lines = vec![
            line("a", 90.0, 115.0, 10.0, 10.0),
            line("b", 190.0, 115.0, 10.0, 10.0),
            line("c", 90.0, 155.0, 10.0, 10.0),
            line("d", 190.0, 155.0, 10.0, 10.0),
        ];
        let runs = detect_ruled_tables(&lines, &graphics, 612.0, 792.0);
        assert_eq!(runs.len(), 1);
    }

    #[test]
    fn ruled_table_page_border_rejected() {
        // Single big rect covering ~the whole page → should NOT be treated as a
        // table even though it has H+V lines on all four sides.
        let graphics = rect_borders(10.0, 10.0, 590.0, 770.0);
        let lines = vec![line("body text", 50.0, 400.0, 10.0, 10.0)];
        let runs = detect_ruled_tables(&lines, &graphics, 612.0, 792.0);
        assert!(
            runs.is_empty(),
            "page-border rect should not become a table, got {runs:?}"
        );
    }

    #[test]
    fn ruled_table_mostly_empty_rejected() {
        // 3×3 grid with text in only one cell — empty fraction 8/9 ≈ 89% >> 30%.
        let mut graphics = Vec::new();
        for y in [100.0_f32, 130.0, 160.0, 190.0] {
            graphics.push(stroke(50.0, y, 350.0, y, 0.5));
        }
        for x in [50.0_f32, 150.0, 250.0, 350.0] {
            graphics.push(stroke(x, 100.0, x, 190.0, 0.5));
        }
        let lines = vec![line("only", 90.0, 115.0, 10.0, 10.0)];
        let runs = detect_ruled_tables(&lines, &graphics, 612.0, 792.0);
        assert!(runs.is_empty());
    }

    #[test]
    fn ruled_table_first_row_bold_becomes_header() {
        // 2×2 with first row text marked all_bold → header promotion.
        let mut graphics = Vec::new();
        for y in [100.0_f32, 140.0, 180.0] {
            graphics.push(stroke(50.0, y, 250.0, y, 0.5));
        }
        for x in [50.0_f32, 150.0, 250.0] {
            graphics.push(stroke(x, 100.0, x, 180.0, 0.5));
        }
        let mut a = line("Name", 90.0, 115.0, 10.0, 10.0);
        let mut b = line("Score", 190.0, 115.0, 10.0, 10.0);
        a.all_bold = true;
        b.all_bold = true;
        let lines = vec![
            a,
            b,
            line("alice", 90.0, 155.0, 10.0, 10.0),
            line("99", 190.0, 155.0, 10.0, 10.0),
        ];
        let runs = detect_ruled_tables(&lines, &graphics, 612.0, 792.0);
        assert_eq!(runs.len(), 1);
        match &runs[0].block {
            Block::Table { header, rows } => {
                assert_eq!(
                    header.as_deref(),
                    Some(&["Name".into(), "Score".into()][..])
                );
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0], vec!["alice", "99"]);
            }
            other => panic!("expected Block::Table, got {other:?}"),
        }
    }

    #[test]
    fn merge_prefers_ruled_when_overlapping() {
        let ruled = vec![TableRun {
            start: 5,
            end: 10,
            block: Block::Table {
                header: None,
                rows: vec![vec!["ruled".into()]],
            },
        }];
        let borderless = vec![TableRun {
            start: 6,
            end: 11,
            block: Block::GridFallback {
                lines: vec!["bl".into()],
            },
        }];
        let merged = merge_table_runs(ruled, borderless);
        assert_eq!(merged.len(), 1);
        assert!(matches!(&merged[0].block, Block::Table { .. }));
    }

    #[test]
    fn hr_emitted_between_lines_by_y_order() {
        let lines = vec![
            line("before the rule", 50.0, 100.0, 10.0, 10.0),
            line("after the rule", 50.0, 300.0, 10.0, 10.0),
        ];
        // Stroke between the two lines, far from either's baseline.
        let p = page_with_graphics(lines, vec![stroke(50.0, 200.0, 450.0, 200.0, 0.5)]);
        let blocks = classify_page(&p, &[]);
        let has_hr = blocks
            .iter()
            .position(|b| matches!(b, Block::HorizontalRule));
        assert!(has_hr.is_some(), "expected an HR block, got {blocks:?}");
        // HR must land between the two paragraphs, not before/after both.
        let pos = has_hr.unwrap();
        assert!(pos > 0 && pos < blocks.len() - 1);
    }
}
