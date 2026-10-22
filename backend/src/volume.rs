//! A NIfTI volume: header metadata, lazily loaded voxel data of volume 0,
//! typed voxel decoding and whole-volume statistics.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{bail, PreviewError, Result};
use crate::header::{self, DType, Endian, Header};
use crate::labels::{self, LabelInfo};
use crate::mrs::{Mrs, Spectrum};
use crate::orientation::{self, Mat4, ViewPlan};
use crate::source::{self, Opened, Storage};
use crate::window::{self, Window};
use crate::voxel::VoxelValue;

pub const DEFAULT_MAX_BYTES: u64 = 3 << 30;
/// Bounds of NIFTI_PREVIEW_MAX_BYTES (16 MiB to 64 GiB).
pub const MAX_BYTES_RANGE: (u64, u64) = (16 << 20, 64 << 30);
pub const MAX_SAMPLES: usize = 1_000_000;
/// Largest slice drawn: 16 megapixels (4096 × 4096), no side above 16384.
/// Bigger slices would need hundreds of MB per view in the previewer itself.
pub const MAX_SLICE_PIXELS: usize = 16 << 20;
pub const MAX_SLICE_SIDE: usize = 16384;

pub struct Data {
    storage: Storage,
    offset: usize,
    /// Bytes of volume 0 (what the views show).
    len: usize,
    /// Spectroscopy: all the data are loaded (volume 0 is their beginning),
    /// or why they could not be.
    all: std::result::Result<(), PreviewError>,
}

pub struct Stats {
    pub window: Window,
    pub labels: LabelInfo,
    pub non_finite: u64,
    pub non_finite_exact: bool,
    pub min: Option<Value>,
    pub max: Option<Value>,
    pub notes: Vec<String>,
}

pub struct Volume {
    pub path: PathBuf,
    pub file_size: u64,
    pub compressed: bool,
    pub header: Header,
    pub affine: Mat4,
    pub orientation_known: bool,
    pub shape3: [usize; 3],
    /// Spectroscopy (NIfTI-MRS) metadata.
    pub mrs: Option<Mrs>,
    max_bytes: u64,
    file: std::fs::File,
    data: Option<Data>,
    stats: Option<Stats>,
    /// Voxel counts already computed by `count_equal`, keyed by exact value.
    counts: std::collections::HashMap<(u8, u128), u64>,
}

/// A byte count from the environment, clamped to `range`; the default when
/// unset or invalid (with a warning on stderr).
pub fn bytes_from_env(name: &str, default: u64, range: (u64, u64)) -> u64 {
    match std::env::var(name) {
        Err(_) => default,
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(n) => n.clamp(range.0, range.1),
            Err(_) => {
                eprintln!("nifti-preview: ignoring {name}={v:?} (not a byte count)");
                default
            }
        },
    }
}

pub fn max_bytes_from_env() -> u64 {
    bytes_from_env("NIFTI_PREVIEW_MAX_BYTES", DEFAULT_MAX_BYTES, MAX_BYTES_RANGE)
}

/// `too_large` when the largest slice of `shape` is above the slice limits.
pub fn check_slice_size(shape: [usize; 3]) -> Result<()> {
    let mut d = shape;
    d.sort_unstable();
    let (a, b) = (d[2], d[1]);
    if a > MAX_SLICE_SIDE || a.saturating_mul(b) > MAX_SLICE_PIXELS {
        bail!("too_large", "Image too large to preview (slices of {a} × {b} voxels, limit 4096 × 4096).");
    }
    Ok(())
}

impl Volume {
    pub fn open(path: &Path) -> Result<Volume> {
        Self::open_with_limit(path, max_bytes_from_env())
    }

    pub fn open_with_limit(path: &Path, max_bytes: u64) -> Result<Volume> {
        let Opened { header, extensions, compressed, file_size, file } = source::open(path)?;
        let mrs = Mrs::detect(&header, &extensions);
        let affine = header.best_affine()?;
        let orientation_known = header.affine_source() != "none" && orientation::orientation_is_usable(&affine);
        let shape3 = header.shape3();
        Ok(Volume {
            path: path.to_path_buf(),
            file_size,
            compressed,
            header,
            affine,
            orientation_known,
            shape3,
            mrs,
            max_bytes,
            file,
            data: None,
            stats: None,
            counts: std::collections::HashMap::new(),
        })
    }

    pub fn n_voxels(&self) -> usize {
        self.shape3.iter().try_fold(1usize, |a, &d| a.checked_mul(d)).unwrap_or(usize::MAX)
    }

    pub fn header_info(&self) -> Value {
        let h = &self.header;
        let zooms = h.zooms();
        let obliq = orientation::obliquity_degrees(&self.affine);
        let mut warnings = Vec::new();
        let single_voxel = self.mrs.is_some() && self.shape3 == [1, 1, 1];
        if single_voxel {
            // A single-voxel spectrum: no views, so nothing to say about them.
        } else if !self.orientation_known {
            warnings.push("No valid qform/sform: anatomical orientation is unknown, showing stored voxel axes.".to_string());
        } else if obliq.is_finite() && obliq > orientation::RESAMPLE_ABOVE_DEGREES {
            let resampled = self.plans("neurological").map(|p| p[0].grid.resampled).unwrap_or(false);
            warnings.push(if resampled {
                format!(
                    "Oblique acquisition ({obliq:.1}°): views are resampled onto the anatomical axes; the stored mode shows the voxels as acquired."
                )
            } else {
                format!("Oblique acquisition ({obliq:.1}°): views follow the nearest anatomical axes, data are not resampled.")
            });
        }
        let n_volumes = h.n_volumes();
        if n_volumes > 1 && self.mrs.is_none() {
            warnings.push(format!("4D image: showing volume 1 of {n_volumes}."));
        }
        if let Some(m) = &self.mrs {
            let fixed = m.first_index_only();
            if !fixed.is_empty() {
                let list: Vec<String> = fixed.iter().map(|(t, n)| format!("{t} ({n})")).collect();
                warnings.push(format!(
                    "The spectrum uses index 0 of {}: these dimensions hold different signals and are not averaged.",
                    list.join(", ")
                ));
            }
        }
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        json!({
            "path": self.path.to_string_lossy(),
            "name": name,
            "file_size": self.file_size,
            "format": h.format(),
            "compressed": self.compressed,
            "shape": h.shape,
            "n_volumes": n_volumes,
            "zooms": zooms,
            "space_units": h.space_units(),
            "time_units": h.time_units(),
            // In spectroscopy, pixdim[4] is the dwell time.
            "repetition_time": if zooms.len() > 3 && self.mrs.is_none() { json!(zooms[3]) } else { Value::Null },
            "dtype": h.dtype.numpy_str(h.endian),
            "datatype": h.dtype.label(),
            "slope": h.slope,
            "inter": h.inter,
            "qform_code": h.qform_code,
            "qform": header::xform_label(h.qform_code),
            "sform_code": h.sform_code,
            "sform": header::xform_label(h.sform_code),
            "affine_source": h.affine_source(),
            "affine": self.affine,
            "orientation": if self.orientation_known { orientation::axis_codes(&self.affine) } else { "unknown".to_string() },
            "orientation_known": self.orientation_known,
            "obliquity_deg": if obliq.is_finite() { json!(obliq) } else { Value::Null },
            "intent": header::intent_label(h.intent_code),
            "intent_code": h.intent_code,
            "descrip": h.descrip,
            "warnings": warnings,
            "header_fixes": h.warnings,
            "mrs": self.mrs.as_ref().map(|m| m.to_json(self.shape3)),
        })
    }

    pub fn plans(&self, mode: &str) -> Result<[ViewPlan; 3]> {
        orientation::build_plans(&self.affine, self.shape3, mode, self.orientation_known)
    }

    pub fn center(&self) -> [usize; 3] {
        [self.shape3[0] / 2, self.shape3[1] / 2, self.shape3[2] / 2]
    }

    // ----------------------------------------------------------------- data

    pub fn ensure_data(&mut self) -> Result<()> {
        if self.data.is_some() {
            return Ok(());
        }
        check_slice_size(self.shape3)?;
        let (h, f) = (&self.header, &self.file);
        // Spectroscopy needs every FID; when they cannot be read (too large,
        // truncated), the maps of volume 0 may still show. A spectrum that is
        // too long is refused before reading them.
        let all = match &self.mrs {
            Some(m) if m.points > crate::mrs::MAX_POINTS => Some(Err(too_long(m.points))),
            Some(_) => Some(source::load_all(f, h, self.compressed, self.file_size, self.max_bytes)),
            None => None,
        };
        let (storage, offset, all) = match all {
            Some(Ok((storage, offset))) => (storage, offset, Ok(())),
            other => {
                let why = match other {
                    Some(Err(e)) => e,
                    _ => PreviewError::new("bad_request", "Not a spectroscopy (NIfTI-MRS) file"),
                };
                let (storage, offset) = source::load_volume0(f, h, self.compressed, self.file_size, self.max_bytes)?;
                (storage, offset, Err(why))
            }
        };
        let len = self.n_voxels() * self.header.dtype.itemsize();
        if storage.bytes().len() < offset + len {
            bail!("corrupt_data", "Image data could not be read; the file is truncated.");
        }
        self.data = Some(Data { storage, offset, len, all });
        Ok(())
    }

    /// Bytes read from the file into memory (or mapped), once data are loaded.
    pub fn loaded_bytes(&self) -> Option<usize> {
        self.data.as_ref().map(|d| d.storage.bytes().len())
    }

    /// Spectrum of a voxel of a spectroscopy file.
    pub fn spectrum(&mut self, voxel: [usize; 3]) -> Result<Spectrum> {
        let Some(mrs) = self.mrs.clone() else { bail!("bad_request", "Not a spectroscopy (NIfTI-MRS) file") };
        if mrs.points > crate::mrs::MAX_POINTS {
            return Err(too_long(mrs.points));
        }
        if (0..3).any(|a| voxel[a] >= self.shape3[a]) {
            bail!("bad_request", "Voxel {:?} outside image {:?}", voxel, self.shape3);
        }
        self.ensure_data()?;
        let d = self.data.as_ref().expect("data loaded");
        if let Err(e) = &d.all {
            return Err(e.clone());
        }
        let raw = &d.storage.bytes()[d.offset..];
        Ok(Spectrum::compute(raw, self.header.dtype, self.header.endian, &self.header.shape, voxel, &mrs))
    }

    fn raw(&self) -> &[u8] {
        let d = self.data.as_ref().expect("data loaded");
        &d.storage.bytes()[d.offset..d.offset + d.len]
    }

    /// Complex data are shown as their magnitude (RGB is shown in colour).
    pub fn conversion_note(&self) -> Option<&'static str> {
        match self.header.dtype {
            DType::C64 | DType::C128 => Some("Complex data shown as magnitude."),
            _ => None,
        }
    }

    /// Converted datatypes (RGB luminance, complex magnitude) are display-only.
    fn converted(&self) -> bool {
        matches!(self.header.dtype, DType::Rgb24 | DType::Rgba32 | DType::C64 | DType::C128)
    }

    /// Scaled (physical) value as f32, the precision used for display.
    #[inline(always)]
    pub fn scaled_f32(&self, raw: &[u8], index: usize) -> f32 {
        if self.header.identity_scaling() || self.converted() {
            decode_f32(raw, index, self.header.dtype, self.header.endian)
        } else {
            let v = decode_f64(raw, index, self.header.dtype, self.header.endian);
            (v * self.header.slope + self.header.inter) as f32
        }
    }

    /// Fill `out` with the scaled values of a view slice (rows = screen y).
    /// On a resampled grid, values are interpolated trilinearly, or taken
    /// from the nearest voxel when `nearest` (labels); outside the image they
    /// are NaN (black).
    pub fn slice_values(&self, plan: &ViewPlan, index: usize, nearest: bool, out: &mut Vec<f32>) -> Result<()> {
        let raw = self.raw();
        let e = self.header.endian;
        let identity = self.header.identity_scaling() || self.converted();
        let (slope, inter) = (self.header.slope, self.header.inter);
        // Monomorphised fast paths for the common datatypes.
        macro_rules! typed {
            ($t:ty, $n:expr) => {{
                let get = |i: usize| -> f32 {
                    let mut a = [0u8; $n];
                    a.copy_from_slice(&raw[i * $n..i * $n + $n]);
                    let v = if e == Endian::Little { <$t>::from_le_bytes(a) } else { <$t>::from_be_bytes(a) };
                    if identity { v as f32 } else { (v as f64 * slope + inter) as f32 }
                };
                self.gather(plan, index, nearest, get, out)
            }};
        }
        match self.header.dtype {
            DType::U8 => typed!(u8, 1),
            DType::I8 => typed!(i8, 1),
            DType::I16 => typed!(i16, 2),
            DType::U16 => typed!(u16, 2),
            DType::I32 => typed!(i32, 4),
            DType::U32 => typed!(u32, 4),
            DType::F32 => typed!(f32, 4),
            DType::F64 => typed!(f64, 8),
            _ => self.gather(plan, index, nearest, |i| self.scaled_f32(raw, i), out),
        }
    }

    fn strides(&self) -> [usize; 3] {
        [1, self.shape3[0], self.shape3[0] * self.shape3[1]]
    }

    fn gather<F: Fn(usize) -> f32>(&self, plan: &ViewPlan, index: usize, nearest: bool, get: F, out: &mut Vec<f32>) -> Result<()> {
        let strides = self.strides();
        out.clear();
        out.try_reserve_exact(plan.width * plan.height).map_err(|_| out_of_memory())?;
        let grid = &plan.grid;
        if grid.resampled {
            let at = |v: [usize; 3]| v[0] * strides[0] + v[1] * strides[1] + v[2] * strides[2];
            for row in 0..plan.height {
                for col in 0..plan.width {
                    let v = grid.to_voxel(plan.grid_point(index, col, row));
                    let x = if nearest {
                        grid.nearest(v).map(|ijk| get(at(ijk)))
                    } else {
                        grid.taps(v).map(|t| t.trilinear(strides, |i| get(i) as f64) as f32)
                    };
                    out.push(x.unwrap_or(f32::NAN));
                }
            }
            return Ok(());
        }
        let base = index * strides[plan.slice_axis];
        let (sh, sv) = (strides[plan.h_axis], strides[plan.v_axis]);
        for row in 0..plan.height {
            let v = if plan.v_flip { plan.height - 1 - row } else { row };
            let row_base = base + v * sv;
            if plan.h_flip {
                for col in (0..plan.width).rev() {
                    out.push(get(row_base + col * sh));
                }
            } else {
                for col in 0..plan.width {
                    out.push(get(row_base + col * sh));
                }
            }
        }
        Ok(())
    }

    pub fn is_rgb(&self) -> bool {
        matches!(self.header.dtype, DType::Rgb24 | DType::Rgba32)
    }

    /// RGB8 pixels of a view slice of an RGB image (interpolated per channel
    /// on a resampled grid, black outside the image).
    pub fn slice_rgb(&self, plan: &ViewPlan, index: usize, out: &mut Vec<u8>) -> Result<()> {
        let raw = self.raw();
        let n = self.header.dtype.itemsize();
        let strides = self.strides();
        let grid = &plan.grid;
        out.clear();
        out.try_reserve_exact(plan.width * plan.height * 3).map_err(|_| out_of_memory())?;
        for row in 0..plan.height {
            for col in 0..plan.width {
                let g = plan.grid_point(index, col, row);
                if !grid.resampled {
                    let o = (g[0] * strides[0] + g[1] * strides[1] + g[2] * strides[2]) * n;
                    out.extend_from_slice(&raw[o..o + 3]);
                } else if let Some(t) = grid.taps(grid.to_voxel(g)) {
                    for ch in 0..3 {
                        let c = t.trilinear(strides, |i| raw[i * n + ch] as f64);
                        out.push((c + 0.5).floor().clamp(0.0, 255.0) as u8);
                    }
                } else {
                    out.extend_from_slice(&[0, 0, 0]);
                }
            }
        }
        Ok(())
    }

    /// The R, G, B bytes of a voxel of an RGB image.
    pub fn rgb_at(&mut self, ijk: [usize; 3]) -> Result<Option<[u8; 3]>> {
        self.ensure_data()?;
        if !self.is_rgb() || (0..3).any(|a| ijk[a] >= self.shape3[a]) {
            return Ok(None);
        }
        let s = self.strides();
        let o = (ijk[0] * s[0] + ijk[1] * s[1] + ijk[2] * s[2]) * self.header.dtype.itemsize();
        let raw = self.raw();
        Ok(Some([raw[o], raw[o + 1], raw[o + 2]]))
    }

    /// Approximate numeric value for callers using the grayscale pipeline.
    /// Readouts and categorical operations must use `exact_value_at`.
    pub fn value_at(&mut self, ijk: [i64; 3]) -> Result<Option<f64>> {
        Ok(self.exact_value_at(ijk)?.map(VoxelValue::as_f64))
    }

    pub fn exact_value_at(&mut self, ijk: [i64; 3]) -> Result<Option<VoxelValue>> {
        self.ensure_data()?;
        let mut idx = 0usize;
        let strides = [1usize, self.shape3[0], self.shape3[0] * self.shape3[1]];
        for a in 0..3 {
            if ijk[a] < 0 || ijk[a] as usize >= self.shape3[a] {
                bail!("bad_request", "Voxel {:?} outside image {:?}", ijk, self.shape3);
            }
            idx += ijk[a] as usize * strides[a];
        }
        let v = self.exact_value(self.raw(), idx);
        Ok(if v.is_finite() { Some(v) } else { None })
    }

    /// Stored scalars retain their integer type, including all 64-bit values.
    fn stored_value(&self, raw: &[u8], i: usize) -> VoxelValue {
        let e = self.header.endian;
        let integer = match self.header.dtype {
            DType::I64 => Some(if e == Endian::Little {
                i64::from_le_bytes(raw[i * 8..i * 8 + 8].try_into().unwrap())
            } else {
                i64::from_be_bytes(raw[i * 8..i * 8 + 8].try_into().unwrap())
            } as i128),
            DType::U64 => Some(if e == Endian::Little {
                u64::from_le_bytes(raw[i * 8..i * 8 + 8].try_into().unwrap())
            } else {
                u64::from_be_bytes(raw[i * 8..i * 8 + 8].try_into().unwrap())
            } as i128),
            DType::U8 | DType::I8 | DType::I16 | DType::U16 | DType::I32 | DType::U32 => {
                Some(decode_f64(raw, i, self.header.dtype, e) as i128)
            }
            _ => None,
        };
        integer.map(VoxelValue::Integer).unwrap_or_else(|| VoxelValue::Float(decode_f64(raw, i, self.header.dtype, e)))
    }

    /// Unscaled scalars retain their stored precision; converted/scaled data
    /// use the float32 physical values of the display pipeline.
    #[inline(always)]
    fn exact_value(&self, raw: &[u8], i: usize) -> VoxelValue {
        if self.header.identity_scaling() && !self.converted() {
            self.stored_value(raw, i)
        } else {
            VoxelValue::Float(self.scaled_f32(raw, i) as f64)
        }
    }

    /// Exact nearest-voxel label values, streamed directly to the renderer.
    /// No expanded scalar buffer is allocated for a large slice.
    pub fn slice_labels<'a>(&'a self, plan: &'a ViewPlan, index: usize) -> impl ExactSizeIterator<Item = VoxelValue> + 'a {
        let raw = self.raw();
        let strides = self.strides();
        let grid = &plan.grid;
        (0..plan.width * plan.height).map(move |i| {
            let g = plan.grid_point(index, i % plan.width, i / plan.width);
            let v = if grid.resampled { grid.nearest(grid.to_voxel(g)) } else { Some(g) };
            v.map_or(VoxelValue::Float(f64::NAN), |v| self.exact_value(raw, v[0] * strides[0] + v[1] * strides[1] + v[2] * strides[2]))
        })
    }

    /// Volume of one voxel in mm³ (|det| of the affine; spatial units taken
    /// as mm when the header does not give them).
    pub fn voxel_volume_mm3(&self) -> f64 {
        let a = &self.affine;
        let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
        let mm = self.mm_per_unit();
        det.abs() * mm * mm * mm
    }

    /// Millimetres per spatial unit of the header (mm when it gives none).
    pub fn mm_per_unit(&self) -> f64 {
        match self.header.space_units() {
            "meter" => 1e3,
            "micron" => 1e-3,
            _ => 1.0,
        }
    }

    /// Number of voxels of volume 0 whose displayed value equals `value`
    /// (one pass over the data, cached per value).
    pub fn count_equal(&mut self, value: impl Into<VoxelValue>) -> Result<u64> {
        self.ensure_data()?;
        let value = value.into();
        let key = value.key();
        if let Some(&n) = self.counts.get(&key) {
            return Ok(n);
        }
        let raw = self.raw();
        let e = self.header.endian;
        let identity = self.header.identity_scaling() && !self.converted();
        macro_rules! typed {
            ($t:ty, $n:expr, $target:expr) => {{
                let target: $t = $target;
                raw.chunks_exact($n).filter(|c| {
                    let a: [u8; $n] = (*c).try_into().unwrap();
                    let v = if e == Endian::Little { <$t>::from_le_bytes(a) } else { <$t>::from_be_bytes(a) };
                    v == target
                }).count() as u64
            }};
        }
        macro_rules! integer {
            ($t:ty, $n:expr) => {{
                if key.0 == 0 {
                    match <$t>::try_from(key.1 as i128) {
                        Ok(target) => typed!($t, $n, target),
                        Err(_) => 0,
                    }
                } else { 0 }
            }};
        }
        let target = value.as_f64();
        let n = match self.header.dtype {
            DType::U8 if identity => integer!(u8, 1),
            DType::I8 if identity => integer!(i8, 1),
            DType::I16 if identity => integer!(i16, 2),
            DType::U16 if identity => integer!(u16, 2),
            DType::I32 if identity => integer!(i32, 4),
            DType::U32 if identity => integer!(u32, 4),
            DType::I64 if identity => integer!(i64, 8),
            DType::U64 if identity => integer!(u64, 8),
            DType::F64 if identity && VoxelValue::Float(target) == value => typed!(f64, 8, target),
            _ if VoxelValue::Float(target as f32 as f64) != value => 0,
            DType::F32 if identity => typed!(f32, 4, target as f32),
            _ => (0..self.n_voxels()).filter(|&i| self.scaled_f32(raw, i) == target as f32).count() as u64,
        };
        self.counts.insert(key, n);
        Ok(n)
    }

    pub fn stats(&mut self) -> Result<&Stats> {
        if self.stats.is_none() {
            self.ensure_data()?;
            let s = self.compute_stats()?;
            self.stats = Some(s);
        }
        Ok(self.stats.as_ref().unwrap())
    }

    pub fn stats_if_loaded(&self) -> Option<&Stats> {
        self.stats.as_ref()
    }

    fn compute_stats(&self) -> Result<Stats> {
        let raw = self.raw();
        let n = self.n_voxels();
        let dtype = self.header.dtype;
        let e = self.header.endian;
        let step = n.div_ceil(MAX_SAMPLES).max(1);
        let sample_scaled = self.sample(raw, n, step)?;

        // The window, the label detection and the NaN count are independent:
        // run the last two on helper threads.
        let float_like = dtype.is_float() || self.converted();
        let identity = self.header.identity_scaling() && !self.converted();
        let detect = || labels::detect(
            |i| self.stored_value(raw, i), self.shape3,
            (0..n).step_by(step).map(|i| self.stored_value(raw, i)), identity, self.header.intent_code,
        );
        let count_nan = || {
            if !float_like {
                (0, true)
            } else if n <= 64 << 20 {
                (count_non_finite(raw, n, dtype, e), true)
            } else {
                let bad = sample_scaled.iter().filter(|v| !v.is_finite()).count() as f64;
                ((bad * n as f64 / sample_scaled.len().max(1) as f64).round() as u64, false)
            }
        };
        // A thread that cannot start (memory or thread limits) runs here instead.
        let (window, label_info, (non_finite, exact)) = std::thread::scope(|scope| {
            let labels = std::thread::Builder::new().spawn_scoped(scope, detect);
            let nan = std::thread::Builder::new().spawn_scoped(scope, count_nan);
            let window = window::compute(&sample_scaled);
            let label_info = match labels {
                Ok(t) => t.join().unwrap_or(labels::NOT_LABELS),
                Err(_) => detect(),
            };
            let nan = match nan {
                Ok(t) => t.join().unwrap_or((0, false)),
                Err(_) => count_nan(),
            };
            (window, label_info, nan)
        });
        let window = window?;

        let mut min: Option<VoxelValue> = None;
        let mut max: Option<VoxelValue> = None;
        for (j, &scaled) in sample_scaled.iter().enumerate() {
            let v = if identity { self.stored_value(raw, j * step) } else { VoxelValue::Float(scaled as f64) };
            if v.is_finite() {
                if min.is_none_or(|m| v.less_than(m)) { min = Some(v); }
                if max.is_none_or(|m| m.less_than(v)) { max = Some(v); }
            }
        }
        let mut notes: Vec<String> = match (&self.mrs, self.conversion_note()) {
            (Some(_), _) if n == 1 => Vec::new(),
            (Some(_), _) => vec!["Maps show the magnitude of the first time point.".to_string()],
            (None, note) => note.map(|s| vec![s.to_string()]).unwrap_or_default(),
        };
        match window.status {
            _ if self.mrs.is_some() && n == 1 => {} // a single voxel is "constant"
            window::Status::NoFinite => notes.push("Volume contains no finite values.".into()),
            window::Status::Constant if min != max => {
                if !label_info.is_labels {
                    notes.push("Values differ below grayscale display precision.".into());
                }
            }
            window::Status::Constant => {
                let value = min.map(VoxelValue::to_json).map(|v| match v {
                    Value::String(s) => s,
                    _ => fmt_g(window.low),
                }).unwrap_or_else(|| fmt_g(window.low));
                notes.push(format!("Constant volume (value {value})."));
            }
            window::Status::Ok => {}
        }
        if non_finite > 0 {
            notes.push(format!("{}{} NaN/Inf voxels (shown black).", if exact { "" } else { "≈" }, non_finite));
        }
        let json_value = |v: VoxelValue| if label_info.is_labels { v.to_label_json() } else { v.to_json() };
        let (min, max) = (min.map(json_value), max.map(json_value));
        Ok(Stats { window, labels: label_info, non_finite, non_finite_exact: exact, min, max, notes })
    }

    /// Strided sample of physical values (f32, for the window).
    #[allow(clippy::int_plus_one)] // `off + $n <= len` reads as "one more item fits"
    fn sample(&self, raw: &[u8], n: usize, step: usize) -> Result<Vec<f32>> {
        let count = n.div_ceil(step);
        let mut scaled: Vec<f32> = Vec::new();
        scaled.try_reserve_exact(count).map_err(|_| out_of_memory())?;
        let identity = self.header.identity_scaling() || self.converted();
        let (slope, inter) = (self.header.slope, self.header.inter);
        let e = self.header.endian;
        macro_rules! typed {
            ($t:ty, $n:expr) => {{
                let mut off = 0usize;
                let stride = step * $n;
                while off + $n <= raw.len() {
                    let mut a = [0u8; $n];
                    a.copy_from_slice(&raw[off..off + $n]);
                    let v = if e == Endian::Little { <$t>::from_le_bytes(a) } else { <$t>::from_be_bytes(a) };
                    scaled.push(if identity { v as f32 } else { (v as f64 * slope + inter) as f32 });
                    off += stride;
                }
            }};
        }
        match self.header.dtype {
            DType::U8 => typed!(u8, 1),
            DType::I8 => typed!(i8, 1),
            DType::I16 => typed!(i16, 2),
            DType::U16 => typed!(u16, 2),
            DType::I32 => typed!(i32, 4),
            DType::U32 => typed!(u32, 4),
            DType::F32 => typed!(f32, 4),
            _ => {
                for i in (0..n).step_by(step) {
                    scaled.push(self.scaled_f32(raw, i));
                }
            }
        }
        Ok(scaled)
    }
}

fn count_non_finite(raw: &[u8], n: usize, dtype: DType, e: Endian) -> u64 {
    match dtype {
        DType::F32 => raw
            .chunks_exact(4)
            .filter(|c| {
                let a = [c[0], c[1], c[2], c[3]];
                !(if e == Endian::Little { f32::from_le_bytes(a) } else { f32::from_be_bytes(a) }).is_finite()
            })
            .count() as u64,
        DType::F64 => raw
            .chunks_exact(8)
            .filter(|c| {
                let a: [u8; 8] = (*c).try_into().unwrap();
                !(if e == Endian::Little { f64::from_le_bytes(a) } else { f64::from_be_bytes(a) }).is_finite()
            })
            .count() as u64,
        _ => (0..n).filter(|&i| !decode_f32(raw, i, dtype, e).is_finite()).count() as u64,
    }
}

/// Python's `format(x, "g")`.
pub fn fmt_g(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    let exp = x.abs().log10().floor() as i32;
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        let s = format!("{:.*}", decimals, x);
        let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
        s
    } else {
        let s = format!("{:.5e}", x);
        let (m, ex) = s.split_once('e').unwrap();
        let m = if m.contains('.') { m.trim_end_matches('0').trim_end_matches('.') } else { m };
        let ex: i32 = ex.parse().unwrap();
        format!("{m}e{}{:02}", if ex < 0 { '-' } else { '+' }, ex.abs())
    }
}

macro_rules! read_le_be {
    ($raw:expr, $off:expr, $t:ty, $n:expr, $e:expr) => {{
        let mut a = [0u8; $n];
        a.copy_from_slice(&$raw[$off..$off + $n]);
        if $e == Endian::Little {
            <$t>::from_le_bytes(a)
        } else {
            <$t>::from_be_bytes(a)
        }
    }};
}

/// Stored value of voxel `i` as f32 (display precision). RGB → luminance,
/// complex → magnitude.
#[inline(always)]
fn out_of_memory() -> PreviewError {
    PreviewError::new("too_large", "Not enough memory to preview this image.")
}

fn too_long(points: usize) -> PreviewError {
    PreviewError::new("too_large", format!("Spectrum too long to preview ({points} points, limit {}).", crate::mrs::MAX_POINTS))
}

pub fn decode_f32(raw: &[u8], i: usize, dtype: DType, e: Endian) -> f32 {
    let o = i * dtype.itemsize();
    match dtype {
        DType::U8 => raw[o] as f32,
        DType::I8 => raw[o] as i8 as f32,
        DType::I16 => read_le_be!(raw, o, i16, 2, e) as f32,
        DType::U16 => read_le_be!(raw, o, u16, 2, e) as f32,
        DType::I32 => read_le_be!(raw, o, i32, 4, e) as f32,
        DType::U32 => read_le_be!(raw, o, u32, 4, e) as f32,
        DType::I64 => read_le_be!(raw, o, i64, 8, e) as f32,
        DType::U64 => read_le_be!(raw, o, u64, 8, e) as f32,
        DType::F32 => read_le_be!(raw, o, f32, 4, e),
        DType::F64 => read_le_be!(raw, o, f64, 8, e) as f32,
        DType::C64 => {
            let re = read_le_be!(raw, o, f32, 4, e);
            let im = read_le_be!(raw, o + 4, f32, 4, e);
            re.hypot(im)
        }
        DType::C128 => {
            let re = read_le_be!(raw, o, f64, 8, e);
            let im = read_le_be!(raw, o + 8, f64, 8, e);
            re.hypot(im) as f32
        }
        DType::Rgb24 | DType::Rgba32 => {
            0.299f32 * raw[o] as f32 + 0.587f32 * raw[o + 1] as f32 + 0.114f32 * raw[o + 2] as f32
        }
    }
}

/// Stored value of voxel `i` as f64 (used for label detection, like numpy).
#[inline(always)]
pub fn decode_f64(raw: &[u8], i: usize, dtype: DType, e: Endian) -> f64 {
    let o = i * dtype.itemsize();
    match dtype {
        DType::I32 => read_le_be!(raw, o, i32, 4, e) as f64,
        DType::U32 => read_le_be!(raw, o, u32, 4, e) as f64,
        DType::I64 => read_le_be!(raw, o, i64, 8, e) as f64,
        DType::U64 => read_le_be!(raw, o, u64, 8, e) as f64,
        DType::F64 => read_le_be!(raw, o, f64, 8, e),
        DType::C128 => {
            let re = read_le_be!(raw, o, f64, 8, e);
            let im = read_le_be!(raw, o + 8, f64, 8, e);
            re.hypot(im) as f32 as f64
        }
        _ => decode_f32(raw, i, dtype, e) as f64,
    }
}

impl From<std::num::TryFromIntError> for PreviewError {
    fn from(_: std::num::TryFromIntError) -> Self {
        PreviewError::new("too_large", "Value out of range")
    }
}
