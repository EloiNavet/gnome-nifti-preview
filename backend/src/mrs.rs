//! NIfTI-MRS (MR spectroscopy): detection, metadata from the JSON header
//! extension (code 44), and the spectrum of a voxel.
//!
//! The data are complex time-domain signals (FIDs) along dimension 4, and
//! dimensions 5 to 7 hold coils, averages, dynamics... (`dim_5`..`dim_7` in
//! the JSON). A spectrum combines all these transients: each is phased by its
//! first point and weighted by its amplitude, the usual combination of
//! uncombined coils. Then comes the Fourier transform. The chemical-shift axis
//! is `ppm = shift - f / SF`, using a finite SpecFreqChemShift when given, otherwise 4.65 ppm for 1H
//! and 2H (water) and 0 for other nuclei, so that high shifts come first, on the left, as usual.
//! On the NIfTI-MRS example data this puts NAA at 2.0 ppm and creatine at
//! 3.0 ppm. See docs/DISPLAY_CONVENTIONS.md.

use serde_json::{json, Map, Value};

use crate::header::{clean_text, DType, Endian, Extension, Header};

pub const EXTENSION_CODE: i32 = 44;
/// Longest spectrum computed (points along dimension 4).
pub const MAX_POINTS: usize = 1 << 16;
/// Default dimension tags when the JSON gives none (NIfTI-MRS standard).
const DEFAULT_TAGS: [&str; 3] = ["DIM_COIL", "DIM_DYN", "DIM_INDIRECT_0"];
/// Dimensions whose transients are repeats of one signal, averaged by the
/// spectrum. The others (DIM_EDIT, DIM_ISIS, DIM_PHASE_CYCLE, DIM_INDIRECT_*,
/// DIM_USER_*) encode different signals: averaging them would mislead.
pub const AVERAGED_TAGS: [&str; 3] = ["DIM_COIL", "DIM_DYN", "DIM_MEAS"];

#[derive(Debug, Clone, PartialEq)]
pub struct Mrs {
    /// `intent_name`, e.g. "mrs_v0_3" (empty when detected by the extension).
    pub format: String,
    pub nucleus: Option<String>,
    pub spectrometer_mhz: Option<f64>,
    pub dwell_s: Option<f64>,
    pub reference_ppm: Option<f64>,
    pub points: usize,
    /// Dimensions 5 to 7: tag and size.
    pub dims: Vec<(String, usize)>,
    pub echo_time_s: Option<f64>,
    pub repetition_time_s: Option<f64>,
}

fn positive(x: Option<f64>) -> Option<f64> {
    x.filter(|v| v.is_finite() && *v > 0.0)
}

/// A number, or the first number of an array.
fn first_number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Array(a) => a.first()?.as_f64(),
        x => x.as_f64(),
    }
}

fn first_string(v: Option<&Value>) -> Option<String> {
    let s = match v? {
        Value::Array(a) => a.first()?.as_str()?,
        x => x.as_str()?,
    };
    let s = clean_text(s);
    (!s.is_empty() && s.chars().count() <= 32).then_some(s)
}

/// The JSON object of the NIfTI-MRS extension (padding NULs removed).
fn extension_json(extensions: &[Extension]) -> Option<Map<String, Value>> {
    let ext = extensions.iter().find(|e| e.code == EXTENSION_CODE)?;
    let end = ext.content.iter().rposition(|&b| b != 0 && !b.is_ascii_whitespace())? + 1;
    match serde_json::from_slice(&ext.content[..end]) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// Larmor frequency per tesla (MHz/T) of the usual nuclei.
pub fn gyromagnetic_mhz_per_t(nucleus: &str) -> Option<f64> {
    Some(match nucleus.to_ascii_uppercase().as_str() {
        "1H" => 42.577478,
        "2H" => 6.536,
        "3HE" => 32.434,
        "7LI" => 16.546,
        "13C" => 10.7084,
        "15N" => 4.316,
        "17O" => 5.772,
        "19F" => 40.078,
        "23NA" => 11.262,
        "31P" => 17.235,
        "129XE" => 11.777,
        _ => return None,
    })
}

/// Shift of the centre of the spectrum: water for 1H and 2H, 0 otherwise.
fn centre_ppm(nucleus: Option<&str>) -> f64 {
    match nucleus {
        Some("1H") | Some("2H") => 4.65,
        _ => 0.0,
    }
}

impl Mrs {
    /// Spectroscopy data: a complex image with a spectral dimension, marked by
    /// `intent_name` "mrs_v…" or by a NIfTI-MRS extension.
    pub fn detect(h: &Header, extensions: &[Extension]) -> Option<Mrs> {
        if !matches!(h.dtype, DType::C64 | DType::C128) || h.shape.len() < 4 {
            return None;
        }
        let json = extension_json(extensions).unwrap_or_default();
        if !h.intent_name.starts_with("mrs_v") && !json.contains_key("SpectrometerFrequency") {
            return None;
        }
        let per_second = match h.time_units() {
            "msec" => 1e-3,
            "usec" => 1e-6,
            _ => 1.0, // seconds, or unset (the standard requires seconds)
        };
        let dims = (4..h.shape.len())
            .map(|i| {
                let tag = first_string(json.get(&format!("dim_{}", i + 1))).unwrap_or_else(|| DEFAULT_TAGS[i - 4].to_string());
                (tag, h.shape[i])
            })
            .collect();
        Some(Mrs {
            format: if h.intent_name.starts_with("mrs_v") { h.intent_name.clone() } else { String::new() },
            nucleus: first_string(json.get("ResonantNucleus")),
            spectrometer_mhz: positive(first_number(json.get("SpectrometerFrequency"))),
            dwell_s: positive(Some(h.pixdim[4] * per_second)),
            reference_ppm: first_number(json.get("SpecFreqChemShift")).filter(|v| v.is_finite()),
            points: h.shape[3],
            dims,
            echo_time_s: first_number(json.get("EchoTime")).filter(|v| v.is_finite() && *v >= 0.0),
            repetition_time_s: positive(first_number(json.get("RepetitionTime"))),
        })
    }

    pub fn transients(&self) -> usize {
        self.dims.iter().map(|d| d.1).try_fold(1usize, |a, d| a.checked_mul(d)).unwrap_or(usize::MAX)
    }

    /// Metadata only needs the count, before the data-size limit is checked:
    /// never materialize a list sized from unchecked higher dimensions here.
    fn combined_count(&self) -> usize {
        self.dims
            .iter()
            .filter(|(tag, _)| AVERAGED_TAGS.contains(&tag.as_str()))
            .fold(1usize, |count, (_, size)| count.saturating_mul(*size))
    }

    /// Dimensions the spectrum does not average (edit conditions, ISIS steps,
    /// phase cycles, indirect or user dimensions): only their index 0 is used.
    pub fn first_index_only(&self) -> Vec<(String, usize)> {
        self.dims.iter().filter(|(t, n)| *n > 1 && !AVERAGED_TAGS.contains(&t.as_str())).cloned().collect()
    }

    /// Flat indices (over dimensions 5 to 7) of the transients averaged: every
    /// coil, dynamic and measurement, index 0 of the other dimensions.
    pub fn combined_transients(&self) -> Vec<usize> {
        let mut out = vec![0usize];
        let mut stride = 1usize;
        for (tag, size) in &self.dims {
            if AVERAGED_TAGS.contains(&tag.as_str()) {
                out = out.iter().flat_map(|&j| (0..*size).map(move |i| j + i * stride)).collect();
            }
            stride = stride.saturating_mul(*size);
        }
        out.sort_unstable();
        out
    }

    pub fn to_json(&self, shape3: [usize; 3]) -> Value {
        let field = match (&self.nucleus, self.spectrometer_mhz) {
            (Some(n), Some(sf)) => gyromagnetic_mhz_per_t(n).map(|g| sf / g),
            _ => None,
        };
        json!({
            "format": self.format,
            "nucleus": self.nucleus,
            "spectrometer_mhz": self.spectrometer_mhz,
            "field_t": field,
            "points": self.points,
            "dwell_s": self.dwell_s,
            "bandwidth_hz": self.dwell_s.map(|d| 1.0 / d),
            "dims": self.dims.iter().map(|(t, n)| json!({"tag": t, "size": n})).collect::<Vec<_>>(),
            "transients": self.transients(),
            "combined": self.combined_count(),
            "first_index_only": self.first_index_only().iter().map(|(t, n)| json!({"tag": t, "size": n})).collect::<Vec<_>>(),
            "single_voxel": shape3 == [1, 1, 1],
            "echo_time_s": self.echo_time_s,
            "repetition_time_s": self.repetition_time_s,
        })
    }
}

// ------------------------------------------------------------------ FFT

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct C {
    pub re: f64,
    pub im: f64,
}

impl C {
    pub fn new(re: f64, im: f64) -> C {
        C { re, im }
    }
    fn mul(self, o: C) -> C {
        C::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
    fn add(self, o: C) -> C {
        C::new(self.re + o.re, self.im + o.im)
    }
    fn sub(self, o: C) -> C {
        C::new(self.re - o.re, self.im - o.im)
    }
    fn conj(self) -> C {
        C::new(self.re, -self.im)
    }
    pub fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }
    fn scale(self, s: f64) -> C {
        C::new(self.re * s, self.im * s)
    }
    fn expi(angle: f64) -> C {
        C::new(angle.cos(), angle.sin())
    }
}

/// In-place FFT of a power-of-two length (`sign` -1: forward, +1: inverse,
/// unnormalised).
fn fft_pow2(a: &mut [C], sign: f64) {
    let n = a.len();
    if n <= 1 {
        return;
    }
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            a.swap(i, j);
        }
    }
    // Twiddles computed directly (no accumulated rounding).
    let twiddle: Vec<C> = (0..n / 2).map(|k| C::expi(sign * 2.0 * std::f64::consts::PI * k as f64 / n as f64)).collect();
    let mut len = 2;
    while len <= n {
        let stride = n / len;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let u = a[start + k];
                let v = a[start + k + len / 2].mul(twiddle[k * stride]);
                a[start + k] = u.add(v);
                a[start + k + len / 2] = u.sub(v);
            }
        }
        len <<= 1;
    }
}

/// Forward DFT of any length: radix 2, or Bluestein's algorithm.
pub fn fft(x: &[C]) -> Vec<C> {
    let n = x.len();
    if n.is_power_of_two() || n <= 1 {
        let mut a = x.to_vec();
        fft_pow2(&mut a, -1.0);
        return a;
    }
    let m = (2 * n - 1).next_power_of_two();
    // w_k = exp(-i pi k^2 / n), with k^2 reduced modulo 2n for accuracy.
    let w: Vec<C> = (0..n)
        .map(|k| {
            let k2 = (k as u64 * k as u64) % (2 * n as u64);
            C::expi(-std::f64::consts::PI * k2 as f64 / n as f64)
        })
        .collect();
    let mut a = vec![C::default(); m];
    let mut b = vec![C::default(); m];
    for k in 0..n {
        a[k] = x[k].mul(w[k]);
        b[k] = w[k].conj();
        if k > 0 {
            b[m - k] = w[k].conj();
        }
    }
    fft_pow2(&mut a, -1.0);
    fft_pow2(&mut b, -1.0);
    for k in 0..m {
        a[k] = a[k].mul(b[k]);
    }
    fft_pow2(&mut a, 1.0);
    (0..n).map(|k| a[k].scale(1.0 / m as f64).mul(w[k])).collect()
}

// ------------------------------------------------------------- spectrum

#[derive(Debug, Clone)]
pub struct Spectrum {
    pub real: Vec<f32>,
    pub magnitude: Vec<f32>,
    /// "ppm", "hz" (no spectrometer frequency) or "points" (no dwell time).
    pub unit: &'static str,
    /// Axis values of the first and last points (linear in between).
    pub first: f64,
    pub last: f64,
    /// Range shown first (min, max), None for the whole spectrum.
    pub default_range: Option<[f64; 2]>,
    /// "real" for 1H, "magnitude" otherwise (no first-order phasing).
    pub default_part: &'static str,
    pub transients: usize,
}

/// Complex sample `i` of the data (non-finite values read as 0).
#[inline(always)]
fn sample(raw: &[u8], i: usize, dtype: DType, e: Endian) -> C {
    let (re, im) = match dtype {
        DType::C64 => {
            let o = i * 8;
            let f = |o: usize| {
                let a = [raw[o], raw[o + 1], raw[o + 2], raw[o + 3]];
                (if e == Endian::Little { f32::from_le_bytes(a) } else { f32::from_be_bytes(a) }) as f64
            };
            (f(o), f(o + 4))
        }
        _ => {
            let o = i * 16;
            let f = |o: usize| {
                let a: [u8; 8] = raw[o..o + 8].try_into().unwrap();
                if e == Endian::Little {
                    f64::from_le_bytes(a)
                } else {
                    f64::from_be_bytes(a)
                }
            };
            (f(o), f(o + 8))
        }
    };
    if re.is_finite() && im.is_finite() {
        C::new(re, im)
    } else {
        C::default()
    }
}

/// Shortest decimal of a float32 as a JSON number (half the size of its
/// float64 widening).
fn short(x: f32) -> Value {
    if !x.is_finite() {
        return Value::Null;
    }
    format!("{x}").parse::<f64>().map(Value::from).unwrap_or(Value::Null)
}

impl Spectrum {
    /// Spectrum of `voxel`. `raw` holds all the data, `shape` the image shape
    /// (at least four dimensions).
    pub fn compute(raw: &[u8], dtype: DType, endian: Endian, shape: &[usize], voxel: [usize; 3], mrs: &Mrs) -> Spectrum {
        let n_vox = shape[0] * shape[1] * shape[2];
        let n = shape[3];
        let combined = mrs.combined_transients();
        let k = combined.len();
        let s = voxel[0] + shape[0] * (voxel[1] + shape[1] * voxel[2]);
        let at = |t: usize, j: usize| sample(raw, s + n_vox * (t + n * j), dtype, endian);

        // Each transient phased by its first point, weighted by its amplitude.
        let mut fid = vec![C::default(); n];
        let mut weight = 0.0;
        for &j in &combined {
            let f0 = at(0, j);
            weight += f0.abs();
            let w = f0.conj();
            for (t, acc) in fid.iter_mut().enumerate() {
                *acc = acc.add(at(t, j).mul(w));
            }
        }
        if weight > 0.0 && weight.is_finite() {
            fid.iter_mut().for_each(|c| *c = c.scale(1.0 / weight));
        } else {
            fid = vec![C::default(); n];
            for &j in &combined {
                for (t, acc) in fid.iter_mut().enumerate() {
                    *acc = acc.add(at(t, j));
                }
            }
            fid.iter_mut().for_each(|c| *c = c.scale(1.0 / k as f64));
        }
        // The first point counts half (no baseline offset).
        fid[0] = fid[0].scale(0.5);

        let x = fft(&fid);
        let half = n / 2;
        let mut spec: Vec<C> = (0..n).map(|i| x[(i + n - half) % n]).collect();

        // Axis: the frequency of point i is (i - n/2) / (n dwell) Hz.
        let nucleus = mrs.nucleus.as_deref();
        let (unit, first, last) = match (mrs.dwell_s, mrs.spectrometer_mhz) {
            (Some(d), Some(sf)) => {
                let c = mrs.reference_ppm.unwrap_or_else(|| centre_ppm(nucleus));
                let hz = |i: usize| (i as f64 - half as f64) / (n as f64 * d);
                ("ppm", c - hz(0) / sf, c - hz(n - 1) / sf)
            }
            (Some(d), None) => {
                let hz = |i: usize| (i as f64 - half as f64) / (n as f64 * d);
                ("hz", -hz(0), -hz(n - 1))
            }
            _ => ("points", 0.0, (n.max(1) - 1) as f64),
        };
        let proton = unit == "ppm" && nucleus == Some("1H");
        let default_range = if proton { Some([0.2, 4.2]) } else { None };

        // Zero-order phase: the shown range integrates to a positive real.
        let axis = |i: usize| if n > 1 { first + (last - first) * i as f64 / (n - 1) as f64 } else { first };
        let mut sum = C::default();
        for (i, v) in spec.iter().enumerate() {
            if default_range.is_none_or(|[lo, hi]| (lo..=hi).contains(&axis(i))) {
                sum = sum.add(*v);
            }
        }
        if sum.abs() > 0.0 {
            let rot = C::expi(-sum.im.atan2(sum.re));
            spec.iter_mut().for_each(|v| *v = v.mul(rot));
        }
        Spectrum {
            real: spec.iter().map(|v| v.re as f32).collect(),
            magnitude: spec.iter().map(|v| v.abs() as f32).collect(),
            unit,
            first,
            last,
            default_range,
            default_part: if proton { "real" } else { "magnitude" },
            transients: k,
        }
    }

    pub fn to_json(&self, voxel: [usize; 3]) -> Value {
        json!({
            "voxel": voxel,
            "unit": self.unit,
            "first": self.first,
            "last": self.last,
            "points": self.real.len(),
            "real": self.real.iter().map(|&x| short(x)).collect::<Vec<_>>(),
            "magnitude": self.magnitude.iter().map(|&x| short(x)).collect::<Vec<_>>(),
            "default_range": self.default_range,
            "default_part": self.default_part,
            "transients": self.transients,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dft(x: &[C]) -> Vec<C> {
        let n = x.len();
        (0..n)
            .map(|k| {
                let mut s = C::default();
                for (t, v) in x.iter().enumerate() {
                    let a = -2.0 * std::f64::consts::PI * ((k * t) % n) as f64 / n as f64;
                    s = s.add(v.mul(C::expi(a)));
                }
                s
            })
            .collect()
    }

    #[test]
    fn fft_matches_the_dft() {
        for n in [1, 2, 3, 7, 12, 64, 100, 257, 1024, 4126] {
            let x: Vec<C> = (0..n).map(|i| C::new((i as f64 * 0.37).sin(), (i as f64 * 1.3).cos() - 0.2)).collect();
            let (a, b) = (fft(&x), dft(&x));
            let scale = b.iter().map(|c| c.abs()).fold(1.0, f64::max);
            let err = a.iter().zip(&b).map(|(p, q)| p.sub(*q).abs()).fold(0.0, f64::max);
            assert!(err < 1e-9 * scale * n as f64, "n={n}: error {err}");
        }
    }
}
