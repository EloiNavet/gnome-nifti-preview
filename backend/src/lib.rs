//! GNOME NIfTI Preview backend: fast NIfTI-1/NIfTI-2 reading and slice rendering for
//! the GNOME Sushi previewer plugin.

pub mod error;
pub mod header;
pub mod labels;
pub mod mrs;
pub mod orientation;
pub mod png;
pub mod render;
pub mod sandbox;
pub mod server;
pub mod source;
pub mod volume;
pub mod voxel;
pub mod window;
