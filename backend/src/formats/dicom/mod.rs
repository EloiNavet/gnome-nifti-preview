//! DICOM: single files (2D, multi-frame, enhanced, Siemens mosaics), series
//! rebuilt from the files of a folder, and DICOMDIRs.
//!
//! Opening reads headers only (up to the pixel data of each file). The
//! image is described by a `Plan`: for each slice of each volume, the file,
//! frame (and mosaic tile) it comes from. Loading decodes every frame once
//! (in parallel), whatever its transfer syntax: native, deflated, RLE, JPEG
//! (baseline, extended, lossless), JPEG-LS, JPEG 2000 and HTJ2K.

pub mod about;
pub mod catalog;
pub mod j2k;
pub mod jpeg;
pub mod jpegls;
pub mod parse;
pub mod pixels;
pub mod rle;
pub mod series;
pub mod tags;

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use catalog::{Catalog, SeriesEntry};
use parse::{Codec, DataSet, Extent, Parsed, PixelData, Syntax};
use pixels::{Colour, Frame, Photometric, PixelDesc, Samples, Store};
use series::{FrameGeo, Mosaic};

use crate::error::{bail, PreviewError, Result};
use crate::header::{DType, Endian};
use crate::image::{DataLayout, Decode, Decoder, Format, ImageHeader};

/// How a DICOM file is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Rebuild the series of a single-frame file from its folder.
    pub series: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { series: true }
    }
}

/// DICOM details kept with the header.
#[derive(Debug, Clone, Default)]
pub struct Info {
    /// MONOCHROME1: low values are shown white.
    pub inverted: bool,
    /// The window the file gives (low, high, in rescaled values).
    pub window: Option<(f64, f64)>,
    /// The voxel and volume of the file opened: a series opens on it.
    pub start: Option<([usize; 3], usize)>,
    /// Patient identification, shown only on request.
    pub patient: Vec<(String, String)>,
    /// This series, and the others of the folder or DICOMDIR.
    pub series_uid: Option<String>,
    pub catalog: Option<Arc<Catalog>>,
    /// The folder of the file, to list the other series when asked.
    pub folder: Option<PathBuf>,
    /// Files the image is made of.
    pub files: usize,
}

/// One file of the image.
#[derive(Debug)]
struct Source {
    path: PathBuf,
    syntax: Syntax,
    pixels: PixelData,
    inflated: Option<Arc<Vec<u8>>>,
    desc: PixelDesc,
    mosaic: Option<Mosaic>,
    /// Encapsulated data: the fragments of each frame.
    frames: Vec<std::ops::Range<usize>>,
}

/// Where each slice comes from.
#[derive(Debug, Clone, Copy)]
struct Slot {
    source: usize,
    frame: usize,
    tile: Option<usize>,
    rescale: (f64, f64),
}

/// The decoding plan of an image (see the module documentation).
#[derive(Debug)]
pub struct Plan {
    sources: Vec<Source>,
    slots: Vec<Option<Slot>>,
    rows: usize,
    cols: usize,
    dtype: DType,
    /// Rescale applied while storing (float32 image).
    rescale_each: bool,
    /// The slot of the file opened: if it cannot be decoded, nothing is
    /// shown; other slices that fail are left black and counted.
    reference: Option<usize>,
    failures: std::sync::Mutex<Vec<String>>,
    /// Bytes of native pixel data past the end of a truncated file (read as
    /// zeros).
    missing: std::sync::atomic::AtomicU64,
}

impl Plan {
    fn slice_bytes(&self) -> usize {
        self.rows * self.cols * self.dtype.itemsize()
    }
}

impl Decode for Plan {
    fn decode(&self, max_bytes: u64) -> Result<Vec<u8>> {
        let slice = self.slice_bytes();
        let total = slice as u64 * self.slots.len() as u64;
        if total > max_bytes {
            return Err(crate::source::too_large("voxel data", total, max_bytes));
        }
        let mut out = crate::source::zeroed(total as usize)?;
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8).min(self.slots.len().max(1));
        // Contiguous runs of slices per thread: the tiles of a mosaic stay
        // together, and each thread decodes a mosaic once.
        let mut work: Vec<Vec<(usize, &mut [u8])>> = (0..threads).map(|_| Vec::new()).collect();
        let n = self.slots.len();
        for (i, chunk) in out.chunks_mut(slice).enumerate() {
            work[i * threads / n.max(1)].push((i, chunk));
        }
        // Progress of a load that waits for this (frames decoded).
        let progress = crate::progress::current();
        crate::progress::expect(self.slots.len() as u64, true);
        let run = |jobs: Vec<(usize, &mut [u8])>| -> Result<()> {
            crate::progress::set(progress.clone());
            let mut files: std::collections::HashMap<usize, File> = std::collections::HashMap::new();
            let mut mosaic: Option<((usize, usize), Vec<u8>)> = None;
            for (i, chunk) in jobs {
                if crate::progress::cancelled() {
                    return Err(crate::progress::cancelled_error());
                }
                crate::progress::advance(1);
                let Some(slot) = self.slots[i] else { continue };
                if let Err(e) = self.decode_slot(slot, &mut files, &mut mosaic, chunk) {
                    if self.reference.is_none_or(|r| r == i) {
                        return Err(e);
                    }
                    chunk.fill(0);
                    let name = self.sources[slot.source].path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.failures.lock().unwrap_or_else(|p| p.into_inner()).push(format!("{name}: {}", e.message));
                }
            }
            Ok(())
        };
        let results: Vec<Result<()>> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            let mut inline = Vec::new();
            for jobs in work {
                match std::thread::Builder::new().spawn_scoped(scope, || run(jobs)) {
                    Ok(h) => handles.push(h),
                    Err(_) => inline.push(()),
                }
            }
            let mut r: Vec<Result<()>> = handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(PreviewError::new("internal", "Internal error while decoding the image."))))
                .collect();
            if !inline.is_empty() {
                r.push(Err(PreviewError::new("too_large", "Not enough resources to decode the image.")));
            }
            r
        });
        for r in results {
            r?;
        }
        Ok(out)
    }

    fn input_bytes(&self) -> u64 {
        self.sources.iter().map(|s| s.pixels.length).sum()
    }

    fn notes(&self) -> Vec<String> {
        let failures = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        let mut notes = match failures.len() {
            0 => Vec::new(),
            1 => vec![format!("1 slice could not be decoded and is shown black ({}).", failures[0])],
            n => vec![format!("{n} slices could not be decoded and are shown black (first: {}).", failures[0])],
        };
        let missing = self.missing.load(std::sync::atomic::Ordering::Relaxed);
        if missing > 0 {
            notes.push(format!("The file is truncated: the last {missing} bytes of its pixel data are missing and read as zeros."));
        }
        notes
    }
}

fn truncated() -> PreviewError {
    PreviewError::new("corrupt_data", "DICOM pixel data could not be read; the file is truncated.")
}

impl Plan {
    fn decode_slot(
        &self,
        slot: Slot,
        files: &mut std::collections::HashMap<usize, File>,
        mosaic: &mut Option<((usize, usize), Vec<u8>)>,
        out: &mut [u8],
    ) -> Result<()> {
        let src = &self.sources[slot.source];
        let d = &src.desc;
        let st = Store { dtype: self.dtype, rescale: self.rescale_each.then_some(slot.rescale) };
        match (src.mosaic, slot.tile) {
            (Some(m), Some(t)) => {
                // The whole mosaic (decoded once for its tiles), then the tile.
                let size = self.dtype.itemsize();
                let key = (slot.source, slot.frame);
                if mosaic.as_ref().is_none_or(|(k, _)| *k != key) {
                    let frame = self.read_frame(slot.source, slot.frame, files)?;
                    let mut whole = crate::source::zeroed(d.pixels() * size)?;
                    pixels::store(d, &frame, st, &mut whole)?;
                    *mosaic = Some((key, whole));
                }
                let whole = &mosaic.as_ref().unwrap().1;
                let (tr, tc) = ((t / m.per_side) * m.rows, (t % m.per_side) * m.cols);
                for r in 0..m.rows {
                    let from = ((tr + r) * d.cols + tc) * size;
                    out[r * m.cols * size..(r + 1) * m.cols * size].copy_from_slice(&whole[from..from + m.cols * size]);
                }
                Ok(())
            }
            _ => {
                let frame = self.read_frame(slot.source, slot.frame, files)?;
                pixels::store(d, &frame, st, out)
            }
        }
    }

    /// The bytes of a source (from its file, or its inflated data set).
    /// `partial`: native pixel data cut by the end of a truncated file are
    /// read up to it, the rest as zeros (counted in `missing`).
    fn bytes_at(&self, src: &Source, slot_source: usize, files: &mut std::collections::HashMap<usize, File>, pos: u64, len: usize, partial: bool) -> Result<Vec<u8>> {
        let end = pos.checked_add(len as u64).ok_or_else(truncated)?;
        let size = match &src.inflated {
            Some(mem) => mem.len() as u64,
            None => {
                let file = match files.entry(slot_source) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => e.insert(crate::source::open_data_file(&src.path)?),
                };
                file.metadata().map_err(crate::source::io_error)?.len()
            }
        };
        if end > size && (!partial || pos >= size) {
            return Err(truncated());
        }
        let have = (end.min(size) - pos) as usize;
        let mut buf = crate::source::zeroed(len)?;
        match &src.inflated {
            Some(mem) => buf[..have].copy_from_slice(&mem[pos as usize..pos as usize + have]),
            None => files[&slot_source].read_exact_at(&mut buf[..have], pos).map_err(crate::source::corrupt)?,
        }
        if have < len {
            self.missing.fetch_add((len - have) as u64, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(buf)
    }

    fn read_frame(&self, index: usize, frame: usize, files: &mut std::collections::HashMap<usize, File>) -> Result<Frame> {
        let src = &self.sources[index];
        let d = &src.desc;
        let encapsulated = match &src.pixels.encapsulated {
            None => {
                let bits = d.frame_bits();
                let start_bit = bits * frame as u64;
                let first = start_bit / 8;
                let len = (start_bit % 8 + bits).div_ceil(8);
                if first + len > src.pixels.length {
                    return Err(truncated());
                }
                // 8-bit samples in an OW value of a big-endian file are
                // byte-swapped in pairs: read whole words and swap them back.
                if src.syntax.big_endian && d.bits_allocated == 8 && &src.pixels.vr == b"OW" {
                    let (start, skip) = (first & !1, (first & 1) as usize);
                    let words = (len + skip as u64).div_ceil(2) * 2;
                    let words = words.min(src.pixels.length - start);
                    let mut bytes = self.bytes_at(src, index, files, src.pixels.offset + start, words as usize, true)?;
                    for pair in bytes.chunks_exact_mut(2) {
                        pair.swap(0, 1);
                    }
                    return pixels::native_frame(d, &bytes[skip..], 0, true);
                }
                let bytes = self.bytes_at(src, index, files, src.pixels.offset + first, len as usize, true)?;
                return pixels::native_frame(d, &bytes, start_bit % 8, src.syntax.big_endian);
            }
            Some(e) => e,
        };
        let range = src.frames.get(frame).cloned().ok_or_else(|| PreviewError::new("corrupt_data", "A DICOM frame has no compressed data."))?;
        let mut data = Vec::new();
        for &(pos, len) in &encapsulated.fragments[range] {
            data.extend_from_slice(&self.bytes_at(src, index, files, pos, len as usize, false)?);
        }
        decode_compressed(src, &data)
    }
}

/// One encapsulated frame.
fn decode_compressed(src: &Source, data: &[u8]) -> Result<Frame> {
    let d = &src.desc;
    let check = |w: usize, h: usize| -> Result<()> {
        if w != d.cols || h != d.rows {
            bail!("corrupt_data", "A compressed DICOM frame is {w} × {h}, the header says {} × {}.", d.cols, d.rows);
        }
        Ok(())
    };
    let ybr_photometric = matches!(d.photometric, Photometric::YbrFull | Photometric::YbrFull422);
    let samples_from = |v: Vec<u16>, components: usize, lossy: bool| -> Result<Samples> {
        if components != d.spp {
            bail!("corrupt_data", "A compressed DICOM frame has {components} components, the header says {}.", d.spp);
        }
        let v = v.into_iter().map(|x| x as u32).collect();
        Ok(if lossy { Samples::Clamped(v) } else { Samples::Int(v) })
    };
    match src.syntax.codec {
        Codec::Native => bail!("internal", "native data decoded as compressed"),
        Codec::EncapsulatedNative => pixels::native_frame(d, data, 0, false),
        Codec::DeflatedFrames => {
            let mut out = Vec::new();
            let limit = d.frame_bits().div_ceil(8) + 16;
            let raw = std::io::Read::read_to_end(&mut std::io::Read::take(flate2::read::DeflateDecoder::new(data), limit), &mut out);
            if raw.is_err() || (out.len() as u64) < d.frame_bits().div_ceil(8) {
                out.clear();
                std::io::Read::read_to_end(&mut std::io::Read::take(flate2::read::ZlibDecoder::new(data), limit), &mut out)
                    .map_err(|e| PreviewError::new("corrupt_data", format!("Deflated DICOM frame could not be read ({e}).")))?;
            }
            pixels::native_frame(d, &out, 0, false)
        }
        Codec::Rle => rle::decode(d, data),
        Codec::Jpeg => {
            let j = jpeg::decode(data)?;
            check(j.width, j.height)?;
            // RGB: converted only when the JPEG says it holds YCbCr (JFIF, Adobe).
            let colour = if j.components == 3 && (ybr_photometric || (d.photometric == Photometric::Rgb && j.declares_ycbcr())) { Colour::Ybr } else { Colour::Rgb };
            Ok(Frame { samples: samples_from(j.samples, j.components, !j.lossless)?, colour })
        }
        Codec::JpegLs => {
            let j = jpegls::decode(data)?;
            check(j.width, j.height)?;
            Ok(Frame { samples: samples_from(j.samples, j.components, false)?, colour: if ybr_photometric { Colour::Ybr } else { Colour::Rgb } })
        }
        Codec::J2k => {
            let j = j2k::decode(data)?;
            check(j.width, j.height)?;
            if j.components != d.spp {
                bail!("corrupt_data", "A compressed DICOM frame has {} components, the header says {}.", j.components, d.spp);
            }
            let samples = if j.signed { Samples::Signed(j.samples) } else { Samples::Int(j.samples.into_iter().map(|x| x as u32).collect()) };
            Ok(Frame { samples, colour: if ybr_photometric { Colour::Ybr } else { Colour::Rgb } })
        }
        Codec::Unsupported(what) => bail!("unsupported_format", "DICOM pixel data compressed with {what} are not supported"),
    }
}

/// The fragments of each frame of encapsulated pixel data.
fn frame_fragments(file: Option<&File>, inflated: Option<&[u8]>, enc: &parse::Encapsulated, frames: usize, set: &DataSet) -> Result<Vec<std::ops::Range<usize>>> {
    let n = enc.fragments.len();
    if n == 0 {
        bail!("corrupt_data", "DICOM pixel data hold no fragment");
    }
    // Offsets: the extended table, else the basic one (relative to the
    // first fragment's item tag).
    let extended: Vec<u64> = set.numbers(tags::EXTENDED_OFFSET_TABLE).iter().map(|&v| v as u64).collect();
    let offsets: Vec<u64> = if extended.len() == frames { extended } else { enc.offsets.iter().map(|&o| o as u64).collect() };
    if frames == 1 {
        return Ok(std::iter::once(0..n).collect());
    }
    if offsets.len() == frames {
        let first = enc.fragments[0].0 - 8;
        let starts: Vec<usize> = offsets
            .iter()
            .map(|&o| enc.fragments.iter().position(|&(p, _)| p - 8 - first == o))
            .collect::<Option<Vec<usize>>>()
            .ok_or_else(|| PreviewError::new("corrupt_data", "The DICOM offset table does not match the fragments"))?;
        return Ok((0..frames).map(|k| starts[k]..if k + 1 < frames { starts[k + 1] } else { n }).collect());
    }
    if n == frames {
        return Ok((0..frames).map(|k| k..k + 1).collect());
    }
    if n < frames {
        bail!("corrupt_data", "DICOM pixel data hold {n} fragments for {frames} frames");
    }
    // A frame ends with the fragment ending with an end-of-image marker.
    let mut out = Vec::new();
    let mut start = 0;
    for (k, &(pos, len)) in enc.fragments.iter().enumerate() {
        let tail_len = (len as usize).min(3);
        let at = pos + len as u64 - tail_len as u64;
        let mut tail = vec![0u8; tail_len];
        match (inflated, file) {
            (Some(m), _) => tail.copy_from_slice(m.get(at as usize..at as usize + tail_len).unwrap_or(&[0, 0, 0][..tail_len])),
            (None, Some(f)) if f.read_exact_at(&mut tail, at).is_err() => break,
            (None, Some(_)) => {}
            _ => {}
        }
        let ends = tail.windows(2).any(|w| w == [0xFF, 0xD9]);
        if ends || k == n - 1 {
            out.push(start..k + 1);
            start = k + 1;
        }
    }
    if out.len() < frames {
        bail!("corrupt_data", "DICOM frames could not be told apart in the pixel data");
    }
    out.truncate(frames);
    Ok(out)
}

fn no_image(p: &Parsed, file_size: u64) -> PreviewError {
    no_image_message(p).with_about(about::about(p, file_size))
}

fn no_image_message(p: &Parsed) -> PreviewError {
    let what = match p.set.text(tags::MODALITY).as_deref() {
        Some("RTPLAN") => "a radiotherapy plan",
        Some("RTSTRUCT") => "radiotherapy structures",
        Some("SR") => "a structured report",
        Some("ECG") | Some("HD") => "a waveform",
        Some("PR") => "a presentation state",
        Some("KO") => "a key object selection",
        _ => "",
    };
    if what.is_empty() {
        return PreviewError::new("unsupported_format", "This DICOM file holds no image.");
    }
    PreviewError::new("unsupported_format", format!("This DICOM file holds {what}, not an image."))
}

/// Open a DICOM file (and its series, or the series of a DICOMDIR).
pub fn open(path: &Path, file: &File, len: u64, opts: Options) -> Result<ImageHeader> {
    let parsed = match parse::parse_file(file, len, Extent::Image) {
        Ok(p) => p,
        // A DICOMDIR with a broken record is read up to it.
        Err(e) => match parse::parse_file(file, len, Extent::All) {
            Ok(p) if catalog::is_dicomdir(&p) => p,
            _ => return Err(e),
        },
    };
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let path = absolute.as_path();
    let folder = path.parent().map(Path::to_path_buf);
    if catalog::is_dicomdir(&parsed) {
        let parsed = parse::parse_file(file, len, Extent::All)?;
        let cat = Arc::new(catalog::dicomdir_catalog(path, &parsed)?);
        // The series with the most images first.
        let best = cat.series.iter().max_by_key(|s| s.images).unwrap();
        return open_entry(best, cat.clone(), folder);
    }
    if !catalog::is_image(&parsed) {
        return Err(no_image(&parsed, len));
    }
    // A series may be one file per slice, or one multi-frame file per volume
    // (Siemens XA): the files of the folder are read in both cases.
    let uid = parsed.set.text(tags::SERIES_INSTANCE);
    if let (true, Some(uid), Some(dir)) = (opts.series, uid.as_deref(), folder.as_deref()) {
        if let Ok((cat, members)) = catalog::scan_folder(dir, Some(uid), Some((path, &parsed))) {
            if !members.is_empty() {
                let clicked = members.iter().position(|(p, _)| p == path);
                return build(members, clicked, Some(Arc::new(cat)), folder);
            }
        }
    }
    build(vec![(path.to_path_buf(), parsed)], Some(0), None, folder)
}

/// Open a series of a catalog (folder or DICOMDIR).
pub fn open_entry(entry: &SeriesEntry, cat: Arc<Catalog>, folder: Option<PathBuf>) -> Result<ImageHeader> {
    let mut members = Vec::new();
    for path in &entry.files {
        let Ok(file) = crate::source::open_data_file(path) else { continue };
        let Ok(meta) = file.metadata() else { continue };
        let Ok(parsed) = parse::parse_file(&file, meta.len(), Extent::Image) else { continue };
        if catalog::is_image(&parsed) {
            members.push((path.clone(), parsed));
        }
    }
    if members.is_empty() {
        bail!("not_found", "None of the files of this series could be read");
    }
    build(members, None, Some(cat), folder)
}

/// The header (and plan) of the image made of these files; `clicked` is the
/// file the image opens on.
fn build(members: Vec<(PathBuf, Parsed)>, clicked: Option<usize>, cat: Option<Arc<Catalog>>, folder: Option<PathBuf>) -> Result<ImageHeader> {
    let mut members = members;
    let mut reference = clicked.unwrap_or(members.len() / 2).min(members.len() - 1);
    // Frames without a position (cines, ultrasound clips) are a whole on
    // their own: other files of the series are not appended to them.
    if members.len() > 1 {
        let (_, p) = &members[reference];
        let placed = p.set.contains(tags::IMAGE_POSITION) || p.set.contains(tags::PER_FRAME_FUNCTIONAL_GROUPS);
        if !placed {
            members = vec![members.swap_remove(reference)];
            reference = 0;
        }
    }
    let mut sources = Vec::new();
    let mut frames: Vec<FrameGeo> = Vec::new();
    let mut ref_frames = 0..0;
    let mut warnings = Vec::new();
    for (k, (path, parsed)) in members.iter().enumerate() {
        let Some(pixels) = parsed.pixels.clone() else { continue };
        let desc = match PixelDesc::read(&parsed.set, pixels.tag) {
            Ok(d) => d,
            Err(e) if k == reference => return Err(e),
            Err(_) => continue,
        };
        let mosaic = series::mosaic_of(&parsed.set, &desc);
        let fragments = match &pixels.encapsulated {
            Some(enc) => {
                let file = if parsed.inflated.is_none() { crate::source::open_data_file(path).ok() } else { None };
                match frame_fragments(file.as_ref(), parsed.inflated.as_deref().map(|v| v.as_slice()), enc, desc.frames, &parsed.set) {
                    Ok(f) => f,
                    Err(e) if k == reference => return Err(e),
                    Err(_) => continue,
                }
            }
            None => Vec::new(),
        };
        if pixels.encapsulated.is_none() && parsed.syntax.codec != Codec::Native && !parsed.syntax.deflated {
            if let Codec::Unsupported(what) = parsed.syntax.codec {
                bail!("unsupported_format", "DICOM pixel data compressed with {what} are not supported");
            }
        }
        let index = sources.len();
        let geos = series::frames_of(&parsed.set, &desc, index);
        if k == reference {
            ref_frames = frames.len()..frames.len() + geos.len();
        }
        frames.extend(geos);
        sources.push(Source {
            path: path.clone(),
            syntax: parsed.syntax.clone(),
            pixels,
            inflated: parsed.inflated.clone(),
            desc,
            mosaic,
            frames: fragments,
        });
    }
    if ref_frames.is_empty() {
        bail!("invalid_image", "The DICOM image could not be read");
    }
    let ref_set = &members[reference].1.set;
    let ref_frame = frames[ref_frames.start].clone();
    let ref_source = &sources[ref_frame.source];
    let tile_dims = |s: &Source| s.mosaic.map_or((s.desc.rows, s.desc.cols), |m| (m.rows, m.cols));
    let dims = tile_dims(ref_source);
    let colour = ref_source.desc.is_colour();
    // Frames that do not fit the reference (another size, colour, or
    // orientation) are left out.
    let same_orientation = |f: &FrameGeo| match (f.orientation, ref_frame.orientation) {
        (Some(a), Some(b)) => a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-3),
        (None, None) => true,
        _ => false,
    };
    let before = frames.len();
    let kept: Vec<FrameGeo> = frames
        .into_iter()
        .filter(|f| tile_dims(&sources[f.source]) == dims && sources[f.source].desc.is_colour() == colour && same_orientation(f))
        .collect();
    if kept.len() < before {
        warnings.push(format!("{} images of the series have another size or orientation and are not shown.", before - kept.len()));
    }
    let asm = series::assemble(&kept);
    warnings.extend(asm.warnings.iter().cloned());

    // The datatype: the files', unless they differ or their rescales do.
    let dtypes: Vec<DType> = sources.iter().map(|s| s.desc.dtype()).collect();
    let first_rescale = kept[0].rescale;
    let uniform_rescale = kept.iter().all(|f| f.rescale == first_rescale);
    let uniform_type = dtypes.iter().all(|&t| t == dtypes[0]);
    let (dtype, rescale_each) = if colour {
        (DType::Rgb24, false)
    } else if uniform_type && uniform_rescale {
        (dtypes[0], false)
    } else if dtypes[0] == DType::F64 && uniform_type {
        (DType::F64, false)
    } else {
        (DType::F32, true)
    };
    let (slope, inter) = if rescale_each || colour { (1.0, 0.0) } else { first_rescale };
    let slots: Vec<Option<Slot>> = asm
        .slots
        .iter()
        .map(|s| s.map(|i| Slot { source: kept[i].source, frame: kept[i].frame, tile: kept[i].tile, rescale: kept[i].rescale }))
        .collect();
    // Where the file opened lies: its slice and volume (a single image), or
    // its volume (a file per volume), else the middle of the first volume.
    let (rows, cols) = dims;
    let here: Vec<usize> = asm
        .slots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_some_and(|i| kept[i].source == ref_frame.source))
        .map(|(p, _)| p)
        .collect();
    let volumes: std::collections::BTreeSet<usize> = here.iter().map(|p| p / asm.nz).collect();
    let located = match (here.as_slice(), volumes.len()) {
        ([p], _) => Some(([cols / 2, rows / 2, p % asm.nz], p / asm.nz)),
        (_, 1) if asm.nt > 1 => Some(([cols / 2, rows / 2, asm.nz / 2], *volumes.first().unwrap())),
        _ => None,
    };
    // A series chosen from a list (no file clicked) opens in its middle.
    let start = if clicked.is_some() { located } else { None };
    let mut shape = vec![cols, rows, asm.nz];
    let mut zooms = asm.zooms.to_vec();
    let mut repetition_time = None;
    if asm.nt > 1 {
        shape.push(asm.nt);
        let tr = if asm.oriented {
            ref_set
                .number(tags::REPETITION_TIME)
                .or_else(|| ref_set.item(tags::SHARED_FUNCTIONAL_GROUPS).and_then(|s| s.item(tags::MR_TIMING_SEQUENCE)).and_then(|m| m.number(tags::REPETITION_TIME)))
        } else {
            ref_set.number(tags::FRAME_TIME)
        };
        repetition_time = tr.filter(|t| *t > 0.0).map(|t| t / 1000.0);
        zooms.push(repetition_time.unwrap_or(1.0));
    }
    let ref_desc = ref_source.desc.clone();
    let reference_slot = located.map(|(v, t)| v[2] + asm.nz * t);
    let plan = Plan { sources, slots, rows, cols, dtype, rescale_each, reference: reference_slot, failures: Default::default(), missing: Default::default() };
    let mut layout = DataLayout::canonical(0);
    layout.decoder = Some(Decoder(Arc::new(plan)));
    let syntax = &members[reference].1.syntax;
    let fields = fields(ref_set, syntax, &ref_desc, members.len(), &shape);
    let patient = patient_fields(ref_set);
    let intent_code = if ref_set.text(tags::MODALITY).as_deref() == Some("SEG") { 1002 } else { 0 };
    let info = Info {
        inverted: ref_desc.photometric == Photometric::Monochrome1,
        window: if colour { None } else { ref_frame.window },
        start,
        patient,
        series_uid: ref_set.text(tags::SERIES_INSTANCE),
        catalog: cat,
        folder,
        files: members.len(),
    };
    Ok(ImageHeader {
        format: Format::Dicom,
        shape,
        dtype,
        endian: Endian::Little,
        zooms,
        slope,
        inter,
        affine: asm.affine,
        affine_source: if asm.oriented { "DICOM" } else { "none" },
        space_units: "mm",
        time_units: if repetition_time.is_some() { "sec" } else { "unknown" },
        repetition_time,
        intent_code,
        descrip: ref_set.text(tags::SERIES_DESCRIPTION).unwrap_or_default(),
        warnings,
        fields,
        layout,
        nifti: None,
        dicom: Some(Box::new(info)),
    })
}

fn date(s: &str) -> String {
    let s = s.trim();
    if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..])
    } else {
        s.to_string()
    }
}

fn number(x: f64) -> String {
    crate::volume::fmt_g((x * 1e4).round() / 1e4)
}

/// Details for the metadata panel.
fn fields(set: &DataSet, syntax: &Syntax, d: &PixelDesc, files: usize, shape: &[usize]) -> Vec<(String, String)> {
    let mut f: Vec<(String, String)> = Vec::new();
    let mut push = |k: &str, v: String| {
        if !v.is_empty() {
            f.push((k.to_string(), v));
        }
    };
    let text = |t| set.text(t).unwrap_or_default();
    push("Modality", text(tags::MODALITY));
    let maker = [text(tags::MANUFACTURER), text(tags::MODEL)].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    push("Scanner", maker);
    if let Some(b) = set.number(tags::FIELD_STRENGTH) {
        push("Field strength", format!("{} T", number(b)));
    }
    let series = match (set.int(tags::SERIES_NUMBER), set.text(tags::SERIES_DESCRIPTION)) {
        (Some(n), Some(d)) => format!("{n} · {d}"),
        (Some(n), None) => n.to_string(),
        (None, Some(d)) => d,
        (None, None) => String::new(),
    };
    push("Series", series);
    push("Protocol", text(tags::PROTOCOL_NAME));
    let sequence = [text(tags::SEQUENCE_NAME), text(tags::SCANNING_SEQUENCE)].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
    push("Sequence", sequence);
    let mut timing = Vec::new();
    for (t, name, unit) in [(tags::REPETITION_TIME, "TR", " ms"), (tags::ECHO_TIME, "TE", " ms"), (tags::INVERSION_TIME, "TI", " ms"), (tags::FLIP_ANGLE, "flip", "°")] {
        if let Some(v) = set.number(t) {
            timing.push(format!("{name} {}{unit}", number(v)));
        }
    }
    push("Timing", timing.join(" · "));
    let mut ct = Vec::new();
    if let Some(k) = set.number(tags::KVP) {
        ct.push(format!("{} kV", number(k)));
    }
    if let Some(e) = set.number(tags::EXPOSURE) {
        ct.push(format!("{} mAs", number(e)));
    }
    if let Some(k) = set.text(tags::CONVOLUTION_KERNEL) {
        ct.push(format!("kernel {k}"));
    }
    push("Exposure", ct.join(" · "));
    push("Body part", text(tags::BODY_PART));
    if let Some(t) = set.number(tags::SLICE_THICKNESS) {
        push("Slice thickness", format!("{} mm", number(t)));
    }
    let study = [text(tags::STUDY_DESCRIPTION), date(&text(tags::STUDY_DATE))].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
    push("Study", study);
    let frames = if d.frames > 1 { format!(", {} frames", d.frames) } else { String::new() };
    let made_of = if files > 1 { format!("{files} files") } else { format!("1 file{frames}") };
    push("Images", if shape.len() > 3 || shape[2] > 1 || files > 1 { made_of } else { String::new() });
    let lossy = if syntax.lossy() { " (lossy)" } else { "" };
    let name = syntax.name();
    push("Transfer syntax", if name.is_empty() { syntax.uid.clone() } else { format!("{name}{lossy}") });
    let photometric = text(tags::PHOTOMETRIC);
    push("Photometric", if d.photometric == Photometric::Monochrome1 { format!("{photometric} (shown inverted)") } else { photometric });
    if !d.float && d.spp == 1 && d.palette.is_none() {
        push("Bits", format!("{} stored of {}, {}", d.bits_stored, d.bits_allocated, if d.signed { "signed" } else { "unsigned" }));
    }
    if let Some(t) = set.text(tags::RESCALE_TYPE) {
        push("Rescale type", t);
    }
    f
}

/// Who the images are of (hidden unless asked for).
fn patient_fields(set: &DataSet) -> Vec<(String, String)> {
    let mut f = Vec::new();
    for (t, k) in [
        (tags::PATIENT_NAME, "Patient"),
        (tags::PATIENT_ID, "Patient ID"),
        (tags::PATIENT_BIRTH_DATE, "Birth date"),
        (tags::PATIENT_SEX, "Sex"),
        (tags::PATIENT_AGE, "Age"),
        (tags::INSTITUTION, "Institution"),
        (tags::REFERRING_PHYSICIAN, "Referring physician"),
        (tags::ACCESSION_NUMBER, "Accession number"),
    ] {
        if let Some(v) = set.text(t) {
            let v = if t == tags::PATIENT_BIRTH_DATE { date(&v) } else { v.replace('^', " ").trim().to_string() };
            if !v.is_empty() {
                f.push((k.to_string(), v));
            }
        }
    }
    f
}
