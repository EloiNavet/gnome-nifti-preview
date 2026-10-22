//! Percentile intensity windowing (0.5–99.5 %) over a strided sample.
//!
//! The window is computed over clearly non-zero voxels when they are a
//! substantial part of the image, so that a large zero background does not
//! drag the lower percentile to zero. Percentiles reproduce numpy's default
//! "linear" method so that results match the nibabel reference.

pub const LOW_PERCENTILE: f64 = 0.5;
pub const HIGH_PERCENTILE: f64 = 99.5;
const NON_ZERO_FLOOR: f64 = 1e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Constant,
    NoFinite,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Constant => "constant",
            Status::NoFinite => "no_finite",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub low: f64,
    pub high: f64,
    pub status: Status,
}

#[allow(clippy::neg_cmp_op_on_partial_ord)] // `!(low < high)` is also true for NaN, on purpose
pub fn compute(sample: &[f32]) -> crate::error::Result<Window> {
    // Count first, then build only the subset that is used.
    let (mut n_finite, mut n_fg, mut n_nz) = (0usize, 0usize, 0usize);
    let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
    for &v in sample {
        if v.is_finite() {
            n_finite += 1;
            min = min.min(v);
            max = max.max(v);
            if v != 0.0 {
                n_nz += 1;
                if (v as f64).abs() > NON_ZERO_FLOOR {
                    n_fg += 1;
                }
            }
        }
    }
    if n_finite == 0 {
        return Ok(Window { low: 0.0, high: 1.0, status: Status::NoFinite });
    }
    let minimum_subset = (n_finite / 20).max(64);
    let keep: fn(f32) -> bool = if n_fg >= minimum_subset {
        |v| v.is_finite() && (v as f64).abs() > NON_ZERO_FLOOR
    } else if n_nz >= minimum_subset {
        |v| v.is_finite() && v != 0.0
    } else {
        |v| v.is_finite()
    };
    // Selection on f32 gives the same order statistics as numpy's float64 copy;
    // interpolation is done in f64.
    let mut source: Vec<f32> = Vec::new();
    source
        .try_reserve_exact(sample.len())
        .map_err(|_| crate::error::PreviewError::new("too_large", "Not enough memory to preview this image."))?;
    source.extend(sample.iter().copied().filter(|&v| keep(v)));
    let [low, high] = percentiles(&mut source, [LOW_PERCENTILE, HIGH_PERCENTILE]);
    let (mut low, mut high) = (low, high);
    if !(low < high) {
        low = min as f64;
        high = max as f64;
    }
    if !(low < high) {
        return Ok(Window { low, high: low, status: Status::Constant });
    }
    Ok(Window { low, high, status: Status::Ok })
}

/// Two increasing percentiles with a single partitioning pass for the second.
fn percentiles(values: &mut [f32], q: [f64; 2]) -> [f64; 2] {
    let n = values.len();
    let lo_index = |q: f64| (((n - 1) as f64 * (q / 100.0)).clamp(0.0, (n - 1) as f64)).floor() as usize;
    let first = percentile(values, q[0]);
    // After selecting around the first index, everything to its right is >= it,
    // so the second percentile can be selected inside that part only.
    let k = lo_index(q[0]);
    if lo_index(q[1]) <= k + 1 {
        return [first, percentile(values, q[1])];
    }
    let right = &mut values[k + 1..];
    let second = percentile_offset(right, q[1], n, k + 1);
    [first, second]
}

/// numpy.percentile(values, q) with the default linear interpolation,
/// using O(n) selection instead of a full sort.
fn percentile(values: &mut [f32], q: f64) -> f64 {
    let n = values.len();
    percentile_offset(values, q, n, 0)
}

/// Percentile of a virtual array of length `n` whose first `offset` (smallest)
/// elements have been removed; `values` holds the remaining ones.
fn percentile_offset(values: &mut [f32], q: f64, n: usize, offset: usize) -> f64 {
    if n == 1 {
        return values[0] as f64;
    }
    let quantile = q / 100.0;
    let virtual_index = ((n - 1) as f64 * quantile).clamp(0.0, (n - 1) as f64);
    let lo = virtual_index.floor() as usize;
    let t = virtual_index - lo as f64;
    let (_, a, right) = values.select_nth_unstable_by(lo - offset, |x, y| x.total_cmp(y));
    let a = *a as f64;
    if lo + 1 >= n {
        return a;
    }
    let b = right.iter().copied().fold(f32::INFINITY, f32::min) as f64;
    lerp(a, b, t)
}

/// numpy's `_lerp`, including its symmetric formulation for t >= 0.5.
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    let diff = b - a;
    if t >= 0.5 {
        b - diff * (1.0 - t)
    } else {
        a + diff * t
    }
}

/// numpy.rint for f32 (round half to even).
#[inline(always)]
pub fn rint(x: f32) -> f32 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        r
    }
}

/// Map physical values to grey levels. Non-finite voxels are black; a constant
/// volume renders its finite voxels mid-grey (black if the constant is zero).
pub fn apply(values: &[f32], w: &Window, out: &mut Vec<u8>) -> crate::error::Result<()> {
    out.clear();
    out.try_reserve_exact(values.len() * 3)
        .map_err(|_| crate::error::PreviewError::new("too_large", "Not enough memory to preview this image."))?;
    match w.status {
        Status::Constant => {
            let g = if w.low != 0.0 { 128 } else { 0 };
            for &v in values {
                let p = if v.is_finite() { g } else { 0 };
                out.extend_from_slice(&[p, p, p]);
            }
        }
        Status::NoFinite => out.resize(values.len() * 3, 0),
        Status::Ok => {
            let low = w.low as f32;
            let span = (w.high - w.low) as f32;
            for &v in values {
                let p = if v.is_finite() {
                    let unit = if span.is_finite() && span > 0.0 {
                        (v - low) / span
                    } else {
                        ((v as f64 - w.low) / (w.high - w.low)) as f32
                    }.clamp(0.0, 1.0);
                    rint(unit * 255.0) as u8
                } else {
                    0
                };
                out.extend_from_slice(&[p, p, p]);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rint_ties_to_even() {
        assert_eq!(rint(0.5), 0.0);
        assert_eq!(rint(1.5), 2.0);
        assert_eq!(rint(2.5), 2.0);
        assert_eq!(rint(127.5), 128.0);
        assert_eq!(rint(2.4), 2.0);
        assert_eq!(rint(-1.5), -2.0);
    }

    #[test]
    fn percentile_matches_numpy_examples() {
        // numpy.percentile(np.arange(10.), [0.5, 99.5]) == [0.045, 8.955]
        let mut v: Vec<f32> = (0..10).map(|x| x as f32).collect();
        assert!((percentile(&mut v, 0.5) - 0.045).abs() < 1e-12);
        let mut v: Vec<f32> = (0..10).rev().map(|x| x as f32).collect();
        assert!((percentile(&mut v, 99.5) - 8.955).abs() < 1e-12);
        let mut v: Vec<f32> = (0..1000).rev().map(|x| (x * 7 % 1000) as f32).collect();
        let [a, b] = percentiles(&mut v, [0.5, 99.5]);
        assert!((a - 4.995).abs() < 1e-9 && (b - 994.005).abs() < 1e-9);
    }

    #[test]
    fn statuses() {
        assert_eq!(compute(&[f32::NAN; 10]).unwrap().status, Status::NoFinite);
        assert_eq!(compute(&[7.0; 100]).unwrap().status, Status::Constant);
        let w = compute(&(0..1000).map(|x| x as f32).collect::<Vec<_>>()).unwrap();
        assert_eq!(w.status, Status::Ok);
        let mut out = Vec::new();
        apply(&[f32::NAN, -1.0, 1e9], &w, &mut out).unwrap();
        assert_eq!(out, vec![0, 0, 0, 0, 0, 0, 255, 255, 255]);
    }

    #[test]
    fn finite_extreme_and_subnormal_ranges_keep_contrast() {
        for (lo, hi) in [(-3e38f32, 3e38f32), (0.0, f32::from_bits(1))] {
            let w = Window { low: lo as f64, high: hi as f64, status: Status::Ok };
            let mut out = Vec::new();
            apply(&[lo, hi, f32::NAN, f32::INFINITY], &w, &mut out).unwrap();
            assert_eq!(out, [0, 0, 0, 255, 255, 255, 0, 0, 0, 0, 0, 0]);
        }
    }
}
