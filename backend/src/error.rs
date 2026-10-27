//! Errors reported to the user with a stable machine-readable code.
//!
//! Codes: not_found, not_a_file, invalid_image, invalid_nifti, unsupported_format,
//! unsupported_datatype, corrupt_data, too_large, bad_request, internal.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewError {
    pub code: &'static str,
    pub message: String,
    /// What is known of a file recognised but not shown (a DICOM report, a
    /// surface overlay): sections for the preview's panel, see `about`.
    pub about: Option<Box<serde_json::Value>>,
}

impl PreviewError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        PreviewError { code, message: message.into(), about: None }
    }

    pub fn with_about(mut self, about: serde_json::Value) -> Self {
        self.about = Some(Box::new(about));
        self
    }
}

/// Sections of `about`: `[{"title", "rows": [[key, value]...]}` or
/// `{"title", "text"}]`, the shape of the preview's panel.
pub fn section(title: &str, rows: Vec<(String, String)>) -> Option<serde_json::Value> {
    (!rows.is_empty()).then(|| serde_json::json!({"title": title, "rows": rows.into_iter().map(|(k, v)| [k, v]).collect::<Vec<_>>()}))
}

/// File sizes as the preview writes them (1023 B, 9.4 KiB, 22 MiB).
pub fn size_text(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    let units = ["KiB", "MiB", "GiB", "TiB"];
    while v >= 1024.0 && unit < units.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v < 10.0 { format!("{v:.1} {}", units[unit]) } else { format!("{v:.0} {}", units[unit]) }
}

impl fmt::Display for PreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for PreviewError {}

pub type Result<T> = std::result::Result<T, PreviewError>;

macro_rules! bail {
    ($code:expr, $($arg:tt)*) => {
        return Err($crate::error::PreviewError::new($code, format!($($arg)*)))
    };
}
pub(crate) use bail;
