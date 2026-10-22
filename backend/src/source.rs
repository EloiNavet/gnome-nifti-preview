//! File access: gzip detection by magic bytes, header probe and extensions,
//! and loading of the first 3D volume, or of all the data of a spectroscopy
//! file (read or mmap for uncompressed files, libdeflate or streaming inflate
//! for gzip).
//!
//! The file is opened once and every later read goes through that descriptor,
//! so swapping the path for a FIFO or a device after the type check has no
//! effect. Every allocation sized from the header is bounded by the image
//! limit (`max_bytes`) or by `MAX_VOX_OFFSET`, and is fallible.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;

use flate2::read::MultiGzDecoder;

use crate::error::{bail, PreviewError, Result};
use crate::header::{self, Extension, Header};

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
}

impl Storage {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Storage::Mapped(m) => m,
            Storage::Owned(v) => v,
        }
    }
}

pub struct Opened {
    pub header: Header,
    pub extensions: Vec<Extension>,
    pub compressed: bool,
    pub file_size: u64,
    pub file: File,
}

fn io_error(e: std::io::Error) -> PreviewError {
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
struct At<'a> {
    file: &'a File,
    pos: u64,
}

impl Read for At<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.file.read_at(buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

/// Read up to `buf.len()` bytes, stopping early only at end of stream.
fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
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

pub fn open(path: &Path) -> Result<Opened> {
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
    let gz_error = |e: std::io::Error| PreviewError::new("invalid_nifti", format!("Not a readable NIfTI file (gzip: {e})"));
    let mut dec = compressed.then(|| MultiGzDecoder::new(BufReader::new(At { file: &file, pos: 0 })));
    let n = match dec.as_mut() {
        Some(d) => read_full(d, &mut probe).map_err(gz_error)?,
        None => read_full(&mut At { file: &file, pos: 0 }, &mut probe).map_err(io_error)?,
    };
    let mut header = header::parse(&probe[..n])?;

    // Extensions: between the header and the voxel data, read up to a limit.
    let start = if header.version == 2 { header::NIFTI2_SIZE } else { header::NIFTI1_SIZE };
    let limit = (start + 4 + header::MAX_EXTENSION_BYTES) as u64;
    let end = header.vox_offset.min(limit) as usize;
    let mut extensions = Vec::new();
    if n > start && probe[start] != 0 && end > start + 4 {
        let mut area = zeroed(end)?;
        let have = n.min(end);
        area[..have].copy_from_slice(&probe[..have]);
        let got = if have == end {
            end
        } else {
            match dec.as_mut() {
                Some(d) => n + read_full(d, &mut area[n..]).map_err(gz_error)?,
                None => n + read_full(&mut At { file: &file, pos: n as u64 }, &mut area[n..]).map_err(io_error)?,
            }
        };
        extensions = header::parse_extensions(&area[start..got.max(start)], header.endian);
        if header.vox_offset > limit {
            header.warnings.push("header extensions beyond the first MiB are ignored".to_string());
        }
    }
    Ok(Opened { header, extensions, compressed, file_size: meta.len(), file })
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
pub fn load_volume0(file: &File, h: &Header, compressed: bool, file_size: u64, max_bytes: u64) -> Result<(Storage, usize)> {
    let vol = h
        .volume_bytes()
        .ok_or_else(|| PreviewError::new("too_large", "Image dimensions overflow"))?;
    load_prefix(file, h, compressed, file_size, max_bytes, vol, "voxel data")
}

/// Load all the data (spectroscopy: every FID of every voxel).
pub fn load_all(file: &File, h: &Header, compressed: bool, file_size: u64, max_bytes: u64) -> Result<(Storage, usize)> {
    let all = h
        .total_bytes()
        .ok_or_else(|| PreviewError::new("too_large", "Image dimensions overflow"))?;
    load_prefix(file, h, compressed, file_size, max_bytes, all, "spectroscopy data")
}

/// The first `vol` bytes of the data.
fn load_prefix(file: &File, h: &Header, compressed: bool, file_size: u64, max_bytes: u64, vol: u64, what: &str) -> Result<(Storage, usize)> {
    if vol > max_bytes {
        return Err(too_large(what, vol, max_bytes));
    }
    let offset = h.vox_offset;
    if offset > MAX_VOX_OFFSET {
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
        if needed <= READ_BELOW {
            let mut out = zeroed(vol_usize)?;
            file.read_exact_at(&mut out, offset).map_err(corrupt)?;
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
    let plausible = file_size <= needed.saturating_add(needed / 16).saturating_add(1 << 20);
    if single && plausible {
        if let Some(bytes) = inflate_whole(file, file_size, needed_usize)? {
            return Ok((Storage::Owned(bytes), offset as usize));
        }
    }
    let mut dec = MultiGzDecoder::new(BufReader::with_capacity(1 << 20, At { file, pos: 0 }));
    let skipped = std::io::copy(&mut (&mut dec).take(offset), &mut std::io::sink()).map_err(corrupt)?;
    let mut out = zeroed(vol_usize)?;
    let got = read_full(&mut dec, &mut out).map_err(corrupt)?;
    if skipped < offset || got < vol_usize {
        bail!("corrupt_data", "Image data could not be read; the file is truncated or corrupted.");
    }
    Ok((Storage::Owned(out), 0))
}

fn corrupt(e: std::io::Error) -> PreviewError {
    PreviewError::new(
        "corrupt_data",
        format!("Image data could not be read; the file may be truncated or corrupted ({e})."),
    )
}

#[cfg(feature = "libdeflate")]
fn inflate_whole(file: &File, file_size: u64, needed: usize) -> Result<Option<Vec<u8>>> {
    // Small streams are read (immune to truncation); big ones are mapped, so
    // the compressed copy does not double the memory used.
    let storage = if file_size <= READ_BELOW {
        let mut buf = zeroed(usize::try_from(file_size)?)?;
        let got = read_full(&mut At { file, pos: 0 }, &mut buf).map_err(io_error)?;
        buf.truncate(got);
        Storage::Owned(buf)
    } else {
        // SAFETY: read-only mapping of the whole file (see load_volume0).
        Storage::Mapped(unsafe { memmap2::Mmap::map(file) }.map_err(io_error)?)
    };
    let compressed = storage.bytes();
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
fn inflate_whole(_file: &File, _file_size: u64, _needed: usize) -> Result<Option<Vec<u8>>> {
    Ok(None)
}
