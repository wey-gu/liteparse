use std::marker::PhantomData;

use crate::bitmap::Bitmap;
use crate::document::Document;
use crate::error::PdfiumError;
use crate::ffi;
use crate::text_page::TextPage;
use crate::types::{Color, RectF};

/// Bounding box of an embedded image object on a page.
/// Coordinates are in PDF points with top-left origin (Y-down).
#[derive(Debug, Clone, Copy)]
pub struct ImageBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// One segment of a vector path. Coordinates are in viewport space
/// (top-left origin, 72 DPI) after the object's matrix has been applied.
#[derive(Debug, Clone, Copy)]
pub struct PathSegment {
    pub kind: SegmentKind,
    pub x: f32,
    pub y: f32,
    /// Whether this segment closes the current subpath back to its MoveTo.
    pub close: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    MoveTo,
    LineTo,
    BezierTo,
}

/// A vector path object extracted from a page. Used by the markdown emitter
/// for ruled-table, horizontal-rule, and figure-cluster detection.
#[derive(Debug, Clone)]
pub struct PathObject {
    /// Object bbox in viewport space (after matrix; from FPDFPageObj_GetBounds).
    pub bbox: RectF,
    pub stroke_color: Option<Color>,
    pub fill_color: Option<Color>,
    pub stroke_width: f32,
    /// True when the path is stroked per its draw mode.
    pub is_stroked: bool,
    /// True when the path is filled (draw-mode fill ≠ NONE).
    pub is_filled: bool,
    pub segments: Vec<PathSegment>,
}

pub struct Page<'doc> {
    pub(crate) handle: pdfium_sys::FPDF_PAGE,
    pub(crate) doc_handle: pdfium_sys::FPDF_DOCUMENT,
    pub(crate) _doc: PhantomData<&'doc Document>,
}

impl Page<'_> {
    pub fn width(&self) -> f32 {
        unsafe { ffi!(FPDF_GetPageWidthF(self.handle)) }
    }

    pub fn height(&self) -> f32 {
        unsafe { ffi!(FPDF_GetPageHeightF(self.handle)) }
    }

    pub fn rotation(&self) -> i32 {
        unsafe { ffi!(FPDFPage_GetRotation(self.handle)) }
    }

    /// Get the page bounding box (CropBox, falls back to MediaBox).
    /// Coordinates in PDF page space.
    pub fn view_box(&self) -> Option<RectF> {
        let mut rect = pdfium_sys::FS_RECTF {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        };
        let ok = unsafe { ffi!(FPDF_GetPageBoundingBox(self.handle, &mut rect)) };
        if ok != 0 {
            Some(RectF {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            })
        } else {
            None
        }
    }

    /// Convert a point from PDF page space to viewport space (top-left origin, 72 DPI).
    /// Mirrors the platform's Parse_pageToViewport using FPDF_PageToDevice at 1000x scale.
    pub fn page_to_viewport(&self, view_box: &RectF, page_x: f32, page_y: f32) -> (f32, f32) {
        let mut vw = view_box.right - view_box.left;
        let mut vh = view_box.top - view_box.bottom;

        let rotation = self.rotation();
        if rotation == 1 || rotation == 3 {
            // 90° or 270° — swap viewport dimensions
            std::mem::swap(&mut vw, &mut vh);
        }

        let device_w = (vw * 1000.0).round() as i32;
        let device_h = (vh * 1000.0).round() as i32;
        let mut dx: i32 = 0;
        let mut dy: i32 = 0;

        unsafe {
            ffi!(FPDF_PageToDevice(
                self.handle,
                0,
                0,
                device_w,
                device_h,
                0, // rotation 0 — PDFium applies page rotation internally
                page_x as f64,
                page_y as f64,
                &mut dx,
                &mut dy,
            ));
        }

        (dx as f32 / 1000.0, dy as f32 / 1000.0)
    }

    /// Convert bounds from PDF page space to viewport space (top-left origin).
    /// Returns RectF with left/top/right/bottom in viewport coordinates.
    pub fn bounds_to_viewport(&self, view_box: &RectF, page_bounds: &RectF) -> RectF {
        let (ll_x, ll_y) = self.page_to_viewport(view_box, page_bounds.left, page_bounds.bottom);
        let (ur_x, ur_y) = self.page_to_viewport(view_box, page_bounds.right, page_bounds.top);

        RectF {
            left: ll_x.min(ur_x),
            top: ll_y.min(ur_y),
            right: ll_x.max(ur_x),
            bottom: ll_y.max(ur_y),
        }
    }

    pub fn text(&self) -> Result<TextPage<'_>, PdfiumError> {
        let handle = unsafe { ffi!(FPDFText_LoadPage(self.handle)) };
        if handle.is_null() {
            return Err(PdfiumError::OperationFailed);
        }
        Ok(TextPage {
            handle,
            _page: PhantomData,
        })
    }

    /// Render the page to a BGRA bitmap at the given DPI.
    pub fn render(&self, dpi: f32) -> Result<Bitmap, PdfiumError> {
        let scale = dpi / 72.0;
        let width = (self.width() * scale).round() as i32;
        let height = (self.height() * scale).round() as i32;

        let bitmap = Bitmap::new(width, height)?;

        // Fill with white (ARGB: 0xFFFFFFFF)
        bitmap.fill_rect(0, 0, width, height, 0xFFFFFFFF);

        let flags = (pdfium_sys::FPDF_ANNOT | pdfium_sys::FPDF_PRINTING) as i32;

        unsafe {
            ffi!(FPDF_RenderPageBitmap(
                bitmap.handle(),
                self.handle,
                0,      // start_x
                0,      // start_y
                width,  // size_x
                height, // size_y
                0,      // rotation
                flags,
            ));
        }

        Ok(bitmap)
    }

    /// Extract bounding boxes of embedded image objects on this page.
    /// Returns coordinates in viewport space (Y-down, top-left origin) in PDF points.
    /// Filters out images smaller than `min_size_pt` and images covering more than
    /// `max_page_coverage` fraction of the page.
    pub fn image_bounds(&self, min_size_pt: f32, max_page_coverage: f32) -> Vec<ImageBounds> {
        let page_width = self.width();
        let page_height = self.height();
        let obj_count = unsafe { ffi!(FPDFPage_CountObjects(self.handle)) };
        let mut results = Vec::new();

        for i in 0..obj_count {
            let obj = unsafe { ffi!(FPDFPage_GetObject(self.handle, i)) };
            if obj.is_null() {
                continue;
            }

            let obj_type = unsafe { ffi!(FPDFPageObj_GetType(obj)) };
            if obj_type != pdfium_sys::FPDF_PAGEOBJ_IMAGE as i32 {
                continue;
            }

            let mut left: f32 = 0.0;
            let mut bottom: f32 = 0.0;
            let mut right: f32 = 0.0;
            let mut top: f32 = 0.0;
            let ok = unsafe {
                ffi!(FPDFPageObj_GetBounds(
                    obj,
                    &mut left,
                    &mut bottom,
                    &mut right,
                    &mut top
                ))
            };
            if ok == 0 {
                continue;
            }

            let w = right - left;
            let h = top - bottom;

            if w < min_size_pt || h < min_size_pt {
                continue;
            }
            if w > page_width * max_page_coverage && h > page_height * max_page_coverage {
                continue;
            }

            // Convert from PDF coords (bottom-left origin) to viewport (top-left origin)
            results.push(ImageBounds {
                x: left,
                y: page_height - top,
                width: w,
                height: h,
            });
        }

        results
    }

    /// Get the rendered bitmap of a specific embedded image object by index.
    /// The index corresponds to the order from iterating page objects (image objects only).
    pub fn render_image_object(&self, image_obj_index: usize) -> Result<Bitmap, PdfiumError> {
        let obj_count = unsafe { ffi!(FPDFPage_CountObjects(self.handle)) };
        let mut image_idx = 0usize;

        for i in 0..obj_count {
            let obj = unsafe { ffi!(FPDFPage_GetObject(self.handle, i)) };
            if obj.is_null() {
                continue;
            }
            let obj_type = unsafe { ffi!(FPDFPageObj_GetType(obj)) };
            if obj_type != pdfium_sys::FPDF_PAGEOBJ_IMAGE as i32 {
                continue;
            }

            if image_idx == image_obj_index {
                let bmp_handle = unsafe {
                    ffi!(FPDFImageObj_GetRenderedBitmap(
                        self.doc_handle,
                        self.handle,
                        obj
                    ))
                };
                if bmp_handle.is_null() {
                    return Err(PdfiumError::OperationFailed);
                }
                // Wrap in our Bitmap (which will call Destroy on drop)
                return Ok(unsafe { Bitmap::from_handle(bmp_handle) });
            }
            image_idx += 1;
        }

        Err(PdfiumError::OperationFailed)
    }

    /// Enumerate vector path objects on this page. Segment points are
    /// transformed into viewport space (top-left origin, 72 DPI) by composing
    /// the object's matrix with the page→viewport transform.
    pub fn path_objects(&self, view_box: &RectF) -> Vec<PathObject> {
        let vp = self.viewport_transform(view_box);
        let obj_count = unsafe { ffi!(FPDFPage_CountObjects(self.handle)) };
        let mut out = Vec::new();

        for i in 0..obj_count {
            let obj = unsafe { ffi!(FPDFPage_GetObject(self.handle, i)) };
            if obj.is_null() {
                continue;
            }
            let obj_type = unsafe { ffi!(FPDFPageObj_GetType(obj)) };
            if obj_type != pdfium_sys::FPDF_PAGEOBJ_PATH as i32 {
                continue;
            }

            // Object → page matrix. Defaults to identity when not available.
            let mut m = pdfium_sys::FS_MATRIX {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: 0.0,
                f: 0.0,
            };
            unsafe { ffi!(FPDFPageObj_GetMatrix(obj, &mut m)) };

            // Bounds are reported in page space already accounting for the
            // matrix — just lift to viewport.
            let mut left = 0.0f32;
            let mut bottom = 0.0f32;
            let mut right = 0.0f32;
            let mut top = 0.0f32;
            let ok = unsafe {
                ffi!(FPDFPageObj_GetBounds(
                    obj,
                    &mut left,
                    &mut bottom,
                    &mut right,
                    &mut top
                ))
            };
            if ok == 0 {
                continue;
            }
            let bbox = vp.transform_bounds(&RectF {
                left,
                top,
                right,
                bottom,
            });

            // Draw mode → is_filled / is_stroked.
            let mut fill_mode = 0i32;
            let mut stroke_bool = 0i32;
            let dm_ok =
                unsafe { ffi!(FPDFPath_GetDrawMode(obj, &mut fill_mode, &mut stroke_bool)) };
            let (is_filled, is_stroked) = if dm_ok != 0 {
                (
                    fill_mode != pdfium_sys::FPDF_FILLMODE_NONE as i32,
                    stroke_bool != 0,
                )
            } else {
                (false, false)
            };

            // Colors are reported as RGBA channels in 0..=255 cuint.
            let stroke_color = read_color(|r, g, b, a| unsafe {
                ffi!(FPDFPageObj_GetStrokeColor(obj, r, g, b, a))
            });
            let fill_color =
                read_color(|r, g, b, a| unsafe { ffi!(FPDFPageObj_GetFillColor(obj, r, g, b, a)) });

            let mut stroke_width = 0.0f32;
            unsafe { ffi!(FPDFPageObj_GetStrokeWidth(obj, &mut stroke_width)) };

            // Walk segments. Points are in the object's local coords; apply
            // matrix → page, then viewport transform.
            let n_segs = unsafe { ffi!(FPDFPath_CountSegments(obj)) };
            let mut segments = Vec::with_capacity(n_segs.max(0) as usize);
            for si in 0..n_segs {
                let seg = unsafe { ffi!(FPDFPath_GetPathSegment(obj, si)) };
                if seg.is_null() {
                    continue;
                }
                let mut sx = 0.0f32;
                let mut sy = 0.0f32;
                let pt_ok = unsafe { ffi!(FPDFPathSegment_GetPoint(seg, &mut sx, &mut sy)) };
                if pt_ok == 0 {
                    continue;
                }
                let ty = unsafe { ffi!(FPDFPathSegment_GetType(seg)) };
                let close = unsafe { ffi!(FPDFPathSegment_GetClose(seg)) } != 0;
                let kind = match ty as u32 {
                    pdfium_sys::FPDF_SEGMENT_MOVETO => SegmentKind::MoveTo,
                    pdfium_sys::FPDF_SEGMENT_LINETO => SegmentKind::LineTo,
                    pdfium_sys::FPDF_SEGMENT_BEZIERTO => SegmentKind::BezierTo,
                    _ => continue,
                };

                // Apply object matrix (FS_MATRIX is column-major a/b/c/d/e/f
                // matching the PDF text-matrix convention used elsewhere).
                let page_x = m.a * sx + m.c * sy + m.e;
                let page_y = m.b * sx + m.d * sy + m.f;
                let (x, y) = vp.transform_point(page_x, page_y);
                segments.push(PathSegment { kind, x, y, close });
            }

            out.push(PathObject {
                bbox,
                stroke_color,
                fill_color,
                stroke_width,
                is_stroked,
                is_filled,
                segments,
            });
        }

        out
    }
}

/// Helper: call a PDFium getter for RGBA color channels and pack into our `Color`.
/// Returns None when the FFI call reports failure.
fn read_color<F>(getter: F) -> Option<Color>
where
    F: FnOnce(*mut u32, *mut u32, *mut u32, *mut u32) -> i32,
{
    let mut r = 0u32;
    let mut g = 0u32;
    let mut b = 0u32;
    let mut a = 0u32;
    let ok = getter(&mut r, &mut g, &mut b, &mut a);
    if ok == 0 {
        return None;
    }
    Some(Color {
        r: r as u8,
        g: g as u8,
        b: b as u8,
        a: a as u8,
    })
}

/// Pre-computed affine transform from PDF page space to viewport space.
/// Avoids repeated FFI calls to `FPDF_PageToDevice` by probing 3 points
/// once and deriving the 6 affine coefficients.
#[derive(Debug, Clone, Copy)]
pub struct ViewportTransform {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl ViewportTransform {
    /// Transform a single point from page space to viewport space.
    #[inline]
    pub fn transform_point(&self, page_x: f32, page_y: f32) -> (f32, f32) {
        (
            self.a * page_x + self.b * page_y + self.e,
            self.c * page_x + self.d * page_y + self.f,
        )
    }

    /// Transform a bounding rect from page space to viewport space.
    #[inline]
    pub fn transform_bounds(&self, page_bounds: &RectF) -> RectF {
        let (ll_x, ll_y) = self.transform_point(page_bounds.left, page_bounds.bottom);
        let (ur_x, ur_y) = self.transform_point(page_bounds.right, page_bounds.top);
        RectF {
            left: ll_x.min(ur_x),
            top: ll_y.min(ur_y),
            right: ll_x.max(ur_x),
            bottom: ll_y.max(ur_y),
        }
    }
}

impl Page<'_> {
    /// Build a `ViewportTransform` by probing 3 points through PDFium.
    /// This makes 3 FFI calls total, after which all transforms are pure math.
    pub fn viewport_transform(&self, view_box: &RectF) -> ViewportTransform {
        let (e, f) = self.page_to_viewport(view_box, 0.0, 0.0);
        let (ax_e, cx_f) = self.page_to_viewport(view_box, 1.0, 0.0);
        let (by_e, dy_f) = self.page_to_viewport(view_box, 0.0, 1.0);

        ViewportTransform {
            a: ax_e - e,
            b: by_e - e,
            c: cx_f - f,
            d: dy_f - f,
            e,
            f,
        }
    }
}

impl Drop for Page<'_> {
    fn drop(&mut self) {
        unsafe { ffi!(FPDF_ClosePage(self.handle)) };
    }
}
