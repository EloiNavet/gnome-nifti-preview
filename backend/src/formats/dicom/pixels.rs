//! Pixel description, native (uncompressed) frames, and the conversion of
//! decoded frames to the stored image: grey values masked to their bits
//! stored and sign-extended, modality LUT, rescale; RGB from RGB, YCbCr or a
//! palette, reduced to 8 bits per channel.

use std::sync::Arc;

use super::parse::DataSet;
use super::tags;
use crate::error::{bail, Result};
use crate::header::DType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Photometric {
    Monochrome1,
    Monochrome2,
    Rgb,
    YbrFull,
    YbrFull422,
    YbrPartial420,
    YbrIct,
    YbrRct,
    Palette,
    Other,
}

impl Photometric {
    pub fn parse(s: &str) -> Photometric {
        match s.trim() {
            "MONOCHROME1" => Photometric::Monochrome1,
            "MONOCHROME2" | "" => Photometric::Monochrome2,
            "RGB" => Photometric::Rgb,
            "YBR_FULL" => Photometric::YbrFull,
            "YBR_FULL_422" => Photometric::YbrFull422,
            "YBR_PARTIAL_420" => Photometric::YbrPartial420,
            "YBR_ICT" => Photometric::YbrIct,
            "YBR_RCT" => Photometric::YbrRct,
            "PALETTE COLOR" => Photometric::Palette,
            _ => Photometric::Other,
        }
    }
}

/// A colour look-up table (palette colour) or a modality LUT.
#[derive(Debug, Clone, PartialEq)]
pub struct Lut {
    /// The first input value mapped.
    pub first: i64,
    /// Output values, in order.
    pub values: Vec<u16>,
    pub bits: u16,
}

impl Lut {
    fn at(&self, v: i64) -> u16 {
        let i = (v - self.first).clamp(0, self.values.len() as i64 - 1);
        self.values[i as usize]
    }

    /// A segmented palette (PS3.3 C.7.9.2): discrete, linear and indirect segments.
    fn read_segmented(set: &DataSet, descriptor: tags::Tag, data: tags::Tag) -> Option<Lut> {
        let d = set.numbers(descriptor);
        if d.len() < 3 {
            return None;
        }
        let n = if d[0] == 0.0 { 65536 } else { d[0] as usize };
        let words = set.words(data);
        let mut out: Vec<u16> = Vec::with_capacity(n);
        expand_segments(&words, &mut out, n, 0)?;
        if out.is_empty() {
            return None;
        }
        out.resize(n, *out.last().unwrap());
        Some(Lut { first: d[1] as u16 as i64, values: out, bits: if d[2] == 0.0 { 16 } else { d[2] as u16 } })
    }

    /// From a descriptor (entries, first value, bits per entry) and its data.
    fn read(set: &DataSet, descriptor: tags::Tag, data: tags::Tag) -> Option<Lut> {
        let d = set.numbers(descriptor);
        if d.len() < 3 {
            return None;
        }
        let n = if d[0] == 0.0 { 65536 } else { d[0] as usize };
        // The first value is US or SS: read as signed when it came as SS.
        let first = if set.get(descriptor).map(|e| &e.vr == b"SS").unwrap_or(false) { d[1] as i64 } else { d[1] as u16 as i64 };
        let bits = d[2] as u16;
        let raw = set.bytes(data)?;
        let values: Vec<u16> = if raw.len() == n && bits == 8 {
            raw.iter().map(|&b| b as u16).collect()
        } else {
            let words = set.words(data);
            if words.len() < n {
                return None;
            }
            // 8-bit entries in 16-bit words: the value is in one of the bytes.
            if bits == 8 && words[..n].iter().any(|&w| w > 255) {
                words[..n].iter().map(|&w| if w > 255 { w >> 8 } else { w }).collect()
            } else {
                words[..n].to_vec()
            }
        };
        if values.is_empty() {
            return None;
        }
        Some(Lut { first, values, bits: if bits == 0 { 16 } else { bits } })
    }
}

/// Expand segmented LUT data into `out` (at most `n` values).
fn expand_segments(words: &[u16], out: &mut Vec<u16>, n: usize, depth: usize) -> Option<()> {
    if depth > 2 {
        return None;
    }
    let mut i = 0;
    while i + 1 < words.len() && out.len() < n {
        let (op, len) = (words[i], words[i + 1] as usize);
        i += 2;
        match op {
            0 => {
                let v = words.get(i..i + len)?;
                out.extend(v.iter().take(n - out.len()));
                i += len;
            }
            1 => {
                let y1 = *words.get(i)? as f64;
                i += 1;
                let y0 = *out.last()? as f64;
                for k in 1..=len.min(n - out.len()) {
                    out.push((y0 + (y1 - y0) * k as f64 / len as f64).round() as u16);
                }
            }
            2 => {
                let lo = *words.get(i)? as usize;
                let hi = *words.get(i + 1)? as usize;
                i += 2;
                // Byte offset of the first segment to copy, and how many.
                let mut at = ((hi << 16) | lo) / 2;
                for _ in 0..len {
                    let (op2, len2) = (*words.get(at)?, *words.get(at + 1)? as usize);
                    let size = 2 + match op2 {
                        0 => len2,
                        1 => 1,
                        _ => return None,
                    };
                    expand_segments(words.get(at..at + size)?, out, n, depth + 1)?;
                    at += size;
                }
            }
            _ => return None,
        }
    }
    Some(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub red: Lut,
    pub green: Lut,
    pub blue: Lut,
}

/// How the pixels of a file are stored.
#[derive(Debug, Clone, PartialEq)]
pub struct PixelDesc {
    pub rows: usize,
    pub cols: usize,
    pub frames: usize,
    pub spp: usize,
    pub bits_allocated: u16,
    pub bits_stored: u16,
    pub high_bit: u16,
    pub signed: bool,
    pub planar: bool,
    pub photometric: Photometric,
    /// Float or Double Float Pixel Data.
    pub float: bool,
    pub palette: Option<Arc<Palette>>,
    pub modality_lut: Option<Arc<Lut>>,
}

/// Images larger than this side are refused before anything is allocated.
pub const MAX_SIDE: usize = 1 << 16;
pub const MAX_FRAMES: usize = 1 << 20;

impl PixelDesc {
    pub fn read(set: &DataSet, pixel_tag: tags::Tag) -> Result<PixelDesc> {
        let num = |t| set.int(t);
        let rows = num(tags::ROWS).unwrap_or(0);
        let cols = num(tags::COLUMNS).unwrap_or(0);
        if rows <= 0 || cols <= 0 || rows as usize > MAX_SIDE || cols as usize > MAX_SIDE {
            bail!("invalid_image", "Invalid DICOM image: {rows} rows × {cols} columns");
        }
        let frames = num(tags::NUMBER_OF_FRAMES).unwrap_or(1).max(1);
        if frames as usize > MAX_FRAMES {
            bail!("invalid_image", "Invalid DICOM image: {frames} frames");
        }
        let float = pixel_tag != tags::PIXEL_DATA;
        let bits_allocated = match pixel_tag {
            tags::FLOAT_PIXEL_DATA => 32,
            tags::DOUBLE_PIXEL_DATA => 64,
            _ => num(tags::BITS_ALLOCATED).unwrap_or(0),
        };
        if !matches!(bits_allocated, 1 | 8 | 16 | 32 | 64) || (bits_allocated == 64 && !float) {
            bail!("unsupported_datatype", "DICOM images of {bits_allocated} bits allocated are not supported");
        }
        let bits_stored = num(tags::BITS_STORED).filter(|&b| (1..=bits_allocated).contains(&b)).unwrap_or(bits_allocated);
        let high_bit = num(tags::HIGH_BIT).filter(|&h| h >= bits_stored - 1 && h < bits_allocated).unwrap_or(bits_stored - 1);
        let spp = num(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
        let photometric = Photometric::parse(&set.string(tags::PHOTOMETRIC).unwrap_or_default());
        if spp != 1 && spp != 3 {
            bail!("unsupported_format", "DICOM images with {spp} samples per pixel are not supported");
        }
        if spp == 3 && !matches!(bits_allocated, 8 | 16 | 32) {
            bail!("unsupported_format", "Colour DICOM images of {bits_allocated} bits are not supported");
        }
        let palette = if photometric == Photometric::Palette && spp == 1 {
            let lut = |d: tags::Tag, data: tags::Tag, segmented: tags::Tag| {
                Lut::read(set, d, data).or_else(|| Lut::read_segmented(set, d, segmented))
            };
            match (
                lut(tags::RED_DESCRIPTOR, tags::RED_DATA, tags::SEGMENTED_RED_DATA),
                lut(tags::GREEN_DESCRIPTOR, tags::GREEN_DATA, tags::tag(0x0028, 0x1222)),
                lut(tags::BLUE_DESCRIPTOR, tags::BLUE_DATA, tags::tag(0x0028, 0x1223)),
            ) {
                (Some(red), Some(green), Some(blue)) => Some(Arc::new(Palette { red, green, blue })),
                _ => None,
            }
        } else {
            None
        };
        let modality_lut = set
            .item(tags::MODALITY_LUT_SEQUENCE)
            .and_then(|lut| Lut::read(lut, tags::LUT_DESCRIPTOR, tags::LUT_DATA))
            .map(Arc::new);
        Ok(PixelDesc {
            rows: rows as usize,
            cols: cols as usize,
            frames: frames as usize,
            spp: spp as usize,
            bits_allocated: bits_allocated as u16,
            bits_stored: bits_stored as u16,
            high_bit: high_bit as u16,
            signed: num(tags::PIXEL_REPRESENTATION) == Some(1),
            planar: spp == 3 && num(tags::PLANAR_CONFIGURATION) == Some(1),
            photometric,
            float,
            palette,
            modality_lut,
        })
    }

    pub fn pixels(&self) -> usize {
        self.rows * self.cols
    }

    /// Bits of one native frame (YBR_FULL_422 stores two samples per pixel).
    pub fn frame_bits(&self) -> u64 {
        let samples = if self.photometric == Photometric::YbrFull422 && self.spp == 3 {
            self.pixels() as u64 * 2
        } else {
            (self.pixels() * self.spp) as u64
        };
        samples * self.bits_allocated as u64
    }

    pub fn is_colour(&self) -> bool {
        self.spp == 3 || self.palette.is_some()
    }

    /// The datatype the image is kept in (before a per-frame rescale, which
    /// makes it float32).
    pub fn dtype(&self) -> DType {
        if self.is_colour() {
            return DType::Rgb24;
        }
        if self.float {
            return if self.bits_allocated == 64 { DType::F64 } else { DType::F32 };
        }
        if self.modality_lut.is_some() {
            return DType::U16;
        }
        match (self.bits_allocated, self.signed) {
            (1, _) | (8, false) => DType::U8,
            (8, true) => DType::I8,
            (16, false) => DType::U16,
            (16, true) => DType::I16,
            (32, false) => DType::U32,
            _ => DType::I32,
        }
    }
}

/// Decoded samples of one frame, interleaved per pixel (spp per pixel).
pub enum Samples {
    /// Raw sample bits (not yet masked to the bits stored), or values
    /// already signed (`Signed`, from JPEG 2000).
    Int(Vec<u32>),
    /// Values of a lossy decoder, which may exceed the bits stored: clamped,
    /// not masked.
    Clamped(Vec<u32>),
    Signed(Vec<i32>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

/// What the three samples of a colour frame are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Rgb,
    /// YCbCr (full range): converted to RGB.
    Ybr,
}

pub struct Frame {
    pub samples: Samples,
    pub colour: Colour,
}

/// The colour space of native samples of a colour image.
pub fn native_colour(p: Photometric) -> Colour {
    match p {
        Photometric::YbrFull | Photometric::YbrFull422 => Colour::Ybr,
        _ => Colour::Rgb,
    }
}

/// One native frame from its bytes. `bit_offset` is where it starts in
/// `bytes` (1-bit images pack frames without padding).
pub fn native_frame(d: &PixelDesc, bytes: &[u8], bit_offset: u64, big_endian: bool) -> Result<Frame> {
    let n = d.pixels();
    let colour = native_colour(d.photometric);
    let short = || crate::error::PreviewError::new("corrupt_data", "DICOM pixel data could not be read; the file is truncated.");
    let samples = match d.bits_allocated {
        1 => {
            let need = (bit_offset + n as u64).div_ceil(8) as usize;
            if bytes.len() < need {
                return Err(short());
            }
            Samples::Int((0..n as u64).map(|i| {
                let b = bit_offset + i;
                ((bytes[(b / 8) as usize] >> (b % 8)) & 1) as u32
            }).collect())
        }
        8 if d.photometric == Photometric::YbrFull422 && d.spp == 3 => {
            // Y1 Y2 Cb Cr for each pair of pixels of a row.
            if bytes.len() < n * 2 {
                return Err(short());
            }
            let mut out = Vec::with_capacity(n * 3);
            for r in 0..d.rows {
                let row = &bytes[r * d.cols * 2..(r + 1) * d.cols * 2];
                for c in 0..d.cols {
                    let pair = c / 2 * 4;
                    let y = row.get(pair + c % 2).copied().unwrap_or(0);
                    let (cb, cr) = (row.get(pair + 2).copied().unwrap_or(128), row.get(pair + 3).copied().unwrap_or(128));
                    out.extend_from_slice(&[y as u32, cb as u32, cr as u32]);
                }
            }
            Samples::Int(out)
        }
        8 => {
            if bytes.len() < n * d.spp {
                return Err(short());
            }
            Samples::Int(bytes[..n * d.spp].iter().map(|&b| b as u32).collect())
        }
        16 => {
            if bytes.len() < n * d.spp * 2 {
                return Err(short());
            }
            Samples::Int(bytes[..n * d.spp * 2].chunks_exact(2).map(|c| {
                (if big_endian { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }) as u32
            }).collect())
        }
        32 => {
            if bytes.len() < n * d.spp * 4 {
                return Err(short());
            }
            let words = bytes[..n * d.spp * 4].chunks_exact(4).map(|c| {
                let a = [c[0], c[1], c[2], c[3]];
                if big_endian { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) }
            });
            if d.float { Samples::F32(words.map(f32::from_bits).collect()) } else { Samples::Int(words.collect()) }
        }
        64 => {
            if bytes.len() < n * 8 {
                return Err(short());
            }
            Samples::F64(bytes[..n * 8].chunks_exact(8).map(|c| {
                let a: [u8; 8] = c.try_into().unwrap();
                if big_endian { f64::from_be_bytes(a) } else { f64::from_le_bytes(a) }
            }).collect())
        }
        b => bail!("unsupported_datatype", "DICOM images of {b} bits allocated are not supported"),
    };
    let samples = if d.planar && d.spp == 3 { interleave(samples, n) } else { samples };
    Ok(Frame { samples, colour })
}

/// Planar (RRR…GGG…BBB…) to interleaved samples.
pub fn interleave(s: Samples, n: usize) -> Samples {
    match s {
        Samples::Int(v) if v.len() >= 3 * n => {
            let mut out = Vec::with_capacity(3 * n);
            for i in 0..n {
                out.extend_from_slice(&[v[i], v[n + i], v[2 * n + i]]);
            }
            Samples::Int(out)
        }
        other => other,
    }
}

/// How a frame is written into the image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Store {
    pub dtype: DType,
    /// Rescale applied while storing (float32 output only).
    pub rescale: Option<(f64, f64)>,
}

/// Write one decoded frame into `out` (the bytes of its slice, native
/// endianness of the output: little endian).
pub fn store(d: &PixelDesc, frame: &Frame, st: Store, out: &mut [u8]) -> Result<()> {
    let n = d.pixels();
    let shift = (d.high_bit + 1).saturating_sub(d.bits_stored) as u32;
    let bs = d.bits_stored as u32;
    let mask: u32 = if bs >= 32 { u32::MAX } else { (1u32 << bs) - 1 };
    let value = |raw: u32| -> i64 {
        let v = (raw >> shift) & mask;
        if d.signed && bs < 64 && (v >> (bs - 1)) & 1 == 1 && bs < 32 {
            v as i64 - (1i64 << bs)
        } else if d.signed && bs == 32 {
            v as i32 as i64
        } else {
            v as i64
        }
    };
    let too_few = || crate::error::PreviewError::new("corrupt_data", "A DICOM frame decoded to fewer pixels than declared.");
    if st.dtype == DType::Rgb24 {
        if out.len() < n * 3 {
            return Err(too_few());
        }
        if let Some(p) = &d.palette {
            let to8 = |l: &Lut, v: i64| -> u8 {
                let x = l.at(v);
                if l.bits > 8 { (x >> (l.bits - 8)) as u8 } else { x as u8 }
            };
            let idx: Vec<i64> = match &frame.samples {
                Samples::Int(v) => v.iter().map(|&r| value(r)).collect(),
                Samples::Clamped(v) => v.iter().map(|&r| r.min(mask) as i64).collect(),
                Samples::Signed(v) => v.iter().map(|&x| x as i64).collect(),
                _ => bail!("corrupt_data", "Palette colour image with float pixels"),
            };
            if idx.len() < n {
                return Err(too_few());
            }
            for (i, &v) in idx.iter().take(n).enumerate() {
                out[3 * i] = to8(&p.red, v);
                out[3 * i + 1] = to8(&p.green, v);
                out[3 * i + 2] = to8(&p.blue, v);
            }
            return Ok(());
        }
        let raw: Vec<i64> = match &frame.samples {
            Samples::Int(v) => v.iter().map(|&r| ((r >> shift) & mask) as i64).collect(),
            Samples::Clamped(v) => v.iter().map(|&r| r.min(mask) as i64).collect(),
            Samples::Signed(v) => v.iter().map(|&x| x as i64).collect(),
            _ => bail!("corrupt_data", "Colour image with float pixels"),
        };
        if raw.len() < 3 * n {
            return Err(too_few());
        }
        let to8 = |v: i64| -> f64 {
            let v = v.max(0);
            match bs.cmp(&8) {
                std::cmp::Ordering::Greater => (v >> (bs - 8)) as f64,
                std::cmp::Ordering::Less => v as f64 * (255.0 / ((1u64 << bs) - 1) as f64),
                std::cmp::Ordering::Equal => v as f64,
            }
        };
        for i in 0..n {
            let (a, b, c) = (to8(raw[3 * i]), to8(raw[3 * i + 1]), to8(raw[3 * i + 2]));
            let rgb = match frame.colour {
                Colour::Rgb => [a, b, c],
                Colour::Ybr => ybr_to_rgb(a, b, c),
            };
            for k in 0..3 {
                out[3 * i + k] = rgb[k].round().clamp(0.0, 255.0) as u8;
            }
        }
        return Ok(());
    }
    let size = st.dtype.itemsize();
    if out.len() < n * size {
        return Err(too_few());
    }
    macro_rules! write_all {
        ($iter:expr) => {{
            let mut count = 0usize;
            for (i, v) in $iter.take(n).enumerate() {
                write_value(st, v, &mut out[i * size..(i + 1) * size]);
                count += 1;
            }
            if count < n {
                return Err(too_few());
            }
        }};
    }
    match &frame.samples {
        Samples::Int(v) => {
            let lut = d.modality_lut.as_deref();
            write_all!(v.iter().map(|&r| {
                let x = value(r);
                Num::I(match lut { Some(l) => l.at(x) as i64, None => x })
            }))
        }
        Samples::Clamped(v) => {
            let lut = d.modality_lut.as_deref();
            write_all!(v.iter().map(|&r| {
                let x = r.min(mask) as i64;
                Num::I(match lut { Some(l) => l.at(x) as i64, None => x })
            }))
        }
        Samples::Signed(v) => {
            let lut = d.modality_lut.as_deref();
            write_all!(v.iter().map(|&x| Num::I(match lut { Some(l) => l.at(x as i64) as i64, None => x as i64 })))
        }
        Samples::F32(v) => write_all!(v.iter().map(|&x| Num::F(x as f64))),
        Samples::F64(v) => write_all!(v.iter().map(|&x| Num::F(x))),
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Num {
    I(i64),
    F(f64),
}

fn write_value(st: Store, v: Num, out: &mut [u8]) {
    let f = || match v {
        Num::I(i) => i as f64,
        Num::F(x) => x,
    };
    let i = || match v {
        Num::I(i) => i,
        Num::F(x) => x as i64,
    };
    match st.dtype {
        DType::U8 => out[0] = i() as u8,
        DType::I8 => out[0] = i() as i8 as u8,
        DType::U16 => out.copy_from_slice(&(i() as u16).to_le_bytes()),
        DType::I16 => out.copy_from_slice(&(i() as i16).to_le_bytes()),
        DType::U32 => out.copy_from_slice(&(i() as u32).to_le_bytes()),
        DType::I32 => out.copy_from_slice(&(i() as i32).to_le_bytes()),
        DType::F32 => {
            let x = match st.rescale {
                Some((slope, inter)) => f() * slope + inter,
                None => f(),
            };
            out.copy_from_slice(&(x as f32).to_le_bytes());
        }
        DType::F64 => out.copy_from_slice(&f().to_le_bytes()),
        _ => {}
    }
}

/// Full-range YCbCr (ITU-R BT.601, JPEG) to RGB.
pub fn ybr_to_rgb(y: f64, cb: f64, cr: f64) -> [f64; 3] {
    [
        y + 1.402 * (cr - 128.0),
        y - 0.344136 * (cb - 128.0) - 0.714136 * (cr - 128.0),
        y + 1.772 * (cb - 128.0),
    ]
}
