//! File-manager thumbnails: the centre slice of one view of volume 0, with its
//! physical aspect ratio, scaled to the size the file manager asks for.
//!
//! GNOME runs thumbnailers in a bubblewrap sandbox that only sees `/usr`, the
//! input file (as `/tmp/gnome-desktop-file-to-thumbnail.<ext>`, so the format
//! is recognised from the content) and the output. `nifti-preview thumbnail`
//! opens its output first, then applies its own sandbox (no writes, no
//! sockets, bounded memory) before it reads the image.

use crate::error::{bail, PreviewError, Result};
use crate::orientation::{self, ViewPlan, VIEWS};
use crate::render::{self, COLORMAPS};
use crate::volume::Volume;

#[derive(Debug, Clone)]
pub struct Options {
    /// Largest side of the thumbnail, in pixels.
    pub size: usize,
    pub view: &'static str,
    pub mode: &'static str,
    pub colormap: &'static str,
    /// Palette of the grey levels and percentiles of the window.
    pub palette: &'static str,
    pub percentiles: (f64, f64),
    /// Colours of label maps: auto, freesurfer or random.
    pub labels: &'static str,
    /// Compressed data that must be inflated to draw volume 0 (estimated from
    /// the file size); larger files get no thumbnail.
    pub max_file_bytes: u64,
    /// Voxel data of volume 0 (the image limit of the preview).
    pub max_image_bytes: u64,
    /// Make thumbnails of files on network file systems too.
    pub network: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            size: 256,
            view: "axial",
            mode: "neurological",
            colormap: "auto",
            palette: "gray",
            labels: "auto",
            percentiles: (crate::window::LOW_PERCENTILE, crate::window::HIGH_PERCENTILE),
            max_file_bytes: 250 << 20,
            network: false,
            max_image_bytes: 1 << 30,
        }
    }
}

pub const MIN_SIZE: usize = 16;
pub const MAX_SIZE: usize = 2048;

pub fn choice(value: &str, choices: &[&'static str], what: &str) -> Result<&'static str> {
    match choices.iter().find(|c| **c == value) {
        Some(c) => Ok(c),
        None => bail!("bad_request", "Unknown {what} {value:?}"),
    }
}

impl Options {
    pub fn set_view(&mut self, v: &str) -> Result<()> {
        self.view = choice(v, &VIEWS, "view")?;
        Ok(())
    }
    pub fn set_mode(&mut self, m: &str) -> Result<()> {
        self.mode = choice(m, &orientation::MODES, "mode")?;
        Ok(())
    }
    pub fn set_colormap(&mut self, c: &str) -> Result<()> {
        self.colormap = choice(c, &COLORMAPS, "colormap")?;
        Ok(())
    }
    pub fn set_labels(&mut self, l: &str) -> Result<()> {
        self.labels = choice(l, &["auto", "freesurfer", "random"], "label colours")?;
        Ok(())
    }
    pub fn set_palette(&mut self, p: &str) -> Result<()> {
        self.palette = choice(p, &crate::window::PALETTES, "palette")?;
        Ok(())
    }
    /// "LOW,HIGH" percentiles of the window.
    pub fn set_percentiles(&mut self, text: &str) -> Result<()> {
        let parsed = text.split_once(',').and_then(|(a, b)| Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?)));
        match parsed {
            Some((lo, hi)) if lo.is_finite() && hi.is_finite() && 0.0 <= lo && lo < hi && hi <= 100.0 => {
                self.percentiles = (lo, hi);
                Ok(())
            }
            _ => bail!("bad_request", "--percentiles must be LOW,HIGH with 0 <= LOW < HIGH <= 100"),
        }
    }
}

/// An RGB8 image.
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

/// Render the thumbnail of `vol`.
pub fn render(vol: &mut Volume, opts: &Options) -> Result<Picture> {
    // Files decides what to thumbnail; a mount of remote storage can look
    // local to it (sshfs, GVfs): reading it is left to the preview.
    if vol.storage.network && !opts.network {
        bail!("unsupported_format", "No thumbnail for a file on a network file system ({}).", vol.storage.fs);
    }
    // A single-voxel spectrum has no image: draw the spectrum itself.
    if vol.mrs.is_some() && vol.shape3 == [1, 1, 1] {
        let s = vol.spectrum([0, 0, 0])?;
        return Ok(spectrum_picture(&s, opts.size.clamp(MIN_SIZE, MAX_SIZE)));
    }
    // Compressed data (a gzipped file, NRRD gzip or bzip2, MetaImage zlib,
    // compressed DICOM) must be decoded up to the end of volume 0, or whole.
    if vol.data_compressed() || vol.header.layout.decoder.is_some() {
        let estimate = vol.bytes_to_read();
        if estimate > opts.max_file_bytes {
            bail!(
                "too_large",
                "No thumbnail: about {} MB of compressed data to read (limit {} MB).",
                estimate >> 20,
                opts.max_file_bytes >> 20
            );
        }
    }
    vol.set_percentiles(opts.percentiles.0, opts.percentiles.1)?;
    vol.palette = opts.palette;
    vol.set_label_colors(opts.labels, None);
    let mode = orientation::effective_mode(opts.mode, vol.orientation_known, &vol.affine);
    let plans = vol.plans(mode)?;
    // A single slice (a radiograph, one DICOM image) is shown in the view
    // that holds it whole, whatever the view chosen.
    let flat = plans.iter().find(|p| p.n_slices == 1 && p.width > 1 && p.height > 1);
    let chosen = plans.iter().find(|p| p.view == opts.view).expect("three views");
    // A view across thin slices (an axial cut through a few coronal slices)
    // is a strip: then the view of the acquired plane.
    let elongation = |p: &ViewPlan| {
        let (w, h) = (p.width as f64 * p.pixel_w.max(1e-6), p.height as f64 * p.pixel_h.max(1e-6));
        w.max(h) / w.min(h).max(1e-6)
    };
    // (Not a view one voxel wide: a line of values stays a line.)
    let squarest = plans.iter().filter(|p| p.width > 1 && p.height > 1).min_by(|a, b| elongation(a).total_cmp(&elongation(b)));
    let chosen = match squarest {
        Some(s) if elongation(chosen) > 3.0 && elongation(s) * 2.0 < elongation(chosen) => s,
        _ => chosen,
    };
    let plan = if vol.shape3.contains(&1) { flat.unwrap_or(chosen) } else { chosen }.clone();
    // A segmentation is shown through the centre of its labels (a small
    // structure is often missed by the centre slice).
    let mut index = plan.n_slices / 2;
    if opts.colormap != "gray" && opts.colormap != "rgb" {
        if let Some((centre, _)) = vol.label_extent()? {
            let g = plan.grid.from_world(orientation::voxel_to_world(&vol.affine, centre));
            index = g[plan.slice_axis].min(plan.n_slices - 1);
        }
    }
    let mut scratch = Vec::new();
    let (rgb, cmap) = render::render_slice(vol, &plan, index, opts.colormap, &mut scratch)?;
    // The physical size of the slice decides the shape of the thumbnail.
    let size = |x: f64| if x.is_finite() && x > 0.0 { x } else { 1.0 };
    let (pw, ph) = (plan.width as f64 * size(plan.pixel_w), plan.height as f64 * size(plan.pixel_h));
    let target = opts.size.clamp(MIN_SIZE, MAX_SIZE) as f64;
    let scale = target / pw.max(ph);
    let w = ((pw * scale).round() as usize).clamp(1, MAX_SIZE);
    let h = ((ph * scale).round() as usize).clamp(1, MAX_SIZE);
    let nearest = cmap == "labels";
    let out = resample(&rgb, plan.width, plan.height, w, h, nearest)?;
    Ok(Picture { width: w, height: h, rgb: out })
}

/// The spectrum over its default range (high shifts on the left, as in the
/// preview), as a light curve on black, 4:3.
fn spectrum_picture(s: &crate::mrs::Spectrum, size: usize) -> Picture {
    let (w, h) = (size, (size * 3 / 4).max(1));
    let mut rgb = vec![0u8; w * h * 3];
    let values = if s.default_part == "real" { &s.real } else { &s.magnitude };
    let n = values.len();
    let axis = |i: usize| if n > 1 { s.first + (s.last - s.first) * i as f64 / (n - 1) as f64 } else { s.first };
    let (lo, hi) = match s.default_range {
        Some([a, b]) => (a.max(s.first.min(s.last)), b.min(s.first.max(s.last))),
        None => (s.first.min(s.last), s.first.max(s.last)),
    };
    let shown: Vec<usize> = (0..n).filter(|&i| (lo..=hi).contains(&axis(i)) && values[i].is_finite()).collect();
    let (mut vmin, mut vmax) = (0.0f32, 0.0f32);
    for &i in &shown {
        vmin = vmin.min(values[i]);
        vmax = vmax.max(values[i]);
    }
    if shown.len() < 2 || vmax <= vmin {
        return Picture { width: w, height: h, rgb };
    }
    let margin = (w / 16).max(2) as f64;
    let descending = s.last < s.first;
    let x_of = |a: f64| {
        let t = (a - lo) / (hi - lo).max(f64::MIN_POSITIVE);
        margin + (if descending { 1.0 - t } else { t }) * (w as f64 - 2.0 * margin)
    };
    let y_of = |v: f32| margin + (1.0 - ((v - vmin) / (vmax - vmin)) as f64) * (h as f64 - 2.0 * margin);
    let mut plot = |x: f64, y: f64| {
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let (px, py) = (x as isize + dx, y as isize + dy);
            if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                let o = (py as usize * w + px as usize) * 3;
                rgb[o..o + 3].copy_from_slice(&[143, 196, 255]);
            }
        }
    };
    for pair in shown.windows(2) {
        let (x0, y0) = (x_of(axis(pair[0])), y_of(values[pair[0]]));
        let (x1, y1) = (x_of(axis(pair[1])), y_of(values[pair[1]]));
        let steps = ((x1 - x0).abs().max((y1 - y0).abs()).ceil() as usize).max(1);
        for k in 0..=steps {
            let t = k as f64 / steps as f64;
            plot(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
        }
    }
    Picture { width: w, height: h, rgb }
}

/// Scale an RGB8 image to `w × h`: nearest voxel for labels (no blended
/// colours), otherwise the average of the source area when shrinking and a
/// bilinear interpolation when enlarging.
pub fn resample(src: &[u8], sw: usize, sh: usize, w: usize, h: usize, nearest: bool) -> Result<Vec<u8>> {
    if sw == 0 || sh == 0 || src.len() < sw * sh * 3 {
        bail!("internal", "Empty slice");
    }
    let mut out = Vec::new();
    out.try_reserve_exact(w * h * 3).map_err(|_| PreviewError::new("too_large", "Not enough memory for the thumbnail."))?;
    let (fx, fy) = (sw as f64 / w as f64, sh as f64 / h as f64);
    let px = |x: usize, y: usize, c: usize| src[(y * sw + x) * 3 + c] as f64;
    for y in 0..h {
        for x in 0..w {
            if nearest {
                let sx = (((x as f64 + 0.5) * fx) as usize).min(sw - 1);
                let sy = (((y as f64 + 0.5) * fy) as usize).min(sh - 1);
                out.extend_from_slice(&src[(sy * sw + sx) * 3..(sy * sw + sx) * 3 + 3]);
            } else if fx >= 1.0 && fy >= 1.0 {
                // Box filter over the source pixels this pixel covers.
                let (x0, x1) = ((x as f64 * fx) as usize, (((x + 1) as f64 * fx).ceil() as usize).min(sw));
                let (y0, y1) = ((y as f64 * fy) as usize, (((y + 1) as f64 * fy).ceil() as usize).min(sh));
                let n = ((x1 - x0) * (y1 - y0)).max(1) as f64;
                for c in 0..3 {
                    let mut sum = 0.0;
                    for yy in y0..y1 {
                        for xx in x0..x1 {
                            sum += px(xx, yy, c);
                        }
                    }
                    out.push((sum / n).round().clamp(0.0, 255.0) as u8);
                }
            } else {
                let sx = ((x as f64 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f64);
                let sy = ((y as f64 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f64);
                let (x0, y0) = (sx.floor() as usize, sy.floor() as usize);
                let (x1, y1) = ((x0 + 1).min(sw - 1), (y0 + 1).min(sh - 1));
                let (tx, ty) = (sx - x0 as f64, sy - y0 as f64);
                for c in 0..3 {
                    let top = px(x0, y0, c) * (1.0 - tx) + px(x1, y0, c) * tx;
                    let bottom = px(x0, y1, c) * (1.0 - tx) + px(x1, y1, c) * tx;
                    out.push((top * (1.0 - ty) + bottom * ty).round().clamp(0.0, 255.0) as u8);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinking_averages_and_labels_stay_pure() {
        // 4×2 image: left half black, right half white.
        let mut src = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                let v = if x < 2 { 0 } else { 255 };
                src.extend_from_slice(&[v, v, v]);
            }
        }
        let out = resample(&src, 4, 2, 2, 1, false).unwrap();
        assert_eq!(out, vec![0, 0, 0, 255, 255, 255]);
        let out = resample(&src, 4, 2, 1, 1, false).unwrap();
        assert_eq!(out, vec![128, 128, 128]);
        let out = resample(&src, 4, 2, 1, 1, true).unwrap();
        assert!(out == vec![0, 0, 0] || out == vec![255, 255, 255]);
    }

    #[test]
    fn enlarging_interpolates() {
        let src = vec![0, 0, 0, 200, 200, 200];
        let out = resample(&src, 2, 1, 4, 1, false).unwrap();
        let grey: Vec<u8> = out.chunks(3).map(|p| p[0]).collect();
        assert_eq!(grey[0], 0);
        assert_eq!(grey[3], 200);
        assert!(grey[1] > 0 && grey[1] < grey[2] && grey[2] < 200, "{grey:?}");
    }
}
