use std::ffi::CString;
use std::sync::Once;

use crate::document::Document;
use crate::error::PdfiumError;
use crate::ffi;

static INIT: Once = Once::new();

pub struct Library {
    _private: (),
}

impl Library {
    pub fn init() -> Library {
        #[cfg(not(target_arch = "wasm32"))]
        pdfium_sys::dynamic::load_default().expect("failed to load pdfium shared library");

        INIT.call_once(|| {
            unsafe { ffi!(FPDF_InitLibrary()) };
        });
        Library { _private: () }
    }

    pub fn load_document(
        &self,
        path: &str,
        password: Option<&str>,
    ) -> Result<Document, PdfiumError> {
        let c_path = CString::new(path).map_err(|_| PdfiumError::FileNotFound)?;
        let c_password = password
            .map(|p| CString::new(p).map_err(|_| PdfiumError::OperationFailed))
            .transpose()?;

        let handle = unsafe {
            ffi!(FPDF_LoadDocument(
                c_path.as_ptr(),
                c_password.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            ))
        };

        if handle.is_null() {
            return Err(PdfiumError::from_last_error());
        }

        Ok(Document { handle })
    }

    pub fn load_document_from_bytes(
        &self,
        data: &[u8],
        password: Option<&str>,
    ) -> Result<Document, PdfiumError> {
        let c_password = password
            .map(|p| CString::new(p).map_err(|_| PdfiumError::OperationFailed))
            .transpose()?;

        let handle = unsafe {
            ffi!(FPDF_LoadMemDocument(
                data.as_ptr() as *const std::ffi::c_void,
                data.len() as i32,
                c_password.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            ))
        };

        if handle.is_null() {
            return Err(PdfiumError::from_last_error());
        }

        // SAFETY: pdfium requires the data buffer to outlive the document.
        // The caller must ensure `data` lives long enough. For owned data,
        // consider passing a Vec and having the Document hold it.
        // For now, this is the caller's responsibility.
        Ok(Document { handle })
    }
}
