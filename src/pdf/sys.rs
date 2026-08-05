//! Raw FFI bindings to the PDFium public C API.
//!
//! Declared by hand (no bindgen) against the headers in
//! `vendor/pdfium/include/`. Only the functions DonSheet uses. All
//! functions are `unsafe`: handle lifetime rules are enforced by the
//! safe wrapper in `engine.rs`.
//!
//! PDFium is NOT thread-safe; every call must be made while holding the
//! global core lock (`engine::core()`).

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use std::os::raw::{c_char, c_double, c_float, c_int, c_uint, c_ulong, c_ushort, c_void};

// ---- opaque handle types -------------------------------------------------

#[repr(C)] pub struct FpdfDocumentT { _private: [u8; 0] }
#[repr(C)] pub struct FpdfPageT { _private: [u8; 0] }
#[repr(C)] pub struct FpdfTextPageT { _private: [u8; 0] }
#[repr(C)] pub struct FpdfBookmarkT { _private: [u8; 0] }
#[repr(C)] pub struct FpdfDestT { _private: [u8; 0] }
#[repr(C)] pub struct FpdfPageObjectT { _private: [u8; 0] }
#[repr(C)] pub struct FpdfActionT { _private: [u8; 0] }

pub type FpdfDocument = *mut FpdfDocumentT;
pub type FpdfPage = *mut FpdfPageT;
pub type FpdfTextpage = *mut FpdfTextPageT;
pub type FpdfBookmark = *mut FpdfBookmarkT;
pub type FpdfDest = *mut FpdfDestT;
pub type FpdfPageobject = *mut FpdfPageObjectT;
pub type FpdfAction = *mut FpdfActionT;

// ---- shared structs ------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FsMatrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FsRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

// ---- error codes (FPDF_GetLastError) -------------------------------------

pub const FPDF_ERR_SUCCESS: u32 = 0;
pub const FPDF_ERR_UNKNOWN: u32 = 1;
pub const FPDF_ERR_FILE: u32 = 2;
pub const FPDF_ERR_FORMAT: u32 = 3;
pub const FPDF_ERR_PASSWORD: u32 = 4;
pub const FPDF_ERR_SECURITY: u32 = 5;
pub const FPDF_ERR_PAGE: u32 = 6;

// ---- font descriptor flags (PDF 1.7 §5.7.1 Table 123) --------------------

pub const FONT_FIXED_PITCH: u32 = 0x1;
pub const FONT_SERIF: u32 = 0x2;
pub const FONT_SYMBOLIC: u32 = 0x4;
pub const FONT_SCRIPT: u32 = 0x8;
pub const FONT_NONSYMBOLIC: u32 = 0x20;
pub const FONT_ITALIC: u32 = 0x40;
pub const FONT_ALL_CAP: u32 = 0x1_0000;
pub const FONT_SMALL_CAP: u32 = 0x2_0000;
pub const FONT_FORCE_BOLD: u32 = 0x4_0000;

// ---- page object types ----------------------------------------------------

pub const FPDF_PAGEOBJ_TEXT: c_int = 1;
pub const FPDF_PAGEOBJ_PATH: c_int = 2;
pub const FPDF_PAGEOBJ_IMAGE: c_int = 3;
pub const FPDF_PAGEOBJ_SHADING: c_int = 4;
pub const FPDF_PAGEOBJ_FORM: c_int = 5;

// ---- library config --------------------------------------------------------

/// FPDF_LIBRARY_CONFIG v2 (zero-initialisable: no font paths, no V8).
#[repr(C)]
pub struct FpdfLibraryConfig {
    pub version: c_int,
    pub m_pUserFontPaths: *mut *const c_char,
    pub m_pIsolate: *mut c_void,
    pub m_pV8EmbedderSlot: *mut c_uint,
}

unsafe extern "C" {
    // ---- fpdfview.h ----
    pub fn FPDF_InitLibrary();
    pub fn FPDF_InitLibraryWithConfig(config: *const FpdfLibraryConfig);
    pub fn FPDF_DestroyLibrary();
    pub fn FPDF_GetLastError() -> c_uint;
    pub fn FPDF_LoadMemDocument64(
        buffer: *const c_void,
        size: usize,
        password: *const c_char,
    ) -> FpdfDocument;
    pub fn FPDF_CloseDocument(document: FpdfDocument);
    pub fn FPDF_GetPageCount(document: FpdfDocument) -> c_int;
    pub fn FPDF_LoadPage(document: FpdfDocument, page_index: c_int) -> FpdfPage;
    pub fn FPDF_ClosePage(page: FpdfPage);
    pub fn FPDF_GetPageWidthF(page: FpdfPage) -> c_float;
    pub fn FPDF_GetPageHeightF(page: FpdfPage) -> c_float;

    // ---- fpdf_text.h ----
    pub fn FPDFText_LoadPage(page: FpdfPage) -> FpdfTextpage;
    pub fn FPDFText_ClosePage(text_page: FpdfTextpage);
    pub fn FPDFText_CountChars(text_page: FpdfTextpage) -> c_int;
    pub fn FPDFText_GetUnicode(text_page: FpdfTextpage, index: c_int) -> c_uint;
    pub fn FPDFText_GetFontInfo(
        text_page: FpdfTextpage,
        index: c_int,
        buffer: *mut c_void,
        buflen: c_ulong,
        flags: *mut c_int,
    ) -> c_ulong;
    pub fn FPDFText_GetFontWeight(text_page: FpdfTextpage, index: c_int) -> c_int;
    pub fn FPDFText_GetFontSize(text_page: FpdfTextpage, index: c_int) -> c_double;
    pub fn FPDFText_GetCharBox(
        text_page: FpdfTextpage,
        index: c_int,
        left: *mut c_double,
        right: *mut c_double,
        bottom: *mut c_double,
        top: *mut c_double,
    );
    pub fn FPDFText_GetLooseCharBox(
        text_page: FpdfTextpage,
        index: c_int,
        rect: *mut FsRect,
    ) -> c_int;
    pub fn FPDFText_GetMatrix(
        text_page: FpdfTextpage,
        index: c_int,
        matrix: *mut FsMatrix,
    ) -> c_int;
    pub fn FPDFText_GetText(
        text_page: FpdfTextpage,
        start_index: c_int,
        count: c_int,
        result: *mut c_ushort,
    ) -> c_ulong;

    // ---- fpdf_doc.h ----
    pub fn FPDF_GetMetaText(
        document: FpdfDocument,
        tag: *const c_char,
        buffer: *mut c_void,
        buflen: c_ulong,
    ) -> c_ulong;
    pub fn FPDFBookmark_GetFirstChild(document: FpdfDocument, bookmark: FpdfBookmark)
        -> FpdfBookmark;
    pub fn FPDFBookmark_GetNextSibling(document: FpdfDocument, bookmark: FpdfBookmark)
        -> FpdfBookmark;
    pub fn FPDFBookmark_GetTitle(
        bookmark: FpdfBookmark,
        buffer: *mut c_void,
        buflen: c_ulong,
    ) -> c_ulong;
    pub fn FPDFBookmark_GetDest(document: FpdfDocument, bookmark: FpdfBookmark) -> FpdfDest;
    pub fn FPDFDest_GetDestPageIndex(document: FpdfDocument, dest: FpdfDest) -> c_int;

    // ---- fpdf_edit.h (read-only usage) ----
    pub fn FPDFPage_CountObjects(page: FpdfPage) -> c_int;
    pub fn FPDFPage_GetObject(page: FpdfPage, index: c_int) -> FpdfPageobject;
    pub fn FPDFPageObj_GetType(object: FpdfPageobject) -> c_int;
    pub fn FPDFImageObj_GetRenderedBitmap(
        document: FpdfDocument,
        page: FpdfPage,
        image_object: FpdfPageobject,
    ) -> *mut c_void;
}
