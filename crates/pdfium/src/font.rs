use crate::ffi;

/// Wrapper around FPDF_FONT obtained from a text object.
/// This is a borrowed handle — it does not own the font and must not outlive
/// the page object it was obtained from.
pub struct Font {
    handle: pdfium_sys::FPDF_FONT,
}

/// Font type enum matching PDFium's FPDF_FONT_TYPE values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontType {
    Unknown,
    Type1,
    TrueType,
    Type0,
    Type3,
    CidType0,
    CidType2,
}

impl Font {
    /// Create a Font from a text page object handle.
    /// Returns None if the object has no font.
    ///
    /// # Safety
    /// `obj` must be a valid `FPDF_PAGEOBJECT` handle obtained from PDFium.
    pub unsafe fn from_text_object(obj: pdfium_sys::FPDF_PAGEOBJECT) -> Option<Self> {
        let handle = unsafe { ffi!(FPDFTextObj_GetFont(obj)) };
        if handle.is_null() {
            None
        } else {
            Some(Font { handle })
        }
    }

    pub fn handle(&self) -> pdfium_sys::FPDF_FONT {
        self.handle
    }

    /// Get the base font name (PostScript name, subset prefix stripped by PDFium).
    pub fn base_name(&self) -> Option<String> {
        let len = unsafe {
            ffi!(FPDFFont_GetBaseFontName(
                self.handle,
                std::ptr::null_mut(),
                0
            ))
        };
        if len == 0 {
            return None;
        }
        let mut buf: Vec<u8> = vec![0; len];
        let written = unsafe {
            ffi!(FPDFFont_GetBaseFontName(
                self.handle,
                buf.as_mut_ptr() as *mut std::ffi::c_char,
                len,
            ))
        };
        if written == 0 {
            return None;
        }
        // Strip trailing NUL
        let str_len = if written > 0 && buf[written - 1] == 0 {
            written - 1
        } else {
            written
        };
        Some(String::from_utf8_lossy(&buf[..str_len]).into_owned())
    }

    /// Get font type.
    pub fn font_type(&self) -> FontType {
        let t = unsafe { ffi!(FPDFFont_GetType(self.handle)) };
        match t {
            pdfium_sys::FPDF_FONT_TYPE_FPDF_FONTTYPE_TYPE1 => FontType::Type1,
            pdfium_sys::FPDF_FONT_TYPE_FPDF_FONTTYPE_TRUETYPE => FontType::TrueType,
            pdfium_sys::FPDF_FONT_TYPE_FPDF_FONTTYPE_TYPE0 => FontType::Type0,
            pdfium_sys::FPDF_FONT_TYPE_FPDF_FONTTYPE_TYPE3 => FontType::Type3,
            pdfium_sys::FPDF_FONT_TYPE_FPDF_FONTTYPE_CID_TYPE0 => FontType::CidType0,
            pdfium_sys::FPDF_FONT_TYPE_FPDF_FONTTYPE_CID_TYPE2 => FontType::CidType2,
            _ => FontType::Unknown,
        }
    }

    /// Whether the font is embedded in the PDF.
    pub fn is_embedded(&self) -> bool {
        unsafe { ffi!(FPDFFont_GetIsEmbedded(self.handle)) != 0 }
    }

    /// Get font ascent for a given em size.
    pub fn ascent(&self, font_size: f32) -> Option<f32> {
        let mut val: f32 = 0.0;
        let ok = unsafe { ffi!(FPDFFont_GetAscent(self.handle, font_size, &mut val)) };
        if ok != 0 { Some(val) } else { None }
    }

    /// Get font descent for a given em size (typically negative).
    pub fn descent(&self, font_size: f32) -> Option<f32> {
        let mut val: f32 = 0.0;
        let ok = unsafe { ffi!(FPDFFont_GetDescent(self.handle, font_size, &mut val)) };
        if ok != 0 { Some(val) } else { None }
    }

    /// Get glyph width using the raw character code.
    pub fn glyph_width_from_char_code(&self, char_code: u32, font_size: f32) -> Option<f32> {
        let mut width: f32 = 0.0;
        let ok = unsafe {
            ffi!(FPDFFont_GetGlyphWidthFromCharCode(
                self.handle,
                char_code,
                font_size,
                &mut width,
            ))
        };
        if ok != 0 { Some(width) } else { None }
    }

    /// Get glyph width using a Unicode codepoint.
    pub fn glyph_width(&self, unicode: u32, font_size: f32) -> Option<f32> {
        let mut width: f32 = 0.0;
        let ok = unsafe {
            ffi!(FPDFFont_GetGlyphWidth(
                self.handle,
                unicode,
                font_size,
                &mut width
            ))
        };
        if ok != 0 { Some(width) } else { None }
    }
}
