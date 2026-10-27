//! Segmentation (label map) detection and categorical colouring.
//!
//! A volume is treated as a label map when it has identity scaling, integral
//! values, at most 256 labels and, as the real discriminator,
//! piecewise-constant content (neighbouring foreground voxels are usually
//! equal in a label map and usually different in an intensity image).

use std::collections::{HashMap, HashSet};

use crate::error::{PreviewError, Result};
use crate::voxel::VoxelValue;

pub const MAX_LABELS: usize = 256;
const PIECEWISE_MIN_PAIRS: u64 = 256;
const PIECEWISE_MAX_DIFF_RATIO: f64 = 0.30;
pub const NIFTI_INTENT_LABEL: i32 = 1002;
const GOLDEN: f64 = 0.618033988749895;

#[derive(Debug, Clone, PartialEq)]
pub struct LabelInfo {
    pub is_labels: bool,
    pub binary: bool,
    pub n_labels: usize,
    pub reason: &'static str,
    /// The labels seen (centre slices and sample), sorted, 0 excluded.
    pub ids: Vec<i128>,
    /// Colours: "binary" (white), "freesurfer", "custom" (a user table) or
    /// "random" (the generic palette); empty when not a label map.
    pub scheme: &'static str,
}

pub const NOT_LABELS: LabelInfo =
    LabelInfo { is_labels: false, binary: false, n_labels: 0, reason: "intensity", ids: Vec::new(), scheme: "" };

/// A centre slice: rows, columns and the voxel index of (row, column).
type SliceAt<'a> = (usize, usize, Box<dyn Fn(usize, usize) -> usize + 'a>);

/// `get(i)` returns the exact stored value of voxel i (F order).
pub fn detect(
    get: impl Fn(usize) -> VoxelValue,
    shape: [usize; 3],
    sample: impl Iterator<Item = VoxelValue>,
    identity_scaling: bool,
    intent_code: i32,
) -> LabelInfo {
    let forced = intent_code == NIFTI_INTENT_LABEL;
    if !identity_scaling && !forced {
        return NOT_LABELS;
    }
    let [nx, ny, nz] = shape;
    let idx = |i: usize, j: usize, k: usize| i + nx * (j + ny * k);
    // The three centre slices, laid out like numpy's data[x,:,:], data[:,y,:], data[:,:,z]
    // (rows, cols) so that horizontal neighbour pairs match the reference.
    let (cx, cy, cz) = (nx / 2, ny / 2, nz / 2);
    let slices: [SliceAt<'_>; 3] = [
        (ny, nz, Box::new(move |r, c| idx(cx, r, c))),
        (nx, nz, Box::new(move |r, c| idx(r, cy, c))),
        (nx, ny, Box::new(move |r, c| idx(r, c, cz))),
    ];

    // Piecewise-constant test on the centre slices first: it rejects noisy
    // intensity images cheaply, before the whole-volume sample is examined.
    let (mut total, mut diff) = (0u64, 0u64);
    for (rows, cols, at) in &slices {
        for r in 0..*rows {
            for c in 0..cols.saturating_sub(1) {
                let a = get(at(r, c));
                let b = get(at(r, c + 1));
                if !a.is_zero() && !b.is_zero() && a.is_finite() && b.is_finite() {
                    total += 1;
                    if a != b {
                        diff += 1;
                    }
                }
            }
        }
    }
    if total >= PIECEWISE_MIN_PAIRS && diff as f64 / total as f64 > PIECEWISE_MAX_DIFF_RATIO && !forced {
        return NOT_LABELS;
    }
    // The centre slices and sample share one bounded set of exact keys.
    // No copy of the million-value sample is needed for label detection.
    let mut labels = HashSet::new();
    let mut ids: HashSet<i128> = HashSet::new();
    let mut add = |v: VoxelValue| {
        if !v.is_finite() { return true; }
        if !v.is_integral() || v.is_negative() { return false; }
        if !v.is_zero() {
            labels.insert(v.key());
            ids.insert(v.label_id());
        }
        labels.len() <= MAX_LABELS
    };
    for (rows, cols, at) in &slices {
        for r in 0..*rows {
            for c in 0..*cols {
                if !add(get(at(r, c))) { return NOT_LABELS; }
            }
        }
    }
    for v in sample {
        if !add(v) { return NOT_LABELS; }
    }
    if labels.is_empty() { return NOT_LABELS; }
    let mut ids: Vec<i128> = ids.into_iter().collect();
    ids.sort_unstable();

    let reason = if forced {
        "intent"
    } else if total >= PIECEWISE_MIN_PAIRS {
        "piecewise-constant"
    } else {
        "sparse-integral"
    };
    let binary = labels.len() == 1;
    LabelInfo { is_labels: true, binary, n_labels: labels.len(), reason, ids, scheme: if binary { "binary" } else { "random" } }
}

/// Python's round() for the positive values used here (ties to even).
fn round_half_even(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// colorsys.hsv_to_rgb
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    if s == 0.0 {
        return (v, v, v);
    }
    let i = (h * 6.0) as i64;
    let f = h * 6.0 - i as f64;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// Deterministic, well-spread colour for a label value (0 is never coloured).
pub fn label_color(label: i128) -> [u8; 3] {
    let hue = if label.unsigned_abs() <= 1 << 24 {
        (label as f64 * GOLDEN).rem_euclid(1.0)
    } else {
        // SplitMix64: mix integer bits before converting to a hue. Adjacent
        // large labels must not collapse during floating-point multiplication.
        let mut x = (label as u64) ^ ((label >> 64) as u64);
        x = x.wrapping_add(0x9e3779b97f4a7c15);
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
        ((x ^ (x >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    };
    let saturation = [0.55, 0.75, 0.95][label.rem_euclid(3) as usize];
    let value = if label.div_euclid(3).rem_euclid(2) == 0 { 1.0 } else { 0.8 };
    let (r, g, b) = hsv_to_rgb(hue, saturation, value);
    [round_half_even(r * 255.0) as u8, round_half_even(g * 255.0) as u8, round_half_even(b * 255.0) as u8]
}

/// RGB image of a label slice (exact values: identifiers above 2^24 keep
/// their own colour). Background and non-finite voxels are black; labels a
/// colour table has take its colour, the others the generic palette.
pub fn colorize(
    values: impl ExactSizeIterator<Item = VoxelValue>,
    binary: bool,
    table: Option<&crate::lut::Table>,
    out: &mut Vec<u8>,
) -> Result<()> {
    out.clear();
    out.try_reserve_exact(values.len() * 3).map_err(|_| PreviewError::new("too_large", "Not enough memory to preview this image."))?;
    let mut cache: HashMap<i128, [u8; 3]> = HashMap::new();
    for v in values {
        let label = v.label_id();
        let rgb = if label == 0 {
            [0, 0, 0]
        } else if binary {
            [255, 255, 255]
        } else {
            *cache.entry(label).or_insert_with(|| table.and_then(|t| t.get(&label)).map_or_else(|| label_color(label), |e| e.rgb))
        };
        out.extend_from_slice(&rgb);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_match_python_reference() {
        // Values printed by nifti_preview_ref.labels.label_color
        assert_eq!(label_color(1), [64, 120, 255]);
        assert_eq!(label_color(2), [154, 255, 13]);
        assert_eq!(label_color(3), [204, 92, 190]);
        assert_eq!(label_color(7), [72, 255, 64]);
        assert_eq!(label_color(1035), [92, 93, 204]);
        assert_eq!(label_color(2035), [101, 64, 255]);
    }
}
