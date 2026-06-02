use crate::error::PdfiumError;
use crate::ffi;
use crate::page::Page;

pub struct Document {
    pub(crate) handle: pdfium_sys::FPDF_DOCUMENT,
}

impl Document {
    pub fn page_count(&self) -> i32 {
        unsafe { ffi!(FPDF_GetPageCount(self.handle)) }
    }

    pub fn page(&self, index: i32) -> Result<Page<'_>, PdfiumError> {
        let handle = unsafe { ffi!(FPDF_LoadPage(self.handle, index)) };
        if handle.is_null() {
            return Err(PdfiumError::PageNotFound);
        }
        Ok(Page {
            handle,
            doc_handle: self.handle,
            _doc: std::marker::PhantomData,
        })
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        unsafe { ffi!(FPDF_CloseDocument(self.handle)) };
    }
}
