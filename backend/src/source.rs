//! File access: gzip detection by magic bytes, format detection from the
//! content, header probe and extensions, and loading of the first 3D volume,
//! or of all the data of a spectroscopy file (read or mmap for uncompressed
//! files, libdeflate or streaming inflate for gzip).
//!
//! The file is opened once and every later read goes through that descriptor,
//! so swapping the path for a FIFO or a device after the type check has no
//! effect. Every allocation sized from the header is bounded by the image
//! limit (`max_bytes`) or by `MAX_VOX_OFFSET`, and is fallible.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;

use flate2::read::MultiGzDecoder;

use crate::error::{bail, PreviewError, Result};
use crate::header::{self, Extension};
use crate::formats::{metaimage, mgh, mif, nrrd};
use crate::image::{self, Encoding, Format, ImageHeader};

/// Largest accepted vox_offset (header + extensions). Real extensions are a
/// few MB at most; larger values are corrupt or hostile.
pub const MAX_VOX_OFFSET: u64 = 256 << 20;
/// Uncompressed files up to this size are read into memory; larger ones are
/// memory-mapped (a file truncated while mapped would kill the backend).
pub const READ_BELOW: u64 = 256 << 20;

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "arm", target_arch = "riscv64")))]
const O_NONBLOCK: i32 = 0o4000;
#[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "arm", target_arch = "riscv64"))))]
const O_NONBLOCK: i32 = 0;

pub enum Storage {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
    /// A mapping shared by the volumes of a large uncompressed 4D file.
    SharedMap(std::sync::Arc<memmap2::Mmap>),
    /// A volume decoded by the background inflater of a gzipped 4D file.
    Shared(std::sync::Arc<Vec<u8>>),
}

impl Storage {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Storage::Mapped(m) => m,
            Storage::Owned(v) => v,
            Storage::SharedMap(m) => m,
            Storage::Shared(v) => v,
        }
    }
}

pub struct Opened {
    pub header: ImageHeader,
    pub extensions: Vec<Extension>,
    pub compressed: bool,
    pub file_size: u64,
    pub file: File,
    /// Local disk or network file system.
    pub storage: crate::storage::Storage,
}

pub(crate) fn io_error(e: std::io::Error) -> PreviewError {
    match e.kind() {
        std::io::ErrorKind::NotFound => PreviewError::new("not_found", "File not found"),
        _ => PreviewError::new("not_found", format!("Cannot access file: {e}")),
    }
}

/// A zero-filled buffer, or `too_large` when memory is short (instead of the
/// process abort of an infallible allocation).
pub fn zeroed(n: usize) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| PreviewError::new("too_large", "Not enough memory to load this volume."))?;
    v.resize(n, 0);
    Ok(v)
}

/// Sequential reader over a file from a given position (positional reads, so
/// it never depends on the descriptor's offset).
pub(crate) struct At<F: std::borrow::Borrow<File>> {
    pub(crate) file: F,
    pub(crate) pos: u64,
}

impl<F: std::borrow::Borrow<File>> Read for At<F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if crate::progress::cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        let want = buf.len().min(READ_BLOCK);
        let n = self.file.borrow().read_at(&mut buf[..want], self.pos)?;
        self.pos += n as u64;
        crate::progress::advance(n as u64);
        Ok(n)
    }
}

/// Blocks of a long read: the progress of a load moves between them, and a
/// cancelled load stops.
pub const READ_BLOCK: usize = 4 << 20;

/// `read_exact_at` in blocks, reporting progress.
pub(crate) fn read_exact_in_blocks(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        if crate::progress::cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        let n = (buf.len() - done).min(READ_BLOCK);
        file.read_exact_at(&mut buf[done..done + n], offset + done as u64)?;
        crate::progress::advance(n as u64);
        done += n;
    }
    Ok(())
}

/// Read up to `buf.len()` bytes, stopping early only at end of stream.
pub(crate) fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

/// How files are opened (DICOM: whether to rebuild the series from the folder).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadOptions {
    pub dicom: crate::formats::dicom::Options,
}

pub fn open(path: &Path) -> Result<Opened> {
    open_with(path, ReadOptions::default())
}

pub fn open_with(path: &Path, options: ReadOptions) -> Result<Opened> {
    // Cheap check first, so that a device is never even opened.
    if !std::fs::metadata(path).map_err(io_error)?.is_file() {
        bail!("not_a_file", "Not a regular file");
    }
    // Non-blocking: if the path was swapped for a FIFO meanwhile, open returns
    // at once and the check on the descriptor below rejects it.
    let file = OpenOptions::new().read(true).custom_flags(O_NONBLOCK).open(path).map_err(io_error)?;
    let meta = file.metadata().map_err(io_error)?;
    if !meta.is_file() {
        bail!("not_a_file", "Not a regular file");
    }
    let mut magic = [0u8; 2];
    let got = read_full(&mut At { file: &file, pos: 0 }, &mut magic).map_err(io_error)?;
    let compressed = got == 2 && magic == [0x1f, 0x8b];
    let mut probe = vec![0u8; header::HEADER_PROBE];
    let gz_error = |e: std::io::Error| PreviewError::new("invalid_image", format!("Not a readable image file (gzip: {e})"));
    let mut dec = compressed.then(|| MultiGzDecoder::new(BufReader::new(At { file: &file, pos: 0 })));
    let n = match dec.as_mut() {
        Some(d) => read_full(d, &mut probe).map_err(gz_error)?,
        None => read_full(&mut At { file: &file, pos: 0 }, &mut probe).map_err(io_error)?,
    };
    let probe = &probe[..n];
    let file_size = meta.len();
    let storage = crate::storage::of(&file, path);
    if storage.network {
        crate::storage::advise_sequential(&file);
    }
    let header = match image::detect(probe) {
        Some(Format::Nifti1 | Format::Nifti2) => {
            let (header, extensions) = open_nifti(&file, probe, dec.as_mut(), gz_error)?;
            return Ok(Opened { header, extensions, compressed, file_size, file, storage });
        }
        Some(Format::Mgh) => {
            let mut h = mgh::parse(probe)?;
            // TR, flip angle, TE and TI follow the data (read for .mgh only:
            // in a .mgz they would need the whole stream).
            if !compressed {
                if let Some(end) = h.total_bytes().and_then(|t| t.checked_add(mgh::HEADER_SIZE as u64)) {
                    let mut tail = [0u8; 16];
                    if end + 16 <= file_size && file.read_exact_at(&mut tail, end).is_ok() {
                        mgh::scan_parameters(&mut h, &tail);
                    }
                }
            }
            h
        }
        Some(Format::Mif) => {
            let text = read_text(probe, dec.as_mut().map(|d| d as &mut dyn Read), &file, mif::header_end, mif::MAX_HEADER_BYTES)?;
            mif::parse(&text, path.parent().unwrap_or(Path::new("")))?
        }
        Some(Format::Nrrd) => {
            if compressed {
                bail!("unsupported_format", "A gzipped NRRD file is not supported (NRRD has its own gzip encoding)");
            }
            let mut text = read_text(probe, None, &file, nrrd::header_end, nrrd::MAX_HEADER_BYTES)?;
            let start = nrrd::header_end(&text).map_or(file_size, |e| e as u64);
            // Lines skipped after the header are found in the bytes read
            // with it: up to 1 MiB of them.
            let head = &text[..(start as usize).min(text.len())];
            if [&b"line skip"[..], b"lineskip"].iter().any(|k| head.windows(k.len()).any(|w| w.eq_ignore_ascii_case(k))) {
                let end = start.saturating_add(1 << 20).min(file_size);
                let mut more = vec![0u8; end.saturating_sub(text.len() as u64) as usize];
                let got = read_full(&mut At { file: &file, pos: text.len() as u64 }, &mut more).map_err(io_error)?;
                text.extend_from_slice(&more[..got]);
            }
            nrrd::parse(&text, start, path)?
        }
        Some(Format::MetaImage) => {
            if compressed {
                bail!("unsupported_format", "A gzipped MetaImage file is not supported (MetaImage has its own compression)");
            }
            let text = read_text(probe, None, &file, metaimage::header_end, metaimage::MAX_HEADER_BYTES)?;
            let start = metaimage::header_end(&text).map_or(file_size, |e| e as u64);
            metaimage::parse(&text[..start.min(text.len() as u64) as usize], start, path, file_size)?
        }
        Some(Format::Dicom) => {
            if compressed {
                bail!("unsupported_format", "A gzipped DICOM file is not supported");
            }
            // Over the network, reading every header of a folder can take
            // long: the file is shown alone.
            let mut dicom = options.dicom;
            dicom.series &= !storage.network;
            crate::formats::dicom::open(path, &file, file_size, dicom)?
        }
        // A file too short for any header, or unknown content.
        _ if n < header::NIFTI1_SIZE && probe.len() < 4 => return Err(PreviewError::new("invalid_image", "File too short to be an image")),
        _ => return Err(image::unrecognised()),
    };
    Ok(Opened { header, extensions: Vec::new(), compressed, file_size, file, storage })
}

/// A text header: `first` bytes, then more from `dec` (a gzip stream) or the
/// raw file, until `end` finds its end or the file ends (at most `limit`).
fn read_text(first: &[u8], mut dec: Option<&mut dyn Read>, file: &File, end: fn(&[u8]) -> Option<usize>, limit: usize) -> Result<Vec<u8>> {
    let mut text = first.to_vec();
    let mut chunk = vec![0u8; 64 << 10];
    while end(&text).is_none() && text.len() < limit {
        let want = chunk.len().min(limit - text.len());
        let got = match dec.as_mut() {
            Some(d) => read_full(d, &mut chunk[..want]).map_err(|e| PreviewError::new("corrupt_data", format!("Header could not be read (gzip: {e})")))?,
            None => read_full(&mut At { file, pos: text.len() as u64 }, &mut chunk[..want]).map_err(io_error)?,
        };
        if got == 0 {
            break;
        }
        text.extend_from_slice(&chunk[..got]);
    }
    if end(&text).is_none() && text.len() >= limit {
        bail!("invalid_image", "Header longer than {} MB", limit >> 20);
    }
    Ok(text)
}

/// Where the voxel data are read from: a gzip stream starting at `gzip_at`
/// in the raw file (or none: raw data), and the position of the first voxel
/// (in the decoded stream, or in the raw file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stream {
    pub gzip_at: Option<u64>,
    pub offset: u64,
    /// On a network file system: never mapped, read in blocks.
    pub network: bool,
}

impl Stream {
    pub fn of(h: &ImageHeader, compressed: bool, file_size: u64) -> Result<Stream> {
        Ok(match h.layout.encoding {
            Encoding::GzipAt(at) => Stream { gzip_at: Some(at), offset: h.layout.offset, network: false },
            Encoding::Stream if h.layout.from_end => {
                let total = h.total_bytes().ok_or_else(|| PreviewError::new("too_large", "Image dimensions overflow"))?;
                let offset = file_size.checked_sub(total).filter(|&o| o >= h.layout.offset).ok_or_else(|| {
                    PreviewError::new("corrupt_data", "Image data could not be read; the file is truncated.")
                })?;
                Stream { gzip_at: None, offset, network: false }
            }
            Encoding::Stream => Stream { gzip_at: compressed.then_some(0), offset: h.layout.offset, network: false },
        })
    }
}

/// NIfTI-1/2: the header, then its extensions (between the header and the
/// voxel data, read up to a limit).
fn open_nifti<R: Read>(
    file: &File,
    probe: &[u8],
    dec: Option<&mut R>,
    gz_error: impl Fn(std::io::Error) -> PreviewError,
) -> Result<(ImageHeader, Vec<Extension>)> {
    let n = probe.len();
    let mut nifti = header::parse(probe)?;
    let start = if nifti.version == 2 { header::NIFTI2_SIZE } else { header::NIFTI1_SIZE };
    let limit = (start + 4 + header::MAX_EXTENSION_BYTES) as u64;
    let end = nifti.vox_offset.min(limit) as usize;
    let mut extensions = Vec::new();
    if n > start && probe[start] != 0 && end > start + 4 {
        let mut area = zeroed(end)?;
        let have = n.min(end);
        area[..have].copy_from_slice(&probe[..have]);
        let got = if have == end {
            end
        } else {
            match dec {
                Some(d) => n + read_full(d, &mut area[n..]).map_err(gz_error)?,
                None => n + read_full(&mut At { file, pos: n as u64 }, &mut area[n..]).map_err(io_error)?,
            }
        };
        extensions = header::parse_extensions(&area[start..got.max(start)], nifti.endian);
        if nifti.vox_offset > limit {
            nifti.warnings.push("header extensions beyond the first MiB are ignored".to_string());
        }
    }
    Ok((ImageHeader::from_nifti(nifti)?, extensions))
}

/// Open a data file named by a header (NRRD `.nhdr`) with the same checks
/// as the image itself: a regular file, opened without blocking.
pub fn open_data_file(path: &Path) -> Result<File> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PreviewError::new("not_found", format!("Data file {} not found", path.display())),
        _ => PreviewError::new("not_found", format!("Cannot access data file {}: {e}", path.display())),
    })?;
    if !meta.is_file() {
        bail!("not_a_file", "Data file {} is not a regular file", path.display());
    }
    let file = OpenOptions::new().read(true).custom_flags(O_NONBLOCK).open(path).map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.is_file() {
        bail!("not_a_file", "Data file {} is not a regular file", path.display());
    }
    Ok(file)
}

/// Open a data file named by a header, as [`open_data_file`], and only if
/// the file opened lies in `root` (the header's folder): a symbolic link to
/// a file elsewhere, or through a folder linked to another place, is
/// refused. The check is made on the file actually opened (`/proc/self/fd`),
/// so a link replaced in between changes nothing; without `/proc`, on the
/// path resolved just before.
pub fn open_data_file_in(path: &Path, root: Option<&Path>) -> Result<File> {
    let file = open_data_file(path)?;
    let Some(root) = root else { return Ok(file) };
    let real = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).or_else(|_| std::fs::canonicalize(path));
    match (real, std::fs::canonicalize(root)) {
        (Ok(real), Ok(root)) if real.starts_with(&root) => Ok(file),
        _ => Err(PreviewError::new(
            "unsupported_format",
            format!("Data file {} leads outside the header's folder (a symbolic link)", header::clean_text(&path.to_string_lossy())),
        )),
    }
}

pub fn too_large(what: &str, bytes: u64, max_bytes: u64) -> PreviewError {
    PreviewError::new(
        "too_large",
        format!(
            "Image too large to preview ({:.0} MB of {what}, limit {:.0} MB: see max-image-mb in ~/.config/gnome-nifti-preview/settings.ini).",
            bytes as f64 / (1u64 << 20) as f64,
            max_bytes as f64 / (1u64 << 20) as f64
        ),
    )
}

/// Load the bytes of the first 3D volume. Returns the storage and the offset of
/// the volume inside it.
pub fn load_volume0(file: &File, h: &ImageHeader, stream: Stream, file_size: u64, max_bytes: u64) -> Result<(Storage, usize)> {
    let vol = h
        .volume_bytes()
        .ok_or_else(|| PreviewError::new("too_large", "Image dimensions overflow"))?;
    load_prefix(file, h, stream, file_size, max_bytes, vol, "voxel data")
}

/// Load all the data (spectroscopy: every FID of every voxel).
pub fn load_all(file: &File, h: &ImageHeader, stream: Stream, file_size: u64, max_bytes: u64) -> Result<(Storage, usize)> {
    let all = h
        .total_bytes()
        .ok_or_else(|| PreviewError::new("too_large", "Image dimensions overflow"))?;
    load_prefix(file, h, stream, file_size, max_bytes, all, "spectroscopy data")
}

/// The first `vol` bytes of the data.
fn load_prefix(file: &File, h: &ImageHeader, stream: Stream, file_size: u64, max_bytes: u64, vol: u64, what: &str) -> Result<(Storage, usize)> {
    if vol > max_bytes {
        return Err(too_large(what, vol, max_bytes));
    }
    let offset = stream.offset;
    let compressed = stream.gzip_at.is_some();
    // A far start costs inflating everything before it (gzip), and in NIfTI
    // means a corrupt header; other raw formats only need it in the file.
    if offset > MAX_VOX_OFFSET && (compressed || h.format.is_nifti()) {
        bail!(
            "corrupt_data",
            "Image data could not be read; the header puts the voxel data {:.0} MB into the file.",
            offset as f64 / (1u64 << 20) as f64
        );
    }
    // Both terms are bounded above, so neither the sum nor the casts overflow.
    let needed = offset + vol;
    let (vol_usize, needed_usize) = (usize::try_from(vol)?, usize::try_from(needed)?);
    // Everything up to the end of the data (a single volume, or load_all).
    let single = h.total_bytes() == Some(vol);

    if !compressed {
        if file_size < needed {
            bail!(
                "corrupt_data",
                "Image data could not be read; the file is truncated ({} bytes, {} needed).",
                file_size,
                needed
            );
        }
        // Over the network a mapping could kill the backend (SIGBUS) when
        // the connection drops: the data are read, in blocks.
        if needed <= READ_BELOW || stream.network {
            crate::progress::expect(vol, false);
            let mut out = zeroed(vol_usize)?;
            read_exact_in_blocks(file, &mut out, offset).map_err(corrupt)?;
            return Ok((Storage::Owned(out), 0));
        }
        // SAFETY: the mapping is read-only and covers [0, needed), which the
        // file size checked above contains. If another process truncates the
        // file while it is mapped, accesses fault and kill this helper process
        // only; the preview then reports that the backend exited.
        let map = unsafe { memmap2::MmapOptions::new().len(needed_usize).map(file) }.map_err(io_error)?;
        return Ok((Storage::Mapped(map), offset as usize));
    }

    // All the data, of plausible compressed size: inflate the whole stream at
    // once (fastest path). Otherwise stream and stop after the bytes wanted.
    let start = stream.gzip_at.unwrap_or(0);
    // Progress: the compressed bytes of the share of the stream needed.
    let share = h.total_bytes().map_or(1.0, |t| (needed as f64 / t.max(1) as f64).min(1.0));
    crate::progress::expect(((file_size - start.min(file_size)) as f64 * share) as u64, false);
    let plausible = file_size.saturating_sub(start) <= needed.saturating_add(needed / 16).saturating_add(1 << 20);
    if single && plausible && (!stream.network || file_size - start.min(file_size) <= READ_BELOW) {
        if let Some(bytes) = inflate_whole(file, start, file_size, needed_usize)? {
            return Ok((Storage::Owned(bytes), offset as usize));
        }
    }
    let capacity = if stream.network { READ_BLOCK } else { 1 << 20 };
    let mut dec = MultiGzDecoder::new(BufReader::with_capacity(capacity, At { file, pos: start }));
    let skipped = std::io::copy(&mut (&mut dec).take(offset), &mut std::io::sink()).map_err(corrupt)?;
    let mut out = zeroed(vol_usize)?;
    let got = read_full(&mut dec, &mut out).map_err(corrupt)?;
    if skipped < offset || got < vol_usize {
        bail!("corrupt_data", "Image data could not be read; the file is truncated or corrupted.");
    }
    Ok((Storage::Owned(out), 0))
}

pub(crate) fn corrupt(e: std::io::Error) -> PreviewError {
    PreviewError::new(
        "corrupt_data",
        format!("Image data could not be read; the file may be truncated or corrupted ({e})."),
    )
}

#[cfg(feature = "libdeflate")]
fn inflate_whole(file: &File, start: u64, file_size: u64, needed: usize) -> Result<Option<Vec<u8>>> {
    // Small streams are read (immune to truncation); big ones are mapped, so
    // the compressed copy does not double the memory used.
    let length = file_size.saturating_sub(start);
    let (storage, from) = if length <= READ_BELOW {
        let mut buf = zeroed(usize::try_from(length)?)?;
        let got = read_full(&mut At { file, pos: start }, &mut buf).map_err(io_error)?;
        buf.truncate(got);
        (Storage::Owned(buf), 0)
    } else {
        // SAFETY: read-only mapping of the whole file (see load_volume0).
        (Storage::Mapped(unsafe { memmap2::Mmap::map(file) }.map_err(io_error)?), usize::try_from(start)?)
    };
    let compressed = &storage.bytes()[from.min(storage.bytes().len())..];
    if compressed.len() < 18 {
        bail!("corrupt_data", "Image data could not be read; the gzip stream is truncated.");
    }
    let isize = u32::from_le_bytes(compressed[compressed.len() - 4..].try_into().unwrap()) as usize;
    // The gzip trailer stores the size modulo 2^32: only trust it when consistent
    // with the header, otherwise (multi-member, trailing data, >4 GiB) stream.
    if isize < needed || isize > needed.saturating_add(1 << 20) {
        return Ok(None);
    }
    let mut out = zeroed(isize)?;
    let mut d = libdeflater::Decompressor::new();
    match d.gzip_decompress(compressed, &mut out) {
        Ok(n) if n >= needed => {
            out.truncate(n);
            Ok(Some(out))
        }
        Ok(_) => Ok(None),
        Err(libdeflater::DecompressionError::InsufficientSpace) => Ok(None),
        Err(_) => bail!("corrupt_data", "Image data could not be read; the file may be truncated or corrupted (invalid gzip data)."),
    }
}

#[cfg(not(feature = "libdeflate"))]
fn inflate_whole(_file: &File, _start: u64, _file_size: u64, _needed: usize) -> Result<Option<Vec<u8>>> {
    Ok(None)
}
