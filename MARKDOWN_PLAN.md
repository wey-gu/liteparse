# Markdown Output for LiteParse — Design Doc

Status: draft
Owner: @loganmarkewich
Last updated: 2026-05-29

## Goal

Add `OutputFormat::Markdown` to LiteParse — a 100% heuristic, dependency-light markdown emitter that produces output suitable for LLM ingestion (RAG, summarization, chunking).

Two non-negotiables:

1. **No new runtime deps.** Pure Rust, runs in Node / Python / WASM with the same code path. Our forked PDFium already ships in all three.
2. **Honest fallback.** When heuristics aren't confident (especially tables), emit the grid-projection text inside a fenced block. The structure stays readable even when we can't classify it.

We will lean on:

- The existing grid projection (anchor system, column detection, rotation, OCR merge).
- Signals our PDFium fork can expose that upstream cannot (structure tree, marked content, paths/rects with stroke+fill, image bounds, outline).

## Prior art: pymupdf4llm

Annotated read of `src/helpers/pymupdf_rag.py` in `pymupdf/RAG`. Worth knowing what they do — most of it is sound — and where they fall short.

### What they do well

| Feature | Approach |
|---|---|
| Heading detection | `IdentifyHeaders`: char-weighted font-size histogram. Body = most frequent size. Larger sizes mapped to `#`..`######` in descending order. |
| TOC-based headings | `TocHeaders`: alt path that uses doc outline; matches spans by string prefix to a TOC entry to get an exact level. |
| Inline styling | Per-span PDF font flags (`flags & 2/8/16`, `char_flags & 1/8`) for italic / mono / bold / strikeout. Per-line shortcut if whole line shares the style. |
| Code blocks | Consecutive all-mono lines → fenced block, with synthetic indent `(x0 - clip.x0) / (fontsize * 0.5)`. |
| Lists | `startswith_bullet()` regex on first char; leading-space indent from x-offset. |
| Tables | `page.find_tables(strategy=...)` (PyMuPDF's vector-graphics-based finder). Drop tables with <2 rows or <2 cols. Emit `Table.to_markdown()`. Subtract table rects from text rects so content doesn't double up. |
| Figures | `get_image_info()` + clustered vector graphics. `is_significant()` filter drops H/V-line-only clusters (underlines, dividers) so they don't become bogus figures. |
| Background-color awareness | Detects page bg color; vector paths filled with bg are ignored. |
| Multi-column | `column_boxes()` — geometry-based column boxes, processed top→bottom, left→right. |
| Annotations | Uses `page.get_links()` to wrap matching spans with markdown link syntax. |
| Header/footer | Fixed user-supplied top/bottom margin in points. |

### Where we can beat them

| Weakness | What we'll do |
|---|---|
| Actively **deletes** `StructTreeRoot` to avoid a perf bug. | Use the structure tree as the highest-priority signal. Our fork can expose it cheaply. |
| Pure Python, slow on large docs. | Native Rust, single-pass over already-projected lines. |
| Borderless tables fall back to nothing useful. | Use grid projection's column detection as a borderless-table detector. When uncertain → fenced projection text. |
| Header/footer = static margin. | Cross-page repetition detection on top/bottom bands. |
| No de-hyphenation. | Conservative join: `word-\nbreak` → `wordbreak` only when next line starts lowercase. |
| Paragraph break = big y-gap only. | Combine y-gap + font change + indent change + anchor change. |
| Reading order from generic column boxes. | We already have anchor-driven projection; reuse it directly. |
| Bold from font flag only (misses synthetic bold). | Combine flag + `font_weight` + font-name substring (`Bold`, `Black`, `Heavy`). |
| Drops invisible text by default. | Keep it gated by config; preserve `confidence` and OCR-vs-native source. |

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│ PDFium (forked)                                             │
│   text items │ images │ paths │ struct tree │ outline       │
└──────────┬──────────────────────────────────────────────────┘
           │
       extract.rs (existing) ────────────────┐
           │                                 │
       projection.rs (existing) ─────────────┤
           │                                 │
           ▼                                 ▼
   ┌──────────────┐               ┌────────────────────┐
   │ ProjectedLine│               │ PageGeometry       │
   │  with style  │               │  paths, images,    │
   │  metadata    │               │  struct nodes,     │
   │  (NEW api)   │               │  outline targets   │
   └──────┬───────┘               └─────────┬──────────┘
          └───────────┬─────────────────────┘
                      ▼
            ┌──────────────────────┐
            │ markdown_layout.rs   │  (NEW)
            │  block classifier    │
            └─────────┬────────────┘
                      ▼
                Vec<Block>
                      ▼
            ┌──────────────────────┐
            │ output/markdown.rs   │  (NEW)
            │  block → markdown    │
            └─────────┬────────────┘
                      ▼
                  String
```

### New types

In `types.rs`:

```rust
pub enum Block {
    Heading { level: u8, text: String, inline: Vec<Span> },
    Paragraph(Vec<Span>),
    ListItem { level: u8, ordered: bool, marker: String, inline: Vec<Span> },
    Table(TableBlock),                    // confident table
    GridFallback(String),                 // projection text in ```/<pre>
    CodeBlock { lang: Option<String>, body: String },
    Figure { image_id: Option<String>, alt: Option<String>, ocr_text: Option<String> },
    HorizontalRule,
    Raw(String),                          // escape hatch
}

pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub strike: bool,
    pub link: Option<String>,
}

pub struct TableBlock {
    pub header: Option<Vec<String>>,
    pub rows: Vec<Vec<String>>,
}
```

### `projection.rs` — exposing line metadata

Today projection produces a joined string. Add an internal API:

```rust
pub struct ProjectedLine {
    pub text: String,
    pub bbox: Rect,
    pub anchor: Anchor,            // Left | Right | Center | Floating
    pub indent_x: f32,             // from column left edge
    pub dominant_font_size: f32,
    pub dominant_font_name: Option<String>,
    pub all_bold: bool,
    pub all_italic: bool,
    pub all_mono: bool,
    pub all_strike: bool,
    pub spans: Vec<TextItem>,      // raw items, preserved
    pub column_id: usize,          // for multi-column reading order
    pub mcid: Option<i32>,         // marked content id
}
```

The text emitter keeps working as-is by joining these lines; the markdown emitter consumes them structurally. **No behavior change to existing outputs.**

## Block classification (markdown_layout.rs)

Order matters — each pass either consumes lines or hands them to the next.

### Pass 0 — Whole-document signals (once per doc)

- Collect font-size → char-count histogram → `body_size`. For OCR, use height as a proxy.
- Build heading-level map: top N sizes > body_size → H1..HN (N capped at 6).
- Load outline / bookmarks → `Vec<OutlineTarget { page, y, level, title }>`. Not available for OCR docs.
- Walk structure tree → `Vec<StructNode { mcid, role, bbox }>` where role ∈ {H1..H6, P, L, LI, Table, TR, TH, TD, Figure, Caption}. Not available for OCR docs.
- Detect repeating top/bottom-band text across pages → header/footer set.

### Pass 1 — Page-level rect classification

Before touching lines, classify rects:

- **Image rects** — from PDFium image objects (already exposed).
- **Table rects** —
  - **Strong**: cluster ruled paths into a grid; verify ≥2 rows × ≥2 cols of cells. Use stroke geometry from the fork.
  - **Medium**: column-projection alignment — group of ≥3 vertically-aligned text "columns" repeated across ≥2 horizontal "rows" inside one column box. This works for docs that required OCR.
  - **Weak (fallback flag)**: lots of short text snippets in a clearly tabular layout but heuristic 1 & 2 disagree → mark as `GridFallback` candidate.
- **Code-block rects** — fill rect with a near-grey/near-white-non-bg color enclosing mono text (PyMuPDF-style is to skip this; we can do better).
- **HR rects** — long thin horizontal path, width > 50% column width, height ≤ 2pt.
- **Figure rects** — significant vector clusters + image rects, merged (`refine_boxes` analog).

Lines whose bbox falls inside a classified rect get tagged and skipped by Pass 2.

### Pass 2 — Line classification (in reading order)

For each `ProjectedLine` not consumed by a rect:

1. If line bbox is inside structure-tree `Figure`/`Table` node → emit accordingly.
2. If `mcid` maps to a structure node with role `H1..H6` → use that level. **Highest-priority heading signal.**
3. Else if bbox y matches an outline target (within tolerance) and text prefix-matches outline title → use outline level.
4. Else if `dominant_font_size > body_size` → use histogram heading map.
5. Else if `startswith_bullet(text)` or `^\d+[.)]\s` → list item; level from `indent_x` bucketing.
6. Else if `all_mono` → start/extend code block.
7. Else → paragraph line. Group consecutive paragraph lines with: same column, no major y-gap (> 1.5× line height), no font-size change, no anchor change.

De-hyphenation runs during paragraph join: if previous line ends `-` and next line starts lowercase ASCII, drop the hyphen and the newline.

### Pass 3 — Inline styling

Per span inside each emitted block:

- bold = font-flag-bold OR `font_weight ≥ 600` OR font-name contains `Bold|Black|Heavy|Semibold`.
- italic = font-flag-italic OR font-name contains `Italic|Oblique`.
- mono = font-flag-mono OR font-name in known mono list (`Courier`, `Mono`, `Consolas`, `Menlo`, `Source Code`, ...).
- link = bbox overlaps an annotation URI link rect.

Per-line shortcut: if all spans share a style, apply once around the whole line text (matches pymupdf4llm; avoids `**foo** **bar** **baz**` noise).

## Tables in detail

This is the hardest part and where we differentiate. Three modes, configurable via `MarkdownConfig::tables`:

- `Off` — never emit tables; tabular regions become `GridFallback`.
- `Auto` (default) — try strong → medium → fallback in order.
- `RuledOnly` — only emit when path-based grid detection succeeds; otherwise fallback.

### Strong: ruled grid

1. Collect horizontal segments (`y0 ≈ y1`, length > 1pt) and vertical segments inside the page clip.
2. Build a set of grid intersection x's and y's by clustering segment endpoints.
3. Walk cells; assign text items whose centroid falls in each cell.
4. Reject if rows < 2 or cols < 2, or if >30% of cells are empty.
5. First row → header iff its cells are bold or have a fill background.

### Medium: borderless via projection columns

The grid projection already tells us column boundaries (anchors). Inside a column box:

1. Look for ≥3 vertical "tracks" of text where x0 clusters within ±2pt.
2. Look for ≥2 horizontal "rows" of those tracks at distinct y bands.
3. Require row-spacing consistency (CV of gaps < 0.5).
4. If matched, emit as markdown table. Otherwise → `GridFallback`.

### Fallback: fenced projection

```text
\`\`\`
<projection.rs output for this rect, untouched>
\`\`\`
```

This preserves visual structure for the LLM and is **strictly better** than mangled markdown.

## Config

```rust
pub struct MarkdownConfig {
    pub headings: HeadingMode,        // Auto | StructTree | Outline | FontSize | Off
    pub tables: TableMode,            // Auto | RuledOnly | Off
    pub images: ImageMode,            // Embed { dir } | Placeholder | Off
    pub code_blocks: bool,            // default true
    pub strip_headers_footers: bool,  // default true
    pub dehyphenate: bool,            // default true
    pub keep_invisible_text: bool,    // default false
    pub max_heading_levels: u8,       // default 6
    pub body_size_floor: Option<f32>, // optional manual override
}
```

Sensible defaults so users can just pass `-f markdown` and get good output. Debatable if we should even expose any options at all in v1. We can always add them later if users want more control.

## Surface area

- `crates/liteparse/src/output/markdown.rs` — new.
- `crates/liteparse/src/markdown_layout.rs` — new.
- `crates/liteparse/src/types.rs` — add `Block`, `Span`, `TableBlock`.
- `crates/liteparse/src/config.rs` — add `OutputFormat::Markdown`, `MarkdownConfig`.
- `crates/liteparse/src/projection.rs` — expose `ProjectedLine` (additive).
- `crates/liteparse/src/main.rs` — `-f markdown`, plus subflags for the config above.
- `crates/liteparse-napi`, `liteparse-python`, `liteparse-wasm` — pass through the new format + config struct.
- `packages/node/src/lib.ts`, `packages/python/liteparse/parser.py` — wrapper API.

## PDFium fork additions needed (incremental)

Order roughly matches build order below.

1. **Image bbox enumeration** — likely already exposed; confirm.
2. **Path objects** with stroke + fill, in viewport coordinates. Needed for ruled-table detection, HR detection, code-block backgrounds.
3. **Structure tree walk** with role + mcid → bbox map. Highest leverage signal.
4. **Outline / bookmarks** with destination page+y per entry. Cheap.
5. **Page background color** — derivable from full-page fill paths once (2) lands.

Each of these is additive in the FFI; we can ship markdown v1 without (3)/(4) and add them later as quality bumps.

The PDFium fork is already on disk in `~/pdfium-binaries` -- we can directly modify here or the bindings in @crates/pdfium-sys and @crates/pdfium.

## Build order

1. **Wiring**: `OutputFormat::Markdown` end-to-end, returning current text output wrapped in `<pre>` fence. Proves CLI, napi, py, wasm plumbing.
2. **`ProjectedLine` API** in `projection.rs` (additive; text/json output unchanged).
3. **Font-size histogram + heading detection.** Paragraph grouping. De-hyphenation.
4. **Lists** (bullet + ordered + nesting from indent).
5. **Code blocks** (all-mono lines).
6. **Inline styling** (bold/italic/mono/strike/link) with per-line shortcut.
7. **Tables — medium (column-alignment) + fallback.** Ship without fork changes.
8. **Header/footer stripping** (cross-page repetition).
9. **Fork: paths exposed** → tables — strong (ruled grid), HR detection, figure-cluster refinement.
10. **Fork: structure tree + outline** → highest-priority heading + table + figure source. Replaces step 3 when available, falls back when not.
11. **Image extraction modes** (placeholder vs embed).

Steps 1–8 are a usable v1. Steps 9–11 are quality bumps that mostly improve specific document classes (tagged PDFs, ruled tables, image-heavy docs).

## Open questions

- **Image embed format in WASM.** No filesystem — base64 data URI? Return as separate blob array alongside the markdown string? Probably the latter; the markdown references `image_0.png` and the API returns a `images: { name, bytes }[]` sidecar.
- **Per-page vs whole-doc emission.** pymupdf4llm emits per page with `\n\n-----\n\n` separators. Cleaner for chunking but loses paragraphs that span page breaks. Recommend per-page by default with a `joinPages: true` option.
- **Should we expose `Block[]` as a structured output?** It's a natural intermediate. JSON consumers might prefer `Block[]` over a markdown string. Cheap to add (`OutputFormat::Blocks`).
- **Should `GridFallback` use `~~~text` or `<pre>`?** Code fence is friendlier to most LLMs; `<pre>` is friendlier to renderers. Default to fenced.
- **Confidence/OCR provenance in markdown.** Drop it? Encode as HTML comments? Most users won't want it; gate behind a config flag.

## Success criteria

- End-to-end on the eval set in `dataset_eval_utils/` produces markdown that scores ≥ pymupdf4llm on whatever similarity metric we settle on (the existing `*_report.html` runs suggest the harness is already there).
- Single-page parse time stays within 2× of current text output for typical documents.
- No new runtime deps; WASM bundle size growth < 100KB.
