//! FreeSurfer MGH / MGZ (`.mgh`, `.mgz`, `.mgh.gz`).
//!
//! A 284-byte big-endian header, then the voxel data (Fortran order, frames
//! last), then optional scan parameters (TR, flip angle, TE, TI). The
//! voxel-to-RAS affine is FreeSurfer's (and nibabel's): the direction cosines
//! `Mdc` scaled by the voxel sizes, with the centre `c_ras` at voxel
//! `dims / 2`. When `goodRASFlag` is 0 the orientation is unknown.

use crate::error::{bail, Result};
use crate::header::{DType, Endian};
use crate::image::{DataLayout, Format, ImageHeader};

pub const HEADER_SIZE: usize = 284;
/// Largest dimension accepted (FreeSurfer volumes are a few hundred voxels;
/// surfaces stored as 1D volumes reach a few hundred thousand).
const MAX_DIM: i32 = 1 << 24;

fn be_i32(b: &[u8], o: usize) -> i32 {
    i32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn be_f32(b: &[u8], o: usize) -> f64 {
    f32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as f64
}

fn invalid(what: impl std::fmt::Display) -> crate::error::PreviewError {
    crate::error::PreviewError::new("invalid_image", format!("Invalid MGH header: {what}"))
}

/// Parse the header (`b` starts at the beginning of the decoded stream).
pub fn parse(b: &[u8]) -> Result<ImageHeader> {
    if b.len() < HEADER_SIZE {
        return Err(invalid("file too short"));
    }
    if be_i32(b, 0) != 1 {
        return Err(invalid("unknown version"));
    }
    let dims: Vec<i32> = (0..4).map(|i| be_i32(b, 4 + 4 * i)).collect();
    if dims.iter().any(|&d| !(1..=MAX_DIM).contains(&d)) {
        return Err(invalid(format_args!("dimensions {dims:?}")));
    }
    let dtype = match be_i32(b, 20) {
        0 => DType::U8,
        1 => DType::I32,
        3 => DType::F32,
        4 => DType::I16,
        10 => DType::U16,
        t => bail!("unsupported_datatype", "MGH datatype {t} is not supported"),
    };
    let good_ras = i16::from_be_bytes([b[28], b[29]]) != 0;
    let mut warnings = Vec::new();
    let mut spacing = [be_f32(b, 30), be_f32(b, 34), be_f32(b, 38)];
    // Direction cosines: columns x, y, z; then the centre.
    let mut mdc = [[0.0f64; 3]; 3];
    for (col, base) in [42usize, 54, 66].into_iter().enumerate() {
        for (row, cosines) in mdc.iter_mut().enumerate() {
            cosines[col] = be_f32(b, base + 4 * row);
        }
    }
    let centre = [be_f32(b, 78), be_f32(b, 82), be_f32(b, 86)];
    let finite = spacing.iter().chain(mdc.iter().flatten()).chain(centre.iter()).all(|v| v.is_finite());
    let usable = good_ras && finite;
    if !usable {
        // FreeSurfer's defaults: coronal slices, 1 mm voxels.
        spacing = [1.0; 3];
        mdc = [[-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]];
        if good_ras {
            warnings.push("non-finite MGH geometry ignored".to_string());
        }
    }
    for s in spacing.iter_mut() {
        if !(s.is_finite() && *s > 0.0) {
            *s = 1.0;
        }
    }
    let shape: Vec<usize> = if dims[3] > 1 { dims.iter().map(|&d| d as usize).collect() } else { dims[..3].iter().map(|&d| d as usize).collect() };
    let mut affine = [[0.0; 4]; 4];
    for r in 0..3 {
        for (c, s) in spacing.iter().enumerate().take(3) {
            affine[r][c] = mdc[r][c] * s;
        }
        let c = if usable { centre[r] } else { 0.0 };
        affine[r][3] = c - (0..3).map(|k| affine[r][k] * (dims[k] as f64 / 2.0)).sum::<f64>();
    }
    affine[3][3] = 1.0;
    let mut zooms = spacing.to_vec();
    if shape.len() == 4 {
        zooms.push(0.0); // the TR comes from the scan parameters after the data, when read
    }
    let fields = vec![("Frames".to_string(), dims[3].to_string())];
    Ok(ImageHeader {
        format: Format::Mgh,
        shape,
        dtype,
        endian: Endian::Big,
        zooms,
        slope: 1.0,
        inter: 0.0,
        affine,
        affine_source: if usable { "vox2ras" } else { "none" },
        space_units: "mm",
        time_units: "msec",
        repetition_time: None,
        intent_code: 0,
        descrip: String::new(),
        warnings,
        fields: if dims[3] > 1 { fields } else { Vec::new() },
        layout: DataLayout::canonical(HEADER_SIZE as u64),
        nifti: None,
        dicom: None,
    })
}

/// Scan parameters after the voxel data: TR (ms), flip angle (rad), TE, TI (ms).
pub fn scan_parameters(h: &mut ImageHeader, tail: &[u8]) {
    if tail.len() < 16 {
        return;
    }
    let (tr, flip, te, ti) = (be_f32(tail, 0), be_f32(tail, 4), be_f32(tail, 8), be_f32(tail, 12));
    let ms = |v: f64| format!("{} ms", crate::volume::fmt_g(v));
    if tr.is_finite() && tr > 0.0 {
        h.fields.push(("TR".into(), ms(tr)));
        if h.ndim() == 4 {
            h.repetition_time = Some(tr);
            h.zooms[3] = tr;
        }
    }
    if te.is_finite() && te > 0.0 {
        h.fields.push(("TE".into(), ms(te)));
    }
    if ti.is_finite() && ti > 0.0 {
        h.fields.push(("TI".into(), ms(ti)));
    }
    if flip.is_finite() && flip > 0.0 {
        h.fields.push(("Flip angle".into(), format!("{}°", crate::volume::fmt_g(flip.to_degrees()))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(dims: [i32; 4], dtype: i32) -> Vec<u8> {
        let mut b = vec![0u8; HEADER_SIZE];
        let put_i = |b: &mut Vec<u8>, o: usize, v: i32| b[o..o + 4].copy_from_slice(&v.to_be_bytes());
        let put_f = |b: &mut Vec<u8>, o: usize, v: f32| b[o..o + 4].copy_from_slice(&v.to_be_bytes());
        put_i(&mut b, 0, 1);
        for (i, d) in dims.iter().enumerate() {
            put_i(&mut b, 4 + 4 * i, *d);
        }
        put_i(&mut b, 20, dtype);
        b[28..30].copy_from_slice(&1i16.to_be_bytes());
        for (i, s) in [1.0f32, 2.0, 3.0].iter().enumerate() {
            put_f(&mut b, 30 + 4 * i, *s);
        }
        // Mdc = LIA (FreeSurfer's conformed space), centre (10, 20, 30).
        for (o, v) in [(42, -1.0f32), (62, -1.0), (70, 1.0), (78, 10.0), (82, 20.0), (86, 30.0)] {
            put_f(&mut b, o, v);
        }
        b
    }

    #[test]
    fn affine_like_nibabel() {
        let h = parse(&header([4, 6, 8, 1], 3)).unwrap();
        assert_eq!(h.shape, vec![4, 6, 8]);
        assert_eq!(h.dtype, DType::F32);
        // nibabel: MdcD = Mdc * delta; affine[:3, 3] = c_ras - MdcD @ dims / 2
        assert_eq!(h.affine[0], [-1.0, 0.0, 0.0, 10.0 + 2.0]);
        assert_eq!(h.affine[1], [0.0, 0.0, 3.0, 20.0 - 12.0]);
        assert_eq!(h.affine[2], [0.0, -2.0, 0.0, 30.0 + 6.0]);
        assert_eq!(h.affine_source, "vox2ras");
    }

    #[test]
    fn frames_and_errors() {
        let h = parse(&header([4, 6, 8, 5], 4)).unwrap();
        assert_eq!(h.shape, vec![4, 6, 8, 5]);
        assert_eq!(h.n_volumes(), 5);
        assert_eq!(parse(&header([4, 0, 8, 1], 3)).unwrap_err().code, "invalid_image");
        assert_eq!(parse(&header([4, 6, 8, 1], 5)).unwrap_err().code, "unsupported_datatype");
        let mut no_ras = header([4, 6, 8, 1], 0);
        no_ras[28..30].copy_from_slice(&0i16.to_be_bytes());
        assert_eq!(parse(&no_ras).unwrap().affine_source, "none");
    }
}
