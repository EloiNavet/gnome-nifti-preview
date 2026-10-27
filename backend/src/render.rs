//! Slice rendering: extraction + grayscale windowing, label colouring, or the
//! colours of an RGB image.

use crate::error::{bail, Result};
use crate::labels;
use crate::orientation::ViewPlan;
use crate::volume::Volume;
use crate::window;

pub const COLORMAPS: [&str; 4] = ["auto", "gray", "labels", "rgb"];

/// "auto" is "rgb" for RGB images, "labels" for label maps, "gray" otherwise;
/// "rgb" is "gray" for an image that is not RGB.
pub fn effective_colormap(volume: &mut Volume, colormap: &str) -> Result<&'static str> {
    Ok(match colormap {
        "auto" => {
            if volume.is_rgb() {
                "rgb"
            } else if volume.stats()?.labels.is_labels {
                "labels"
            } else {
                "gray"
            }
        }
        "gray" => "gray",
        "labels" => "labels",
        "rgb" => {
            if volume.is_rgb() {
                "rgb"
            } else {
                "gray"
            }
        }
        other => bail!("bad_request", "Unknown colormap {other:?}"),
    })
}

/// The colormaps worth offering for this image, its default first.
pub fn colormap_choices(volume: &mut Volume) -> Result<Vec<&'static str>> {
    Ok(if volume.is_rgb() {
        vec!["rgb", "gray"]
    } else if volume.stats()?.labels.is_labels {
        vec!["labels", "gray"]
    } else {
        vec!["gray"]
    })
}

/// Render one slice as RGB8 (rows top to bottom). `scratch` is reused between calls.
pub fn render_slice(
    volume: &mut Volume,
    plan: &ViewPlan,
    index: usize,
    colormap: &str,
    scratch: &mut Vec<f32>,
) -> Result<(Vec<u8>, &'static str)> {
    if index >= plan.n_slices {
        bail!("bad_request", "slice index {index} out of range 0..{}", plan.n_slices - 1);
    }
    volume.ensure_data()?;
    volume.stats()?;
    let cmap = effective_colormap(volume, colormap)?;
    let mut rgb = Vec::new();
    if cmap == "rgb" {
        volume.slice_rgb(plan, index, &mut rgb)?;
        return Ok((rgb, cmap));
    }
    if cmap == "labels" {
        let binary = volume.stats()?.labels.binary;
        let table = volume.label_table();
        labels::colorize(volume.slice_labels(plan, index), binary, table, &mut rgb)?;
    } else {
        volume.slice_values(plan, index, false, scratch)?;
        let w = volume.display_window()?;
        window::apply_with(scratch, &w, volume.inverted(), &mut rgb)?;
        window::colour(&mut rgb, volume.palette);
    }
    Ok((rgb, cmap))
}
