//! JPEG-LS (ITU-T T.87) decoding, lossless and near-lossless, 2 to 16 bits,
//! one to three components, not interleaved, line or sample interleaved,
//! with default or preset coding parameters. The algorithm follows the
//! standard (annex A), as CharLS implements it. Mapping tables, restart
//! markers and the HP colour transforms are refused.

use crate::error::{bail, PreviewError, Result};

pub struct Decoded {
    pub width: usize,
    pub height: usize,
    pub components: usize,
    pub precision: u8,
    /// Interleaved samples, `components` per pixel.
    pub samples: Vec<u16>,
}

const MAX_PIXELS: usize = 1 << 28;

fn corrupt(what: impl std::fmt::Display) -> PreviewError {
    PreviewError::new("corrupt_data", format!("JPEG-LS data could not be decoded: {what}."))
}

const J: [i32; 32] = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13, 14, 15];

#[derive(Clone, Copy)]
struct Params {
    maxval: i32,
    t1: i32,
    t2: i32,
    t3: i32,
    reset: i32,
}

#[derive(Clone, Copy)]
struct Traits {
    maxval: i32,
    near: i32,
    range: i32,
    qbpp: i32,
    limit: i32,
    t1: i32,
    t2: i32,
    t3: i32,
    reset: i32,
}

fn ceil_log2(mut n: i32) -> i32 {
    let mut k = 0;
    n -= 1;
    while n > 0 {
        k += 1;
        n >>= 1;
    }
    k
}

impl Traits {
    fn new(p: Params, near: i32, bits: i32) -> Result<Traits> {
        let maxval = p.maxval;
        if maxval < 1 || near < 0 || near > (maxval / 2).min(255) {
            return Err(corrupt("bad coding parameters"));
        }
        let range = (maxval + 2 * near) / (2 * near + 1) + 1;
        let qbpp = ceil_log2(range);
        let bpp = ceil_log2(maxval + 1).max(2);
        let limit = 2 * (bpp + bpp.max(8));
        let clamp = |i: i32, j: i32| if i > maxval || i < j { j } else { i };
        let (d1, d2, d3) = if maxval >= 128 {
            let factor = (maxval.min(4095) + 128) / 256;
            let t1 = clamp(factor + 2 + 3 * near, near + 1);
            let t2 = clamp(factor * (7 - 3) + 3 + 5 * near, t1);
            (t1, t2, clamp(factor * (21 - 4) + 4 + 7 * near, t2))
        } else {
            let factor = 256 / (maxval + 1);
            let t1 = clamp((3 / factor + 3 * near).max(2), near + 1);
            let t2 = clamp((7 / factor + 5 * near).max(3), t1);
            (t1, t2, clamp((21 / factor + 7 * near).max(4), t2))
        };
        let t1 = if p.t1 > 0 { p.t1 } else { d1 };
        let t2 = if p.t2 > 0 { p.t2 } else { d2 };
        let t3 = if p.t3 > 0 { p.t3 } else { d3 };
        let reset = if p.reset > 0 { p.reset } else { 64 };
        if !(near < t1 && t1 <= t2 && t2 <= t3 && t3 <= maxval.max(1)) && (p.t1 > 0 || p.t2 > 0 || p.t3 > 0) {
            return Err(corrupt("bad thresholds"));
        }
        let _ = bits;
        Ok(Traits { maxval, near, range, qbpp, limit, t1, t2, t3, reset })
    }

    fn quantize(&self, d: i32) -> i32 {
        if d <= -self.t3 {
            -4
        } else if d <= -self.t2 {
            -3
        } else if d <= -self.t1 {
            -2
        } else if d < -self.near {
            -1
        } else if d <= self.near {
            0
        } else if d < self.t1 {
            1
        } else if d < self.t2 {
            2
        } else if d < self.t3 {
            3
        } else {
            4
        }
    }

    fn correct(&self, p: i32) -> i32 {
        p.clamp(0, self.maxval)
    }

    fn reconstruct(&self, px: i32, err: i32) -> i32 {
        // (In 64 bits: a corrupt stream must not overflow.)
        let (near, step) = (self.near as i64, 2 * self.near as i64 + 1);
        let mut v = px as i64 + err as i64 * step;
        if v < -near {
            v += self.range as i64 * step;
        } else if v > self.maxval as i64 + near {
            v -= self.range as i64 * step;
        }
        v.clamp(0, self.maxval as i64) as i32
    }
}

#[derive(Clone, Copy)]
struct Context {
    a: i32,
    b: i32,
    c: i32,
    n: i32,
}

#[derive(Clone, Copy)]
struct RunContext {
    ritype: i32,
    a: i32,
    n: i32,
    nn: i32,
}

/// Bits of a JPEG-LS scan: after a 0xFF byte, the next byte holds 7 bits; a
/// 0xFF followed by a byte with its high bit set is a marker (zeros after).
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
    after_ff: bool,
    ended: bool,
    overrun: u32,
}

impl Bits<'_> {
    fn fill(&mut self) {
        while self.n <= 48 {
            if self.ended || self.pos >= self.data.len() {
                self.ended = true;
                self.overrun += 8;
                self.n += 8;
                continue;
            }
            let b = self.data[self.pos];
            if b == 0xFF && self.data.get(self.pos + 1).is_none_or(|&x| x & 0x80 != 0) {
                self.ended = true;
                continue;
            }
            self.pos += 1;
            if self.after_ff {
                self.acc |= ((b & 0x7F) as u64) << (64 - 7 - self.n);
                self.n += 7;
            } else {
                self.acc |= (b as u64) << (64 - 8 - self.n);
                self.n += 8;
            }
            self.after_ff = b == 0xFF;
        }
    }

    fn bits(&mut self, k: u32) -> Result<i32> {
        if k == 0 {
            return Ok(0);
        }
        if self.n < k {
            self.fill();
        }
        let v = (self.acc >> (64 - k)) as i32;
        self.acc <<= k;
        self.n -= k;
        if self.overrun > 64 * 8 {
            return Err(corrupt("data end before the image"));
        }
        Ok(v)
    }

    fn bit(&mut self) -> Result<bool> {
        Ok(self.bits(1)? == 1)
    }

    /// Zeros before the next 1 (consumed), at most `max`.
    fn high_bits(&mut self, max: i32) -> Result<i32> {
        let mut count = 0;
        loop {
            if self.n < 16 {
                self.fill();
            }
            if self.acc == 0 {
                count += self.n as i32;
                self.acc = 0;
                self.n = 0;
                if count > max || self.overrun > 64 * 8 {
                    return Err(corrupt("bad Golomb code"));
                }
                continue;
            }
            let z = self.acc.leading_zeros();
            if z >= self.n {
                count += self.n as i32;
                self.acc = 0;
                self.n = 0;
            } else {
                count += z as i32;
                self.acc <<= z + 1;
                self.n -= z + 1;
                if count > max {
                    return Err(corrupt("bad Golomb code"));
                }
                return Ok(count);
            }
            if count > max {
                return Err(corrupt("bad Golomb code"));
            }
        }
    }
}

struct Decoder<'a> {
    t: Traits,
    bits: Bits<'a>,
    ctx: Vec<Context>,
    run: [RunContext; 2],
    run_index: i32,
}

impl Decoder<'_> {
    fn value(&mut self, k: i32, limit: i32) -> Result<i32> {
        let high = self.bits.high_bits(limit)?;
        if high >= limit - (self.t.qbpp + 1) {
            return Ok(self.bits.bits(self.t.qbpp as u32)? + 1);
        }
        if k == 0 {
            return Ok(high);
        }
        Ok((high << k) + self.bits.bits(k as u32)?)
    }

    fn regular(&mut self, qs: i32, pred: i32) -> Result<i32> {
        let sign = if qs < 0 { -1 } else { 1 };
        let q = qs.unsigned_abs() as usize;
        let c = self.ctx[q];
        let mut k = 0;
        while (c.n << k) < c.a {
            k += 1;
            if k > 24 {
                return Err(corrupt("bad context"));
            }
        }
        let px = self.t.correct(pred + sign * c.c);
        let m = self.value(k, self.t.limit)?;
        let mut err = if m & 1 == 1 { -((m + 1) >> 1) } else { m >> 1 };
        if k == 0 && self.t.near == 0 && 2 * c.b + c.n - 1 < 0 {
            err = !err;
        }
        if err.abs() > 65535 {
            return Err(corrupt("error value out of range"));
        }
        // Context update (A.6).
        let ctx = &mut self.ctx[q];
        ctx.a += err.abs();
        ctx.b += err * (2 * self.t.near + 1);
        if ctx.n == self.t.reset {
            ctx.a >>= 1;
            ctx.b >>= 1;
            ctx.n >>= 1;
        }
        ctx.n += 1;
        if ctx.b + ctx.n <= 0 {
            ctx.b += ctx.n;
            if ctx.b <= -ctx.n {
                ctx.b = -ctx.n + 1;
            }
            if ctx.c > -128 {
                ctx.c -= 1;
            }
        } else if ctx.b > 0 {
            ctx.b -= ctx.n;
            if ctx.b > 0 {
                ctx.b = 0;
            }
            if ctx.c < 127 {
                ctx.c += 1;
            }
        }
        Ok(self.t.reconstruct(px, sign * err))
    }

    fn ri_error(&mut self, which: usize) -> Result<i32> {
        let c = self.run[which];
        let temp = c.a + (c.n >> 1) * c.ritype;
        let (mut ntest, mut k) = (c.n, 0);
        while ntest < temp {
            ntest <<= 1;
            k += 1;
            if k > 24 {
                return Err(corrupt("bad run context"));
            }
        }
        let limit = self.t.limit - J[self.run_index as usize] - 1;
        let em = self.value(k, limit)?;
        // As for regular samples: a larger error is a corrupt stream.
        if !(0..=2 * 65535 + 2).contains(&em) {
            return Err(corrupt("error value out of range"));
        }
        let t = em + c.ritype;
        let map = t & 1 == 1;
        let abs = (t + map as i32) / 2;
        let err = if (k != 0 || 2 * c.nn >= c.n) == map { -abs } else { abs };
        let ctx = &mut self.run[which];
        if err < 0 {
            ctx.nn += 1;
        }
        ctx.a += (em + 1 - ctx.ritype) >> 1;
        if ctx.n == self.t.reset {
            ctx.a >>= 1;
            ctx.n >>= 1;
            ctx.nn >>= 1;
        }
        ctx.n += 1;
        Ok(err)
    }

    /// Length of a run of samples equal to the left one, at most `max`.
    fn run_length(&mut self, max: usize) -> Result<usize> {
        let mut index = 0usize;
        while self.bits.bit()? {
            let step = 1usize << J[self.run_index as usize];
            let count = step.min(max - index);
            index += count;
            if count == step {
                self.run_index = (self.run_index + 1).min(31);
            }
            if index == max {
                break;
            }
        }
        if index != max {
            let j = J[self.run_index as usize];
            if j > 0 {
                index += self.bits.bits(j as u32)? as usize;
            }
        }
        if index > max {
            return Err(corrupt("run past the line"));
        }
        Ok(index)
    }

    /// One line of one component. `prev` and `cur` have width + 2 entries
    /// (entry 0 is x = -1).
    fn line(&mut self, prev: &mut [i32], cur: &mut [i32], width: usize) -> Result<()> {
        prev[width + 1] = prev[width];
        cur[0] = prev[1];
        let mut x = 0usize;
        while x < width {
            let i = x + 1;
            let (ra, rb, rc, rd) = (cur[i - 1], prev[i], prev[i - 1], prev[i + 1]);
            let qs = (self.t.quantize(rd - rb) * 9 + self.t.quantize(rb - rc)) * 9 + self.t.quantize(rc - ra);
            if qs != 0 {
                cur[i] = self.regular(qs, predict(ra, rb, rc))?;
                x += 1;
            } else {
                let n = self.run_length(width - x)?;
                for v in cur[i..i + n].iter_mut() {
                    *v = ra;
                }
                x += n;
                if x < width {
                    let rb = prev[x + 1];
                    let v = if (ra - rb).abs() <= self.t.near {
                        let e = self.ri_error(1)?;
                        self.t.reconstruct(ra, e)
                    } else {
                        let e = self.ri_error(0)?;
                        self.t.reconstruct(rb, e * if rb - ra < 0 { -1 } else { 1 })
                    };
                    cur[x + 1] = v;
                    self.run_index = (self.run_index - 1).max(0);
                    x += 1;
                }
            }
        }
        Ok(())
    }

    /// One line of three interleaved components (sample interleaving).
    fn line3(&mut self, prev: &mut [[i32; 3]], cur: &mut [[i32; 3]], width: usize) -> Result<()> {
        prev[width + 1] = prev[width];
        cur[0] = prev[1];
        let mut x = 0usize;
        while x < width {
            let i = x + 1;
            let (ra, rb, rc, rd) = (cur[i - 1], prev[i], prev[i - 1], prev[i + 1]);
            let qs: [i32; 3] = std::array::from_fn(|c| {
                (self.t.quantize(rd[c] - rb[c]) * 9 + self.t.quantize(rb[c] - rc[c])) * 9 + self.t.quantize(rc[c] - ra[c])
            });
            if qs.iter().any(|&q| q != 0) {
                let mut v = [0; 3];
                for c in 0..3 {
                    v[c] = self.regular(qs[c], predict(ra[c], rb[c], rc[c]))?;
                }
                cur[i] = v;
                x += 1;
            } else {
                let n = self.run_length(width - x)?;
                for v in cur[i..i + n].iter_mut() {
                    *v = ra;
                }
                x += n;
                if x < width {
                    let rb = prev[x + 1];
                    let mut v = [0; 3];
                    for c in 0..3 {
                        let e = self.ri_error(0)?;
                        v[c] = self.t.reconstruct(rb[c], e * if rb[c] - ra[c] < 0 { -1 } else { 1 });
                    }
                    cur[x + 1] = v;
                    self.run_index = (self.run_index - 1).max(0);
                    x += 1;
                }
            }
        }
        Ok(())
    }
}

fn predict(ra: i32, rb: i32, rc: i32) -> i32 {
    if ra < rb {
        if rc < ra {
            return rb;
        }
        if rc > rb {
            return ra;
        }
    } else {
        if rc < rb {
            return ra;
        }
        if rc > ra {
            return rb;
        }
    }
    ra + rb - rc
}

struct Frame {
    precision: u8,
    width: usize,
    height: usize,
    ids: Vec<u8>,
}

pub fn decode(data: &[u8]) -> Result<Decoded> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(corrupt("no start of image"));
    }
    let mut pos = 2usize;
    let mut frame: Option<Frame> = None;
    let mut preset = Params { maxval: 0, t1: 0, t2: 0, t3: 0, reset: 0 };
    let mut out: Vec<u16> = Vec::new();
    loop {
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
            0xD8 => continue,
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
            0xF7 => {
                if seg.len() < 6 {
                    return Err(corrupt("short frame header"));
                }
                let precision = seg[0];
                let height = u16::from_be_bytes([seg[1], seg[2]]) as usize;
                let width = u16::from_be_bytes([seg[3], seg[4]]) as usize;
                let nc = seg[5] as usize;
                if !(2..=16).contains(&precision) || width == 0 || height == 0 || !(1..=4).contains(&nc) || seg.len() < 6 + 3 * nc {
                    return Err(corrupt("bad frame header"));
                }
                if width * height * nc > MAX_PIXELS {
                    bail!("too_large", "JPEG-LS image too large ({width} × {height})");
                }
                let ids = (0..nc).map(|i| seg[6 + 3 * i]).collect();
                for i in 0..nc {
                    if seg[7 + 3 * i] != 0x11 {
                        bail!("unsupported_format", "Subsampled JPEG-LS is not supported");
                    }
                }
                out = vec![0u16; width * height * nc];
                frame = Some(Frame { precision, width, height, ids });
            }
            0xF8 => {
                if seg.first() == Some(&1) && seg.len() >= 11 {
                    let w = |o: usize| u16::from_be_bytes([seg[o], seg[o + 1]]) as i32;
                    preset = Params { maxval: w(1), t1: w(3), t2: w(5), t3: w(7), reset: w(9) };
                } else {
                    bail!("unsupported_format", "JPEG-LS mapping tables are not supported");
                }
            }
            0xDD => {
                if seg.len() >= 2 && u16::from_be_bytes([seg[0], seg[1]]) != 0 {
                    bail!("unsupported_format", "JPEG-LS with restart markers is not supported");
                }
            }
            0xE8 if seg.starts_with(b"mrfx") => bail!("unsupported_format", "JPEG-LS with an HP colour transform is not supported"),
            0xDA => {
                let Some(f) = frame.as_ref() else { return Err(corrupt("scan before the frame header")) };
                if seg.is_empty() {
                    return Err(corrupt("short scan header"));
                }
                let ns = seg[0] as usize;
                if ns == 0 || seg.len() < 1 + 2 * ns + 3 {
                    return Err(corrupt("bad scan header"));
                }
                let mut comps = Vec::new();
                for i in 0..ns {
                    let id = seg[1 + 2 * i];
                    if seg[2 + 2 * i] != 0 {
                        bail!("unsupported_format", "JPEG-LS mapping tables are not supported");
                    }
                    let Some(c) = f.ids.iter().position(|&x| x == id) else { return Err(corrupt("unknown component")) };
                    comps.push(c);
                }
                let near = seg[1 + 2 * ns] as i32;
                let ilv = seg[2 + 2 * ns];
                if seg[3 + 2 * ns] & 15 != 0 {
                    bail!("unsupported_format", "JPEG-LS point transform is not supported");
                }
                let maxval = if preset.maxval > 0 { preset.maxval } else { (1i32 << f.precision) - 1 };
                let t = Traits::new(Params { maxval, ..preset }, near, f.precision as i32)?;
                let end = scan(&data[pos..], &t, f, &comps, ilv, &mut out)?;
                pos += end;
            }
            _ => {}
        }
    }
    let Some(f) = frame else { return Err(corrupt("no frame header")) };
    Ok(Decoded { width: f.width, height: f.height, components: f.ids.len(), precision: f.precision, samples: out })
}

/// Decode one scan into `out` (interleaved image); returns the bytes used.
fn scan(data: &[u8], t: &Traits, f: &Frame, comps: &[usize], ilv: u8, out: &mut [u16]) -> Result<usize> {
    let a0 = ((t.range + 32) / 64).max(2);
    let mut d = Decoder {
        t: *t,
        bits: Bits { data, pos: 0, acc: 0, n: 0, after_ff: false, ended: false, overrun: 0 },
        ctx: vec![Context { a: a0, b: 0, c: 0, n: 1 }; 365],
        run: [RunContext { ritype: 0, a: a0, n: 1, nn: 0 }, RunContext { ritype: 1, a: a0, n: 1, nn: 0 }],
        run_index: 0,
    };
    let (w, h, nc) = (f.width, f.height, f.ids.len());
    match (ilv, comps.len()) {
        (2, 3) => {
            let mut prev = vec![[0i32; 3]; w + 2];
            let mut cur = vec![[0i32; 3]; w + 2];
            for y in 0..h {
                d.line3(&mut prev, &mut cur, w)?;
                for x in 0..w {
                    for k in 0..3 {
                        out[(y * w + x) * nc + comps[k]] = cur[x + 1][k] as u16;
                    }
                }
                std::mem::swap(&mut prev, &mut cur);
            }
        }
        (2, _) => bail!("unsupported_format", "JPEG-LS sample interleaving of {} components is not supported", comps.len()),
        (0 | 1, _) => {
            if ilv == 0 && comps.len() != 1 {
                return Err(corrupt("several components in a non-interleaved scan"));
            }
            let n = comps.len();
            let mut prev = vec![vec![0i32; w + 2]; n];
            let mut cur = vec![vec![0i32; w + 2]; n];
            let mut run_index = vec![0i32; n];
            for y in 0..h {
                for k in 0..n {
                    d.run_index = run_index[k];
                    d.line(&mut prev[k], &mut cur[k], w)?;
                    run_index[k] = d.run_index;
                    for x in 0..w {
                        out[(y * w + x) * nc + comps[k]] = cur[k][x + 1] as u16;
                    }
                    std::mem::swap(&mut prev[k], &mut cur[k]);
                }
            }
        }
        _ => return Err(corrupt(format_args!("interleave mode {ilv}"))),
    }
    Ok(d.bits.pos)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_huge_run_interruption_error_fails_cleanly() {
        // Found by fuzzing: an error value that overflowed the reconstruction.
        assert!(super::decode(include_bytes!("testdata/overflow.jls")).is_err());
    }

    use super::*;

    /// The 8 × 8 test image the streams of `testdata/` were made from
    /// (CharLS through pyjpegls).
    fn image(i: usize) -> u16 {
        ((i * 37) % 251) as u16
    }

    #[test]
    fn lossless_gray() {
        let d = decode(include_bytes!("testdata/gray8.jls")).unwrap();
        assert_eq!((d.width, d.height, d.components, d.precision), (8, 8, 1, 8));
        assert_eq!(d.samples, (0..64).map(image).collect::<Vec<_>>());
    }

    #[test]
    fn near_lossless_16_bits() {
        let d = decode(include_bytes!("testdata/near16.jls")).unwrap();
        for (i, &v) in d.samples.iter().enumerate() {
            assert!((v as i32 - image(i) as i32 * 200).abs() <= 3, "sample {i}: {v}");
        }
    }

    #[test]
    fn sample_interleaved_colour() {
        let d = decode(include_bytes!("testdata/rgb_ilv2.jls")).unwrap();
        assert_eq!(d.components, 3);
        for i in 0..64 {
            let a = image(i);
            assert_eq!(&d.samples[3 * i..3 * i + 3], &[a, 255 - a, a / 2]);
        }
    }

    #[test]
    fn corrupt_streams_fail() {
        let good = include_bytes!("testdata/rgb_ilv2.jls");
        for cut in (0..good.len()).step_by(7) {
            let _ = decode(&good[..cut]);
        }
        let mut flipped = good.to_vec();
        for i in (40..flipped.len()).step_by(5) {
            flipped[i] ^= 0x5A;
        }
        let _ = decode(&flipped);
    }
}
