//! ITK MetaImage (`.mha` with the data attached, `.mhd` with the data in
//! another file): a text header of `Key = Value` lines ending with
//! `ElementDataFile`.
//!
//! Geometry as ITK reads it: physical (LPS) = `Offset` + Σᵢ indexᵢ ·
//! `ElementSpacing`ᵢ · row i of `TransformMatrix` (the direction of axis i),
//! turned into RAS. Data: raw, or zlib-compressed (`CompressedData = True`,
//! decoded whole); several channels per voxel (`ElementNumberOfChannels`)
//! are an RGB image (3 or 4 unsigned chars) or volumes. A list or a pattern
//! of data files is refused.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{bail, PreviewError, Result};
use crate::header::{clean_text, DType, Endian};
use crate::image::{DataFile, DataLayout, Decode, Decoder, Format, ImageHeader, StorageAxis};

pub const MAX_HEADER_BYTES: usize = 1 << 20;

fn invalid(what: impl std::fmt::Display) -> PreviewError {
    PreviewError::new("invalid_image", format!("Invalid MetaImage header: {what}"))
}

/// Whether the content starts like a MetaImage header.
pub fn looks_like(probe: &[u8]) -> bool {
    let first = probe.split(|&c| c == b'\n').next().unwrap_or(&[]);
    let Ok(first) = std::str::from_utf8(first) else { return false };
    let key = first.split('=').next().unwrap_or("").trim();
    first.contains('=') && matches!(key, "ObjectType" | "NDims" | "Comment" | "ObjectSubType" | "FormTypeName")
        && (key != "ObjectType" || first.split('=').nth(1).is_some_and(|v| v.trim() == "Image"))
}

/// Length of the header (through the `ElementDataFile` line), if complete.
pub fn header_end(text: &[u8]) -> Option<usize> {
    let mut start = 0;
    while let Some(nl) = text[start..].iter().position(|&c| c == b'\n') {
        let line = &text[start..start + nl];
        let key = line.split(|&c| c == b'=').next().unwrap_or(&[]);
        if key.trim_ascii() == b"ElementDataFile" {
            return Some(start + nl + 1);
        }
        start += nl + 1;
    }
    None
}

fn dtype(t: &str) -> Result<DType> {
    Ok(match t.trim() {
        "MET_UCHAR" => DType::U8,
        "MET_CHAR" => DType::I8,
        "MET_USHORT" => DType::U16,
        "MET_SHORT" => DType::I16,
        "MET_UINT" | "MET_ULONG" => DType::U32,
        "MET_INT" | "MET_LONG" => DType::I32,
        "MET_ULONG_LONG" => DType::U64,
        "MET_LONG_LONG" => DType::I64,
        "MET_FLOAT" => DType::F32,
        "MET_DOUBLE" => DType::F64,
        t => bail!("unsupported_datatype", "MetaImage element type {t} is not supported"),
    })
}

/// zlib-compressed data, decoded whole.
#[derive(Debug)]
struct Zlib {
    path: PathBuf,
    offset: u64,
    bytes: u64,
    compressed: u64,
}

impl Decode for Zlib {
    fn decode(&self, max_bytes: u64) -> Result<Vec<u8>> {
        if self.bytes > max_bytes {
            return Err(crate::source::too_large("voxel data", self.bytes, max_bytes));
        }
        let file = crate::source::open_data_file(&self.path)?;
        crate::progress::expect(self.compressed, false);
        let reader = std::io::BufReader::new(crate::source::At { file: &file, pos: self.offset });
        // Grown as the data arrive: a short stream allocates little.
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(reader).take(self.bytes).read_to_end(&mut out).map_err(crate::source::corrupt)?;
        if (out.len() as u64) < self.bytes {
            bail!("corrupt_data", "Image data could not be read; the compressed data hold {} of {} bytes.", out.len(), self.bytes);
        }
        Ok(out)
    }

    fn input_bytes(&self) -> u64 {
        self.compressed
    }
}

/// Parse the header `text` of the file at `path`; the data start at
/// `data_start` in it (attached data).
pub fn parse(text: &[u8], data_start: u64, path: &Path, file_size: u64) -> Result<ImageHeader> {
    let text = std::str::from_utf8(text).map_err(|_| invalid("not text"))?;
    let mut pairs: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        pairs.push((k.trim().to_string(), v.trim().to_string()));
        if k.trim() == "ElementDataFile" {
            break;
        }
    }
    let get = |k: &str| pairs.iter().find(|(key, _)| key.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str());
    let yes = |k: &str| get(k).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let numbers = |k: &str| -> Option<Vec<f64>> {
        get(k).map(|v| v.split_whitespace().map(|x| x.parse::<f64>().unwrap_or(f64::NAN)).collect())
    };
    if get("ObjectType").is_some_and(|t| t != "Image") {
        bail!("unsupported_format", "MetaImage object of type {} is not an image", clean_text(get("ObjectType").unwrap()));
    }
    let ndims: usize = get("NDims").and_then(|v| v.parse().ok()).filter(|n| (1..=7).contains(n)).ok_or_else(|| invalid("NDims"))?;
    let sizes: Vec<usize> = get("DimSize")
        .map(|v| v.split_whitespace().map(|x| x.parse().unwrap_or(0)).collect())
        .filter(|s: &Vec<usize>| s.len() >= ndims && s.iter().take(ndims).all(|&d| d > 0 && d <= 1 << 24))
        .ok_or_else(|| invalid("DimSize"))?;
    let sizes = &sizes[..ndims];
    let mut dt = dtype(get("ElementType").ok_or_else(|| invalid("ElementType"))?)?;
    let channels: usize = get("ElementNumberOfChannels").and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
    if channels > 1024 {
        return Err(invalid(format_args!("{channels} channels")));
    }
    let endian = if yes("BinaryDataByteOrderMSB") || yes("ElementByteOrderMSB") { Endian::Big } else { Endian::Little };
    if get("BinaryData").is_some_and(|v| v.eq_ignore_ascii_case("false")) {
        bail!("unsupported_format", "MetaImage files with text data are not supported");
    }

    // Logical axes: the spatial ones (at most three), then channels as volumes.
    let colour = dt == DType::U8 && matches!(channels, 3 | 4);
    if colour {
        dt = if channels == 3 { DType::Rgb24 } else { DType::Rgba32 };
    }
    let ns = ndims.min(3);
    let mut shape: Vec<usize> = sizes[..ns].to_vec();
    shape.resize(3, 1);
    shape.extend_from_slice(&sizes[ns..]);
    let mut layout = DataLayout::canonical(0);
    if channels > 1 && !colour {
        // Channels vary fastest in the file: a last logical axis of rank 0.
        shape.push(channels);
        let n = shape.len();
        layout.axes = (0..n).map(|a| StorageAxis { rank: if a == n - 1 { 0 } else { a + 1 }, reversed: false }).collect();
    }

    // Geometry (LPS), then RAS.
    let spacing = numbers("ElementSpacing").or(numbers("ElementSize")).unwrap_or_default();
    let origin = numbers("Offset").or(numbers("Position")).or(numbers("Origin")).unwrap_or_default();
    let matrix = numbers("TransformMatrix").or(numbers("Rotation")).or(numbers("Orientation"));
    let mut affine = [[0.0; 4]; 4];
    affine[3][3] = 1.0;
    let mut zooms = vec![1.0; shape.len()];
    let mut oriented = ndims >= 2;
    for i in 0..3 {
        let s = spacing.get(i).copied().filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
        zooms[i] = s;
        let dir: [f64; 3] = match &matrix {
            Some(m) if m.len() >= ndims * ndims && i < ndims => {
                std::array::from_fn(|r| if r < ndims { m[i * ndims + r] } else { 0.0 })
            }
            _ => std::array::from_fn(|r| if r == i { 1.0 } else { 0.0 }),
        };
        if !dir.iter().all(|x| x.is_finite()) {
            oriented = false;
        }
        for r in 0..3 {
            let sign = if r < 2 { -1.0 } else { 1.0 };
            affine[r][i] = sign * dir[r] * s;
        }
    }
    if ndims < 3 {
        oriented = false;
    }
    for (r, row) in affine.iter_mut().enumerate().take(3) {
        let o = origin.get(r).copied().filter(|x| x.is_finite()).unwrap_or(0.0);
        row[3] = if r < 2 { -o } else { o };
    }
    let total = shape.iter().try_fold(dt.itemsize() as u64, |a, &d| a.checked_mul(d as u64)).ok_or_else(|| invalid("too large"))?;

    // Where the data are.
    let spec = get("ElementDataFile").ok_or_else(|| invalid("ElementDataFile"))?;
    let header_size: i64 = get("HeaderSize").and_then(|v| v.parse().ok()).unwrap_or(0);
    let compressed = yes("CompressedData");
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let (data_path, start) = if spec.eq_ignore_ascii_case("LOCAL") {
        (path.to_path_buf(), data_start)
    } else if spec.starts_with("LIST") || spec.split_whitespace().count() > 1 {
        bail!("unsupported_format", "MetaImage data in several files are not supported");
    } else {
        (crate::image::data_file_in(&dir, spec, "MetaImage")?, 0)
    };
    let local = data_path == path;
    if compressed {
        if header_size == -1 {
            bail!("unsupported_format", "MetaImage HeaderSize -1 with compressed data is not supported");
        }
        let offset = start + header_size.max(0) as u64;
        let size = if local { file_size } else { std::fs::metadata(&data_path).map(|m| m.len()).unwrap_or(0) };
        let compressed_size = get("CompressedDataSize").and_then(|v| v.parse().ok()).unwrap_or(size.saturating_sub(offset));
        layout.decoder = Some(Decoder(std::sync::Arc::new(Zlib { path: data_path.clone(), offset, bytes: total, compressed: compressed_size })));
    } else if local {
        if header_size == -1 {
            layout.from_end = true;
            layout.offset = start;
        } else {
            layout.offset = start + header_size.max(0) as u64;
        }
    } else {
        layout.files = vec![DataFile { path: data_path.clone(), line_skip: 0, byte_skip: header_size }];
    }

    let mut fields = Vec::new();
    if let Some(o) = get("AnatomicalOrientation") {
        fields.push(("Anatomical orientation".to_string(), clean_text(o)));
    }
    fields.push(("Encoding".to_string(), if compressed { "zlib" } else { "raw" }.to_string()));
    if !local {
        fields.push(("Data file".to_string(), clean_text(&data_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())));
    }
    if channels > 1 && !colour {
        fields.push(("Channels".to_string(), format!("{channels} (as volumes)")));
    }
    if let Some(m) = get("Modality") {
        fields.push(("Modality".to_string(), clean_text(m)));
    }
    let mut h = ImageHeader {
        format: Format::MetaImage,
        shape,
        dtype: dt,
        endian,
        zooms,
        slope: 1.0,
        inter: 0.0,
        affine,
        affine_source: if oriented { "TransformMatrix" } else { "none" },
        space_units: "mm",
        time_units: "unknown",
        repetition_time: None,
        intent_code: 0,
        descrip: get("Comment").map(clean_text).unwrap_or_default(),
        warnings: Vec::new(),
        fields,
        layout,
        nifti: None,
        dicom: None,
    };
    h.normalize_storage();
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_and_end() {
        assert!(looks_like(b"ObjectType = Image\nNDims = 3\n"));
        assert!(looks_like(b"NDims = 2\n"));
        assert!(!looks_like(b"ObjectType = Tube\n"));
        assert!(!looks_like(b"mrtrix image\n"));
        let h = b"ObjectType = Image\nNDims = 3\nElementDataFile = LOCAL\nDATA";
        assert_eq!(header_end(h), Some(h.len() - 4));
    }

    #[test]
    fn geometry_as_itk() {
        let text = b"ObjectType = Image\nNDims = 3\nTransformMatrix = 1 0 0 0 -1 0 0 0 1\nOffset = -10 20 -30\n\
ElementSpacing = 2 3 4\nDimSize = 5 6 7\nElementType = MET_SHORT\nElementDataFile = LOCAL\n";
        let h = parse(text, text.len() as u64, Path::new("/x.mha"), 10_000).unwrap();
        assert_eq!(h.shape, vec![5, 6, 7]);
        assert_eq!(h.dtype, DType::I16);
        // Axis 1 points to -y in LPS: +y in RAS.
        assert_eq!(h.affine[0], [-2.0, 0.0, 0.0, 10.0]);
        assert_eq!(h.affine[1], [0.0, 3.0, 0.0, -20.0]);
        assert_eq!(h.affine[2], [0.0, 0.0, 4.0, -30.0]);
        assert_eq!(h.layout.offset, text.len() as u64);
    }
}
