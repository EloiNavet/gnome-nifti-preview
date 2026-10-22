//! Display geometry: how voxel axes map onto the screen for each view.
//!
//! A view is a permutation/flip of a grid, chosen with a port of
//! `nibabel.orientations.io_orientation` (nearest anatomical axis). The grid is
//! the stored voxel grid, except for oblique images in the anatomical modes:
//! they are resampled onto a grid aligned with the world axes (`Grid::world`).
//! Edge labels are always computed from the grid's affine, so the letters on
//! screen tell the truth whatever the mode. Conventions are documented in
//! docs/DISPLAY_CONVENTIONS.md:
//!
//! * screen x grows to the right, screen y grows downwards;
//! * neurological: patient right on screen right (axial, coronal);
//! * radiological: patient right on screen left (axial, coronal);
//! * axial: anterior at the top; coronal and sagittal: superior at the top;
//! * sagittal: anterior on the left (both RAS modes);
//! * stored: raw voxel axes, first axis to the right, last displayed axis upwards.

// Matrix code reads better with explicit indices.
#![allow(clippy::needless_range_loop)]

use serde_json::{json, Value};

use crate::error::{bail, Result};
use crate::volume::{MAX_SLICE_PIXELS, MAX_SLICE_SIDE};

pub type Mat4 = [[f64; 4]; 4];
pub const VIEWS: [&str; 3] = ["coronal", "sagittal", "axial"];
pub const MODES: [&str; 3] = ["neurological", "radiological", "stored"];

/// In the anatomical modes, images more oblique than this are resampled.
pub const RESAMPLE_ABOVE_DEGREES: f64 = 1.0;
/// Resampled slices may always have this many pixels (see `Grid::world`).
pub const RESAMPLED_SLICE_PIXELS: usize = 1 << 20;

const LETTERS: [(&str, &str); 3] = [("L", "R"), ("P", "A"), ("I", "S")];

fn opposite(letter: &str) -> &'static str {
    match letter {
        "L" => "R",
        "R" => "L",
        "P" => "A",
        "A" => "P",
        "I" => "S",
        "S" => "I",
        _ => "?",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewPlan {
    pub view: &'static str,
    pub slice_axis: usize,
    pub h_axis: usize,
    pub h_flip: bool,
    pub v_axis: usize,
    pub v_flip: bool,
    pub width: usize,
    pub height: usize,
    pub n_slices: usize,
    pub pixel_w: f64,
    pub pixel_h: f64,
    pub left: &'static str,
    pub right: &'static str,
    pub top: &'static str,
    pub bottom: &'static str,
    pub slice_up: i32,
    pub slice_up_label: &'static str,
    /// Slices moved by one scroll step.
    pub slice_step: usize,
    pub title: String,
    /// The grid the view is cut from (axes and sizes above refer to it).
    pub grid: Grid,
}

impl ViewPlan {
    pub fn to_json(&self) -> Value {
        json!({
            "view": self.view,
            "slice_axis": self.slice_axis,
            "h_axis": self.h_axis,
            "h_flip": self.h_flip,
            "v_axis": self.v_axis,
            "v_flip": self.v_flip,
            "width": self.width,
            "height": self.height,
            "n_slices": self.n_slices,
            "pixel_w": self.pixel_w,
            "pixel_h": self.pixel_h,
            "labels": {"left": self.left, "right": self.right, "top": self.top, "bottom": self.bottom},
            "slice_up": self.slice_up,
            "slice_up_label": self.slice_up_label,
            "slice_step": self.slice_step,
            "title": self.title,
        })
    }

    /// Grid point under image pixel (col, row) of slice `index`.
    pub fn grid_point(&self, index: usize, col: usize, row: usize) -> [usize; 3] {
        let mut g = [0usize; 3];
        g[self.slice_axis] = index;
        g[self.h_axis] = if self.h_flip { self.width - 1 - col } else { col };
        g[self.v_axis] = if self.v_flip { self.height - 1 - row } else { row };
        g
    }
}

/// The grid the views are cut from: the stored voxel grid, or for an oblique
/// image in an anatomical mode, a grid aligned with the world (RAS) axes that
/// covers the whole image.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    pub shape: [usize; 3],
    /// Grid index to world coordinates.
    pub affine: Mat4,
    pub resampled: bool,
    /// Grid slices per scroll step along each axis.
    pub steps: [usize; 3],
    voxel_shape: [usize; 3],
    /// Inverse of the image's 3×3 matrix and the image's translation
    /// (resampled grids only).
    inv: M3,
    t: [f64; 3],
}

/// Trilinear interpolation weights around a point of the voxel grid.
pub struct Taps {
    i0: [usize; 3],
    i1: [usize; 3],
    f: [f64; 3],
}

#[inline(always)]
fn lerp(a: f64, b: impl FnOnce() -> f64, t: f64) -> f64 {
    if t == 0.0 {
        a
    } else {
        a + (b() - a) * t
    }
}

impl Taps {
    /// Interpolated value; `get` reads voxel `i` (index with strides `s`).
    #[inline(always)]
    pub fn trilinear(&self, s: [usize; 3], get: impl Fn(usize) -> f64) -> f64 {
        let (i, f) = (&self.i0, &self.f);
        let j = &self.i1;
        let at = |x: usize, y: usize, z: usize| get(x * s[0] + y * s[1] + z * s[2]);
        let plane = |z: usize| {
            let x0 = lerp(at(i[0], i[1], z), || at(j[0], i[1], z), f[0]);
            lerp(x0, || lerp(at(i[0], j[1], z), || at(j[0], j[1], z), f[0]), f[1])
        };
        lerp(plane(i[2]), || plane(j[2]), f[2])
    }
}

/// Position along a voxel axis of `n` voxels, None outside the image (more
/// than half a voxel beyond the first or last centre).
#[inline(always)]
fn inside(x: f64, n: usize) -> Option<f64> {
    if x >= -0.5 && x <= n as f64 - 0.5 {
        Some(x.clamp(0.0, (n - 1) as f64))
    } else {
        None
    }
}

impl Grid {
    /// The stored voxel grid.
    pub fn voxel(affine: &Mat4, shape3: [usize; 3]) -> Grid {
        Grid {
            shape: shape3,
            affine: *affine,
            resampled: false,
            steps: [1; 3],
            voxel_shape: shape3,
            inv: [[0.0; 3]; 3],
            t: [0.0; 3],
        }
    }

    /// A grid aligned with the world axes covering the image (the centres of
    /// its corner voxels), as fine as its smallest voxel size. Scrolling moves
    /// by the size of the voxel axis closest to the slice normal (`steps`).
    /// None for images that are flat (a dimension of 1) or degenerate.
    ///
    /// Slices have at most 16 times the pixels of the largest stored slice
    /// (and at least 1 Mpixel): a tiny file with a degenerate affine cannot
    /// ask for huge slices.
    pub fn world(affine: &Mat4, shape3: [usize; 3]) -> Option<Grid> {
        if shape3.iter().any(|&n| n < 2) {
            return None;
        }
        let w2v = ras_axes(affine)?;
        let m: M3 = [
            [affine[0][0], affine[0][1], affine[0][2]],
            [affine[1][0], affine[1][1], affine[1][2]],
            [affine[2][0], affine[2][1], affine[2][2]],
        ];
        let inv = inv3(&m)?;
        let sizes = [norm(column(affine, 0)), norm(column(affine, 1)), norm(column(affine, 2))];
        let mut spacing = sizes[0].min(sizes[1]).min(sizes[2]);
        let (mut lo, mut hi) = ([0.0; 3], [0.0; 3]);
        for r in 0..3 {
            lo[r] = affine[r][3];
            hi[r] = affine[r][3];
            for c in 0..3 {
                let e = affine[r][c] * (shape3[c] - 1) as f64;
                lo[r] += e.min(0.0);
                hi[r] += e.max(0.0);
            }
        }
        // Coarser if needed to keep slices within the slice limits.
        let mut stored = shape3;
        stored.sort_unstable();
        let budget = MAX_SLICE_PIXELS.min(stored[2].saturating_mul(stored[1]).saturating_mul(16).max(RESAMPLED_SLICE_PIXELS));
        let shape = loop {
            let mut shape = [0usize; 3];
            for r in 0..3 {
                let n = ((hi[r] - lo[r]) / spacing + 1e-6).floor();
                if !(0.0..1e9).contains(&n) {
                    return None;
                }
                shape[r] = n as usize + 1;
            }
            let mut d = shape;
            d.sort_unstable();
            let (a, b) = (d[2], d[1]);
            if a <= MAX_SLICE_SIDE && a * b <= budget {
                break shape;
            }
            let f = (a as f64 / MAX_SLICE_SIDE as f64).max((a as f64 * b as f64 / budget as f64).sqrt());
            spacing *= f * 1.001;
        };
        let mut g = [[0.0; 4]; 4];
        let mut steps = [1usize; 3];
        for r in 0..3 {
            g[r][r] = spacing;
            g[r][3] = lo[r];
            steps[r] = ((sizes[w2v[r].0] / spacing + 0.5).floor() as usize).max(1);
        }
        g[3][3] = 1.0;
        Some(Grid {
            shape,
            affine: g,
            resampled: true,
            steps,
            voxel_shape: shape3,
            inv,
            t: [affine[0][3], affine[1][3], affine[2][3]],
        })
    }

    pub fn center(&self) -> [usize; 3] {
        [self.shape[0] / 2, self.shape[1] / 2, self.shape[2] / 2]
    }

    /// Continuous voxel coordinates of grid point `g`.
    #[inline(always)]
    pub fn to_voxel(&self, g: [usize; 3]) -> [f64; 3] {
        let g = [g[0] as f64, g[1] as f64, g[2] as f64];
        if !self.resampled {
            return g;
        }
        let a = &self.affine;
        let d = [
            (a[0][3] + a[0][0] * g[0]) - self.t[0],
            (a[1][3] + a[1][1] * g[1]) - self.t[1],
            (a[2][3] + a[2][2] * g[2]) - self.t[2],
        ];
        let m = &self.inv;
        [
            m[0][0] * d[0] + m[0][1] * d[1] + m[0][2] * d[2],
            m[1][0] * d[0] + m[1][1] * d[1] + m[1][2] * d[2],
            m[2][0] * d[0] + m[2][1] * d[1] + m[2][2] * d[2],
        ]
    }

    /// Voxel nearest to voxel coordinates `v`, None outside the image.
    #[inline(always)]
    pub fn nearest(&self, v: [f64; 3]) -> Option<[usize; 3]> {
        let mut out = [0usize; 3];
        for c in 0..3 {
            out[c] = (inside(v[c], self.voxel_shape[c])? + 0.5).floor() as usize;
        }
        Some(out)
    }

    /// Interpolation weights at voxel coordinates `v`, None outside the image.
    #[inline(always)]
    pub fn taps(&self, v: [f64; 3]) -> Option<Taps> {
        let mut t = Taps { i0: [0; 3], i1: [0; 3], f: [0.0; 3] };
        for c in 0..3 {
            let x = inside(v[c], self.voxel_shape[c])?;
            let b = x.floor();
            t.i0[c] = b as usize;
            t.i1[c] = (t.i0[c] + 1).min(self.voxel_shape[c] - 1);
            t.f[c] = x - b;
        }
        Some(t)
    }

    /// Grid point closest to world point `w`, clamped to the grid; the centre
    /// when the grid's matrix cannot be inverted.
    pub fn from_world(&self, w: [f64; 3]) -> [usize; 3] {
        let a = &self.affine;
        let m: M3 = [[a[0][0], a[0][1], a[0][2]], [a[1][0], a[1][1], a[1][2]], [a[2][0], a[2][1], a[2][2]]];
        let Some(inv) = inv3(&m) else { return self.center() };
        let d = [w[0] - a[0][3], w[1] - a[1][3], w[2] - a[2][3]];
        let mut out = [0usize; 3];
        for c in 0..3 {
            let x = (inv[c][0] * d[0] + inv[c][1] * d[1] + inv[c][2] * d[2] + 0.5).floor();
            out[c] = if x >= 0.0 { (x.min((self.shape[c] - 1) as f64)) as usize } else { 0 };
        }
        out
    }

    pub fn to_json(&self) -> Value {
        json!({"shape": self.shape, "affine": self.affine, "resampled": self.resampled})
    }
}

fn column(a: &Mat4, c: usize) -> [f64; 3] {
    [a[0][c], a[1][c], a[2][c]]
}

fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// Index of the first maximum of |v| (numpy.argmax semantics).
fn argmax_abs(v: [f64; 3]) -> usize {
    let mut best = 0;
    for i in 1..3 {
        if v[i].abs() > v[best].abs() {
            best = i;
        }
    }
    best
}

pub fn direction_letter(v: [f64; 3]) -> &'static str {
    if v.iter().any(|x| !x.is_finite()) || v.iter().all(|&x| x == 0.0) {
        return "?";
    }
    let axis = argmax_abs(v);
    if v[axis] > 0.0 {
        LETTERS[axis].1
    } else {
        LETTERS[axis].0
    }
}

pub fn axis_codes(affine: &Mat4) -> String {
    (0..3).map(|i| direction_letter(column(affine, i))).collect()
}

/// nibabel.affines.obliquity, largest angle in degrees (NaN if undefined).
pub fn obliquity_degrees(affine: &Mat4) -> f64 {
    let vs = [norm(column(affine, 0)), norm(column(affine, 1)), norm(column(affine, 2))];
    let mut worst: f64 = 0.0;
    for row in affine.iter().take(3) {
        let mut best = f64::NEG_INFINITY;
        for (c, &size) in vs.iter().enumerate() {
            best = best.max((row[c] / size).abs());
        }
        let angle = best.min(1.0).acos();
        if !angle.is_finite() {
            return f64::NAN;
        }
        worst = worst.max(angle);
    }
    worst.to_degrees()
}

pub fn voxel_to_world(affine: &Mat4, ijk: [f64; 3]) -> [f64; 3] {
    let mut out = [0.0; 3];
    for (i, o) in out.iter_mut().enumerate() {
        *o = affine[i][0] * ijk[0] + affine[i][1] * ijk[1] + affine[i][2] * ijk[2] + affine[i][3];
    }
    out
}

type M3 = [[f64; 3]; 3];

fn det3(m: &M3) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// Inverse transpose of a 3x3 matrix.
fn inv_t(m: &M3) -> Option<M3> {
    let d = det3(m);
    if d.abs() < 1e-12 || !d.is_finite() {
        return None;
    }
    let mut c = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let (i1, i2) = ((i + 1) % 3, (i + 2) % 3);
            let (j1, j2) = ((j + 1) % 3, (j + 2) % 3);
            c[i][j] = (m[i1][j1] * m[i2][j2] - m[i1][j2] * m[i2][j1]) / d;
        }
    }
    Some(c)
}

/// Inverse of an image's 3×3 matrix; None when (nearly) singular relative to
/// its column sizes.
fn inv3(m: &M3) -> Option<M3> {
    let d = det3(m);
    let scale = norm([m[0][0], m[1][0], m[2][0]]) * norm([m[0][1], m[1][1], m[2][1]]) * norm([m[0][2], m[1][2], m[2][2]]);
    if !(d.is_finite() && d.abs() > 1e-9 * scale) {
        return None;
    }
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let (i1, i2) = ((i + 1) % 3, (i + 2) % 3);
            let (j1, j2) = ((j + 1) % 3, (j + 2) % 3);
            r[j][i] = (m[i1][j1] * m[i2][j2] - m[i1][j2] * m[i2][j1]) / d;
        }
    }
    Some(r)
}

/// Orthogonal factor of the polar decomposition (equals nibabel's P @ Qs from
/// the SVD for full-rank matrices), by Newton iteration.
fn polar_orthogonal(m: &M3) -> Option<M3> {
    let mut r = *m;
    for _ in 0..100 {
        let it = inv_t(&r)?;
        let mut next = [[0.0; 3]; 3];
        let mut delta: f64 = 0.0;
        for i in 0..3 {
            for j in 0..3 {
                next[i][j] = 0.5 * (r[i][j] + it[i][j]);
                delta = delta.max((next[i][j] - r[i][j]).abs());
            }
        }
        r = next;
        if delta < 1e-15 {
            break;
        }
    }
    Some(r)
}

/// For each world axis, the voxel axis that best matches it and its sign.
/// None when the affine is degenerate. Port of nibabel's io_orientation.
fn ras_axes(affine: &Mat4) -> Option<[(usize, i32); 3]> {
    let mut rs = [[0.0; 3]; 3];
    for c in 0..3 {
        let col = column(affine, c);
        let mut z = norm(col);
        if z == 0.0 {
            z = 1.0;
        }
        for r in 0..3 {
            rs[r][c] = col[r] / z;
        }
    }
    if rs.iter().flatten().any(|x| !x.is_finite()) {
        return None;
    }
    let mut r = polar_orthogonal(&rs)?;
    let mut world_to_voxel: [Option<(usize, i32)>; 3] = [None; 3];
    for in_ax in 0..3 {
        let col = [r[0][in_ax], r[1][in_ax], r[2][in_ax]];
        if col.iter().all(|x| x.abs() <= 1e-8) {
            return None;
        }
        let out_ax = argmax_abs(col);
        let sign = if col[out_ax] < 0.0 { -1 } else { 1 };
        if world_to_voxel[out_ax].is_some() {
            return None;
        }
        world_to_voxel[out_ax] = Some((in_ax, sign));
        r[out_ax] = [0.0; 3];
    }
    Some([world_to_voxel[0]?, world_to_voxel[1]?, world_to_voxel[2]?])
}

/// A usable orientation needs a finite affine (rotation, zooms and
/// translation) whose axes map onto distinct anatomical axes.
pub fn orientation_is_usable(affine: &Mat4) -> bool {
    affine.iter().take(3).flatten().all(|x| x.is_finite()) && ras_axes(affine).is_some()
}

fn nearest_view_name(normal: [f64; 3]) -> Option<&'static str> {
    match direction_letter(normal) {
        "R" | "L" => Some("sagittal"),
        "A" | "P" => Some("coronal"),
        "S" | "I" => Some("axial"),
        _ => None,
    }
}

/// The three view plans of an image in a display mode. Oblique images are
/// resampled onto world axes in the anatomical modes (see `Grid::world`).
pub fn build_plans(affine: &Mat4, shape3: [usize; 3], mode: &str, orientation_known: bool) -> Result<[ViewPlan; 3]> {
    if !MODES.contains(&mode) {
        bail!("bad_request", "Unknown mode {mode:?}");
    }
    if orientation_known && mode != "stored" && orientation_is_usable(affine) && obliquity_degrees(affine) > RESAMPLE_ABOVE_DEGREES {
        if let Some(grid) = Grid::world(affine, shape3) {
            return Ok(plans_on(grid, mode, true));
        }
    }
    Ok(plans_on(Grid::voxel(affine, shape3), mode, orientation_known))
}

fn plans_on(grid: Grid, mode: &str, orientation_known: bool) -> [ViewPlan; 3] {
    let (affine, shape3) = (&grid.affine, grid.shape);
    let w2v = if orientation_known { ras_axes(affine) } else { None };
    let mode = if w2v.is_none() { "stored" } else { mode };
    let cols = [column(affine, 0), column(affine, 1), column(affine, 2)];
    let sizes = cols.map(|c| {
        let n = norm(c);
        if n.is_finite() && n > 0.0 {
            n
        } else {
            1.0
        }
    });
    let scale = |v: [f64; 3], s: f64| [v[0] * s, v[1] * s, v[2] * s];

    let make = |view: &'static str| -> ViewPlan {
        let (slice_axis, h_axis, h_flip, v_axis, v_flip, slice_up);
        if mode == "stored" {
            let (s, h, v) = match view {
                "axial" => (2, 0, 1),
                "coronal" => (1, 0, 2),
                _ => (0, 1, 2),
            };
            slice_axis = s;
            h_axis = h;
            v_axis = v;
            h_flip = false;
            v_flip = true;
            slice_up = 1;
        } else {
            let w2v = w2v.unwrap();
            let (sw, (hw, mut hs), (vw, vs)) = match view {
                "axial" => (2, (0, 1), (1, -1)),
                "coronal" => (1, (0, 1), (2, -1)),
                _ => (0, (1, -1), (2, -1)),
            };
            if mode == "radiological" && view != "sagittal" {
                hs = -hs;
            }
            let (sa, ss) = w2v[sw];
            let (ha, hvs) = w2v[hw];
            let (va, vvs) = w2v[vw];
            slice_axis = sa;
            h_axis = ha;
            v_axis = va;
            h_flip = hs * hvs < 0;
            v_flip = vs * vvs < 0;
            slice_up = ss;
        }
        let (left, right, top, bottom, up_label);
        if w2v.is_none() {
            left = "?";
            right = "?";
            top = "?";
            bottom = "?";
            up_label = "?";
        } else {
            right = direction_letter(scale(cols[h_axis], if h_flip { -1.0 } else { 1.0 }));
            bottom = direction_letter(scale(cols[v_axis], if v_flip { -1.0 } else { 1.0 }));
            left = opposite(right);
            top = opposite(bottom);
            up_label = direction_letter(scale(cols[slice_axis], slice_up as f64));
        }
        let title = if mode == "stored" {
            let nearest = if w2v.is_some() { nearest_view_name(cols[slice_axis]) } else { None };
            match nearest {
                Some(n) => format!("Axis {slice_axis} (≈ {n})"),
                None => format!("Axis {slice_axis}"),
            }
        } else {
            let mut t = view.to_string();
            t[..1].make_ascii_uppercase();
            t
        };
        ViewPlan {
            view,
            slice_axis,
            h_axis,
            h_flip,
            v_axis,
            v_flip,
            width: shape3[h_axis],
            height: shape3[v_axis],
            n_slices: shape3[slice_axis],
            pixel_w: sizes[h_axis],
            pixel_h: sizes[v_axis],
            left,
            right,
            top,
            bottom,
            slice_up,
            slice_up_label: up_label,
            slice_step: grid.steps[slice_axis],
            title,
            grid: grid.clone(),
        }
    };
    [make("coronal"), make("sagittal"), make("axial")]
}

pub fn effective_mode<'a>(mode: &'a str, orientation_known: bool, affine: &Mat4) -> &'a str {
    if orientation_known && ras_axes(affine).is_some() {
        mode
    } else {
        "stored"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(x: f64, y: f64, z: f64) -> Mat4 {
        [[x, 0.0, 0.0, 0.0], [0.0, y, 0.0, 0.0], [0.0, 0.0, z, 0.0], [0.0, 0.0, 0.0, 1.0]]
    }

    #[test]
    fn ras_neurological_labels() {
        let p = build_plans(&diag(1.0, 1.0, 1.0), [10, 12, 14], "neurological", true).unwrap();
        let axial = &p[2];
        assert_eq!((axial.left, axial.right, axial.top, axial.bottom), ("L", "R", "A", "P"));
        assert_eq!((axial.width, axial.height, axial.n_slices), (10, 12, 14));
        let sag = &p[1];
        assert_eq!((sag.left, sag.right, sag.top, sag.bottom), ("A", "P", "S", "I"));
        assert!(sag.h_flip);
    }

    #[test]
    fn lps_radiological() {
        let p = build_plans(&diag(-1.0, -1.0, 1.0), [10, 12, 14], "radiological", true).unwrap();
        let axial = &p[2];
        assert_eq!((axial.left, axial.right), ("R", "L"));
        assert_eq!(axial.slice_up_label, "S");
        assert_eq!(axis_codes(&diag(-1.0, -1.0, 1.0)), "LPS");
    }

    #[test]
    fn degenerate_affine() {
        assert!(!orientation_is_usable(&diag(1.0, 0.0, 1.0)));
        let p = build_plans(&diag(1.0, 0.0, 1.0), [4, 5, 6], "neurological", true).unwrap();
        assert!(p[2].title.starts_with("Axis 2"));
    }

    #[test]
    fn world_grid_within_slice_limits_and_consistent() {
        let t = 30f64.to_radians();
        let a = [
            [0.01 * t.cos(), -t.sin(), 0.0, 5.0],
            [0.01 * t.sin(), t.cos(), 0.0, -7.0],
            [0.0, 0.0, 1.0, 2.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let g = Grid::world(&a, [4000, 300, 200]).unwrap();
        let mut d = g.shape;
        d.sort_unstable();
        assert!(d[2] <= MAX_SLICE_SIDE && d[2] * d[1] <= MAX_SLICE_PIXELS, "{:?}", g.shape);
        assert!(g.affine[0][0] > 0.01, "coarser than the smallest voxel");
        for p in [[0, 0, 0], g.center(), [g.shape[0] - 1, g.shape[1] - 1, g.shape[2] - 1]] {
            let w = voxel_to_world(&g.affine, p.map(|x| x as f64));
            assert_eq!(g.from_world(w), p);
            let through_voxels = voxel_to_world(&a, g.to_voxel(p));
            assert!((0..3).all(|i| (w[i] - through_voxels[i]).abs() < 1e-6), "{w:?} {through_voxels:?}");
        }
        // The image's corner voxels are inside the grid.
        for c in [[0.0, 0.0, 0.0], [3999.0, 299.0, 199.0], [0.0, 299.0, 0.0], [3999.0, 0.0, 199.0]] {
            let v = g.to_voxel(g.from_world(voxel_to_world(&a, c)));
            assert!((0..3).all(|i| (v[i] - c[i]).abs() <= [3.0, 0.05, 0.05][i] * 2.0), "{c:?} -> {v:?}");
        }
        assert!(Grid::world(&diag(1.0, 1.0, 1.0), [5, 5, 1]).is_none(), "flat image");

        // A tiny image whose affine has a microscopic voxel axis (found by
        // fuzzing): slices stay small instead of reaching the slice limits.
        let a = [
            [7.6e-6, 0.0, 0.0, -10.0],
            [0.0, 0.94, -0.85, -12.0],
            [0.0, 0.34, 2.35, -8.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let g = Grid::world(&a, [8, 7, 6]).unwrap();
        let mut d = g.shape;
        d.sort_unstable();
        assert!(d[2] * d[1] <= RESAMPLED_SLICE_PIXELS, "{:?}", g.shape);
        let g = Grid::world(&a, [8, 700, 600]).unwrap();
        let mut d = g.shape;
        d.sort_unstable();
        assert!(d[2] * d[1] > RESAMPLED_SLICE_PIXELS, "16 × 700 × 600 allowed: {:?}", g.shape);
        assert!(d[2] * d[1] <= 16 * 700 * 600, "{:?}", g.shape);
    }

    #[test]
    fn obliquity() {
        assert!(obliquity_degrees(&diag(1.0, 2.0, 3.0)).abs() < 1e-9);
        let t = 20f64.to_radians();
        let a = [[t.cos(), -t.sin(), 0.0, 0.0], [t.sin(), t.cos(), 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        assert!((obliquity_degrees(&a) - 20.0).abs() < 1e-9);
    }
}
