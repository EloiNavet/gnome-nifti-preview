//! nitty: medical images in the terminal, at a glance or interactively.
//!
//! The image library (`gnome_nifti_preview`) reads every format, orients the
//! views, windows the grey levels and colours label maps: it draws each
//! slice as RGB8. This crate only puts those pixels on a terminal (half
//! blocks, or the Kitty, Sixel and iTerm2 image protocols) and handles the
//! interaction. Nothing here talks to the terminal through terminfo or a
//! TUI library: the few sequences used are written by hand (see
//! docs/ARCHITECTURE.md).

pub mod app;
pub mod args;
pub mod blocks;
pub mod caps;
pub mod color;
pub mod draw;
pub mod glance;
pub mod graphics;
pub mod input;
pub mod layout;
pub mod picture;
pub mod screen;
pub mod text;
pub mod tty;
pub mod viewer;
