use napi::bindgen_prelude::*;
use napi_derive::napi;

mod types;

use types::{
    JsLiteParseConfig, JsPageComplexityStats, JsPageInput, JsParseResult, JsScreenshotResult,
    JsTextItem,
};

/// Main LiteParse parser class.
#[napi]
pub struct LiteParse {
    inner: liteparse::parser::LiteParse,
    config: liteparse::config::LiteParseConfig,
}

#[napi]
impl LiteParse {
    /// Create a new LiteParse instance with optional configuration.
    /// Any fields not provided will use defaults.
    #[napi(constructor)]
    pub fn new(config: Option<JsLiteParseConfig>) -> Self {
        let rust_config = config.map(|c| c.into_rust()).unwrap_or_default();
        let inner = liteparse::parser::LiteParse::new(rust_config.clone());
        Self {
            inner,
            config: rust_config,
        }
    }

    /// Parse a document. Accepts a file path (string) or raw PDF bytes (Buffer).
    #[napi]
    pub async fn parse(&self, input: Either<String, Buffer>) -> Result<JsParseResult> {
        use liteparse::types::PdfInput;

        let pdf_input = match input {
            Either::A(path) => PdfInput::Path(path),
            Either::B(buf) => PdfInput::Bytes(buf.to_vec()),
        };

        let result = self
            .inner
            .parse_input(pdf_input)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;

        Ok(JsParseResult::from_rust(&result, &self.config))
    }

    /// Parse from pre-extracted pages, skipping PDFium text extraction.
    ///
    /// The caller supplies pages already populated with text items in viewport
    /// space (top-left origin, 72 DPI). Runs only grid projection + the
    /// configured output formatter, so it never loads PDFium. Use when an
    /// external extractor owns text extraction (e.g. to keep its own
    /// font-recovery pipeline).
    #[napi]
    pub fn parse_pages(&self, pages: Vec<JsPageInput>) -> Result<JsParseResult> {
        let rust_pages: Vec<_> = pages.iter().map(JsPageInput::to_rust).collect();
        let result = self.inner.parse_from_pages(rust_pages, Vec::new());
        Ok(JsParseResult::from_rust(&result, &self.config))
    }

    /// Determine per-page complexity. Returns one entry per parsed page with
    /// signals (text coverage, images, garbled text, vector area) and a
    /// `needsOcr` verdict — a cheap pre-OCR check to decide whether a document
    /// needs advanced parsing. Accepts a file path (string) or raw PDF bytes.
    #[napi]
    pub async fn is_complex(
        &self,
        input: Either<String, Buffer>,
    ) -> Result<Vec<JsPageComplexityStats>> {
        use liteparse::types::PdfInput;

        let pdf_input = match input {
            Either::A(path) => PdfInput::Path(path),
            Either::B(buf) => PdfInput::Bytes(buf.to_vec()),
        };

        let stats = self
            .inner
            .is_complex(pdf_input)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;

        Ok(stats.iter().map(JsPageComplexityStats::from_rust).collect())
    }

    /// Take screenshots of document pages. Returns PNG image buffers.
    ///
    /// Non-PDF files are automatically converted to PDF before rendering when
    /// LibreOffice/ImageMagick are available.
    #[napi]
    pub async fn screenshot(
        &self,
        input: Either<String, Buffer>,
        page_numbers: Option<Vec<u32>>,
    ) -> Result<Vec<JsScreenshotResult>> {
        use liteparse::types::PdfInput;

        let pdf_input = match input {
            Either::A(path) => PdfInput::Path(path),
            Either::B(buf) => PdfInput::Bytes(buf.to_vec()),
        };

        let results = self
            .inner
            .screenshot_input(pdf_input, page_numbers)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;

        Ok(results
            .into_iter()
            .map(|r| JsScreenshotResult {
                page_num: r.page_num,
                width: r.width,
                height: r.height,
                image_buffer: r.image_bytes.into(),
            })
            .collect())
    }

    /// Get the current configuration.
    #[napi(getter)]
    pub fn config(&self) -> JsLiteParseConfig {
        JsLiteParseConfig::from_rust(&self.config)
    }
}

/// Search text items for phrase matches, returning merged items with combined bounding boxes.
#[napi]
pub fn search_items(
    items: Vec<JsTextItem>,
    phrase: String,
    case_sensitive: Option<bool>,
) -> Vec<JsTextItem> {
    let rust_items: Vec<_> = items.iter().map(|i| i.to_rust()).collect();
    let options = liteparse::search::SearchOptions {
        phrase,
        case_sensitive: case_sensitive.unwrap_or(false),
    };
    liteparse::search::search_items(&rust_items, &options)
        .iter()
        .map(JsTextItem::from_rust)
        .collect()
}
