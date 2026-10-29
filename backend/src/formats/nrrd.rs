//! NRRD (`.nrrd` with the data attached, `.nhdr` with the data in other
//! files): a text header up to the first empty line.
//!
//! Axes with a `space directions` vector are spatial; the others (`none`:
//! diffusion gradients, sequences, segment layers...) become volumes, whatever
//! their place in the file. A first axis of kind `RGB-color` (unsigned char)
//! makes an RGB image. The affine is converted to RAS from the `space` (LPS
//! for 3D Slicer). Encodings: raw, gzip, bzip2, and the text ones, `ascii`
//! and `hex`; detached data: one file, a `LIST` or a `%d` pattern.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{bail, PreviewError, Result};
use crate::header::{clean_text, DType, Endian};
use crate::image::{DataFile, DataLayout, Decode, Decoder, Encoding, Format, ImageHeader, StorageAxis};

pub const MAGIC: &[u8] = b"NRRD000";
pub const MAX_HEADER_BYTES: usize = 4 << 20;
const MAX_DIMENSION: usize = 16;
/// Detached data files of one image, at most.
pub const MAX_FILES: usize = 16384;

fn invalid(what: impl std::fmt::Display) -> PreviewError {
    PreviewError::new("invalid_image", format!("Invalid NRRD header: {what}"))
}

/// Data written as text, decoded whole: `ascii` (numbers separated by
/// white space or commas, written as little-endian values of the type) or
/// `hex` (two digits per byte, in the order of the file).
#[derive(Debug)]
struct Text {
    /// Each file, where its text starts, and the lines to skip there.
    parts: Vec<Part>,
    hex: bool,
    dtype: DType,
    /// Values (ascii) or bytes (hex) to read.
    count: u64,
    input: u64,
}

/// Longest number of an ascii file.
const MAX_TOKEN: usize = 64;

impl Text {
    /// One ascii value, as little-endian bytes of the type.
    fn value(&self, token: &str, out: &mut Vec<u8>) -> Option<()> {
        let int = || token.parse::<i64>().ok().or_else(|| token.parse::<f64>().ok().filter(|x| x.is_finite()).map(|x| x.round() as i64));
        match self.dtype {
            DType::F32 => out.extend_from_slice(&token.parse::<f32>().ok()?.to_le_bytes()),
            DType::F64 => out.extend_from_slice(&token.parse::<f64>().ok()?.to_le_bytes()),
            DType::U64 => out.extend_from_slice(&token.parse::<u64>().ok().or_else(|| int().map(|v| v.max(0) as u64))?.to_le_bytes()),
            DType::I64 => out.extend_from_slice(&int()?.to_le_bytes()),
            DType::I32 => out.extend_from_slice(&(int()?.clamp(i32::MIN as i64, i32::MAX as i64) as i32).to_le_bytes()),
            DType::U32 => out.extend_from_slice(&(int()?.clamp(0, u32::MAX as i64) as u32).to_le_bytes()),
            DType::I16 => out.extend_from_slice(&(int()?.clamp(i16::MIN as i64, i16::MAX as i64) as i16).to_le_bytes()),
            DType::U16 => out.extend_from_slice(&(int()?.clamp(0, u16::MAX as i64) as u16).to_le_bytes()),
            DType::I8 => out.push(int()?.clamp(i8::MIN as i64, i8::MAX as i64) as i8 as u8),
            _ => out.push(int()?.clamp(0, u8::MAX as i64) as u8),
        }
        Some(())
    }
}

impl Decode for Text {
    fn decode(&self, max_bytes: u64) -> Result<Vec<u8>> {
        let width = if self.hex { 1 } else { self.dtype.itemsize().clamp(1, 8) as u64 };
        let bytes = self.count.saturating_mul(width);
        if bytes > max_bytes {
            return Err(crate::source::too_large("voxel data", bytes, max_bytes));
        }
        crate::progress::expect(self.input, false);
        let mut out = Vec::new();
        out.try_reserve_exact(bytes as usize).map_err(|_| PreviewError::new("too_large", "Not enough memory to load this image."))?;
        let mut done = 0u64;
        let mut token = String::new();
        let mut nibble: Option<u8> = None;
        for (path, root, start, lines) in &self.parts {
            if done == self.count {
                break;
            }
            let file = crate::source::open_data_file_in(path, root.as_deref())?;
            let mut reader = BufReader::with_capacity(1 << 20, crate::source::At { file: &file, pos: *start });
            let mut skip = *lines;
            loop {
                let buf = reader.fill_buf().map_err(crate::source::corrupt)?;
                if buf.is_empty() || done == self.count {
                    break;
                }
                let mut used = 0;
                for &c in buf {
                    used += 1;
                    if skip > 0 {
                        skip -= (c == b'\n') as usize;
                        continue;
                    }
                    if self.hex {
                        let Some(d) = (c as char).to_digit(16) else {
                            if c.is_ascii_whitespace() {
                                continue;
                            }
                            bail!("corrupt_data", "NRRD hex data: unexpected character in {}", path.display());
                        };
                        match nibble.take() {
                            None => nibble = Some(d as u8),
                            Some(high) => {
                                out.push((high << 4) | d as u8);
                                done += 1;
                            }
                        }
                    } else if c.is_ascii_whitespace() || c == b',' {
                        if !token.is_empty() {
                            self.value(&token, &mut out).ok_or_else(|| PreviewError::new("corrupt_data", format!("NRRD ascii data: {:?} is not a number", clean_text(&token))))?;
                            token.clear();
                            done += 1;
                        }
                    } else if token.len() < MAX_TOKEN && c.is_ascii() {
                        token.push(c as char);
                    } else {
                        bail!("corrupt_data", "NRRD ascii data: unexpected text in {}", path.display());
                    }
                    if done == self.count {
                        break;
                    }
                }
                reader.consume(used);
            }
            // A number that ends a file.
            if !token.is_empty() && done < self.count {
                self.value(&token, &mut out).ok_or_else(|| PreviewError::new("corrupt_data", format!("NRRD ascii data: {:?} is not a number", clean_text(&token))))?;
                token.clear();
                done += 1;
            }
        }
        if done < self.count {
            bail!("corrupt_data", "Image data could not be read; the file holds {done} of {} values.", self.count);
        }
        Ok(out)
    }

    fn input_bytes(&self) -> u64 {
        self.input
    }
}

/// bzip2-compressed data, decoded whole: each file in turn, its first lines
/// skipped, then `skip` bytes of what it decompresses to.
#[derive(Debug)]
struct Bzip2 {
    parts: Vec<Part>,
    skip: u64,
    bytes: u64,
    input: u64,
}

impl Decode for Bzip2 {
    fn decode(&self, max_bytes: u64) -> Result<Vec<u8>> {
        if self.bytes > max_bytes {
            return Err(crate::source::too_large("voxel data", self.bytes, max_bytes));
        }
        crate::progress::expect(self.input, false);
        let mut out = Vec::new();
        for (path, root, start, lines) in &self.parts {
            if out.len() as u64 >= self.bytes {
                break;
            }
            let file = crate::source::open_data_file_in(path, root.as_deref())?;
            let mut reader = BufReader::with_capacity(1 << 20, crate::source::At { file: &file, pos: *start });
            let mut line = Vec::new();
            for _ in 0..*lines {
                line.clear();
                if reader.by_ref().take(1 << 20).read_until(b'\n', &mut line).map_err(crate::source::corrupt)? == 0 {
                    bail!("corrupt_data", "Data file {}: fewer lines than the header skips", path.display());
                }
            }
            let mut part = Vec::new();
            let want = (self.bytes - out.len() as u64).saturating_add(self.skip);
            crate::formats::bzip2::decompress(reader, usize::try_from(want)?, &mut part)?;
            out.extend_from_slice(part.get(self.skip as usize..).unwrap_or(&[]));
        }
        if (out.len() as u64) < self.bytes {
            bail!("corrupt_data", "Image data could not be read; the bzip2 data hold {} of {} bytes.", out.len(), self.bytes);
        }
        out.truncate(self.bytes as usize);
        Ok(out)
    }

    fn input_bytes(&self) -> u64 {
        self.input
    }
}

/// The files a decoder reads (this one after its header, or the detached
/// ones, taken from `layout`: where their data start, lines to skip) and
/// about how many bytes they hold.
fn decoded_parts(layout: &mut DataLayout, path: &Path) -> (Vec<Part>, u64) {
    let parts: Vec<Part> = if layout.files.is_empty() {
        vec![(path.to_path_buf(), None, layout.offset, 0)]
    } else {
        std::mem::take(&mut layout.files).into_iter().map(|f| (f.path, f.root, 0, f.line_skip)).collect()
    };
    let input = parts.iter().map(|(p, _, start, _)| std::fs::metadata(p).map_or(0, |m| m.len().saturating_sub(*start))).sum();
    (parts, input)
}

/// A file a decoder reads: its path, the folder it must stay in (detached
/// data), where its data start and the lines to skip.
type Part = (PathBuf, Option<PathBuf>, u64, usize);

/// Length of the header in `text` (up to and including the empty line), if
/// complete.
pub fn header_end(text: &[u8]) -> Option<usize> {
    let mut start = 0;
    while let Some(nl) = text[start..].iter().position(|&c| c == b'\n') {
        let line = &text[start..start + nl];
        if line.is_empty() || line == b"\r" {
            return Some(start + nl + 1);
        }
        start += nl + 1;
    }
    None
}

fn dtype(t: &str) -> Result<DType> {
    let t = t.trim().to_ascii_lowercase();
    let t = t.as_str();
    Ok(match t {
        "signed char" | "int8" | "int8_t" => DType::I8,
        "uchar" | "unsigned char" | "uint8" | "uint8_t" => DType::U8,
        "short" | "short int" | "signed short" | "signed short int" | "int16" | "int16_t" => DType::I16,
        "ushort" | "unsigned short" | "unsigned short int" | "uint16" | "uint16_t" => DType::U16,
        "int" | "signed int" | "int32" | "int32_t" => DType::I32,
        "uint" | "unsigned int" | "uint32" | "uint32_t" => DType::U32,
        "longlong" | "long long" | "long long int" | "signed long long" | "signed long long int" | "int64" | "int64_t" => DType::I64,
        "ulonglong" | "unsigned long long" | "unsigned long long int" | "uint64" | "uint64_t" => DType::U64,
        "float" => DType::F32,
        "double" => DType::F64,
        _ => bail!("unsupported_datatype", "NRRD type {t} is not supported"),
    })
}

/// "(1,0,0)" -> [1, 0, 0]; "none" -> None.
fn vector(v: &str) -> Result<Option<Vec<f64>>> {
    let v = v.trim();
    if v == "none" {
        return Ok(None);
    }
    let inner = v.strip_prefix('(').and_then(|x| x.strip_suffix(')')).ok_or_else(|| invalid(format_args!("vector {v}")))?;
    Ok(Some(inner.split(',').map(|x| x.trim().parse::<f64>().unwrap_or(f64::NAN)).collect()))
}

/// The vectors of `space directions`: "(1,0,0) none (0,1,0)".
fn vectors(v: &str) -> Result<Vec<Option<Vec<f64>>>> {
    let mut out = Vec::new();
    let mut rest = v.trim();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("none") {
            out.push(None);
            rest = r.trim_start();
        } else if rest.starts_with('(') {
            let close = rest.find(')').ok_or_else(|| invalid(format_args!("vectors {v}")))?;
            out.push(vector(&rest[..=close])?);
            rest = rest[close + 1..].trim_start();
        } else {
            return Err(invalid(format_args!("vectors {v}")));
        }
    }
    Ok(out)
}

/// Signs turning the axes of a `space` into RAS, None when the space is not
/// anatomical (scanner-xyz, 3D-right-handed...).
fn ras_signs(space: &str) -> Option<[f64; 3]> {
    let s = space.trim().to_ascii_lowercase();
    let s = s.strip_suffix("-time").unwrap_or(&s);
    let words: Vec<&str> = s.split('-').collect();
    let names: Vec<String> = if words.len() == 3 {
        words.iter().map(|w| w.chars().next().unwrap_or('?').to_string()).collect()
    } else if s.len() == 3 || (s.len() == 4 && s.ends_with('t')) {
        s.chars().take(3).map(|c| c.to_string()).collect()
    } else {
        return None;
    };
    let sign = |n: &str, pos: &str, neg: &str| match n {
        x if x == pos => Some(1.0),
        x if x == neg => Some(-1.0),
        _ => None,
    };
    Some([sign(&names[0], "r", "l")?, sign(&names[1], "a", "p")?, sign(&names[2], "s", "i")?])
}

/// Parse a header. `text` holds the header (and, for `.nhdr`, the whole file);
/// `data_start` is where the data of an attached file begin; detached files
/// are relative to the folder of `path`, the header's file.
pub fn parse(bytes: &[u8], data_start: u64, path: &Path) -> Result<ImageHeader> {
    let dir = path.parent().unwrap_or(Path::new(""));
    let text = String::from_utf8_lossy(&bytes[..header_end(bytes).unwrap_or(bytes.len())]);
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    if !first.starts_with("NRRD000") {
        return Err(invalid("no NRRD magic"));
    }
    let mut f: Vec<(String, String)> = Vec::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut list_files: Vec<String> = Vec::new();
    let mut in_list = false;
    for line in lines {
        if in_list {
            if !line.trim().is_empty() {
                list_files.push(line.trim().to_string());
            }
            continue;
        }
        if line.trim().is_empty() {
            break; // end of the header
        }
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(":=") {
            pairs.push((k.trim().to_string(), v.trim().to_string()));
        } else if let Some((k, v)) = line.split_once(": ") {
            let key = k.trim().to_ascii_lowercase();
            if (key == "data file" || key == "datafile") && v.trim().starts_with("LIST") {
                in_list = true;
            }
            f.push((key, v.trim().to_string()));
        }
    }
    let get = |k: &str| f.iter().rev().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    let dimension: usize = get("dimension").and_then(|d| d.parse().ok()).ok_or_else(|| invalid("no dimension"))?;
    if !(1..=MAX_DIMENSION).contains(&dimension) {
        return Err(invalid(format_args!("dimension {dimension}")));
    }
    let sizes: Vec<usize> = get("sizes").ok_or_else(|| invalid("no sizes"))?.split_whitespace()
        .map(|s| s.parse::<usize>().ok().filter(|&n| n > 0)).collect::<Option<_>>().ok_or_else(|| invalid("sizes"))?;
    if sizes.len() != dimension {
        return Err(invalid(format_args!("{} sizes for dimension {dimension}", sizes.len())));
    }
    let mut dt = dtype(get("type").ok_or_else(|| invalid("no type"))?)?;
    let mut endian = match get("endian").map(|e| e.to_ascii_lowercase()) {
        Some(e) if e == "big" => Endian::Big,
        _ => Endian::Little,
    };
    let encoding = get("encoding").ok_or_else(|| invalid("no encoding"))?.to_ascii_lowercase();
    // Text: Some(hex).
    let (gzip, text, bzip2) = match encoding.as_str() {
        "raw" => (false, None, false),
        "gzip" | "gz" => (true, None, false),
        "bzip2" | "bz2" => (false, None, true),
        "ascii" | "text" | "txt" => (false, Some(false), false),
        "hex" => (false, Some(true), false),
        other => bail!("unsupported_format", "NRRD encoding {other} is not supported (raw, gzip, bzip2, ascii and hex are)"),
    };
    if text == Some(false) {
        // Numbers are decoded as little-endian values.
        endian = Endian::Little;
    }
    let kinds: Vec<String> = get("kinds").map(|k| k.split_whitespace().map(|x| x.to_string()).collect()).unwrap_or_default();
    let directions = match get("space directions") {
        Some(v) => Some(vectors(v)?),
        None => None,
    };
    if let Some(d) = &directions {
        if d.len() != dimension {
            return Err(invalid(format_args!("{} space directions for dimension {dimension}", d.len())));
        }
    }
    let space = get("space").map(|s| s.to_string());
    let origin = match get("space origin") {
        Some(o) => vector(o)?,
        None => None,
    };
    let spacings: Vec<f64> = get("spacings").map(|s| s.split_whitespace().map(|x| x.parse().unwrap_or(f64::NAN)).collect()).unwrap_or_default();

    // A first axis of colour components (unsigned char) makes an RGB image.
    let colour = kinds.first().map(|k| k.to_ascii_lowercase()).filter(|k| ["rgb-color", "rgba-color", "3-color", "4-color"].contains(&k.as_str()));
    let mut first_file_axis = 0;
    if colour.is_some() && dt == DType::U8 && matches!(sizes[0], 3 | 4) && dimension >= 3 {
        dt = if sizes[0] == 3 { DType::Rgb24 } else { DType::Rgba32 };
        first_file_axis = 1;
    }
    // Spatial axes: those with a direction (or, without directions, the first three).
    let file_axes: Vec<usize> = (first_file_axis..dimension).collect();
    let is_spatial = |a: usize| match &directions {
        Some(d) => d[a].is_some(),
        None => a < first_file_axis + 3,
    };
    let spatial: Vec<usize> = file_axes.iter().copied().filter(|&a| is_spatial(a)).collect();
    let others: Vec<usize> = file_axes.iter().copied().filter(|&a| !is_spatial(a)).collect();
    if spatial.is_empty() || spatial.len() > 3 {
        return Err(invalid(format_args!("{} spatial axes", spatial.len())));
    }
    let logical: Vec<usize> = spatial.iter().chain(others.iter()).copied().collect();
    let mut shape: Vec<usize> = logical.iter().map(|&a| sizes[a]).collect();
    let mut axes: Vec<StorageAxis> = logical.iter().map(|&a| StorageAxis { rank: a - first_file_axis, reversed: false }).collect();
    // Fewer than three spatial axes: singleton ones after them.
    while shape.len() < 3 || spatial.len() + (shape.len() - logical.len()) < 3 {
        let at = spatial.len() + (shape.len() - logical.len());
        shape.insert(at, 1);
        axes.insert(at, StorageAxis { rank: usize::MAX - at, reversed: false });
    }
    // Ranks of inserted axes come last but do not matter (size 1): renumber.
    let mut order: Vec<usize> = (0..axes.len()).collect();
    order.sort_by_key(|&i| axes[i].rank);
    for (rank, &i) in order.iter().enumerate() {
        axes[i].rank = rank;
    }

    // Affine: directions (columns) and origin, turned into RAS.
    let signs = space.as_deref().and_then(ras_signs);
    let mut affine = [[0.0; 4]; 4];
    affine[3][3] = 1.0;
    let mut zooms = vec![1.0; shape.len()];
    let mut usable = directions.is_some();
    for (j, &a) in spatial.iter().enumerate() {
        match directions.as_ref().and_then(|d| d[a].clone()) {
            Some(v) if v.len() >= 3 && v.iter().take(3).all(|x| x.is_finite()) => {
                for r in 0..3 {
                    affine[r][j] = v[r] * signs.map_or(1.0, |s| s[r]);
                }
                let norm = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                zooms[j] = if norm > 0.0 { norm } else { 1.0 };
            }
            _ => {
                usable = false;
                let s = spacings.get(a).copied().filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
                affine[j][j] = s;
                zooms[j] = s;
            }
        }
    }
    for (j, row) in affine.iter_mut().enumerate().take(3).skip(spatial.len()) {
        // A singleton spatial axis: unit length along the remaining direction.
        row[j] = 1.0;
    }
    if let Some(o) = &origin {
        if o.len() >= 3 && o.iter().take(3).all(|x| x.is_finite()) {
            for r in 0..3 {
                affine[r][3] = o[r] * signs.map_or(1.0, |s| s[r]);
            }
        }
    }
    for (k, &a) in others.iter().enumerate() {
        let j = 3 + k;
        if j < zooms.len() {
            zooms[j] = spacings.get(a).copied().filter(|s| s.is_finite()).unwrap_or(0.0);
        }
    }
    let oriented = usable && signs.is_some();

    // Where the data are.
    let line_skip: usize = get("line skip").or(get("lineskip")).and_then(|v| v.parse().ok()).unwrap_or(0);
    let byte_skip: i64 = get("byte skip").or(get("byteskip")).and_then(|v| v.parse().ok()).unwrap_or(0);
    if byte_skip < -1 {
        return Err(invalid(format_args!("byte skip {byte_skip}")));
    }
    let attached = get("data file").or(get("datafile")).is_none();
    if (text.is_some() && byte_skip != 0) || (bzip2 && byte_skip < 0) {
        bail!("unsupported_format", "NRRD byte skip {byte_skip} with {encoding} encoding is not supported");
    }
    if byte_skip == -1 && gzip && attached {
        bail!("unsupported_format", "NRRD byte skip -1 with gzip encoding is not supported in an attached file");
    }
    let mut layout = DataLayout::canonical(0);
    layout.axes = axes;
    match get("data file").or(get("datafile")) {
        None => {
            // Lines to skip after the header (before any decompression): in
            // the bytes read with it.
            let mut data_start = data_start;
            if line_skip > 0 {
                let after = bytes.get(data_start as usize..).unwrap_or(&[]);
                let ends: Vec<usize> = after.iter().enumerate().filter(|(_, &c)| c == b'\n').map(|(i, _)| i).take(line_skip).collect();
                if ends.len() < line_skip {
                    bail!("unsupported_format", "NRRD line skip beyond the first MB after the header is not supported");
                }
                data_start += ends[line_skip - 1] as u64 + 1;
            }
            if text.is_some() || bzip2 {
                layout.offset = data_start;
            } else if gzip {
                layout.encoding = Encoding::GzipAt(data_start);
                layout.offset = byte_skip.max(0) as u64;
            } else if byte_skip == -1 {
                layout.from_end = true;
                layout.offset = data_start;
            } else {
                layout.offset = data_start + byte_skip as u64;
            }
        }
        Some(spec) => {
            let names = detached_files(spec, &list_files)?;
            layout.files = names
                .into_iter()
                .map(|n| {
                    let (path, root) = crate::image::data_file_in(dir, &n, "NRRD")?;
                    Ok(DataFile { path, root: Some(root), line_skip, byte_skip })
                })
                .collect::<Result<_>>()?;
            layout.files_gzip = gzip;
        }
    }

    let mut fields = Vec::new();
    if let Some(s) = &space {
        fields.push(("Space".to_string(), clean_text(s)));
    }
    if !kinds.is_empty() {
        fields.push(("Kinds".to_string(), clean_text(&kinds.join(" "))));
    }
    fields.push(("Encoding".to_string(), match (gzip, text) {
        (true, _) => "gzip",
        (_, Some(false)) => "ascii",
        (_, Some(true)) => "hex",
        _ if bzip2 => "bzip2",
        _ => "raw",
    }.to_string()));
    if !layout.files.is_empty() {
        let n = layout.files.len();
        let first = layout.files[0].path.file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
        fields.push(("Data file".to_string(), if n == 1 { clean_text(&first) } else { format!("{n} files") }));
    }
    let gradients = pairs.iter().filter(|(k, _)| k.starts_with("DWMRI_gradient_")).count();
    if gradients > 0 {
        let b = pairs.iter().find(|(k, _)| k == "DWMRI_b-value").map(|(_, v)| clean_text(v));
        fields.push(("Diffusion".to_string(), match b {
            Some(b) => format!("{gradients} gradients, b = {b} s/mm²"),
            None => format!("{gradients} gradients"),
        }));
    }
    if let Some((_, m)) = pairs.iter().find(|(k, _)| k == "modality") {
        fields.push(("Modality".to_string(), clean_text(m)));
    }
    if let Some(hex) = text {
        // The whole image is decoded from its text, in the order of the file.
        let (parts, input) = decoded_parts(&mut layout, path);
        let values = sizes.iter().try_fold(1u64, |a, &d| a.checked_mul(d as u64)).ok_or_else(|| invalid("sizes"))?;
        let base = if matches!(dt, DType::Rgb24 | DType::Rgba32) { DType::U8 } else { dt };
        let count = if hex { values.saturating_mul(base.itemsize() as u64) } else { values };
        layout.offset = 0;
        layout.decoder = Some(Decoder(Arc::new(Text { parts, hex, dtype: base, count, input })));
    }
    if bzip2 {
        let (parts, input) = decoded_parts(&mut layout, path);
        // (The sizes count the components of a colour image.)
        let item = if matches!(dt, DType::Rgb24 | DType::Rgba32) { 1 } else { dt.itemsize() as u64 };
        let bytes = sizes.iter().try_fold(item, |a, &d| a.checked_mul(d as u64)).ok_or_else(|| invalid("sizes"))?;
        layout.offset = 0;
        layout.decoder = Some(Decoder(Arc::new(Bzip2 { parts, skip: byte_skip.max(0) as u64, bytes, input })));
    }
    let mut h = ImageHeader {
        format: Format::Nrrd,
        shape,
        dtype: dt,
        endian,
        zooms,
        slope: 1.0,
        inter: 0.0,
        affine,
        affine_source: if oriented { "space directions" } else { "none" },
        space_units: "mm",
        time_units: "unknown",
        repetition_time: None,
        intent_code: 0,
        descrip: get("content").map(clean_text).unwrap_or_default(),
        warnings: Vec::new(),
        fields,
        layout,
        nifti: None,
        dicom: None,
    };
    h.normalize_storage();
    Ok(h)
}

/// The detached data files: one name, `LIST [subdim]` (names on the lines
/// that follow) or `FORMAT MIN MAX STEP [subdim]` (a printf `%d` pattern).
fn detached_files(spec: &str, list: &[String]) -> Result<Vec<String>> {
    let parts: Vec<&str> = spec.split_whitespace().collect();
    let names: Vec<String> = match parts.as_slice() {
        ["LIST", ..] => list.to_vec(),
        [pattern, min, max, step, ..] if pattern.contains('%') => {
            let (min, max, step): (i64, i64, i64) = (
                min.parse().map_err(|_| invalid(format_args!("data file {spec}")))?,
                max.parse().map_err(|_| invalid(format_args!("data file {spec}")))?,
                step.parse().map_err(|_| invalid(format_args!("data file {spec}")))?,
            );
            // (Checked: hostile bounds must not overflow.)
            let count = max
                .checked_sub(min)
                .and_then(|span| span.checked_div(step))
                .filter(|&c| c >= 0 && (c as u64) < MAX_FILES as u64)
                .ok_or_else(|| invalid(format_args!("data file {spec}")))?;
            // min + k·step stays between min and max.
            (0..=count).map(|k| printf_int(pattern, min + k * step)).collect::<Result<Vec<_>>>()?
        }
        _ => vec![spec.to_string()],
    };
    if names.is_empty() || names.len() > MAX_FILES {
        return Err(invalid(format_args!("{} data files", names.len())));
    }
    for n in &names {
        if n.is_empty() || n.contains('\0') {
            return Err(invalid("empty data file name"));
        }
    }
    Ok(names)
}

/// A printf pattern with one integer conversion: %d, %3d, %03d, %i.
fn printf_int(pattern: &str, value: i64) -> Result<String> {
    let start = pattern.find('%').ok_or_else(|| invalid("pattern"))?;
    let rest = &pattern[start + 1..];
    let conv = rest.find(['d', 'i', 'u']).ok_or_else(|| invalid(format_args!("pattern {pattern}")))?;
    let spec = &rest[..conv];
    if !spec.chars().all(|c| c.is_ascii_digit()) {
        return Err(invalid(format_args!("pattern {pattern}")));
    }
    let width: usize = spec.trim_start_matches('0').parse().unwrap_or(0);
    let zero = spec.starts_with('0');
    let number = if zero { format!("{value:0width$}") } else { format!("{value:width$}") };
    Ok(format!("{}{}{}", &pattern[..start], number, &rest[conv + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(body: &str) -> String {
        format!("NRRD0004\n{body}\n")
    }

    #[test]
    fn lps_becomes_ras() {
        let t = header("type: short\ndimension: 3\nspace: left-posterior-superior\nsizes: 30 20 10\n\
space directions: (1,0,0) (0,2,0) (0,0,3)\nendian: little\nencoding: raw\nspace origin: (5,6,7)\n");
        let h = parse(t.as_bytes(), 300, Path::new("/x/h.nrrd")).unwrap();
        assert_eq!(h.shape, vec![30, 20, 10]);
        assert_eq!(h.affine[0], [-1.0, 0.0, 0.0, -5.0]);
        assert_eq!(h.affine[1], [0.0, -2.0, 0.0, -6.0]);
        assert_eq!(h.affine[2], [0.0, 0.0, 3.0, 7.0]);
        assert_eq!(h.affine_source, "space directions");
        assert_eq!(h.layout.offset, 300);
        assert!(h.layout.is_direct());
    }

    #[test]
    fn list_axis_first_is_interleaved_and_gzip_follows_the_header() {
        let t = header("type: unsigned short\ndimension: 4\nspace: right-anterior-superior\nsizes: 43 12 10 8\n\
space directions: none (-1.5,0,0) (0,-1.5,0) (0,0,1.5)\nkinds: list domain domain domain\nendian: little\nencoding: gzip\n\
DWMRI_b-value:=1000\nDWMRI_gradient_0000:=0 0 0\nDWMRI_gradient_0001:=1 0 0\n");
        let h = parse(t.as_bytes(), 512, Path::new("/x/h.nrrd")).unwrap();
        assert_eq!(h.shape, vec![12, 10, 8, 43]);
        assert_eq!(h.n_volumes(), 43);
        assert_eq!(h.layout.encoding, Encoding::GzipAt(512));
        assert_eq!(h.layout.offset, 0);
        let (strides, _) = h.layout.strides(&h.shape);
        assert_eq!(strides, vec![43, 43 * 12, 43 * 120, 1]);
        assert!(h.fields.iter().any(|(k, v)| k == "Diffusion" && v == "2 gradients, b = 1000 s/mm²"));
    }

    #[test]
    fn rgb_detached_and_refusals() {
        let t = header("type: uchar\ndimension: 4\nsizes: 3 5 6 7\nkinds: RGB-color domain domain domain\nencoding: raw\ndata file: img.raw\n");
        let h = parse(t.as_bytes(), 0, Path::new("/data/h.nhdr")).unwrap();
        assert_eq!(h.dtype, DType::Rgb24);
        assert_eq!(h.shape, vec![5, 6, 7]);
        assert_eq!(h.layout.files[0].path, Path::new("/data/img.raw"));
        assert_eq!(h.affine_source, "none");
        let list = "NRRD0004\ntype: float\ndimension: 3\nsizes: 4 4 3\nencoding: raw\ndata file: LIST 2\na.raw\nb.raw\nc.raw\n";
        let h = parse(list.as_bytes(), 0, Path::new("/d/h.nhdr")).unwrap();
        assert_eq!(h.layout.files.len(), 3);
        let pattern = header("type: float\ndimension: 3\nsizes: 4 4 3\nencoding: gzip\ndata file: slice%03d.raw.gz 1 3 1 2\n");
        let h = parse(pattern.as_bytes(), 0, Path::new("/d/h.nhdr")).unwrap();
        assert_eq!(h.layout.files[2].path, Path::new("/d/slice003.raw.gz"));
        assert!(h.layout.files_gzip);
        for (bad, code) in [
            ("type: float\ndimension: 3\nsizes: 4 4 3\nencoding: bzip2\nbyte skip: -1\n", "unsupported_format"),
            ("type: float\ndimension: 3\nsizes: 4 4 3\nencoding: lz4\n", "unsupported_format"),
            ("type: float\ndimension: 3\nsizes: 4 4 3\nencoding: ascii\nbyte skip: 4\n", "unsupported_format"),
            ("type: block\ndimension: 3\nsizes: 4 4 3\nencoding: raw\n", "unsupported_datatype"),
            ("type: float\ndimension: 3\nsizes: 4 0 3\nencoding: raw\n", "invalid_image"),
            ("type: float\ndimension: 2\nsizes: 4 4 3\nencoding: raw\n", "invalid_image"),
            ("type: float\ndimension: 3\nsizes: 4 4 3\nencoding: gzip\nbyte skip: -1\n", "unsupported_format"),
        ] {
            assert_eq!(parse(header(bad).as_bytes(), 0, Path::new("/h.nrrd")).unwrap_err().code, code, "{bad}");
        }
    }

    #[test]
    fn patterns_and_spaces() {
        assert_eq!(printf_int("s%03d.raw", 7).unwrap(), "s007.raw");
        assert_eq!(printf_int("s%d.raw", 12).unwrap(), "s12.raw");
        assert_eq!(ras_signs("left-posterior-superior"), Some([-1.0, -1.0, 1.0]));
        assert_eq!(ras_signs("RAS"), Some([1.0, 1.0, 1.0]));
        assert_eq!(ras_signs("LPST"), Some([-1.0, -1.0, 1.0]));
        assert_eq!(ras_signs("right-anterior-superior-time"), Some([1.0, 1.0, 1.0]));
        assert_eq!(ras_signs("scanner-xyz"), None);
    }
}
