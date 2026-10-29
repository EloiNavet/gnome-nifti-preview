//! MRtrix image format (`.mif`, `.mif.gz`): a text header from
//! `mrtrix image` to `END`, then the data in the same file.
//!
//! `dim`, `vox` and `transform` describe the image axes; `layout` how they
//! are stored (`-0,-1,+2,+3`: axis 0 fastest and reversed...). The affine is
//! MRtrix's scanner transform `[R·diag(vox) | t]` (RAS, like NIfTI). A
//! header alone (`.mih`) names the files that hold the data, relative to its
//! folder, and may end without `END`. `comments` (often patient details
//! copied from DICOM) are not shown.

use std::path::Path;

use crate::error::{bail, Result};
use crate::header::{DType, Endian};
use crate::image::{DataFile, DataLayout, Format, ImageHeader, StorageAxis};

pub const MAGIC: &[u8] = b"mrtrix image";
/// Longest header read (a diffusion scheme of a few thousand directions fits).
pub const MAX_HEADER_BYTES: usize = 4 << 20;
const MAX_DIMS: usize = 7;

fn invalid(what: impl std::fmt::Display) -> crate::error::PreviewError {
    crate::error::PreviewError::new("invalid_image", format!("Invalid MRtrix header: {what}"))
}

/// End of the header (just after the `END` line) in `text`, if there.
pub fn header_end(text: &[u8]) -> Option<usize> {
    let mut start = 0;
    while let Some(nl) = text[start..].iter().position(|&c| c == b'\n') {
        let line = &text[start..start + nl];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line == b"END" {
            return Some(start + nl + 1);
        }
        start += nl + 1;
    }
    None
}

fn numbers(v: &str) -> Vec<f64> {
    v.split(',').map(|x| x.trim().parse::<f64>().unwrap_or(f64::NAN)).collect()
}

/// The datatype and byte order of a `datatype:` value (`Bit`: one bit per voxel).
fn datatype(v: &str) -> Result<(DType, Endian, bool)> {
    let v = v.trim();
    let lower = v.to_ascii_lowercase();
    let endian = if lower.ends_with("be") { Endian::Big } else { Endian::Little };
    let base = lower.trim_end_matches("le").trim_end_matches("be");
    Ok(match base {
        "bit" => (DType::U8, Endian::Little, true),
        "int8" => (DType::I8, endian, false),
        "uint8" => (DType::U8, endian, false),
        "int16" => (DType::I16, endian, false),
        "uint16" => (DType::U16, endian, false),
        "int32" => (DType::I32, endian, false),
        "uint32" => (DType::U32, endian, false),
        "int64" => (DType::I64, endian, false),
        "uint64" => (DType::U64, endian, false),
        "float32" => (DType::F32, endian, false),
        "float64" => (DType::F64, endian, false),
        "cfloat32" => (DType::C64, endian, false),
        "cfloat64" => (DType::C128, endian, false),
        _ => bail!("unsupported_datatype", "MRtrix datatype {v} is not supported"),
    })
}

/// Parse a header (`text` holds at least everything up to `END`, or the
/// whole of a `.mih`); data files are relative to `dir`.
pub fn parse(text: &[u8], dir: &Path) -> Result<ImageHeader> {
    let (end, complete) = match header_end(text) {
        Some(e) => (e, true),
        None => (text.len(), false),
    };
    let text = String::from_utf8_lossy(&text[..end]);
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("mrtrix image") {
        return Err(invalid("first line is not \"mrtrix image\""));
    }
    let (mut dim, mut vox, mut layout, mut dt, mut scaling, mut version) = (None, None, None, None, None, None);
    let (mut transform, mut scheme, mut files) = (Vec::new(), Vec::new(), Vec::new());
    for line in lines {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "dim" => dim = Some(value.to_string()),
            "vox" => vox = Some(numbers(value)),
            "layout" => layout = Some(value.to_string()),
            "datatype" => dt = Some(value.to_string()),
            "file" => files.push(value.to_string()),
            "scaling" => scaling = Some(numbers(value)),
            "transform" => transform.push(numbers(value)),
            "dw_scheme" => scheme.push(numbers(value)),
            "mrtrix_version" => version = Some(crate::header::clean_text(value)),
            _ => {}
        }
    }
    let dim = dim.ok_or_else(|| invalid("no dim"))?;
    let shape: Vec<usize> = dim.split(',').map(|x| x.trim().parse::<usize>().ok().filter(|&n| n > 0)).collect::<Option<_>>()
        .ok_or_else(|| invalid(format_args!("dim: {dim}")))?;
    if !(2..=MAX_DIMS).contains(&shape.len()) {
        return Err(invalid(format_args!("{} dimensions", shape.len())));
    }
    let n = shape.len();
    let (dtype, endian, bits) = datatype(&dt.ok_or_else(|| invalid("no datatype"))?)?;
    // file: . OFFSET (the data follow in this file), or the files that hold
    // them one after the other: NAME [OFFSET].
    let first = files.first().ok_or_else(|| invalid("no file"))?;
    let mut parts = first.split_whitespace();
    let mut detached = Vec::new();
    let offset = match (parts.next(), parts.next()) {
        (Some("."), Some(o)) => o.parse::<u64>().map_err(|_| invalid(format_args!("file: {first}")))?,
        (Some("."), None) => end as u64,
        _ => {
            for f in &files {
                let mut parts = f.split_whitespace();
                let (Some(name), skip) = (parts.next(), parts.next()) else { return Err(invalid("empty file entry")) };
                let skip = skip.map_or(Ok(0), |o| o.parse::<i64>().map_err(|_| invalid(format_args!("file: {f}"))))?;
                if name == "." || skip < 0 || detached.len() >= crate::formats::nrrd::MAX_FILES {
                    return Err(invalid(format_args!("file: {f}")));
                }
                let (path, root) = crate::image::data_file_in(dir, name, "MRtrix")?;
                detached.push(DataFile { path, root: Some(root), line_skip: 0, byte_skip: skip });
            }
            0
        }
    };
    if detached.is_empty() && (!complete || files.len() > 1) {
        return Err(invalid(if complete { "several files for data in this file" } else { "no END line" }));
    }
    if detached.is_empty() && offset < end as u64 {
        return Err(invalid("the data start inside the header"));
    }
    // layout: +0,-1,+2 (ranks with an optional sign each)
    let mut axes = Vec::with_capacity(n);
    if let Some(l) = &layout {
        for tok in l.split(',') {
            let tok = tok.trim();
            let (reversed, digits) = match tok.as_bytes().first() {
                Some(b'-') => (true, &tok[1..]),
                Some(b'+') => (false, &tok[1..]),
                _ => (false, tok),
            };
            let rank = digits.parse::<usize>().map_err(|_| invalid(format_args!("layout: {l}")))?;
            axes.push(StorageAxis { rank, reversed });
        }
        let mut ranks: Vec<usize> = axes.iter().map(|a| a.rank).collect();
        ranks.sort_unstable();
        if axes.len() != n || ranks != (0..n).collect::<Vec<_>>() {
            return Err(invalid(format_args!("layout {l} for {n} dimensions")));
        }
    }
    // A 2D image is one slice: a third axis of 1.
    let mut shape = shape;
    if shape.len() == 2 {
        shape.push(1);
        if !axes.is_empty() {
            axes.push(StorageAxis { rank: 2, reversed: false });
        }
    }
    let n = shape.len();
    let vox = vox.unwrap_or_default();
    let mut zooms: Vec<f64> = (0..n).map(|i| vox.get(i).copied().unwrap_or(f64::NAN)).collect();
    let mut warnings = Vec::new();
    for z in zooms.iter_mut().take(3) {
        if !(z.is_finite() && *z > 0.0) {
            *z = 1.0;
        }
    }
    let tr = zooms.get(3).copied().filter(|t| t.is_finite() && *t > 0.0);
    for z in zooms.iter_mut().skip(3) {
        if !z.is_finite() {
            *z = 0.0;
        }
    }
    // The scanner transform: 3 rows of 4.
    let mut affine = [[0.0; 4]; 4];
    affine[3][3] = 1.0;
    let known = transform.len() == 3 && transform.iter().all(|r| r.len() == 4 && r.iter().all(|v| v.is_finite()));
    for r in 0..3 {
        for (c, z) in zooms.iter().enumerate().take(3) {
            let rot = if known { transform[r][c] } else if r == c { 1.0 } else { 0.0 };
            affine[r][c] = rot * z;
        }
        affine[r][3] = if known { transform[r][3] } else { 0.0 };
    }
    if !transform.is_empty() && !known {
        warnings.push("invalid MRtrix transform ignored".to_string());
    }
    let (slope, inter) = match scaling.as_deref() {
        Some([offset, scale]) if scale.is_finite() && *scale != 0.0 && offset.is_finite() => (*scale, *offset),
        _ => (1.0, 0.0),
    };
    let mut fields = Vec::new();
    if let Some(l) = &layout {
        fields.push(("Layout".to_string(), l.clone()));
    }
    if let Some(s) = diffusion_summary(&scheme) {
        fields.push(("Diffusion".to_string(), s));
    }
    if let Some(v) = version {
        fields.push(("MRtrix".to_string(), v));
    }
    if let Some(f) = detached.first() {
        let name = f.path.file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
        fields.push(("Data file".to_string(), if detached.len() == 1 { crate::header::clean_text(&name) } else { format!("{} files", detached.len()) }));
    }
    let mut layout = DataLayout::canonical(offset);
    layout.axes = axes;
    layout.bits = bits;
    layout.files = detached;
    let mut h = ImageHeader {
        format: Format::Mif,
        shape,
        dtype,
        endian,
        zooms,
        slope,
        inter,
        affine,
        affine_source: if known { "transform" } else { "none" },
        space_units: "mm",
        time_units: if tr.is_some() { "sec" } else { "unknown" },
        repetition_time: tr,
        intent_code: 0,
        descrip: String::new(),
        warnings,
        fields,
        layout,
        nifti: None,
        dicom: None,
    };
    h.normalize_storage();
    Ok(h)
}

/// "68 directions, b = 0, 3000 s/mm²" from the rows of a diffusion scheme.
pub fn diffusion_summary(scheme: &[Vec<f64>]) -> Option<String> {
    if scheme.is_empty() {
        return None;
    }
    // Shells within 5 % (or 50 s/mm²) are one shell, shown as their mean.
    let mut shells: Vec<(f64, usize)> = Vec::new();
    for row in scheme {
        if let Some(&b) = row.get(3).filter(|b| b.is_finite()) {
            match shells.iter_mut().find(|(sum, n)| {
                let mean = *sum / *n as f64;
                (mean - b).abs() <= (0.05 * mean).max(50.0)
            }) {
                Some((sum, n)) => {
                    *sum += b;
                    *n += 1;
                }
                None => shells.push((b, 1)),
            }
        }
    }
    let mut means: Vec<f64> = shells.iter().map(|(sum, n)| ((sum / *n as f64) / 50.0).round() * 50.0).collect();
    means.sort_by(f64::total_cmp);
    let b: Vec<String> = means.iter().map(|s| format!("{s}")).collect();
    Some(format!("{} directions, b = {} s/mm²", scheme.len(), b.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DWI: &str = "mrtrix image\ndim: 6,8,9,68\nvox: 2.5,2.5,2.5,nan\nlayout: -0,-1,+2,+3\ndatatype: UInt16LE\n\
transform: 1,0,0,10\ntransform: 0,1,0,20\ntransform: 0,0,1,30\ndw_scheme: 0,0,0,0\ndw_scheme: 0,0,1,2950\n\
dw_scheme: 0,1,0,3000\ncomments: PATIENT NAME\nfile: . 512\nEND\n";

    #[test]
    fn reversed_axes_become_the_stored_grid() {
        let h = parse(DWI.as_bytes(), Path::new("/d")).unwrap();
        assert_eq!(h.shape, vec![6, 8, 9, 68]);
        assert_eq!((h.dtype, h.endian), (DType::U16, Endian::Little));
        // -0, -1: the first two axes count from their end; same physical points.
        assert_eq!(h.affine[0], [-2.5, 0.0, 0.0, 10.0 + 2.5 * 5.0]);
        assert_eq!(h.affine[1], [0.0, -2.5, 0.0, 20.0 + 2.5 * 7.0]);
        assert_eq!(h.affine[2], [0.0, 0.0, 2.5, 30.0]);
        assert!(h.layout.is_canonical(), "{:?}", h.layout.axes);
        assert_eq!(h.layout.offset, 512);
        assert!(h.fields.iter().any(|(k, v)| k == "Diffusion" && v == "3 directions, b = 0, 3000 s/mm²"), "{:?}", h.fields);
        assert!(!h.fields.iter().any(|(_, v)| v.contains("PATIENT")));
    }

    #[test]
    fn permuted_and_interleaved_layouts() {
        let t = "mrtrix image\ndim: 4,5,6\nvox: 1,2,3\nlayout: -0,+2,-1\ndatatype: Int32BE\nfile: . 200\nEND\n";
        let h = parse(t.as_bytes(), Path::new("/d")).unwrap();
        // Stored order: axis 0 (reversed), axis 2 (reversed), axis 1.
        assert_eq!(h.shape, vec![4, 6, 5]);
        assert_eq!(h.zooms, vec![1.0, 3.0, 2.0]);
        assert_eq!(h.affine[1], [0.0, 0.0, 2.0, 0.0]);
        assert_eq!(h.affine[2], [0.0, -3.0, 0.0, 15.0]);
        assert!(h.layout.is_canonical());
        let fod = "mrtrix image\ndim: 6,8,9,45\nvox: 2,2,2,nan\nlayout: -1,-2,+3,+0\ndatatype: Float32LE\nfile: . 300\nEND\n";
        let h = parse(fod.as_bytes(), Path::new("/d")).unwrap();
        assert!(!h.layout.is_canonical(), "volumes interleaved with voxels");
        let (strides, _) = h.layout.strides(&h.shape);
        assert_eq!(strides, vec![45, 45 * 6, 45 * 6 * 8, 1]);
    }

    #[test]
    fn bits_scaling_and_errors() {
        let t = "mrtrix image\ndim: 6,8,9\nlayout: +0,+1,+2\ndatatype: Bit\nscaling: 1.5,2\nfile: . 128\nEND\n";
        let h = parse(t.as_bytes(), Path::new("/d")).unwrap();
        assert!(h.layout.bits && h.dtype == DType::U8);
        assert_eq!((h.slope, h.inter), (2.0, 1.5));
        for bad in [
            "mrtrix image\ndim: 6,8\ndatatype: Float32LE\nfile: other.dat -4\nEND\n",
            "mrtrix image\ndim: 6,8\ndatatype: Float32LE\nfile: . 64\nfile: . 128\nEND\n",
            "mrtrix image\ndim: 6,0,9\ndatatype: Float32LE\nfile: . 64\nEND\n",
            "mrtrix image\ndim: 6,8,9\nlayout: +0,+0,+2\ndatatype: Float32LE\nfile: . 64\nEND\n",
            "mrtrix image\ndim: 6,8,9\ndatatype: Float16LE\nfile: . 64\nEND\n",
            "mrtrix image\ndim: 6,8,9\ndatatype: Int8\nfile: . 64\n",
        ] {
            assert!(parse(bad.as_bytes(), Path::new("/d")).is_err(), "{bad}");
        }
    }

    #[test]
    fn two_dimensions_are_one_slice() {
        let t = "mrtrix image\ndim: 4,5\nvox: 1,2\nlayout: -1,+0\ndatatype: UInt8\nfile: . 128\nEND\n";
        let h = parse(t.as_bytes(), Path::new("/d")).unwrap();
        assert_eq!(h.shape, vec![5, 4, 1]);
        assert_eq!(h.zooms, vec![2.0, 1.0, 1.0]);
    }

    #[test]
    fn header_alone_names_its_data_files() {
        // A .mih may end without END.
        let t = "mrtrix image\ndim: 6,8,9,2\ndatatype: Float32LE\nfile: a.dat\nfile: b.dat 16\n";
        let h = parse(t.as_bytes(), Path::new("/d")).unwrap();
        let files: Vec<_> = h.layout.files.iter().map(|f| (f.path.clone(), f.byte_skip)).collect();
        assert_eq!(files, vec![(Path::new("/d/a.dat").to_path_buf(), 0), (Path::new("/d/b.dat").to_path_buf(), 16)]);
        assert!(h.fields.iter().any(|(k, v)| k == "Data file" && v == "2 files"), "{:?}", h.fields);
    }
}
