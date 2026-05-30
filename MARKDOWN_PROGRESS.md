# Markdown Output — Implementation Progress

Tracks build-order progress against [MARKDOWN_PLAN.md](MARKDOWN_PLAN.md). Update as steps complete.

## Status legend

- ✅ done
- 🚧 in progress
- ⬜ not started

## Build order

| # | Step | Status | Notes |
|---|---|---|---|
| 1 | Wire `OutputFormat::Markdown` end-to-end | ✅ | Stub returns per-page text wrapped in ` ```text ` fence with `<!-- page N -->` comments and `\n\n-----\n\n` separators. `md` alias accepted. |
| 2 | `ProjectedLine` API in `projection.rs` (additive) | ✅ | `Rect`, `ProjectedLine`, `Anchor::Floating` added. `ParsedPage.projected_lines` is `#[serde(skip)]` — JSON/text output byte-identical. `column_id` + `indent_x` are placeholders. |
| 3 | Font-size histogram, headings, paragraph grouping, de-hyphenation | ✅ | New `markdown_layout.rs` builds a global char-weighted size histogram, maps sizes > body to H1..H6, groups consecutive lines into paragraphs (same anchor/column/font/indent + gap ≤ 1.5× line height), and de-hyphenates `word-\nlowercase` joins. `output/markdown.rs` now renders blocks instead of stubbing a fence (fence retained as fallback for pages without `projected_lines`). |
| 4 | Lists (bullet + ordered + nesting from indent) | ✅ | `parse_list_marker` recognizes Unicode bullets (`•·◦▪▸▶●○■□`) and decimal markers (`1.` / `12)`). Nesting derived from `indent_x` bucketed by 12pt steps relative to the first item of the run. Wrapped continuation lines reuse the paragraph-continuation rule so footnote-style hanging indents merge correctly. Original marker preserved (`138.` stays `138.`) so footnote/section numbering survives. ASCII `-`/`*`/`+` deliberately excluded to avoid false matches in prose. |
| 5 | Code blocks (all-mono lines) | ✅ | `Block::CodeBlock { lines }` accumulates consecutive `all_mono` lines and emits a triple-backtick fence (falls back to `~~~` if body contains `` ``` ``). Detection runs before heading/list/paragraph passes so mono content always wins. |
| 6 | Inline styling (bold/italic/mono/strike/link) with per-line shortcut | 🚧 | `ProjectedLine.all_bold/italic/mono` populated via char-weighted majority from `font_flags` (PDF descriptor bits: FixedPitch=1, Italic=64, ForceBold=262144) + `font_weight ≥ 600` + font-name heuristics (Bold/Black/Heavy/Semibold, Italic/Oblique, Courier/Mono/Consolas/Menlo/…). **Per-line shortcut only:** `Block::Paragraph`/`ListItem` carry `bold`/`italic` flags that flip off if any constituent line drops the style — when both stay true the whole block renders as `**…**` / `*…*` / `***…***`. Headings deliberately skip emphasis (the `#` is the marker). **Per-span (mid-line) styling, strike, and links are deferred.** |
| 7 | Tables — medium (column-alignment) + fallback | ⬜ | Ship without fork changes. |
| 8 | Header/footer stripping (cross-page repetition) | ⬜ | |
| 9 | Fork: paths exposed → strong (ruled grid) tables, HR detection, figure clustering | ⬜ | Requires PDFium fork changes. |
| 10 | Fork: structure tree + outline → highest-priority headings | ⬜ | Requires PDFium fork changes. |
| 11 | Image extraction modes (placeholder vs embed) | ⬜ | |

Steps 1–8 = usable v1. Steps 9–11 = quality bumps for specific doc classes.

## Carried-forward items

Things deferred from earlier steps that need to be revisited:

- **`ProjectedLine.column_id`** — currently always `0`. Real column detection needs to mine projection.rs's existing block/anchor data. Required before tables (step 7) can do per-column reasoning.
- **`ProjectedLine.indent_x`** — currently equals `bbox.x`. Should become offset from the column's left edge once column_id is real. Needed for list nesting (step 4).
- **`ProjectedLine.all_strike`** — still placeholder `false`. PDFium doesn't expose strikethrough as a span attribute; would need either path-cluster detection (a horizontal line at mid-glyph) or marked-content role hints. Not blocking v1.
- **Mid-line (per-span) emphasis** — current emitter only applies emphasis when *every* line of a paragraph/list-item shares the style. Mixed-style lines render plain. Picking this up requires rebuilding paragraph text from `ProjectedLine.spans` instead of `line.text`, escaping markdown specials per span, and a per-line shortcut when all spans agree.
- **Inline links** — needs PDF annotation link rects exposed via the fork; deferred with the rest of the structure-tree-tier work (step 10).
- **`MarkdownConfig`** — not yet introduced. Holding off until at least one heuristic has a knob worth exposing. Default-only v1 is fine per the plan.

## Open decisions still pending (from plan)

- Per-page vs whole-doc emission (default per-page with `joinPages` opt-in?).
- `Block[]` as a structured output format (`OutputFormat::Blocks`)?
- `GridFallback` fence style: ``` ```text ``` vs `<pre>`. Currently using ` ```text `.
- Confidence/OCR provenance encoding in markdown.

## Notes / surprises

- The Node CLI formats output client-side from `ParseResult`, so the markdown stub currently lives in *both* `output/markdown.rs` (Rust) and `formatResult()` (TS). The Rust path is now the real emitter; the TS path is still the old fenced-text stub. Needs to call into a native `format_markdown` (napi) before this becomes user-visible from the Node CLI — still pending; bumping to step 7/8 follow-up.
- **Step 5/6 sanity check on `paper.pdf`:** headings still detect (`##` / `####` / `#####`), no false code blocks fired on prose (font-name heuristics correctly skip serif/sans body fonts), and no spurious bold paragraphs. No mono content on this paper to exercise the fence path — covered by unit tests instead.
- Smoke test on `paper.pdf` (a two-column arXiv paper): headings (`#`/`##`/`###`/`####`) detect correctly and de-hyphenation joins `prede-/fined → predefined`. Two columns interleave on the same lines — expected until real `column_id` lands (carry-forward from step 2).
- **Tuning that came out of single-column smoke tests (`hard_5.pdf`, `llama2_4pages.pdf`):**
  - PDFium can return `font_size: 1.0` for every glyph when the font size is baked into the text matrix instead of the Tf operator. `build_projected_lines` now falls back to char-weighted `bbox.height` when the histogram size is ≤ 1.5pt — necessary for `llama2_4pages.pdf` to even detect headings.
  - `ProjectedLine.text` carries column-alignment spaces from the grid projection. The paragraph emitter runs `collapse_whitespace` on every line before joining so prose doesn't look like `for    instance`.
  - Justified body text alternates dominant anchors (Left / Right / Floating) line-by-line. Treating any anchor change as a paragraph break shreds normal paragraphs. The break rule now only fires on a *centered* vs *non-centered* flip.
  - Two font-size tolerances: tight (`0.6pt`) for matching the heading map, loose (`2.5pt`) for paragraph continuation, since the height fallback varies a few points based on descenders.
- **Known limitation:** de-hyphenation joins `closed-/source → closedsource`. We can't tell line-break hyphens apart from genuine compound hyphens without a dictionary. pymupdf4llm has the same issue; not blocking.
- **Footnote follow-up:** academic/legal docs often have footnote numbers like `138.` mid-paragraph (`...settlement outcomes.138`) that are real superscripts. They render attached to the preceding word, but the dedicated footnote block at the page bottom is correctly captured as `138. <text>`. The inline marker is informational; not worth splitting in v1.
- `cargo build --workspace` fails on macOS because `liteparse-python` needs to link against Python (build via maturin instead). Use `cargo build -p liteparse -p liteparse-napi` + `cargo check -p liteparse-python -p liteparse-wasm` for local verification.
