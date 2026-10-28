//! The picture of a view: the slice the library draws (RGB8, cached),
//! scaled by the library's resampler to the pixels the terminal has for it,
//! with the crosshair drawn at that size so that it never vanishes when the
//! slice is shrunk.

use std::sync::Arc;

use gnome_nifti_preview::error::Result;
use gnome_nifti_preview::orientation::ViewPlan;
use gnome_nifti_preview::render;
use gnome_nifti_preview::thumbnail;
use gnome_nifti_preview::view::SliceCache;
use gnome_nifti_preview::volume::Volume;

/// The crosshair colour of the Sushi viewer (#61adff).
pub const CROSSHAIR: [u8; 3] = [0x61, 0xad, 0xff];

/// An RGB8 image.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
}

/// Physical width / height of a view (voxel sizes taken as 1 when unusable).
pub fn physical_aspect(plan: &ViewPlan) -> f64 {
    let size = |x: f64| if x.is_finite() && x > 0.0 { x } else { 1.0 };
    let a = plan.width as f64 * size(plan.pixel_w) / (plan.height as f64 * size(plan.pixel_h));
    if a.is_finite() && a > 0.0 {
        a.clamp(1e-3, 1e3)
    } else {
        1.0
    }
}

/// The slice `index` of `plan` as the library draws it (from `cache` when
/// already drawn), at the size of the slice.
pub fn slice(
    vol: &mut Volume,
    mode: &'static str,
    plan: &ViewPlan,
    index: usize,
    colormap: &str,
    cache: &mut SliceCache,
    scratch: &mut Vec<f32>,
) -> Result<(Arc<Vec<u8>>, &'static str)> {
    let cmap = render::effective_colormap(vol, colormap)?;
    let key = (mode, cmap, plan.view, index, vol.current_volume());
    if let Some((rgb, _, _)) = cache.get(&key) {
        return Ok((rgb, cmap));
    }
    let (rgb, cmap) = render::render_slice(vol, plan, index, cmap, scratch)?;
    let rgb = Arc::new(rgb);
    cache.put(key, (rgb.clone(), plan.width, plan.height));
    Ok((rgb, cmap))
}

/// The slice scaled to `w × h` pixels: the nearest voxel for a label map
/// (no blended colours), an average when shrinking, bilinear when enlarging.
pub fn scaled(rgb: &[u8], plan: &ViewPlan, cmap: &str, w: usize, h: usize) -> Result<Picture> {
    let out = thumbnail::resample(rgb, plan.width, plan.height, w.max(1), h.max(1), cmap == "labels")?;
    Ok(Picture { w: w.max(1), h: h.max(1), rgb: out })
}

/// Draw the crosshair through image pixel (col, row) of `plan` on a picture
/// of that view: one pixel wide, half blended with `CROSSHAIR`.
pub fn crosshair(pic: &mut Picture, plan: &ViewPlan, (col, row): (usize, usize)) {
    let x = (((col as f64 + 0.5) * pic.w as f64 / plan.width as f64) as usize).min(pic.w - 1);
    let y = (((row as f64 + 0.5) * pic.h as f64 / plan.height as f64) as usize).min(pic.h - 1);
    let blend = |px: &mut [u8]| {
        for c in 0..3 {
            px[c] = ((px[c] as u16 + CROSSHAIR[c] as u16) / 2) as u8;
        }
    };
    for yy in 0..pic.h {
        let o = (yy * pic.w + x) * 3;
        blend(&mut pic.rgb[o..o + 3]);
    }
    for xx in (0..pic.w).filter(|&xx| xx != x) {
        let o = (y * pic.w + xx) * 3;
        blend(&mut pic.rgb[o..o + 3]);
    }
}

/// The pixel of a `plan` slice under pixel (x, y) of a picture of it.
pub fn slice_pixel(plan: &ViewPlan, pic_w: usize, pic_h: usize, x: usize, y: usize) -> (usize, usize) {
    let col = ((x as f64 + 0.5) * plan.width as f64 / pic_w.max(1) as f64) as usize;
    let row = ((y as f64 + 0.5) * plan.height as f64 / pic_h.max(1) as f64) as usize;
    (col.min(plan.width - 1), row.min(plan.height - 1))
}
