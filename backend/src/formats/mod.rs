//! Readers of the formats other than NIfTI (see `header.rs`): each turns its
//! header into an `ImageHeader`.

pub mod bzip2;
pub mod dicom;
pub mod metaimage;
pub mod mgh;
pub mod mif;
pub mod nrrd;
