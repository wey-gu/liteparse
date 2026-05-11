use super::{OcrEngine, OcrOptions, OcrResult};
use tesseract_rs::{TessPageIteratorLevel, TesseractAPI};

pub struct TesseractOcrEngine {
    tessdata_path: Option<String>,
}

impl TesseractOcrEngine {
    pub fn new(tessdata_path: Option<String>) -> Self {
        Self { tessdata_path }
    }

    fn normalize_language(lang: &str) -> &str {
        match lang.to_lowercase().trim() {
            "en" => "eng",
            "fr" => "fra",
            "de" => "deu",
            "es" => "spa",
            "it" => "ita",
            "pt" => "por",
            "ru" => "rus",
            "zh" | "zh-cn" => "chi_sim",
            "zh-tw" => "chi_tra",
            "ja" => "jpn",
            "ko" => "kor",
            "ar" => "ara",
            "hi" => "hin",
            "th" => "tha",
            "vi" => "vie",
            _ => lang,
        }
    }
}

impl OcrEngine for TesseractOcrEngine {
    fn name(&self) -> &str {
        "tesseract"
    }

    fn recognize(
        &self,
        image_data: &[u8],
        width: u32,
        height: u32,
        options: &OcrOptions,
    ) -> Result<Vec<OcrResult>, Box<dyn std::error::Error>> {
        let language = Self::normalize_language(&options.language);

        let api = TesseractAPI::new();

        // Determine tessdata path: explicit config > TESSDATA_PREFIX env > tesseract-rs default
        let tessdata_path = self
            .tessdata_path
            .clone()
            .or_else(|| std::env::var("TESSDATA_PREFIX").ok());

        match &tessdata_path {
            Some(path) => api.init(path, language)?,
            None => {
                // tesseract-rs with build-tesseract downloads eng.traineddata automatically
                // and caches it; use its default path
                let default_path = default_tessdata_dir();
                api.init(&default_path, language)?;
            }
        }

        // Set image from raw RGB bytes (3 bytes per pixel)
        let bytes_per_pixel = 3;
        let bytes_per_line = width as i32 * bytes_per_pixel;
        api.set_image(
            image_data,
            width as i32,
            height as i32,
            bytes_per_pixel,
            bytes_per_line,
        )?;

        api.recognize()?;

        let iter = api.get_iterator()?;

        let mut results = Vec::new();
        loop {
            if let Ok((text, left, top, right, bottom, confidence)) = iter.get_word_with_bounds() {
                // tesseract-rs returns confidence 0-100, normalize to 0-1
                let conf = confidence / 100.0;

                // Filter low confidence (below 30%, matching TS behavior)
                if conf > 0.3 && !text.trim().is_empty() {
                    results.push(OcrResult {
                        text,
                        bbox: [left as f32, top as f32, right as f32, bottom as f32],
                        confidence: conf,
                    });
                }
            }

            match iter.next(TessPageIteratorLevel::RIL_WORD) {
                Ok(true) => continue,
                _ => break,
            }
        }

        Ok(results)
    }
}

/// Default tessdata directory used by tesseract-rs build-tesseract feature.
fn default_tessdata_dir() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{}/Library/Application Support/tesseract-rs/tessdata", home);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var("HOME").ok() {
            return format!("{}/.tesseract-rs/tessdata", home);
        }
    }
    "tessdata".to_string()
}
