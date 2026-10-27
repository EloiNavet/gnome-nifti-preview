//! JPEG (ITU-T T.81) as DICOM uses it: baseline and extended sequential DCT
//! with Huffman coding, 8 or 12 bits (processes 1, 2 and 4), and lossless
//! (process 14, every predictor, 2 to 16 bits, point transform). Progressive,
//! hierarchical and arithmetic-coded images are refused.
//!
//! The decoder returns the components as coded (no colour conversion): the
//! caller knows from the DICOM photometric interpretation, and from the
//! JFIF/Adobe markers kept here, whether they are YCbCr.

use crate::error::{bail, PreviewError, Result};

/// Largest image decoded (pixels), checked before allocating.
const MAX_PIXELS: usize = 1 << 28;

pub struct Decoded {
    pub width: usize,
    pub height: usize,
    pub components: usize,
    pub precision: u8,
    /// Interleaved samples, `components` per pixel, rows top to bottom.
    pub samples: Vec<u16>,
    pub jfif: bool,
    /// Adobe APP14 colour transform flag (0: none, 1: YCbCr, 2: YCCK).
    pub adobe_transform: Option<u8>,
    pub component_ids: Vec<u8>,
    pub lossless: bool,
}

impl Decoded {
    /// Whether the markers say the components are YCbCr (JFIF, or an Adobe
    /// marker with a transform).
    pub fn declares_ycbcr(&self) -> bool {
        self.components == 3 && (self.jfif || self.adobe_transform.is_some_and(|t| t != 0))
    }

}

fn corrupt(what: impl std::fmt::Display) -> PreviewError {
    PreviewError::new("corrupt_data", format!("JPEG data could not be decoded: {what}."))
}

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61,
    54, 47, 55, 62, 63,
];

#[derive(Clone)]
struct Huffman {
    /// 8-bit lookahead: (symbol, length), length 0 when the code is longer.
    fast: Vec<(u8, u8)>,
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn new(counts: &[u8; 16], values: Vec<u8>) -> Result<Huffman> {
        let mut fast = vec![(0u8, 0u8); 256];
        let mut maxcode = [-1i32; 18];
        let mut valptr = [0i32; 17];
        let mut mincode = [0i32; 17];
        let mut code = 0i32;
        let mut k = 0usize;
        for len in 1..=16usize {
            let n = counts[len - 1] as usize;
            valptr[len] = k as i32;
            mincode[len] = code;
            for _ in 0..n {
                if k >= values.len() {
                    return Err(corrupt("Huffman table shorter than its counts"));
                }
                if len <= 8 {
                    let first = (code << (8 - len)) as usize;
                    for e in fast.iter_mut().skip(first).take(1 << (8 - len)) {
                        *e = (values[k], len as u8);
                    }
                }
                code += 1;
                k += 1;
            }
            if n > 0 {
                maxcode[len] = code - 1;
            }
            if code > (1 << len) {
                return Err(corrupt("invalid Huffman table"));
            }
            code <<= 1;
        }
        maxcode[17] = i32::MAX;
        Ok(Huffman { fast, maxcode, valptr, mincode, values })
    }
}

/// Bits of the entropy-coded data (byte stuffing removed). At a marker, or
/// at the end of the data, it reads zeros.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
    marker: Option<u8>,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Bits { data, pos, acc: 0, n: 0, marker: None }
    }

    fn fill(&mut self) {
        while self.n <= 56 {
            let mut byte = 0u8;
            if self.marker.is_none() && self.pos < self.data.len() {
                let b = self.data[self.pos];
                if b == 0xFF {
                    let next = self.data.get(self.pos + 1).copied().unwrap_or(0xD9);
                    if next == 0x00 {
                        self.pos += 2;
                        byte = 0xFF;
                    } else if next == 0xFF {
                        self.pos += 1;
                        continue;
                    } else {
                        self.marker = Some(next);
                    }
                } else {
                    self.pos += 1;
                    byte = b;
                }
            }
            self.acc |= (byte as u64) << (56 - self.n);
            self.n += 8;
        }
    }

    fn peek(&mut self, k: u32) -> u32 {
        if self.n < k {
            self.fill();
        }
        (self.acc >> (64 - k)) as u32
    }

    fn consume(&mut self, k: u32) {
        self.acc <<= k;
        self.n -= k;
    }

    fn get(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        let v = self.peek(k);
        self.consume(k);
        v
    }

    fn decode(&mut self, h: &Huffman) -> Result<u8> {
        let p = self.peek(16);
        let (sym, len) = h.fast[(p >> 8) as usize];
        if len > 0 {
            self.consume(len as u32);
            return Ok(sym);
        }
        for len in 9..=16usize {
            let code = (p >> (16 - len)) as i32;
            if code <= h.maxcode[len] {
                self.consume(len as u32);
                let i = h.valptr[len] + code - h.mincode[len];
                return h.values.get(i as usize).copied().ok_or_else(|| corrupt("bad Huffman code"));
            }
        }
        Err(corrupt("bad Huffman code"))
    }

    /// `s` bits as a signed difference (T.81 EXTEND).
    fn receive_extend(&mut self, s: u32) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = self.get(s) as i32;
        if v < (1 << (s - 1)) { v - (1 << s) + 1 } else { v }
    }

    /// At a restart: drop the bits left, step over the RSTn marker.
    fn restart(&mut self) {
        self.acc = 0;
        self.n = 0;
        if let Some(m) = self.marker {
            if (0xD0..=0xD7).contains(&m) {
                self.pos += 2;
                self.marker = None;
            }
            return;
        }
        // Look for the marker (data padded before it).
        while self.pos + 1 < self.data.len() {
            if self.data[self.pos] == 0xFF && (0xD0..=0xD7).contains(&self.data[self.pos + 1]) {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
    }
}

#[derive(Clone)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// Samples of the component (padded to whole MCUs).
    plane: Vec<u16>,
    width: usize,
    height: usize,
}

struct Frame {
    precision: u8,
    width: usize,
    height: usize,
    lossless: bool,
    comps: Vec<Component>,
    hmax: usize,
    vmax: usize,
    mcux: usize,
    mcuy: usize,
}

pub fn decode(data: &[u8]) -> Result<Decoded> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(corrupt("no start of image"));
    }
    let mut pos = 2usize;
    let mut qt = [[0u16; 64]; 4];
    let mut dc: [Option<Huffman>; 4] = Default::default();
    let mut ac: [Option<Huffman>; 4] = Default::default();
    let mut frame: Option<Frame> = None;
    let mut restart = 0usize;
    let mut jfif = false;
    let mut adobe = None;
    loop {
        // Next marker (fill bytes allowed).
        while pos < data.len() && data[pos] != 0xFF {
            pos += 1;
        }
        while pos < data.len() && data[pos] == 0xFF {
            pos += 1;
        }
        if pos >= data.len() {
            break;
        }
        let marker = data[pos];
        pos += 1;
        match marker {
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            0xD9 => break,
            _ => {}
        }
        if pos + 2 > data.len() {
            return Err(corrupt("truncated marker"));
        }
        let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        if len < 2 || pos + len > data.len() {
            return Err(corrupt("truncated marker segment"));
        }
        let seg = &data[pos + 2..pos + len];
        pos += len;
        match marker {
            0xE0 => jfif |= seg.starts_with(b"JFIF\0"),
            0xEE => {
                if seg.len() >= 12 && seg.starts_with(b"Adobe") {
                    adobe = Some(seg[11]);
                }
            }
            0xDB => read_dqt(seg, &mut qt)?,
            0xC4 => read_dht(seg, &mut dc, &mut ac)?,
            0xDD => {
                if seg.len() < 2 {
                    return Err(corrupt("bad restart interval"));
                }
                restart = u16::from_be_bytes([seg[0], seg[1]]) as usize;
            }
            0xC0 | 0xC1 | 0xC3 => {
                if frame.is_some() {
                    return Err(corrupt("two frame headers"));
                }
                frame = Some(read_sof(seg, marker == 0xC3)?);
            }
            0xC2 => bail!("unsupported_format", "Progressive JPEG in DICOM is not supported"),
            0xC5..=0xC7 => bail!("unsupported_format", "Hierarchical JPEG in DICOM is not supported"),
            0xC8..=0xCF if marker != 0xCC => bail!("unsupported_format", "Arithmetic-coded JPEG in DICOM is not supported"),
            0xDA => {
                let Some(f) = frame.as_mut() else { return Err(corrupt("scan before the frame header")) };
                pos = scan(data, pos, seg, f, &qt, &dc, &ac, restart)?;
            }
            0xDC => bail!("unsupported_format", "JPEG with a DNL marker is not supported"),
            _ => {} // APPn, COM, DAC…
        }
    }
    let Some(f) = frame else { return Err(corrupt("no frame header")) };
    // Upsample (libjpeg's "fancy" triangle filter for 2:1 chroma, else the
    // nearest sample) and interleave.
    let nc = f.comps.len();
    let mut samples = vec![0u16; f.width * f.height * nc];
    for (c, comp) in f.comps.iter().enumerate() {
        let full = upsample(comp, f.width, f.height, f.hmax, f.vmax, f.lossless);
        for (i, &v) in full.iter().enumerate() {
            samples[i * nc + c] = v;
        }
    }
    Ok(Decoded {
        width: f.width,
        height: f.height,
        components: nc,
        precision: f.precision,
        samples,
        jfif,
        adobe_transform: adobe,
        component_ids: f.comps.iter().map(|c| c.id).collect(),
        lossless: f.lossless,
    })
}

/// A component at the image size.
fn upsample(comp: &Component, width: usize, height: usize, hmax: usize, vmax: usize, lossless: bool) -> Vec<u16> {
    let (fh, fv) = (hmax / comp.h, vmax / comp.v);
    let exact = hmax % comp.h == 0 && vmax % comp.v == 0;
    // Samples actually coded (without the padding to whole blocks).
    let dw = (width * comp.h).div_ceil(hmax).min(comp.width);
    let dh = (height * comp.v).div_ceil(vmax).min(comp.height);
    let mut out = vec![0u16; width * height];
    if dw == 0 || dh == 0 {
        return out; // a component without samples (a corrupt frame)
    }
    let at = |x: usize, y: usize| comp.plane[y.min(dh - 1) * comp.width + x.min(dw - 1)] as i32;
    if !lossless && exact && fh == 2 && (fv == 1 || fv == 2) && dw >= 2 {
        for y in 0..height {
            let iy = y / fv;
            // Vertical: three quarters of this row, a quarter of the nearest other.
            let colsum = |x: usize| -> i32 {
                if fv == 1 {
                    at(x, iy) * 4
                } else {
                    let other = if y % 2 == 0 { iy.saturating_sub(1) } else { iy + 1 };
                    at(x, iy) * 3 + at(x, other)
                }
            };
            let row = &mut out[y * width..(y + 1) * width];
            for (x, o) in row.iter_mut().enumerate() {
                let ix = x / 2;
                let this = colsum(ix);
                let v = if x % 2 == 0 {
                    if ix == 0 { (this * 4 + 8) >> 4 } else { (this * 3 + colsum(ix - 1) + 8) >> 4 }
                } else if ix + 1 >= dw {
                    (this * 4 + 7) >> 4
                } else {
                    (this * 3 + colsum(ix + 1) + 7) >> 4
                };
                *o = v as u16;
            }
        }
        return out;
    }
    for y in 0..height {
        let sy = y * comp.v / vmax;
        for x in 0..width {
            out[y * width + x] = at(x * comp.h / hmax, sy) as u16;
        }
    }
    out
}

fn read_dqt(mut seg: &[u8], qt: &mut [[u16; 64]; 4]) -> Result<()> {
    while !seg.is_empty() {
        let (pq, tq) = ((seg[0] >> 4) as usize, (seg[0] & 15) as usize);
        if tq > 3 || pq > 1 {
            return Err(corrupt("bad quantisation table"));
        }
        let size = 64 * (pq + 1);
        if seg.len() < 1 + size {
            return Err(corrupt("short quantisation table"));
        }
        for k in 0..64 {
            qt[tq][k] = if pq == 0 { seg[1 + k] as u16 } else { u16::from_be_bytes([seg[1 + 2 * k], seg[2 + 2 * k]]) };
        }
        seg = &seg[1 + size..];
    }
    Ok(())
}

fn read_dht(mut seg: &[u8], dc: &mut [Option<Huffman>; 4], ac: &mut [Option<Huffman>; 4]) -> Result<()> {
    while !seg.is_empty() {
        if seg.len() < 17 {
            return Err(corrupt("short Huffman table"));
        }
        let (tc, th) = (seg[0] >> 4, (seg[0] & 15) as usize);
        if tc > 1 || th > 3 {
            return Err(corrupt("bad Huffman table"));
        }
        let counts: [u8; 16] = seg[1..17].try_into().unwrap();
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        if total > 256 || seg.len() < 17 + total {
            return Err(corrupt("short Huffman table"));
        }
        let table = Huffman::new(&counts, seg[17..17 + total].to_vec())?;
        if tc == 0 { dc[th] = Some(table) } else { ac[th] = Some(table) }
        seg = &seg[17 + total..];
    }
    Ok(())
}

fn read_sof(seg: &[u8], lossless: bool) -> Result<Frame> {
    if seg.len() < 6 {
        return Err(corrupt("short frame header"));
    }
    let precision = seg[0];
    let height = u16::from_be_bytes([seg[1], seg[2]]) as usize;
    let width = u16::from_be_bytes([seg[3], seg[4]]) as usize;
    let nc = seg[5] as usize;
    let ok_precision = if lossless { (2..=16).contains(&precision) } else { precision == 8 || precision == 12 };
    if !ok_precision {
        return Err(corrupt(format_args!("{precision}-bit samples")));
    }
    if height == 0 {
        bail!("unsupported_format", "JPEG with a DNL marker is not supported");
    }
    if width == 0 || nc == 0 || nc > 4 || seg.len() < 6 + 3 * nc {
        return Err(corrupt("bad frame header"));
    }
    if width.saturating_mul(height).saturating_mul(nc) > MAX_PIXELS {
        bail!("too_large", "JPEG image too large ({width} × {height})");
    }
    let mut comps = Vec::new();
    for i in 0..nc {
        let b = &seg[6 + 3 * i..9 + 3 * i];
        let (h, v) = ((b[1] >> 4) as usize, (b[1] & 15) as usize);
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) || b[2] > 3 {
            return Err(corrupt("bad component"));
        }
        if lossless && (h != 1 || v != 1) && nc > 1 {
            bail!("unsupported_format", "Subsampled lossless JPEG is not supported");
        }
        comps.push(Component { id: b[0], h, v, tq: b[2] as usize, plane: Vec::new(), width: 0, height: 0 });
    }
    let hmax = comps.iter().map(|c| c.h).max().unwrap();
    let vmax = comps.iter().map(|c| c.v).max().unwrap();
    let unit = if lossless { 1 } else { 8 };
    let mcux = width.div_ceil(unit * hmax);
    let mcuy = height.div_ceil(unit * vmax);
    for c in comps.iter_mut() {
        c.width = mcux * c.h * unit;
        c.height = mcuy * c.v * unit;
        c.plane = vec![0u16; c.width * c.height];
    }
    Ok(Frame { precision, width, height, lossless, comps, hmax, vmax, mcux, mcuy })
}

/// One scan; returns the position after its entropy-coded data.
#[allow(clippy::too_many_arguments)]
fn scan(
    data: &[u8],
    pos: usize,
    seg: &[u8],
    f: &mut Frame,
    qt: &[[u16; 64]; 4],
    dc: &[Option<Huffman>; 4],
    ac: &[Option<Huffman>; 4],
    restart: usize,
) -> Result<usize> {
    if seg.is_empty() {
        return Err(corrupt("short scan header"));
    }
    let ns = seg[0] as usize;
    if ns == 0 || ns > f.comps.len() || seg.len() < 1 + 2 * ns + 3 {
        return Err(corrupt("bad scan header"));
    }
    let mut members = Vec::new();
    for i in 0..ns {
        let id = seg[1 + 2 * i];
        let t = seg[2 + 2 * i];
        let Some(c) = f.comps.iter().position(|c| c.id == id) else { return Err(corrupt("scan of an unknown component")) };
        let (td, ta) = ((t >> 4) as usize, (t & 15) as usize);
        if td > 3 || ta > 3 {
            return Err(corrupt("bad table selector"));
        }
        members.push((c, td, ta));
    }
    let ss = seg[1 + 2 * ns];
    let al = seg[3 + 2 * ns] & 15;
    let mut bits = Bits::new(data, pos);
    if f.lossless {
        lossless_scan(&mut bits, f, &members, dc, restart, ss, al)?;
    } else {
        dct_scan(&mut bits, f, &members, qt, dc, ac, restart)?;
    }
    // Continue after the scan: at its ending marker.
    let mut p = bits.pos;
    while p + 1 < data.len() && !(data[p] == 0xFF && data[p + 1] != 0 && !(0xD0..=0xD7).contains(&data[p + 1])) {
        p += 1;
    }
    Ok(p)
}

/// IDCT basis: T[x][u] = c(u) cos((2x+1)uπ/16) / 2.
fn idct_table() -> [[f32; 8]; 8] {
    let mut t = [[0f32; 8]; 8];
    for (x, row) in t.iter_mut().enumerate() {
        for (u, v) in row.iter_mut().enumerate() {
            let c = if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
            *v = (c * ((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI / 16.0).cos() / 2.0) as f32;
        }
    }
    t
}

fn dct_scan(
    bits: &mut Bits,
    f: &mut Frame,
    members: &[(usize, usize, usize)],
    qt: &[[u16; 64]; 4],
    dc: &[Option<Huffman>; 4],
    ac: &[Option<Huffman>; 4],
    restart: usize,
) -> Result<()> {
    let t = idct_table();
    let top = (1i32 << f.precision) - 1;
    let shift = 1i32 << (f.precision - 1);
    let mut pred = vec![0i32; members.len()];
    let mut tables = Vec::new();
    for &(c, td, ta) in members {
        let (Some(d), Some(a)) = (dc[td].as_ref(), ac[ta].as_ref()) else { return Err(corrupt("missing Huffman table")) };
        tables.push((c, d, a, qt[f.comps[c].tq]));
    }
    let block = |bits: &mut Bits, k: usize, comp: &mut Component, bx: usize, by: usize, pred: &mut i32| -> Result<()> {
        let (_, d, a, q) = &tables[k];
        let mut coef = [0f32; 64];
        let s = bits.decode(d)? as u32;
        if s > 16 {
            return Err(corrupt("bad DC difference"));
        }
        *pred = pred.wrapping_add(bits.receive_extend(s));
        coef[0] = *pred as f32 * q[0] as f32;
        let mut i = 1usize;
        while i < 64 {
            let rs = bits.decode(a)?;
            let (r, s) = ((rs >> 4) as usize, (rs & 15) as u32);
            if s == 0 {
                if r == 15 {
                    i += 16;
                    continue;
                }
                break;
            }
            i += r;
            if i > 63 {
                return Err(corrupt("coefficient index past the block"));
            }
            coef[ZIGZAG[i]] = bits.receive_extend(s) as f32 * q[i] as f32;
            i += 1;
        }
        // Rows, then columns.
        let mut tmp = [0f32; 64];
        for v in 0..8 {
            for x in 0..8 {
                let mut s = 0f32;
                for u in 0..8 {
                    s += coef[v * 8 + u] * t[x][u];
                }
                tmp[v * 8 + x] = s;
            }
        }
        for (y, basis) in t.iter().enumerate() {
            let py = by * 8 + y;
            if py >= comp.height {
                break;
            }
            for x in 0..8 {
                let px = bx * 8 + x;
                if px >= comp.width {
                    break;
                }
                let mut s = 0f32;
                for (v, b) in basis.iter().enumerate() {
                    s += b * tmp[v * 8 + x];
                }
                comp.plane[py * comp.width + px] = ((s.round() as i32) + shift).clamp(0, top) as u16;
            }
        }
        Ok(())
    };
    let mut done = 0usize;
    let mut check_restart = |bits: &mut Bits, pred: &mut [i32]| {
        if restart > 0 && done > 0 && done % restart == 0 {
            bits.restart();
            pred.iter_mut().for_each(|p| *p = 0);
        }
        done += 1;
    };
    if members.len() == 1 {
        // Non-interleaved: the component's blocks in raster order.
        let c = members[0].0;
        let comp_w = (f.width * f.comps[c].h).div_ceil(f.hmax).div_ceil(8);
        let comp_h = (f.height * f.comps[c].v).div_ceil(f.vmax).div_ceil(8);
        let mut comp = std::mem::take(&mut f.comps[c]);
        let result = (|| {
            for by in 0..comp_h {
                for bx in 0..comp_w {
                    check_restart(bits, &mut pred);
                    block(bits, 0, &mut comp, bx, by, &mut pred[0])?;
                }
            }
            Ok(())
        })();
        f.comps[c] = comp;
        return result;
    }
    let mut comps: Vec<Component> = members.iter().map(|&(c, _, _)| std::mem::take(&mut f.comps[c])).collect();
    let result = (|| {
        for my in 0..f.mcuy {
            for mx in 0..f.mcux {
                check_restart(bits, &mut pred);
                for (k, comp) in comps.iter_mut().enumerate() {
                    for v in 0..comp.v {
                        for h in 0..comp.h {
                            block(bits, k, comp, mx * comp.h + h, my * comp.v + v, &mut pred[k])?;
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    for (k, &(c, _, _)) in members.iter().enumerate() {
        f.comps[c] = std::mem::take(&mut comps[k]);
    }
    result
}

impl Default for Component {
    fn default() -> Self {
        Component { id: 0, h: 1, v: 1, tq: 0, plane: Vec::new(), width: 0, height: 0 }
    }
}

fn lossless_scan(
    bits: &mut Bits,
    f: &mut Frame,
    members: &[(usize, usize, usize)],
    dc: &[Option<Huffman>; 4],
    restart: usize,
    predictor: u8,
    pt: u8,
) -> Result<()> {
    if !(1..=7).contains(&predictor) {
        return Err(corrupt(format_args!("predictor {predictor}")));
    }
    let p = f.precision as u32;
    if pt as u32 >= p {
        return Err(corrupt("bad point transform"));
    }
    let mut tables = Vec::new();
    for &(c, td, _) in members {
        let Some(d) = dc[td].as_ref() else { return Err(corrupt("missing Huffman table")) };
        tables.push((c, d));
    }
    let (w, h) = (f.width, f.height);
    let initial = 1i32 << (p - pt as u32 - 1);
    let mask = if p >= 16 { 0xFFFF } else { (1i32 << p) - 1 };
    let mut done = 0usize;
    // Rows of the current restart interval start here (first-row rules).
    let mut interval_row = 0usize;
    let mut interval_col = 0usize;
    let mut planes: Vec<Vec<u16>> = members.iter().map(|&(c, _, _)| std::mem::take(&mut f.comps[c].plane)).collect();
    let widths: Vec<usize> = members.iter().map(|&(c, _, _)| f.comps[c].width).collect();
    let result = (|| -> Result<()> {
        for y in 0..h {
            for x in 0..w {
                if restart > 0 && done > 0 && done % restart == 0 {
                    bits.restart();
                    interval_row = y;
                    interval_col = x;
                }
                done += 1;
                for (k, &(_, table)) in tables.iter().enumerate() {
                    let pw = widths[k];
                    let plane = &mut planes[k];
                    let at = |xx: usize, yy: usize| (plane[yy * pw + xx] >> pt) as i32;
                    let pred = if y == interval_row && x == interval_col {
                        initial
                    } else if y == interval_row {
                        at(x - 1, y)
                    } else if x == 0 {
                        at(0, y - 1)
                    } else {
                        let (ra, rb, rc) = (at(x - 1, y), at(x, y - 1), at(x - 1, y - 1));
                        match predictor {
                            1 => ra,
                            2 => rb,
                            3 => rc,
                            4 => ra + rb - rc,
                            5 => ra + ((rb - rc) >> 1),
                            6 => rb + ((ra - rc) >> 1),
                            _ => (ra + rb) >> 1,
                        }
                    };
                    let s = bits.decode(table)? as u32;
                    let diff = match s {
                        0..=15 => bits.receive_extend(s),
                        16 => 32768,
                        _ => return Err(corrupt("bad difference category")),
                    };
                    let v = (pred + diff) & mask;
                    plane[y * pw + x] = ((v << pt) & 0xFFFF) as u16;
                }
            }
        }
        Ok(())
    })();
    for (k, &(c, _, _)) in members.iter().enumerate() {
        f.comps[c].plane = std::mem::take(&mut planes[k]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_component_without_samples_fails_cleanly() {
        // Found by fuzzing: an empty component broke the upsampling.
        let _ = decode(include_bytes!("testdata/empty_component.jpg"));
    }

    /// A lossless JPEG (predictor 1, one component, 8 bits) written with a
    /// Huffman table where every category has a 4-bit code.
    fn lossless_jpeg(width: usize, height: usize, pixels: &[u8], predictor: u8) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        // DHT: class 0 table 0, 17 codes of length 5 (categories 0..16).
        let mut counts = [0u8; 16];
        counts[4] = 17;
        out.extend_from_slice(&[0xFF, 0xC4, 0, (2 + 1 + 16 + 17) as u8, 0x00]);
        out.extend_from_slice(&counts);
        out.extend((0..17u8).collect::<Vec<_>>());
        // SOF3
        out.extend_from_slice(&[0xFF, 0xC3, 0, 11, 8, 0, height as u8, 0, width as u8, 1, 1, 0x11, 0]);
        // SOS
        out.extend_from_slice(&[0xFF, 0xDA, 0, 8, 1, 1, 0x00, predictor, 0, 0]);
        let mut bits: Vec<bool> = Vec::new();
        let px = |x: usize, y: usize| pixels[y * width + x] as i32;
        for y in 0..height {
            for x in 0..width {
                let pred = if x == 0 && y == 0 {
                    128
                } else if y == 0 {
                    px(x - 1, y)
                } else if x == 0 {
                    px(x, y - 1)
                } else {
                    let (a, b, c) = (px(x - 1, y), px(x, y - 1), px(x - 1, y - 1));
                    match predictor { 1 => a, 2 => b, 4 => a + b - c, _ => (a + b) >> 1 }
                };
                let d = (px(x, y) - pred).rem_euclid(256);
                let d = if d >= 128 { d - 256 } else { d };
                let s = if d == 0 { 0 } else { 32 - (d.unsigned_abs()).leading_zeros() };
                for k in (0..5).rev() {
                    bits.push((s >> k) & 1 == 1);
                }
                let v = if d < 0 { d + (1 << s) - 1 } else { d } as u32;
                for k in (0..s).rev() {
                    bits.push((v >> k) & 1 == 1);
                }
            }
        }
        while bits.len() % 8 != 0 {
            bits.push(true);
        }
        for chunk in bits.chunks(8) {
            let b = chunk.iter().fold(0u8, |a, &bit| (a << 1) | bit as u8);
            out.push(b);
            if b == 0xFF {
                out.push(0);
            }
        }
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }

    #[test]
    fn lossless_predictors() {
        let (w, h) = (7, 5);
        let pixels: Vec<u8> = (0..w * h).map(|i| ((i * 37 + (i / 7) * 11) % 251) as u8).collect();
        for predictor in [1, 2, 4, 7] {
            let d = decode(&lossless_jpeg(w, h, &pixels, predictor)).unwrap();
            assert_eq!((d.width, d.height, d.components), (w, h, 1));
            let got: Vec<u8> = d.samples.iter().map(|&v| v as u8).collect();
            assert_eq!(got, pixels, "predictor {predictor}");
        }
    }

    fn image(i: usize) -> i32 {
        ((i * 37) % 251) as i32
    }

    #[test]
    fn baseline_gray_and_colour() {
        // Written by libjpeg (Pillow) from the 8 × 8 image above, quality 90.
        let d = decode(include_bytes!("testdata/gray8.jpg")).unwrap();
        assert_eq!((d.width, d.height, d.components, d.precision), (8, 8, 1, 8));
        let worst = d.samples.iter().enumerate().map(|(i, &v)| (v as i32 - image(i)).abs()).max().unwrap();
        assert!(worst <= 24, "{worst}");
        // Colour, 4:2:0, JFIF: YCbCr components.
        let d = decode(include_bytes!("testdata/rgb420.jpg")).unwrap();
        assert_eq!(d.components, 3);
        assert!(d.declares_ycbcr());
    }

    #[test]
    fn refuses_garbage() {
        assert!(decode(&[0xFF, 0xD8, 0xFF, 0xD9]).is_err());
        assert!(decode(b"not a jpeg").is_err());
        let mut j = lossless_jpeg(4, 4, &[9; 16], 1);
        j.truncate(j.len() / 2);
        let _ = decode(&j);
    }
}
