//! Errors reported to the user with a stable machine-readable code.
//!
//! Codes: not_found, not_a_file, invalid_nifti, unsupported_format,
//! unsupported_datatype, corrupt_data, too_large, bad_request, internal.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewError {
    pub code: &'static str,
    pub message: String,
}

impl PreviewError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        PreviewError { code, message: message.into() }
    }
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
