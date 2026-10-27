//! DICOM data sets: the file meta group, the transfer syntaxes, elements in
//! explicit or implicit VR, little or big endian, sequences of defined or
//! undefined length, and where the pixel data are (native, or the fragments
//! of encapsulated data). Parsing stops at the pixel data: their bytes are
//! never read here.
//!
//! Everything is bounded: the depth of nested sequences, the number of
//! elements, the bytes of values kept (larger values are skipped, not read),
//! the number of fragments. Lengths are checked against the end of the file
//! or of the enclosing item before anything is read.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;

use super::tags::{self, Tag};
use crate::error::{bail, PreviewError, Result};

pub const UNDEFINED: u32 = 0xFFFF_FFFF;
/// Nested sequences deeper than this are refused.
const MAX_DEPTH: usize = 12;
/// Elements kept in one file, at most.
const MAX_ELEMENTS: usize = 2_000_000;
/// Values longer than this are skipped (not read), except the few large ones
/// used (palettes, look-up tables, Siemens CSA header, offset tables).
const KEEP_BYTES: u32 = 64 << 10;
const KEEP_LARGE_BYTES: u32 = 4 << 20;
/// Bytes of values kept per file, at most.
const MAX_KEPT_BYTES: u64 = 64 << 20;
/// Fragments of encapsulated pixel data, at most.
pub const MAX_FRAGMENTS: usize = 1 << 20;
/// A deflated data set is inflated in memory up to this size.
pub const MAX_INFLATED: u64 = 1 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Native,
    /// Encapsulated, but not compressed (1.2.840.10008.1.2.1.98).
    EncapsulatedNative,
    /// Each frame deflated (1.2.840.10008.1.2.8.1).
    DeflatedFrames,
    Rle,
    Jpeg,
    JpegLs,
    J2k,
    Unsupported(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Syntax {
    pub uid: String,
    pub explicit: bool,
    pub big_endian: bool,
    /// The whole data set is deflated (1.2.840.10008.1.2.1.99).
    pub deflated: bool,
    pub codec: Codec,
}

impl Syntax {
    pub fn from_uid(uid: &str) -> Syntax {
        let uid = uid.trim_end_matches(['\0', ' ']).to_string();
        let (explicit, big_endian, deflated, codec) = match uid.as_str() {
            "1.2.840.10008.1.2" => (false, false, false, Codec::Native),
            "1.2.840.10008.1.2.1" => (true, false, false, Codec::Native),
            "1.2.840.10008.1.2.1.99" => (true, false, true, Codec::Native),
            "1.2.840.10008.1.2.2" => (true, true, false, Codec::Native),
            "1.2.840.10008.1.2.1.98" => (true, false, false, Codec::EncapsulatedNative),
            "1.2.840.10008.1.2.8.1" => (true, false, false, Codec::DeflatedFrames),
            "1.2.840.10008.1.2.5" => (true, false, false, Codec::Rle),
            "1.2.840.10008.1.2.4.80" | "1.2.840.10008.1.2.4.81" => (true, false, false, Codec::JpegLs),
            "1.2.840.10008.1.2.4.90" | "1.2.840.10008.1.2.4.91" | "1.2.840.10008.1.2.4.92" | "1.2.840.10008.1.2.4.93"
            | "1.2.840.10008.1.2.4.201" | "1.2.840.10008.1.2.4.202" | "1.2.840.10008.1.2.4.203" => {
                (true, false, false, Codec::J2k)
            }
            "1.2.840.10008.1.2.4.94" | "1.2.840.10008.1.2.4.95" => {
                (true, false, false, Codec::Unsupported("JPIP (pixel data held on a server)"))
            }
            "1.2.840.10008.1.2.4.110" | "1.2.840.10008.1.2.4.111" | "1.2.840.10008.1.2.4.112" => {
                (true, false, false, Codec::Unsupported("JPEG XL"))
            }
            u if u.starts_with("1.2.840.10008.1.2.4.10") => (true, false, false, Codec::Unsupported("video (MPEG or HEVC)")),
            // Baseline, extended, lossless and the retired JPEG processes:
            // the decoder reads the process from the frame header.
            u if u.starts_with("1.2.840.10008.1.2.4.5") || u.starts_with("1.2.840.10008.1.2.4.6") || u == "1.2.840.10008.1.2.4.70" => {
                (true, false, false, Codec::Jpeg)
            }
            _ => (true, false, false, Codec::Unsupported("an unknown transfer syntax")),
        };
        Syntax { uid, explicit, big_endian, deflated, codec }
    }

    pub fn name(&self) -> &'static str {
        match self.uid.as_str() {
            "1.2.840.10008.1.2" => "Implicit VR Little Endian",
            "1.2.840.10008.1.2.1" => "Explicit VR Little Endian",
            "1.2.840.10008.1.2.1.99" => "Deflated Explicit VR Little Endian",
            "1.2.840.10008.1.2.2" => "Explicit VR Big Endian",
            "1.2.840.10008.1.2.1.98" => "Encapsulated Uncompressed",
            "1.2.840.10008.1.2.8.1" => "Deflated Image Frame Compression",
            "1.2.840.10008.1.2.5" => "RLE Lossless",
            "1.2.840.10008.1.2.4.50" => "JPEG Baseline",
            "1.2.840.10008.1.2.4.51" => "JPEG Extended",
            "1.2.840.10008.1.2.4.57" => "JPEG Lossless",
            "1.2.840.10008.1.2.4.70" => "JPEG Lossless SV1",
            "1.2.840.10008.1.2.4.80" => "JPEG-LS Lossless",
            "1.2.840.10008.1.2.4.81" => "JPEG-LS Near-Lossless",
            "1.2.840.10008.1.2.4.90" => "JPEG 2000 Lossless",
            "1.2.840.10008.1.2.4.91" => "JPEG 2000",
            "1.2.840.10008.1.2.4.92" => "JPEG 2000 Part 2 Lossless",
            "1.2.840.10008.1.2.4.93" => "JPEG 2000 Part 2",
            "1.2.840.10008.1.2.4.201" => "HTJ2K Lossless",
            "1.2.840.10008.1.2.4.202" => "HTJ2K Lossless RPCL",
            "1.2.840.10008.1.2.4.203" => "HTJ2K",
            _ if self.codec == Codec::Jpeg => "JPEG",
            _ => "",
        }
    }

    /// A lossy compression (the values shown are approximate).
    pub fn lossy(&self) -> bool {
        matches!(
            self.uid.as_str(),
            "1.2.840.10008.1.2.4.50" | "1.2.840.10008.1.2.4.51" | "1.2.840.10008.1.2.4.81" | "1.2.840.10008.1.2.4.91"
                | "1.2.840.10008.1.2.4.93" | "1.2.840.10008.1.2.4.203"
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bytes(Vec<u8>),
    Seq(Vec<Item>),
    /// Too long to keep: only its length is known.
    Skipped(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// As stored (explicit VR), or from the dictionary (implicit VR).
    pub vr: [u8; 2],
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Position of the item tag in the file (DICOMDIR records refer to it).
    pub offset: u64,
    pub set: DataSet,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DataSet {
    pub elements: BTreeMap<Tag, Element>,
    pub big_endian: bool,
}

/// Where the pixel data are, in the file (or in the inflated data set).
#[derive(Debug, Clone, PartialEq)]
pub struct PixelData {
    pub tag: Tag,
    pub vr: [u8; 2],
    /// Native: the first byte and the length (UNDEFINED is impossible here).
    pub offset: u64,
    pub length: u64,
    /// Encapsulated: the basic offset table, then each fragment (position of
    /// its first byte, length).
    pub encapsulated: Option<Encapsulated>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Encapsulated {
    pub offsets: Vec<u32>,
    pub fragments: Vec<(u64, u32)>,
}

/// A parsed file.
#[derive(Debug, Clone)]
pub struct Parsed {
    pub meta: DataSet,
    pub set: DataSet,
    pub syntax: Syntax,
    pub pixels: Option<PixelData>,
    /// A deflated data set, inflated: the pixel data positions refer to it.
    pub inflated: Option<std::sync::Arc<Vec<u8>>>,
}

// ---------------------------------------------------------------- reading

#[derive(Clone, Copy)]
enum Src<'a> {
    File(&'a File),
    Mem(&'a [u8]),
}

/// Positional reads through a window, so that element headers cost no system
/// call each.
struct Reader<'a> {
    src: Src<'a>,
    len: u64,
    pos: u64,
    window: Vec<u8>,
    window_at: u64,
    /// Bytes read at the next refill: small first (most headers are a few
    /// kB, and the folder of a series is read file after file), then more.
    chunk: usize,
}

const WINDOW: usize = 1 << 20;
const FIRST_CHUNK: usize = 8 << 10;

fn truncated() -> PreviewError {
    PreviewError::new("invalid_image", "Invalid DICOM file: truncated header")
}

impl<'a> Reader<'a> {
    fn file(file: &'a File, len: u64, pos: u64) -> Self {
        Reader { src: Src::File(file), len, pos, window: Vec::new(), window_at: 0, chunk: FIRST_CHUNK }
    }

    fn mem(bytes: &'a [u8], pos: u64) -> Self {
        Reader { src: Src::Mem(bytes), len: bytes.len() as u64, pos, window: Vec::new(), window_at: 0, chunk: FIRST_CHUNK }
    }

    fn remaining(&self) -> u64 {
        self.len.saturating_sub(self.pos)
    }

    /// The next `n` bytes (n at most WINDOW), without consuming them.
    fn peek(&mut self, n: usize) -> Result<&[u8]> {
        if self.pos + n as u64 > self.len {
            return Err(truncated());
        }
        match self.src {
            Src::Mem(b) => {
                let p = self.pos as usize;
                Ok(&b[p..p + n])
            }
            Src::File(f) => {
                let inside = self.pos >= self.window_at && self.pos + n as u64 <= self.window_at + self.window.len() as u64;
                if !inside {
                    let want = (self.len - self.pos).min(self.chunk.max(n) as u64) as usize;
                    self.chunk = (self.chunk * 2).min(WINDOW);
                    self.window.resize(want, 0);
                    f.read_exact_at(&mut self.window, self.pos).map_err(|_| truncated())?;
                    self.window_at = self.pos;
                }
                let p = (self.pos - self.window_at) as usize;
                Ok(&self.window[p..p + n])
            }
        }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.peek(N)?);
        self.pos += N as u64;
        Ok(a)
    }

    fn bytes(&mut self, n: u64) -> Result<Vec<u8>> {
        if n > self.remaining() {
            return Err(truncated());
        }
        let n = n as usize;
        let mut v = vec![0u8; n];
        match self.src {
            Src::Mem(b) => v.copy_from_slice(&b[self.pos as usize..self.pos as usize + n]),
            Src::File(_) if n <= WINDOW => v.copy_from_slice(self.peek(n)?),
            Src::File(f) => f.read_exact_at(&mut v, self.pos).map_err(|_| truncated())?,
        }
        self.pos += n as u64;
        Ok(v)
    }

    fn skip(&mut self, n: u64) -> Result<()> {
        if n > self.remaining() {
            return Err(truncated());
        }
        self.pos += n;
        Ok(())
    }
}

struct Parser<'a> {
    r: Reader<'a>,
    explicit: bool,
    big_endian: bool,
    elements: usize,
    kept: u64,
    /// Stop at the pixel data of the top-level data set (else skip it).
    stop_at_pixels: bool,
    pixels: Option<PixelData>,
    /// Keep the items read before a broken one (DICOMDIR records).
    lenient: bool,
}

impl Parser<'_> {
    fn u16(&self, b: [u8; 2]) -> u16 {
        if self.big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
    }

    fn u32(&self, b: [u8; 4]) -> u32 {
        if self.big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
    }

    fn tag(&mut self) -> Result<Tag> {
        let g = self.take_u16()?;
        let e = self.take_u16()?;
        Ok(tags::tag(g, e))
    }

    fn take_u16(&mut self) -> Result<u16> {
        let b = self.r.take::<2>()?;
        Ok(self.u16(b))
    }

    fn take_u32(&mut self) -> Result<u32> {
        let b = self.r.take::<4>()?;
        Ok(self.u32(b))
    }

    /// Elements until `end` (or the end of the file at the top level), or
    /// until an item delimiter when `delimited`.
    fn dataset(&mut self, end: Option<u64>, delimited: bool, depth: usize) -> Result<DataSet> {
        if depth > MAX_DEPTH {
            bail!("invalid_image", "Invalid DICOM file: sequences nested more than {MAX_DEPTH} deep");
        }
        let mut set = DataSet { elements: BTreeMap::new(), big_endian: self.big_endian };
        loop {
            match end {
                Some(e) if self.r.pos >= e => break,
                None if !delimited && self.r.remaining() == 0 => break,
                _ => {}
            }
            // Trailing padding shorter than an element header ends the file.
            if depth == 0 && end.is_none() && self.r.remaining() < 8 {
                break;
            }
            let tag = self.tag()?;
            if tag == tags::ITEM_DELIMITATION {
                self.take_u32()?;
                if delimited {
                    break;
                }
                continue;
            }
            if tag == tags::SEQUENCE_DELIMITATION || tag == tags::ITEM {
                bail!("invalid_image", "Invalid DICOM file: misplaced item or sequence delimiter");
            }
            let (vr, length) = if self.explicit {
                let vr = self.r.take::<2>()?;
                if !tags::is_vr(vr) {
                    bail!("invalid_image", "Invalid DICOM file: bad value representation in element ({:04X},{:04X})", tag >> 16, tag & 0xFFFF);
                }
                if tags::long_length(vr) {
                    self.r.take::<2>()?;
                    (vr, self.take_u32()?)
                } else {
                    (vr, self.take_u16()? as u32)
                }
            } else {
                (tags::vr_of(tag), self.take_u32()?)
            };
            self.elements += 1;
            if self.elements > MAX_ELEMENTS {
                bail!("invalid_image", "Invalid DICOM file: more than {MAX_ELEMENTS} elements");
            }
            if depth == 0 && tags::is_pixel_data(tag) {
                let pixels = self.pixel_data(tag, vr, length)?;
                if self.stop_at_pixels {
                    self.pixels = Some(pixels);
                    break;
                }
                self.pixels = Some(pixels);
                continue;
            }
            let effective = if &vr == b"UN" { tags::vr_of(tag) } else { vr };
            let value = if length == UNDEFINED {
                if &vr == b"UN" && &effective != b"SQ" {
                    // An unknown sequence of undefined length is encoded in implicit VR LE.
                    let (explicit, big) = (self.explicit, self.big_endian);
                    self.explicit = false;
                    self.big_endian = false;
                    let s = self.sequence(None, depth + 1);
                    self.explicit = explicit;
                    self.big_endian = big;
                    Value::Seq(s?)
                } else if &effective == b"SQ" || (!self.explicit && !tags::is_pixel_data(tag)) {
                    Value::Seq(self.sequence(None, depth + 1)?)
                } else if matches!(&vr, b"OB" | b"OW") || tags::is_pixel_data(tag) {
                    // Encapsulated pixel data of a nested image (an icon): skipped.
                    self.pixel_data(tag, vr, length)?;
                    Value::Skipped(0)
                } else {
                    bail!("invalid_image", "Invalid DICOM file: undefined length for a {} value", String::from_utf8_lossy(&vr));
                }
            } else {
                let length = length as u64;
                if let Some(e) = end {
                    if self.r.pos + length > e {
                        bail!("invalid_image", "Invalid DICOM file: element longer than its item");
                    }
                }
                if &effective == b"SQ" && (&vr == b"SQ" || !self.explicit) {
                    let stop = self.r.pos + length;
                    if stop > self.r.len && !self.lenient {
                        return Err(truncated());
                    }
                    let items = self.sequence(Some(stop.min(self.r.len)), depth + 1)?;
                    self.r.pos = stop.min(self.r.len);
                    Value::Seq(items)
                } else {
                    let limit = if large_value(tag) { KEEP_LARGE_BYTES } else { KEEP_BYTES } as u64;
                    if length <= limit && self.kept + length <= MAX_KEPT_BYTES {
                        self.kept += length;
                        Value::Bytes(self.r.bytes(length)?)
                    } else {
                        self.r.skip(length)?;
                        Value::Skipped(length)
                    }
                }
            };
            set.elements.insert(tag, Element { vr: effective, value });
        }
        Ok(set)
    }

    /// Items until `end`, or until the sequence delimiter.
    fn sequence(&mut self, end: Option<u64>, depth: usize) -> Result<Vec<Item>> {
        if depth > MAX_DEPTH {
            bail!("invalid_image", "Invalid DICOM file: sequences nested more than {MAX_DEPTH} deep");
        }
        let mut items = Vec::new();
        loop {
            if let Some(e) = end {
                if self.r.pos >= e {
                    break;
                }
            }
            let offset = self.r.pos;
            let tag = self.tag()?;
            let length = self.take_u32()?;
            match tag {
                tags::SEQUENCE_DELIMITATION => break,
                tags::ITEM_DELIMITATION => continue,
                tags::ITEM => {
                    self.elements += 1;
                    if self.elements > MAX_ELEMENTS {
                        bail!("invalid_image", "Invalid DICOM file: more than {MAX_ELEMENTS} elements");
                    }
                    let set = if length == UNDEFINED {
                        self.dataset(None, true, depth)
                    } else {
                        let stop = self.r.pos + length as u64;
                        if stop > self.r.len || end.is_some_and(|e| stop > e) {
                            Err(truncated())
                        } else {
                            let s = self.dataset(Some(stop), false, depth);
                            self.r.pos = stop;
                            s
                        }
                    };
                    match set {
                        Ok(set) => items.push(Item { offset, set }),
                        // A broken record ends the directory: the ones before are kept.
                        Err(_) if self.lenient => break,
                        Err(e) => return Err(e),
                    }
                }
                _ => bail!("invalid_image", "Invalid DICOM file: unexpected element ({:04X},{:04X}) in a sequence", tag >> 16, tag & 0xFFFF),
            }
        }
        Ok(items)
    }

    fn pixel_data(&mut self, tag: Tag, vr: [u8; 2], length: u32) -> Result<PixelData> {
        let offset = self.r.pos;
        if length != UNDEFINED {
            // Native: its bytes are read when the image is loaded (a
            // truncated file is reported then).
            let length = length as u64;
            let skip = length.min(self.r.remaining());
            self.r.skip(skip)?;
            return Ok(PixelData { tag, vr, offset, length, encapsulated: None });
        }
        if tag != tags::PIXEL_DATA {
            bail!("invalid_image", "Invalid DICOM file: undefined length for float pixel data");
        }
        let mut enc = Encapsulated::default();
        let mut first = true;
        loop {
            let t = self.tag()?;
            let len = self.take_u32()?;
            match t {
                tags::SEQUENCE_DELIMITATION => break,
                tags::ITEM if len == UNDEFINED => bail!("invalid_image", "Invalid DICOM file: pixel data fragment of undefined length"),
                tags::ITEM if first => {
                    first = false;
                    let bot = self.r.bytes(len as u64)?;
                    enc.offsets = bot.chunks_exact(4).map(|c| self.u32([c[0], c[1], c[2], c[3]])).collect();
                }
                tags::ITEM => {
                    if enc.fragments.len() >= MAX_FRAGMENTS {
                        bail!("invalid_image", "Invalid DICOM file: more than {MAX_FRAGMENTS} pixel data fragments");
                    }
                    enc.fragments.push((self.r.pos, len));
                    // A truncated last fragment is kept: its frame fails to decode.
                    let skip = (len as u64).min(self.r.remaining());
                    self.r.skip(skip)?;
                    if skip < len as u64 {
                        break;
                    }
                }
                _ => bail!("invalid_image", "Invalid DICOM file: unexpected element in the pixel data"),
            }
            if self.r.remaining() < 8 {
                break;
            }
        }
        Ok(PixelData { tag, vr, offset, length: self.r.pos - offset, encapsulated: Some(enc) })
    }
}

/// Values that may be long and are read anyway.
fn large_value(t: Tag) -> bool {
    matches!(
        t,
        tags::RED_DATA | tags::GREEN_DATA | tags::BLUE_DATA | tags::SEGMENTED_RED_DATA | tags::LUT_DATA
            | tags::SIEMENS_CSA_IMAGE | tags::EXTENDED_OFFSET_TABLE | tags::EXTENDED_OFFSET_LENGTHS
            | tags::GRID_FRAME_OFFSETS
    ) || t == tags::tag(0x0028, 0x1222)
        || t == tags::tag(0x0028, 0x1223)
}

/// Whether `probe` starts like a DICOM data set without the 128-byte preamble
/// (group 0002 or 0008 first, either byte order).
pub fn starts_without_preamble(probe: &[u8]) -> bool {
    if probe.len() < 8 {
        return false;
    }
    let le = (u16::from_le_bytes([probe[0], probe[1]]), u16::from_le_bytes([probe[2], probe[3]]));
    let be = (u16::from_be_bytes([probe[0], probe[1]]), u16::from_be_bytes([probe[2], probe[3]]));
    let plausible = |(g, e): (u16, u16)| (g == 0x0002 || g == 0x0008) && e <= 0x0020;
    let explicit = tags::is_vr([probe[4], probe[5]]);
    // Implicit VR: a short little-endian length.
    let implicit_le = u32::from_le_bytes([probe[4], probe[5], probe[6], probe[7]]) < 256;
    (plausible(le) && (explicit || implicit_le)) || (plausible(be) && explicit)
}

/// What to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extent {
    /// Up to the pixel data (the image itself).
    Image,
    /// Every element (a DICOMDIR has no pixel data, and its records may be long).
    All,
}

/// Parse a file opened by the caller (`len` bytes).
pub fn parse_file(file: &File, len: u64, extent: Extent) -> Result<Parsed> {
    let mut head = [0u8; 132];
    let got = crate::source::read_full(&mut crate::source::At { file, pos: 0 }, &mut head).map_err(crate::source::io_error)?;
    let preamble = got >= 132 && &head[128..132] == b"DICM";
    if !preamble && !starts_without_preamble(&head[..got]) {
        bail!("invalid_image", "Not a DICOM file");
    }
    let start = if preamble { 132 } else { 0 };
    let mut p = Parser {
        r: Reader::file(file, len, start),
        explicit: true,
        big_endian: false,
        elements: 0,
        kept: 0,
        stop_at_pixels: extent == Extent::Image,
        pixels: None,
        lenient: extent == Extent::All,
    };
    // The meta group: explicit VR little endian, whatever follows.
    let meta = meta_group(&mut p)?;
    let ts_uid = meta.string(tags::TRANSFER_SYNTAX);
    let mut syntax = match &ts_uid {
        Some(uid) if !uid.is_empty() => Syntax::from_uid(uid),
        // No meta group (or no transfer syntax in it): guess from the data.
        _ => guess_syntax(&mut p)?,
    };
    if syntax.deflated {
        let rest = inflate(file, p.r.pos, len)?;
        let inflated = std::sync::Arc::new(rest);
        let mut q = Parser {
            r: Reader::mem(&inflated, 0),
            explicit: true,
            big_endian: false,
            elements: 0,
            kept: 0,
            stop_at_pixels: extent == Extent::Image,
            pixels: None,
            lenient: extent == Extent::All,
        };
        let set = q.dataset(None, false, 0)?;
        let pixels = q.pixels.take();
        return Ok(Parsed { meta, set, syntax, pixels, inflated: Some(inflated.clone()) });
    }
    p.big_endian = syntax.big_endian;
    p.explicit = syntax.explicit;
    // Some files declare one VR encoding and use the other: trust the bytes.
    if p.r.remaining() >= 6 {
        let b = p.r.peek(6)?;
        let looks_explicit = tags::is_vr([b[4], b[5]]);
        if looks_explicit != p.explicit {
            p.explicit = looks_explicit;
            syntax.explicit = looks_explicit;
        }
    }
    let set = p.dataset(None, false, 0)?;
    let pixels = p.pixels.take();
    Ok(Parsed { meta, set, syntax, pixels, inflated: None })
}

fn meta_group(p: &mut Parser) -> Result<DataSet> {
    let mut meta = DataSet::default();
    loop {
        if p.r.remaining() < 8 {
            break;
        }
        let b: [u8; 8] = p.r.peek(8)?.try_into().unwrap();
        let group = u16::from_le_bytes([b[0], b[1]]);
        if group != 0x0002 {
            break;
        }
        let vr = [b[4], b[5]];
        if !tags::is_vr(vr) {
            // A meta group in implicit VR (non-conformant): parse it so.
            let t = tags::tag(group, u16::from_le_bytes([b[2], b[3]]));
            let len = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as u64;
            p.r.skip(8)?;
            if len > KEEP_BYTES as u64 {
                bail!("invalid_image", "Invalid DICOM file: bad file meta information");
            }
            let v = p.r.bytes(len)?;
            meta.elements.insert(t, Element { vr: tags::vr_of(t), value: Value::Bytes(v) });
            continue;
        }
        let set = {
            // One element at a time, so that a group length is not trusted.
            let save = p.stop_at_pixels;
            p.stop_at_pixels = true;
            let end = p.r.pos + if tags::long_length(vr) { 12 } else { 8 };
            let len = if tags::long_length(vr) {
                u32::from_le_bytes(p.r.peek(12)?[8..12].try_into().unwrap()) as u64
            } else {
                u16::from_le_bytes([b[6], b[7]]) as u64
            };
            let r = p.dataset(Some(end + len), false, 1);
            p.stop_at_pixels = save;
            r?
        };
        meta.elements.extend(set.elements);
    }
    Ok(meta)
}

/// No transfer syntax: implicit or explicit VR from the first element, and
/// big endian when the group number reads so.
fn guess_syntax(p: &mut Parser) -> Result<Syntax> {
    let b: [u8; 8] = p.r.peek(8)?.try_into().unwrap();
    let explicit = tags::is_vr([b[4], b[5]]);
    let big = u16::from_le_bytes([b[0], b[1]]) > 0x00FF && u16::from_be_bytes([b[0], b[1]]) <= 0x00FF;
    Ok(Syntax::from_uid(match (explicit, big) {
        (false, _) => "1.2.840.10008.1.2",
        (true, false) => "1.2.840.10008.1.2.1",
        (true, true) => "1.2.840.10008.1.2.2",
    }))
}

/// The deflated data set after the meta group (raw deflate, RFC 1951).
fn inflate(file: &File, start: u64, len: u64) -> Result<Vec<u8>> {
    let reader = crate::source::At { file, pos: start };
    let mut dec = flate2::read::DeflateDecoder::new(std::io::BufReader::new(reader.take(len.saturating_sub(start))));
    let mut out = Vec::new();
    (&mut dec).take(MAX_INFLATED + 1).read_to_end(&mut out).map_err(|e| {
        PreviewError::new("corrupt_data", format!("Deflated DICOM data set could not be read ({e})"))
    })?;
    if out.len() as u64 > MAX_INFLATED {
        bail!("too_large", "Deflated DICOM data set larger than {} MB", MAX_INFLATED >> 20);
    }
    Ok(out)
}

// ---------------------------------------------------------------- values

impl DataSet {
    pub fn get(&self, t: Tag) -> Option<&Element> {
        self.elements.get(&t)
    }

    pub fn contains(&self, t: Tag) -> bool {
        self.elements.contains_key(&t)
    }

    pub fn bytes(&self, t: Tag) -> Option<&[u8]> {
        match &self.elements.get(&t)?.value {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn items(&self, t: Tag) -> &[Item] {
        match self.elements.get(&t).map(|e| &e.value) {
            Some(Value::Seq(items)) => items,
            _ => &[],
        }
    }

    /// First item of a sequence.
    pub fn item(&self, t: Tag) -> Option<&DataSet> {
        self.items(t).first().map(|i| &i.set)
    }

    /// The whole text value (UTF-8 when valid, else Latin-1), trimmed.
    pub fn string(&self, t: Tag) -> Option<String> {
        let b = self.bytes(t)?;
        let s = match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(_) => b.iter().map(|&c| c as char).collect(),
        };
        let s = s.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string();
        Some(s)
    }

    /// A text value for display (control characters removed), None when empty.
    pub fn text(&self, t: Tag) -> Option<String> {
        let s = crate::header::clean_text(&self.string(t)?);
        (!s.is_empty()).then_some(s)
    }

    /// The values of a multi-valued text attribute.
    pub fn strings(&self, t: Tag) -> Vec<String> {
        self.string(t).map(|s| s.split('\\').map(|v| v.trim().to_string()).collect()).unwrap_or_default()
    }

    /// Numbers, whatever the VR: decimal or integer strings, or binary values.
    pub fn numbers(&self, t: Tag) -> Vec<f64> {
        let Some(e) = self.elements.get(&t) else { return Vec::new() };
        let Value::Bytes(b) = &e.value else { return Vec::new() };
        let big = self.big_endian;
        macro_rules! binary {
            ($ty:ty, $n:expr) => {
                b.chunks_exact($n)
                    .map(|c| {
                        let a: [u8; $n] = c.try_into().unwrap();
                        (if big { <$ty>::from_be_bytes(a) } else { <$ty>::from_le_bytes(a) }) as f64
                    })
                    .collect()
            };
        }
        match &e.vr {
            b"US" | b"OW" => binary!(u16, 2),
            b"SS" => binary!(i16, 2),
            b"UL" | b"OL" => binary!(u32, 4),
            b"SL" => binary!(i32, 4),
            b"FL" | b"OF" => binary!(f32, 4),
            b"FD" | b"OD" => binary!(f64, 8),
            b"UV" | b"OV" => binary!(u64, 8),
            b"SV" => binary!(i64, 8),
            _ => {
                let s = match std::str::from_utf8(b) {
                    Ok(s) => s,
                    Err(_) => return Vec::new(),
                };
                s.split('\\').filter_map(|v| v.trim_matches(|c: char| c == '\0' || c.is_whitespace()).parse::<f64>().ok()).collect()
            }
        }
    }

    pub fn number(&self, t: Tag) -> Option<f64> {
        self.numbers(t).first().copied().filter(|v| v.is_finite())
    }

    pub fn int(&self, t: Tag) -> Option<i64> {
        self.number(t).filter(|v| v.fract() == 0.0 && v.abs() < 9e15).map(|v| v as i64)
    }

    /// Unsigned 16-bit words of an OW/US value (palette and LUT data).
    pub fn words(&self, t: Tag) -> Vec<u16> {
        let Some(b) = self.bytes(t) else { return Vec::new() };
        b.chunks_exact(2).map(|c| if self.big_endian { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }).collect()
    }

    /// Attribute tags of an AT value.
    pub fn attribute_tags(&self, t: Tag) -> Vec<Tag> {
        let Some(b) = self.bytes(t) else { return Vec::new() };
        b.chunks_exact(4)
            .map(|c| {
                let (g, e) = if self.big_endian {
                    (u16::from_be_bytes([c[0], c[1]]), u16::from_be_bytes([c[2], c[3]]))
                } else {
                    (u16::from_le_bytes([c[0], c[1]]), u16::from_le_bytes([c[2], c[3]]))
                };
                tags::tag(g, e)
            })
            .collect()
    }

    /// A private attribute: `element` (low byte) in the block reserved by
    /// `creator` in `group` (None when the creator is absent).
    pub fn private(&self, group: u16, creator: &str, element: u8) -> Option<Tag> {
        (0x10u16..=0xFF).find_map(|block| {
            let c = self.string(tags::tag(group, block))?;
            (c.eq_ignore_ascii_case(creator)).then(|| tags::tag(group, (block << 8) | element as u16))
        })
    }
}
