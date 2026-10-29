//! Format-neutral description of an image file.
//!
//! Every reader (NIfTI, MGH, MRtrix, NRRD, DICOM) turns its own header into an
//! [`ImageHeader`]: the shape, the datatype, the scaling, the voxel-to-world
//! affine (in RAS, like NIfTI), and where the voxel data are and how they are
//! laid out. The rest of the backend (orientation, windowing, labels,
//! rendering, protocol) only sees this description.

use crate::error::Result;
use crate::header::{self, DType, Endian, Header};
use crate::orientation::Mat4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Nifti1,
    Nifti2,
    Mgh,
    Mif,
    Nrrd,
    MetaImage,
    Dicom,
}

impl Format {
    /// The name shown in the metadata panel (`header.format`).
    pub fn label(self, compressed: bool) -> &'static str {
        match self {
            Format::Nifti1 => "NIfTI-1",
            Format::Nifti2 => "NIfTI-2",
            Format::Mgh if compressed => "MGZ",
            Format::Mgh => "MGH",
            Format::Mif => "MRtrix",
            Format::Nrrd => "NRRD",
            Format::MetaImage => "MetaImage",
            Format::Dicom => "DICOM",
        }
    }

    pub fn is_nifti(self) -> bool {
        matches!(self, Format::Nifti1 | Format::Nifti2)
    }
}

/// Where the encoded voxel data start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// In the file as read: raw, or inside the gzip stream of a gzipped file
    /// (`offset` is then a position in the decompressed stream).
    Stream,
    /// A gzip stream that starts at this position of the raw file, after a
    /// plain text header (NRRD `encoding: gzip`); `offset` is then a position
    /// in the decompressed stream.
    GzipAt(u64),
}

/// One logical axis as stored on disk: its rank in the storage order (0 is
/// the fastest-varying) and whether it is stored reversed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageAxis {
    pub rank: usize,
    pub reversed: bool,
}

/// A file holding (part of) the voxel data of a detached header (NRRD `.nhdr`).
#[derive(Debug, Clone, PartialEq)]
pub struct DataFile {
    pub path: std::path::PathBuf,
    /// The folder the file must stay in (see [`data_file_in`]); None for
    /// the header's own file.
    pub root: Option<std::path::PathBuf>,
    /// Lines, then bytes, to skip at the start of the file (after
    /// decompression for bytes); `byte_skip` -1: the data end the file.
    pub line_skip: usize,
    pub byte_skip: i64,
}

/// Where the voxel data are and how they are ordered.
#[derive(Debug, Clone, PartialEq)]
pub struct DataLayout {
    pub offset: u64,
    pub encoding: Encoding,
    /// Per logical axis; empty means the canonical order (Fortran order of
    /// the logical axes, none reversed), like NIfTI.
    pub axes: Vec<StorageAxis>,
    /// MRtrix `Bit`: one bit per voxel, most significant first (shown as uint8).
    pub bits: bool,
    /// Detached data (NRRD `.nhdr`): these files, one after the other, hold
    /// the data; `gzip` tells whether each is a gzip stream.
    pub files: Vec<DataFile>,
    pub files_gzip: bool,
    /// The data end the file (NRRD `byte skip: -1`): `offset` is resolved
    /// from the file size.
    pub from_end: bool,
    /// Data only a decoder can produce (DICOM): decoded whole, in canonical order.
    pub decoder: Option<Decoder>,
}

/// Voxel data that only a decoder can produce (DICOM: compressed frames,
/// frames spread over the files of a series, colour conversions). It
/// decodes all of them at once, in the canonical order of the header, as
/// its datatype (little endian).
pub trait Decode: Send + Sync + std::fmt::Debug {
    fn decode(&self, max_bytes: u64) -> Result<Vec<u8>>;
    /// What could not be decoded (parts left black), after `decode`.
    fn notes(&self) -> Vec<String> {
        Vec::new()
    }
    /// About how many bytes decoding reads.
    fn input_bytes(&self) -> u64 {
        0
    }
}

#[derive(Clone, Debug)]
pub struct Decoder(pub std::sync::Arc<dyn Decode>);

impl PartialEq for Decoder {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl DataLayout {
    pub fn canonical(offset: u64) -> Self {
        DataLayout {
            offset,
            encoding: Encoding::Stream,
            axes: Vec::new(),
            bits: false,
            files: Vec::new(),
            files_gzip: false,
            from_end: false,
            decoder: None,
        }
    }

    pub fn is_canonical(&self) -> bool {
        self.axes.iter().enumerate().all(|(i, a)| a.rank == i && !a.reversed)
    }

    /// Volumes can be read one at a time where they are stored (canonical
    /// order, one stream in the file, whole bytes, nothing to decode).
    pub fn is_direct(&self) -> bool {
        self.is_canonical() && !self.bits && self.files.is_empty() && self.decoder.is_none()
    }

    /// Element strides of the logical axes in the stored data, and the
    /// element of logical index 0 (reversed axes count from their end).
    pub fn strides(&self, shape: &[usize]) -> (Vec<i64>, i64) {
        let n = shape.len();
        let axes: Vec<StorageAxis> = if self.axes.len() == n {
            self.axes.clone()
        } else {
            (0..n).map(|rank| StorageAxis { rank, reversed: false }).collect()
        };
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&a| axes[a].rank);
        let mut strides = vec![0i64; n];
        let mut step = 1i64;
        for &a in &order {
            strides[a] = step;
            step = step.saturating_mul(shape[a] as i64);
        }
        let mut base = 0i64;
        for a in 0..n {
            if axes[a].reversed {
                base += (shape[a] as i64 - 1) * strides[a];
                strides[a] = -strides[a];
            }
        }
        (strides, base)
    }
}

#[derive(Debug, Clone)]
pub struct ImageHeader {
    pub format: Format,
    /// Logical axes: the three spatial axes first, then the others.
    pub shape: Vec<usize>,
    pub dtype: DType,
    pub endian: Endian,
    /// Spacing along each logical axis (spatial units, then time units).
    pub zooms: Vec<f64>,
    /// Normalised: (1, 0) when the file carries no valid scaling.
    pub slope: f64,
    pub inter: f64,
    /// Voxel indices to world coordinates (RAS, in `space_units`).
    pub affine: Mat4,
    /// What the affine comes from (`sform`, `qform`, `vox2ras`, `transform`,
    /// `space directions`, `DICOM`), or `none` when the file gives no
    /// orientation.
    pub affine_source: &'static str,
    pub space_units: &'static str,
    pub time_units: &'static str,
    /// Time between volumes, in `time_units`, when the file gives it.
    pub repetition_time: Option<f64>,
    /// NIfTI intent code; label formats without one use NIFTI_INTENT_LABEL.
    pub intent_code: i32,
    pub descrip: String,
    /// Repairs applied to an out-of-spec header (shown to the user).
    pub warnings: Vec<String>,
    /// Format-specific details for the metadata panel, in display order.
    pub fields: Vec<(String, String)>,
    pub layout: DataLayout,
    /// The NIfTI header itself (qform/sform, intent, spectroscopy).
    pub nifti: Option<Header>,
    /// DICOM: display defaults, where to open, patient details, other series.
    pub dicom: Option<Box<crate::formats::dicom::Info>>,
}

impl ImageHeader {
    pub fn from_nifti(h: Header) -> Result<ImageHeader> {
        let affine = h.best_affine()?;
        let repetition_time = (h.shape.len() > 3).then(|| h.pixdim[4]);
        Ok(ImageHeader {
            format: if h.version == 2 { Format::Nifti2 } else { Format::Nifti1 },
            shape: h.shape.clone(),
            dtype: h.dtype,
            endian: h.endian,
            zooms: h.zooms(),
            slope: h.slope,
            inter: h.inter,
            affine,
            affine_source: h.affine_source(),
            space_units: h.space_units(),
            time_units: h.time_units(),
            repetition_time,
            intent_code: h.intent_code,
            descrip: h.descrip.clone(),
            warnings: h.warnings.clone(),
            fields: Vec::new(),
            layout: DataLayout::canonical(h.vox_offset),
            nifti: Some(h),
            dicom: None,
        })
    }

    pub fn ndim(&self) -> usize {
        self.shape.len()
    }

    /// Put the spatial axes (the first three) in their storage order, and
    /// count reversed ones from their stored end, by changing the affine: the
    /// voxel grid becomes the stored one, so its data need no copy. What may
    /// remain is the place of the other axes among them (volumes interleaved
    /// with voxels), handled when volumes are read.
    pub fn normalize_storage(&mut self) {
        let n = self.shape.len();
        if self.layout.axes.len() != n {
            self.layout.axes.clear();
            return;
        }
        let ns = n.min(3);
        let mut spatial: Vec<usize> = (0..ns).collect();
        spatial.sort_by_key(|&a| self.layout.axes[a].rank);
        let old = self.affine;
        let mut affine = old;
        for (j, &a) in spatial.iter().enumerate() {
            let reversed = self.layout.axes[a].reversed;
            for r in 0..3 {
                affine[r][j] = if reversed { -old[r][a] } else { old[r][a] };
                if reversed {
                    affine[r][3] += old[r][a] * (self.shape[a] as f64 - 1.0);
                }
            }
        }
        self.affine = affine;
        let (shape, zooms, axes) = (self.shape.clone(), self.zooms.clone(), self.layout.axes.clone());
        for (j, &a) in spatial.iter().enumerate() {
            self.shape[j] = shape[a];
            if let Some(z) = zooms.get(a) {
                self.zooms[j] = *z;
            }
            self.layout.axes[j] = StorageAxis { rank: axes[a].rank, reversed: false };
        }
        if self.layout.is_canonical() {
            self.layout.axes.clear();
        }
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

    /// Bytes of one 3D volume, None on overflow.
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
        self.zooms.clone()
    }

    pub fn identity_scaling(&self) -> bool {
        self.slope == 1.0 && self.inter == 0.0
    }

    pub fn space_units(&self) -> &'static str {
        self.space_units
    }

    pub fn time_units(&self) -> &'static str {
        self.time_units
    }

    pub fn affine_source(&self) -> &'static str {
        self.affine_source
    }

    /// Byte offset of the voxel data (in the decoded stream).
    pub fn data_offset(&self) -> u64 {
        self.layout.offset
    }
}

/// The file formats recognised from the first bytes of the (decompressed)
/// content, whatever the file name.
pub fn detect(probe: &[u8]) -> Option<Format> {
    // DICOM first: the 128-byte preamble before "DICM" is free and some files
    // carry a NIfTI-like size there.
    if probe.len() >= 132 && &probe[128..132] == b"DICM" {
        return Some(Format::Dicom);
    }
    if probe.len() >= 4 {
        let first = [probe[0], probe[1], probe[2], probe[3]];
        let (le, be) = (i32::from_le_bytes(first), i32::from_be_bytes(first));
        if le == 348 || be == 348 {
            return Some(Format::Nifti1);
        }
        if le == 540 || be == 540 {
            return Some(Format::Nifti2);
        }
    }
    if probe.starts_with(b"mrtrix image") {
        return Some(Format::Mif);
    }
    if probe.starts_with(b"NRRD000") {
        return Some(Format::Nrrd);
    }
    if crate::formats::metaimage::looks_like(probe) {
        return Some(Format::MetaImage);
    }
    if looks_like_mgh(probe) {
        return Some(Format::Mgh);
    }
    // DICOM written without its preamble (non-conformant, but common).
    if crate::formats::dicom::parse::starts_without_preamble(probe) {
        return Some(Format::Dicom);
    }
    None
}

/// MGH has no magic number: version 1 (big-endian), positive dimensions and
/// a known datatype.
fn looks_like_mgh(b: &[u8]) -> bool {
    if b.len() < 30 {
        return false;
    }
    let int = |o: usize| i32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    // version, width, height, depth, frames, type
    int(0) == 1 && (1..5).all(|i| int(4 * i) > 0) && [0, 1, 3, 4, 10].contains(&int(20))
}

/// A data file named by a header (NRRD `.nhdr`, MRtrix `.mih`, MetaImage
/// `.mhd`), in the header's folder `dir`: never an absolute path or one
/// through `..`, so that a header cannot show other files. Returns its path
/// and the folder it must stay in: a symbolic link that leads elsewhere is
/// refused when the file is opened ([`crate::source::open_data_file_in`]).
pub fn data_file_in(dir: &std::path::Path, name: &str, format: &str) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let p = std::path::Path::new(name);
    if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(crate::error::PreviewError::new(
            "unsupported_format",
            format!("{format} data file outside the header's folder: {}", header::clean_text(name)),
        ));
    }
    let root = if dir.as_os_str().is_empty() { std::path::Path::new(".") } else { dir };
    Ok((dir.join(p), root.to_path_buf()))
}

/// Error for content that no reader recognises.
pub fn unrecognised() -> crate::error::PreviewError {
    crate::error::PreviewError::new(
        "invalid_image",
        "Not a recognised image file (NIfTI, MGH/MGZ, MRtrix, NRRD, MetaImage or DICOM)",
    )
}

/// Parse a NIfTI header from the probe (kept apart so that the NIfTI error
/// messages stay specific).
pub fn parse_nifti(probe: &[u8]) -> Result<ImageHeader> {
    ImageHeader::from_nifti(header::parse(probe)?)
}

/// `bail!` for readers of text headers: the line that could not be used.
pub fn bad_header(format: Format, what: impl std::fmt::Display) -> crate::error::PreviewError {
    crate::error::PreviewError::new("invalid_image", format!("Invalid {} header: {what}", format.label(false)))
}
