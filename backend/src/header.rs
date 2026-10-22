//! NIfTI-1 (348-byte) and NIfTI-2 (540-byte) single-file headers.
//!
//! Parsing is strict about what would make the data unreadable and lenient,
//! like nibabel's `check_fix`, about what can be repaired safely: invalid
//! xform codes become 0, a qfac other than ±1 becomes 1, zero pixdims become
//! 1 and negative pixdims take their absolute value. Every repair is reported
//! in `warnings`.

use crate::error::{bail, Result};

pub const NIFTI1_SIZE: usize = 348;
pub const NIFTI2_SIZE: usize = 540;
/// Enough bytes to parse either header.
pub const HEADER_PROBE: usize = 544;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    U8,
    I8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    C64,
    C128,
    Rgb24,
    Rgba32,
}

impl DType {
    pub fn from_code(code: i16) -> Result<DType> {
        Ok(match code {
            2 => DType::U8,
            4 => DType::I16,
            8 => DType::I32,
            16 => DType::F32,
            32 => DType::C64,
            64 => DType::F64,
            128 => DType::Rgb24,
            256 => DType::I8,
            512 => DType::U16,
            768 => DType::U32,
            1024 => DType::I64,
            1280 => DType::U64,
            1792 => DType::C128,
            2304 => DType::Rgba32,
            1536 | 2048 => bail!("unsupported_datatype", "128-bit float datatypes are not supported"),
            1 => bail!("unsupported_datatype", "binary (1-bit) datatype is not supported"),
            c => bail!("invalid_nifti", "Unknown NIfTI datatype code {c}"),
        })
    }

    pub fn itemsize(self) -> usize {
        match self {
            DType::U8 | DType::I8 => 1,
            DType::I16 | DType::U16 => 2,
            DType::Rgb24 => 3,
            DType::I32 | DType::U32 | DType::F32 | DType::Rgba32 => 4,
            DType::I64 | DType::U64 | DType::F64 | DType::C64 => 8,
            DType::C128 => 16,
        }
    }

    /// nibabel's datatype label.
    pub fn label(self) -> &'static str {
        match self {
            DType::U8 => "uint8",
            DType::I8 => "int8",
            DType::I16 => "int16",
            DType::U16 => "uint16",
            DType::I32 => "int32",
            DType::U32 => "uint32",
            DType::I64 => "int64",
            DType::U64 => "uint64",
            DType::F32 => "float32",
            DType::F64 => "float64",
            DType::C64 => "complex64",
            DType::C128 => "complex128",
            DType::Rgb24 => "RGB",
            DType::Rgba32 => "RGBA",
        }
    }

    /// numpy's `str(dtype)` for the stored dtype, including byte order.
    pub fn numpy_str(self, endian: Endian) -> String {
        let (code, name) = match self {
            DType::U8 => return "uint8".into(),
            DType::I8 => return "int8".into(),
            DType::Rgb24 => return "[('R', 'u1'), ('G', 'u1'), ('B', 'u1')]".into(),
            DType::Rgba32 => return "[('R', 'u1'), ('G', 'u1'), ('B', 'u1'), ('A', 'u1')]".into(),
            DType::I16 => ("i2", "int16"),
            DType::U16 => ("u2", "uint16"),
            DType::I32 => ("i4", "int32"),
            DType::U32 => ("u4", "uint32"),
            DType::I64 => ("i8", "int64"),
            DType::U64 => ("u8", "uint64"),
            DType::F32 => ("f4", "float32"),
            DType::F64 => ("f8", "float64"),
            DType::C64 => ("c8", "complex64"),
            DType::C128 => ("c16", "complex128"),
        };
        match endian {
            Endian::Little => name.into(),
            Endian::Big => format!(">{code}"),
        }
    }

    pub fn is_integer(self) -> bool {
        matches!(
            self,
            DType::U8 | DType::I8 | DType::I16 | DType::U16 | DType::I32 | DType::U32 | DType::I64 | DType::U64
        )
    }

    pub fn is_float(self) -> bool {
        matches!(self, DType::F32 | DType::F64)
    }
}

#[derive(Debug, Clone)]
pub struct Header {
    pub version: u8,
    pub endian: Endian,
    /// dim[1..=dim[0]]
    pub shape: Vec<usize>,
    pub dtype: DType,
    pub pixdim: [f64; 8],
    pub vox_offset: u64,
    /// Normalised: (1, 0) when the file carries no valid scaling.
    pub slope: f64,
    pub inter: f64,
    pub qform_code: i32,
    pub sform_code: i32,
    pub quatern: [f64; 3],
    pub qoffset: [f64; 3],
    pub srow: [[f64; 4]; 3],
    pub xyzt_units: i32,
    pub intent_code: i32,
    pub intent_name: String,
    pub descrip: String,
    pub warnings: Vec<String>,
}

/// A header extension: its code (`ecode`) and content.
#[derive(Debug, Clone)]
pub struct Extension {
    pub code: i32,
    pub content: Vec<u8>,
}

/// Header extensions are read up to this many bytes after the header; real
/// ones are a few kB (NIfTI-MRS JSON, DICOM tags), larger ones are skipped.
pub const MAX_EXTENSION_BYTES: usize = 1 << 20;

struct Reader<'a> {
    b: &'a [u8],
    e: Endian,
}

impl Reader<'_> {
    fn arr<const N: usize>(&self, off: usize) -> [u8; N] {
        let mut a = [0u8; N];
        a.copy_from_slice(&self.b[off..off + N]);
        if self.e == Endian::Big {
            a.reverse();
        }
        a
    }
    fn i16(&self, o: usize) -> i16 {
        i16::from_le_bytes(self.arr(o))
    }
    fn i32(&self, o: usize) -> i32 {
        i32::from_le_bytes(self.arr(o))
    }
    fn i64(&self, o: usize) -> i64 {
        i64::from_le_bytes(self.arr(o))
    }
    fn f32(&self, o: usize) -> f64 {
        f32::from_le_bytes(self.arr(o)) as f64
    }
    fn f64(&self, o: usize) -> f64 {
        f64::from_le_bytes(self.arr(o))
    }
    fn text(&self, o: usize, n: usize) -> String {
        let raw = &self.b[o..o + n];
        let end = raw.iter().position(|&c| c == 0).unwrap_or(n);
        String::from_utf8_lossy(&raw[..end]).trim().to_string()
    }
}

const VALID_XFORM: [i32; 6] = [0, 1, 2, 3, 4, 5];

pub fn parse(b: &[u8]) -> Result<Header> {
    if b.len() < NIFTI1_SIZE {
        bail!("invalid_nifti", "File too short to contain a NIfTI header");
    }
    let first = [b[0], b[1], b[2], b[3]];
    let (version, endian) = match (i32::from_le_bytes(first), i32::from_be_bytes(first)) {
        (348, _) => (1u8, Endian::Little),
        (_, 348) => (1, Endian::Big),
        (540, _) => (2, Endian::Little),
        (_, 540) => (2, Endian::Big),
        _ => bail!("invalid_nifti", "Not a NIfTI file (unrecognised header size)"),
    };
    let r = Reader { b, e: endian };
    let mut warnings = Vec::new();
    let mut dim = [0i64; 8];
    let mut pixdim = [0f64; 8];
    let (datatype, vox_offset, slope, inter, qform_code, sform_code, intent_code, intent_name, xyzt_units, descrip);
    let mut quatern = [0f64; 3];
    let mut qoffset = [0f64; 3];
    let mut srow = [[0f64; 4]; 3];

    if version == 1 {
        match &b[344..348] {
            b"n+1\0" => {}
            b"ni1\0" => bail!("unsupported_format", "NIfTI-1 header/image pairs (.hdr/.img) are not supported"),
            _ => bail!("invalid_nifti", "Not a NIfTI-1 file (bad magic)"),
        }
        for i in 0..8 {
            dim[i] = r.i16(40 + 2 * i) as i64;
            pixdim[i] = r.f32(76 + 4 * i);
        }
        datatype = r.i16(70);
        let off = r.f32(108);
        vox_offset = check_offset(off, 352)?;
        slope = r.f32(112);
        inter = r.f32(116);
        intent_code = r.i16(68) as i32;
        intent_name = clean_text(&r.text(328, 16));
        xyzt_units = b[123] as i32;
        qform_code = r.i16(252) as i32;
        sform_code = r.i16(254) as i32;
        for i in 0..3 {
            quatern[i] = r.f32(256 + 4 * i);
            qoffset[i] = r.f32(268 + 4 * i);
        }
        for (row, values) in srow.iter_mut().enumerate() {
            for (c, v) in values.iter_mut().enumerate() {
                *v = r.f32(280 + 16 * row + 4 * c);
            }
        }
        descrip = clean_text(&r.text(148, 80));
    } else {
        if b.len() < NIFTI2_SIZE {
            bail!("invalid_nifti", "File too short to contain a NIfTI-2 header");
        }
        if &b[4..12] != b"n+2\0\r\n\x1a\n" {
            if &b[4..8] == b"ni2\0" {
                bail!("unsupported_format", "NIfTI-2 header/image pairs are not supported");
            }
            bail!("invalid_nifti", "Not a NIfTI-2 file (bad magic)");
        }
        for i in 0..8 {
            dim[i] = r.i64(16 + 8 * i);
            pixdim[i] = r.f64(104 + 8 * i);
        }
        datatype = r.i16(12);
        let off = r.i64(168) as f64;
        vox_offset = check_offset(off, 544)?;
        slope = r.f64(176);
        inter = r.f64(184);
        qform_code = r.i32(344);
        sform_code = r.i32(348);
        for i in 0..3 {
            quatern[i] = r.f64(352 + 8 * i);
            qoffset[i] = r.f64(376 + 8 * i);
        }
        for (row, values) in srow.iter_mut().enumerate() {
            for (c, v) in values.iter_mut().enumerate() {
                *v = r.f64(400 + 32 * row + 8 * c);
            }
        }
        xyzt_units = r.i32(500);
        intent_code = r.i32(504);
        intent_name = clean_text(&r.text(508, 16));
        descrip = clean_text(&r.text(240, 80));
    }

    let ndim = dim[0];
    if !(1..=7).contains(&ndim) {
        bail!("invalid_nifti", "Invalid number of dimensions ({ndim})");
    }
    let ndim = ndim as usize;
    let mut shape = Vec::with_capacity(ndim);
    for &d in &dim[1..=ndim] {
        if d <= 0 {
            bail!("invalid_nifti", "Invalid image dimensions {:?}", &dim[1..=ndim]);
        }
        shape.push(usize::try_from(d).map_err(|_| crate::error::PreviewError::new("invalid_nifti", "dimension overflow"))?);
    }
    if shape.len() < 2 {
        bail!("invalid_nifti", "Invalid image dimensions {:?}", shape);
    }
    let dtype = DType::from_code(datatype)?;

    // Non-finite sizes would make every view degenerate.
    if pixdim[1..4].iter().any(|p| !p.is_finite()) {
        pixdim[1..4].iter_mut().filter(|p| !p.is_finite()).for_each(|p| *p = 1.0);
        warnings.push("pixdim[1,2,3] should be finite; setting non-finite dims to 1".to_string());
    }
    if pixdim[4..].iter().any(|p| !p.is_finite()) {
        pixdim[4..].iter_mut().filter(|p| !p.is_finite()).for_each(|p| *p = 0.0);
        warnings.push("pixdim[4..7] should be finite; setting non-finite values to 0".to_string());
    }
    // nibabel check_fix equivalents
    let spatial = &mut pixdim[1..4];
    if spatial.contains(&0.0) {
        spatial.iter_mut().filter(|p| **p == 0.0).for_each(|p| *p = 1.0);
        warnings.push("pixdim[1,2,3] should be non-zero; setting 0 dims to 1".to_string());
    }
    if spatial.iter().any(|&p| p < 0.0) {
        spatial.iter_mut().for_each(|p| *p = p.abs());
        warnings.push("pixdim[1,2,3] should be positive; setting to abs of pixdim values".to_string());
    }
    if pixdim[0] != 1.0 && pixdim[0] != -1.0 {
        pixdim[0] = 1.0;
    }
    let qform_code = fix_xform(qform_code, "qform_code", &mut warnings);
    let sform_code = fix_xform(sform_code, "sform_code", &mut warnings);

    // nibabel: a zero or non-finite slope means "no scaling"
    let (slope, inter) = if slope == 0.0 || !slope.is_finite() {
        (1.0, 0.0)
    } else if !inter.is_finite() {
        bail!("invalid_nifti", "Valid slope but invalid intercept {inter}");
    } else {
        (slope, inter)
    };

    Ok(Header {
        version,
        endian,
        shape,
        dtype,
        pixdim,
        vox_offset,
        slope,
        inter,
        qform_code,
        sform_code,
        quatern,
        qoffset,
        srow,
        xyzt_units,
        intent_code,
        intent_name,
        descrip,
        warnings,
    })
}

/// Parse the extensions that follow the header. `bytes` starts at the 4-byte
/// extension flag (offset 348 or 540) and ends at the voxel data or at the
/// read limit. Each extension is `esize` (a multiple of 16, header included),
/// `ecode`, then `esize - 8` bytes. Anything malformed ends the list.
pub fn parse_extensions(bytes: &[u8], endian: Endian) -> Vec<Extension> {
    let mut out = Vec::new();
    if bytes.len() < 4 || bytes[0] == 0 {
        return out;
    }
    let int = |o: usize| {
        let a = [bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]];
        if endian == Endian::Little {
            i32::from_le_bytes(a)
        } else {
            i32::from_be_bytes(a)
        }
    };
    let mut pos = 4usize;
    while pos + 8 <= bytes.len() {
        let (esize, code) = (int(pos), int(pos + 4));
        if esize < 16 || esize % 16 != 0 {
            break;
        }
        let end = pos.saturating_add(esize as usize);
        if end > bytes.len() {
            break;
        }
        out.push(Extension { code, content: bytes[pos + 8..end].to_vec() });
        pos = end;
    }
    out
}

/// Header text as displayed: control characters and bidirectional overrides
/// removed (they could fake lines or reorder the panel), tabs and line
/// breaks turned into spaces.
pub fn clean_text(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\r' => Some(' '),
            '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => None,
            '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => None,
            c => Some(c),
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn check_offset(off: f64, minimum: u64) -> Result<u64> {
    if !off.is_finite() || off < 0.0 {
        bail!("invalid_nifti", "Invalid vox_offset {off}");
    }
    let off = off as u64;
    if off == 0 {
        // Unset: data follow the header and the 4-byte extension flag.
        return Ok(minimum);
    }
    if off < minimum {
        bail!("invalid_nifti", "vox_offset {off} too low for a single-file NIfTI (minimum {minimum})");
    }
    Ok(off)
}

fn fix_xform(code: i32, name: &str, warnings: &mut Vec<String>) -> i32 {
    if VALID_XFORM.contains(&code) {
        code
    } else {
        warnings.push(format!("{name} {code} not valid; setting to 0"));
        0
    }
}

impl Header {
    pub fn ndim(&self) -> usize {
        self.shape.len()
    }

    pub fn shape3(&self) -> [usize; 3] {
        let mut s = [1usize; 3];
        for (i, &n) in self.shape.iter().take(3).enumerate() {
            s[i] = n;
        }
        s
    }

    pub fn n_volumes(&self) -> usize {
        self.shape.iter().skip(3).try_fold(1usize, |a, &d| a.checked_mul(d)).unwrap_or(usize::MAX).max(1)
    }

    /// Bytes of the first 3D volume, None on overflow.
    pub fn volume_bytes(&self) -> Option<u64> {
        let mut n: u64 = self.dtype.itemsize() as u64;
        for &d in self.shape.iter().take(3) {
            n = n.checked_mul(d as u64)?;
        }
        Some(n)
    }

    pub fn total_bytes(&self) -> Option<u64> {
        let mut n = self.volume_bytes()?;
        for &d in self.shape.iter().skip(3) {
            n = n.checked_mul(d as u64)?;
        }
        Some(n)
    }

    pub fn zooms(&self) -> Vec<f64> {
        self.pixdim[1..=self.ndim()].to_vec()
    }

    pub fn identity_scaling(&self) -> bool {
        self.slope == 1.0 && self.inter == 0.0
    }

    pub fn format(&self) -> &'static str {
        if self.version == 2 {
            "NIfTI-2"
        } else {
            "NIfTI-1"
        }
    }

    pub fn space_units(&self) -> &'static str {
        unit_label(self.xyzt_units.rem_euclid(8))
    }

    pub fn time_units(&self) -> &'static str {
        unit_label(self.xyzt_units - self.xyzt_units.rem_euclid(8))
    }

    pub fn affine_source(&self) -> &'static str {
        if self.sform_code != 0 {
            "sform"
        } else if self.qform_code != 0 {
            "qform"
        } else {
            "none"
        }
    }

    /// nibabel's `get_best_affine`: sform, else qform, else the base affine.
    pub fn best_affine(&self) -> Result<[[f64; 4]; 4]> {
        if self.sform_code != 0 {
            let s = self.srow;
            return Ok([s[0], s[1], s[2], [0.0, 0.0, 0.0, 1.0]]);
        }
        if self.qform_code != 0 {
            return self.qform_affine();
        }
        Ok(self.base_affine())
    }

    fn qform_affine(&self) -> Result<[[f64; 4]; 4]> {
        let [x, y, z] = self.quatern;
        let w2 = 1.0 - (x * x + y * y + z * z);
        let threshold = -(f32::EPSILON as f64) * 3.0;
        let w = if w2 < 0.0 {
            if w2 < threshold {
                bail!("invalid_nifti", "Invalid qform quaternion (|b,c,d| > 1)");
            }
            0.0
        } else {
            w2.sqrt()
        };
        let r = quat2mat(w, x, y, z);
        let vox = [self.pixdim[1], self.pixdim[2], self.pixdim[3] * self.pixdim[0]];
        let mut a = [[0.0; 4]; 4];
        for i in 0..3 {
            for (j, spacing) in vox.iter().enumerate() {
                a[i][j] = r[i][j] * spacing;
            }
            a[i][3] = self.qoffset[i];
        }
        a[3][3] = 1.0;
        Ok(a)
    }

    fn base_affine(&self) -> [[f64; 4]; 4] {
        let shape = self.shape3();
        let mut zooms = [1.0f64; 3];
        for (i, z) in zooms.iter_mut().enumerate().take(self.ndim().min(3)) {
            *z = self.pixdim[i + 1];
        }
        zooms[0] = -zooms[0];
        let mut a = [[0.0; 4]; 4];
        for i in 0..3 {
            a[i][i] = zooms[i];
            a[i][3] = -((shape[i] as f64 - 1.0) / 2.0) * zooms[i];
        }
        a[3][3] = 1.0;
        a
    }
}

/// nibabel.quaternions.quat2mat
fn quat2mat(w: f64, x: f64, y: f64, z: f64) -> [[f64; 3]; 3] {
    let nq = w * w + x * x + y * y + z * z;
    if nq < f64::EPSILON {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let s = 2.0 / nq;
    let (xs, ys, zs) = (x * s, y * s, z * s);
    let (wx, wy, wz) = (w * xs, w * ys, w * zs);
    let (xx, xy, xz) = (x * xs, x * ys, x * zs);
    let (yy, yz, zz) = (y * ys, y * zs, z * zs);
    [
        [1.0 - (yy + zz), xy - wz, xz + wy],
        [xy + wz, 1.0 - (xx + zz), yz - wx],
        [xz - wy, yz + wx, 1.0 - (xx + yy)],
    ]
}

pub fn unit_label(code: i32) -> &'static str {
    match code {
        0 => "unknown",
        1 => "meter",
        2 => "mm",
        3 => "micron",
        8 => "sec",
        16 => "msec",
        24 => "usec",
        32 => "hz",
        40 => "ppm",
        48 => "rads",
        _ => "unknown",
    }
}

pub fn xform_label(code: i32) -> &'static str {
    match code {
        1 => "scanner",
        2 => "aligned",
        3 => "talairach",
        4 => "mni",
        5 => "template",
        _ => "unknown",
    }
}

pub fn intent_label(code: i32) -> &'static str {
    match code {
        0 => "none",
        2 => "correlation",
        3 => "t test",
        4 => "f test",
        5 => "z score",
        6 => "chi2",
        7 => "beta",
        8 => "binomial",
        9 => "gamma",
        10 => "poisson",
        11 => "normal",
        12 => "non central f test",
        13 => "non central chi2",
        14 => "logistic",
        15 => "laplace",
        16 => "uniform",
        17 => "non central t test",
        18 => "weibull",
        19 => "chi",
        20 => "inverse gaussian",
        21 => "extreme value 1",
        22 => "p value",
        23 => "log p value",
        24 => "log10 p value",
        1001 => "estimate",
        1002 => "label",
        1003 => "neuroname",
        1004 => "general matrix",
        1005 => "symmetric matrix",
        1006 => "displacement vector",
        1007 => "vector",
        1008 => "pointset",
        1009 => "triangle",
        1010 => "quaternion",
        1011 => "dimensionless",
        2001 => "time series",
        2002 => "node index",
        2003 => "rgb vector",
        2004 => "rgba vector",
        2005 => "shape",
        2006 => "fnirt disp field",
        2007 => "fnirt cubic spline coef",
        2008 => "fnirt dct coef",
        2009 => "fnirt quad spline coef",
        2016 => "topup cubic spline coef ",
        2017 => "topup quad spline coef",
        2018 => "topup field",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nifti1(dims: &[i16], datatype: i16) -> Vec<u8> {
        let mut b = vec![0u8; 352];
        b[0..4].copy_from_slice(&348i32.to_le_bytes());
        b[40..42].copy_from_slice(&(dims.len() as i16).to_le_bytes());
        for (i, d) in dims.iter().enumerate() {
            b[42 + 2 * i..44 + 2 * i].copy_from_slice(&d.to_le_bytes());
        }
        b[70..72].copy_from_slice(&datatype.to_le_bytes());
        for i in 0..4 {
            b[76 + 4 * i..80 + 4 * i].copy_from_slice(&1f32.to_le_bytes());
        }
        b[108..112].copy_from_slice(&352f32.to_le_bytes());
        b[344..348].copy_from_slice(b"n+1\0");
        b
    }

    #[test]
    fn parses_minimal_nifti1() {
        let h = parse(&nifti1(&[4, 5, 6], 4)).unwrap();
        assert_eq!(h.version, 1);
        assert_eq!(h.shape, vec![4, 5, 6]);
        assert_eq!(h.dtype, DType::I16);
        assert_eq!(h.affine_source(), "none");
        let a = h.best_affine().unwrap();
        assert_eq!(a[0][0], -1.0); // nibabel's base affine flips x
        assert_eq!(a[0][3], 1.5);
    }

    #[test]
    fn big_endian_header() {
        let mut b = nifti1(&[4, 5, 6], 4);
        // swap every field we read
        let swap = |b: &mut Vec<u8>, off: usize, n: usize| b[off..off + n].reverse();
        swap(&mut b, 0, 4);
        for i in 0..4 {
            swap(&mut b, 40 + 2 * i, 2);
        }
        swap(&mut b, 70, 2);
        for i in 0..4 {
            swap(&mut b, 76 + 4 * i, 4);
        }
        swap(&mut b, 108, 4);
        let h = parse(&b).unwrap();
        assert_eq!(h.endian, Endian::Big);
        assert_eq!(h.shape, vec![4, 5, 6]);
    }

    #[test]
    fn rejects_garbage_and_pairs() {
        assert_eq!(parse(&[0u8; 100]).unwrap_err().code, "invalid_nifti");
        assert_eq!(parse(&[7u8; 400]).unwrap_err().code, "invalid_nifti");
        let mut b = nifti1(&[4, 5, 6], 4);
        b[344..348].copy_from_slice(b"ni1\0");
        assert_eq!(parse(&b).unwrap_err().code, "unsupported_format");
        let mut b = nifti1(&[4, 0, 6], 4);
        assert_eq!(parse(&b).unwrap_err().code, "invalid_nifti");
        b = nifti1(&[4, 5, 6], 1536);
        assert_eq!(parse(&b).unwrap_err().code, "unsupported_datatype");
        b = nifti1(&[4, 5, 6], 3);
        assert_eq!(parse(&b).unwrap_err().code, "invalid_nifti");
    }

    #[test]
    fn identity_quaternion_qform() {
        let mut b = nifti1(&[4, 5, 6], 4);
        b[252..254].copy_from_slice(&1i16.to_le_bytes());
        b[268..272].copy_from_slice(&(-10f32).to_le_bytes());
        let h = parse(&b).unwrap();
        let a = h.best_affine().unwrap();
        assert_eq!(a[0], [1.0, 0.0, 0.0, -10.0]);
        assert_eq!(a[2][2], 1.0);
    }

    #[test]
    fn fixes_like_nibabel() {
        let mut b = nifti1(&[4, 5, 6], 4);
        b[80..84].copy_from_slice(&(-2f32).to_le_bytes()); // pixdim[1]
        b[76..80].copy_from_slice(&0f32.to_le_bytes()); // qfac
        b[252..254].copy_from_slice(&9i16.to_le_bytes()); // invalid qform_code
        b[112..116].copy_from_slice(&0f32.to_le_bytes()); // slope 0 => identity
        let h = parse(&b).unwrap();
        assert_eq!(h.pixdim[1], 2.0);
        assert_eq!(h.pixdim[0], 1.0);
        assert_eq!(h.qform_code, 0);
        assert!(h.identity_scaling());
        assert_eq!(h.warnings.len(), 2);
    }
}
