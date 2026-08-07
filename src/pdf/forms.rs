//! AcroForm widgets → machine-readable data.
//!
//! A form is only "hard" when tools treat it as pictures. fpdf_annot gives
//! us every widget's name, type, flags and current value, directly and
//! deterministically. This module turns them into triples an LLM can act
//! on (and a user can be told to fill).
//!
//! Requires an FPDF_FORMHANDLE (the form-fill environment), which
//! engine::load_document initializes when `LoadOpts::want_forms` is set.

#![allow(dead_code)]
use std::os::raw::c_ulong;

use super::sys::*;

/// What the widget IS, at the PDF level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetKind {
    PushButton,
    Checkbox,
    RadioButton,
    ComboBox,
    ListBox,
    TextField,
    Signature,
    Unknown(i32),
}

impl WidgetKind {
    fn from_raw(t: i32) -> Self {
        match t {
            FPDF_FORMFIELD_PUSHBUTTON => WidgetKind::PushButton,
            FPDF_FORMFIELD_CHECKBOX => WidgetKind::Checkbox,
            FPDF_FORMFIELD_RADIOBUTTON => WidgetKind::RadioButton,
            FPDF_FORMFIELD_COMBOBOX => WidgetKind::ComboBox,
            FPDF_FORMFIELD_LISTBOX => WidgetKind::ListBox,
            FPDF_FORMFIELD_TEXTFIELD => WidgetKind::TextField,
            FPDF_FORMFIELD_SIGNATURE => WidgetKind::Signature,
            other => WidgetKind::Unknown(other),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WidgetKind::PushButton => "button",
            WidgetKind::Checkbox => "checkbox",
            WidgetKind::RadioButton => "radio",
            WidgetKind::ComboBox => "dropdown",
            WidgetKind::ListBox => "list",
            WidgetKind::TextField => "text",
            WidgetKind::Signature => "signature",
            WidgetKind::Unknown(_) => "field",
        }
    }
}

/// One form widget, de-serialized.
#[derive(Clone, Debug)]
pub struct FormWidget {
    /// PDF-user-space rect: left, top, right, bottom (PDFium FS_RECTF).
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub kind: WidgetKind,
    pub name: String,
    pub value: String,
    /// The export/"on" value for checkboxes and radio buttons ("Off" here
    /// means unchecked).
    pub export_value: String,
    pub read_only: bool,
    pub required: bool,
}

impl FormWidget {
    /// Is this checkable widget currently on?
    pub fn checked(&self) -> bool {
        matches!(self.kind, WidgetKind::Checkbox | WidgetKind::RadioButton)
            && !self.value.is_empty()
            && self.value != "Off"
    }

    /// Display name: Acrobat chains field names as
    /// `topmostSubform[0].Page1[0]....`. The last segment is the
    /// human-meaningful one.
    fn display_name(&self) -> String {
        let last = self.name.rsplit('.').next().unwrap_or(&self.name);
        let trimmed = last
            .trim_end_matches(|ch: char| ch.is_ascii_digit())
            .trim_end_matches('[');
        if trimmed.is_empty() {
            self.name.clone()
        } else {
            trimmed.to_string()
        }
    }

    /// One-line render for the document stream. Placement (which section it
    /// belongs to) is done by the layout pipeline via the rect.
    pub fn markdown_line(&self) -> String {
        let mut s = String::new();
        let name = self.display_name();
        match self.kind {
            WidgetKind::Checkbox | WidgetKind::RadioButton => {
                let marker = if self.checked() { "[x]" } else { "[ ]" };
                let label = if self.export_value.is_empty() || self.export_value == "Off" {
                    name.as_str()
                } else {
                    self.export_value.as_str()
                };
                s.push_str(&format!("- {marker} {label}"));
            }
            WidgetKind::TextField | WidgetKind::ListBox | WidgetKind::ComboBox => {
                if self.value.is_empty() {
                    s.push_str(&format!("- **{name}**: _"));
                    s.push('\u{2002}');
                    s.push_str("___");
                } else {
                    s.push_str(&format!("- **{name}**: {}", self.value));
                }
            }
            WidgetKind::Signature => {
                s.push_str(&format!(
                    "- **[signature field{}]**",
                    if name.is_empty() {
                        String::new()
                    } else {
                        format!(" `{name}`")
                    }
                ));
            }
            WidgetKind::PushButton | WidgetKind::Unknown(_) => {
                s.push_str(&format!("- [{}: {}]", self.kind.label(), name));
            }
        }
        let mut hints = String::new();
        if self.required {
            hints.push_str(" (required)");
        }
        if self.read_only {
            hints.push_str(" (read-only)");
        }
        s.push_str(&hints);
        s
    }
}

/// UTF-16LE field strings, pdfium-buffer convention (returns bytes incl NUL).
fn field_string(
    handle: FpdfFormhandle,
    annot: FpdfAnnotation,
    f: unsafe extern "C" fn(
        FpdfFormhandle,
        FpdfAnnotation,
        *mut std::ffi::c_void,
        c_ulong,
    ) -> c_ulong,
) -> String {
    unsafe {
        let need = f(handle, annot, std::ptr::null_mut(), 0) as usize;
        if need == 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (need / 2).max(1)];
        let got = f(
            handle,
            annot,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            (buf.len() * 2) as c_ulong,
        ) as usize;
        let units = (got / 2).saturating_sub(1).min(buf.len());
        String::from_utf16_lossy(&buf[..units])
    }
}

/// Enumerate every widget on the given open page.
///
/// SAFETY: caller holds the global PDFium core lock; `handle` and `page`
/// are live.
pub(crate) unsafe fn collect_widgets(handle: FpdfFormhandle, page: FpdfPage) -> Vec<FormWidget> {
    let n = unsafe { FPDFPage_GetAnnotCount(page) };
    let mut out = Vec::new();
    for i in 0..n {
        let annot = unsafe { FPDFPage_GetAnnot(page, i) };
        if annot.is_null() {
            continue;
        }
        let is_widget = unsafe { FPDFAnnot_GetSubtype(annot) } == FPDF_ANNOT_ANNOTATION_WIDGET;
        if !is_widget {
            unsafe { FPDFPage_CloseAnnot(annot) };
            continue;
        }
        let mut rect = FsRect::default();
        let rect_ok = unsafe { FPDFAnnot_GetRect(annot, &mut rect as *mut _) } != 0;
        let kind = WidgetKind::from_raw(unsafe { FPDFAnnot_GetFormFieldType(handle, annot) });
        let flags = unsafe { FPDFAnnot_GetFormFieldFlags(handle, annot) };
        let name = field_string(handle, annot, FPDFAnnot_GetFormFieldName);
        let value = field_string(handle, annot, FPDFAnnot_GetFormFieldValue);
        let export_value = field_string(handle, annot, FPDFAnnot_GetFormFieldExportValue);
        if rect_ok {
            out.push(FormWidget {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
                kind,
                name: name.trim().to_string(),
                value: value.trim().to_string(),
                export_value: export_value.trim().to_string(),
                read_only: (flags as u32 & 0x1) != 0,
                required: (flags as u32 & 0x2) != 0,
            });
        }
        unsafe { FPDFPage_CloseAnnot(annot) };
    }
    out
}
