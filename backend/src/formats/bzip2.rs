//! bzip2 decompression (NRRD's `bzip2` encoding), written from the format:
//! each block is a Burrows–Wheeler transform whose move-to-front indices
//! and runs are Huffman coded, then the bytes run-length coded. Every
//! length, table and index read is checked, and each block's CRC (also of
//! the block the output stops in): a bad stream is an error, never a panic.
//! The output stops at `limit` bytes.

use std::io::Read;

use crate::error::{PreviewError, Result};

const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
const END_MAGIC: u64 = 0x1772_4538_5090;
const MAX_GROUPS: usize = 6;
const MAX_SELECTORS: usize = 18002;
const MAX_CODE_LEN: usize = 20;
const GROUP_SIZE: usize = 50;

fn corrupt(what: &str) -> PreviewError {
    PreviewError::new("corrupt_data", format!("bzip2 data could not be read: {what}"))
}

/// Bits, most significant first.
struct Bits<R: Read> {
    r: R,
    buf: u64,
    n: u32,
}

impl<R: Read> Bits<R> {
    /// One more byte in the buffer; false at the end of the input.
    fn fill(&mut self) -> Result<bool> {
        let mut b = [0u8; 1];
        loop {
            match self.r.read(&mut b) {
                Ok(0) => return Ok(false),
                Ok(_) => {
                    self.buf = (self.buf << 8) | b[0] as u64;
                    self.n += 8;
                    return Ok(true);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(crate::source::corrupt(e)),
            }
        }
    }

    fn bits(&mut self, k: u32) -> Result<u32> {
        while self.n < k {
            if !self.fill()? {
                return Err(corrupt("the stream ends early"));
            }
        }
        self.n -= k;
        Ok(((self.buf >> self.n) & ((1u64 << k) - 1)) as u32)
    }

    fn bit(&mut self) -> Result<bool> {
        Ok(self.bits(1)? == 1)
    }

    /// The next byte boundary (streams start on one).
    fn align(&mut self) {
        self.n -= self.n % 8;
    }

    /// Another stream follows (its first byte is read), or the input ends.
    fn more(&mut self) -> Result<bool> {
        Ok(self.n >= 8 || self.fill()?)
    }
}

fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (i, e) in t.iter_mut().enumerate() {
        let mut c = (i as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 { (c << 1) ^ 0x04C1_1DB7 } else { c << 1 };
        }
        *e = c;
    }
    t
}

/// A canonical Huffman code (codes in order of length, then of symbol).
struct Code {
    /// Per length: first code, index of its first symbol in `symbols`, count.
    first: [u32; MAX_CODE_LEN + 1],
    start: [usize; MAX_CODE_LEN + 1],
    count: [u32; MAX_CODE_LEN + 1],
    symbols: Vec<u16>,
}

impl Code {
    fn new(lengths: &[u8]) -> Code {
        let mut count = [0u32; MAX_CODE_LEN + 1];
        for &l in lengths {
            count[l as usize] += 1;
        }
        let (mut first, mut start) = ([0u32; MAX_CODE_LEN + 1], [0usize; MAX_CODE_LEN + 1]);
        let (mut code, mut index) = (0u32, 0usize);
        for len in 1..=MAX_CODE_LEN {
            first[len] = code;
            start[len] = index;
            code = (code + count[len]) << 1;
            index += count[len] as usize;
        }
        let mut symbols = vec![0u16; lengths.len()];
        let mut next = start;
        for (s, &l) in lengths.iter().enumerate() {
            symbols[next[l as usize]] = s as u16;
            next[l as usize] += 1;
        }
        Code { first, start, count, symbols }
    }

    fn decode<R: Read>(&self, b: &mut Bits<R>) -> Result<u16> {
        let mut code = 0u32;
        for len in 1..=MAX_CODE_LEN {
            code = (code << 1) | b.bits(1)?;
            let k = code.wrapping_sub(self.first[len]);
            if code >= self.first[len] && k < self.count[len] {
                return Ok(self.symbols[self.start[len] + k as usize]);
            }
        }
        Err(corrupt("bad Huffman code"))
    }
}

/// Decompress the bzip2 streams of `input` (one after the other) into `out`,
/// at most `limit` bytes.
pub fn decompress(input: impl Read, limit: usize, out: &mut Vec<u8>) -> Result<()> {
    let table = crc_table();
    let mut b = Bits { r: input, buf: 0, n: 0 };
    let mut streams = 0;
    while out.len() < limit && (streams == 0 || b.more()?) {
        streams += 1;
        if b.bits(8)? != b'B' as u32 || b.bits(8)? != b'Z' as u32 || b.bits(8)? != b'h' as u32 {
            return Err(corrupt("no bzip2 signature"));
        }
        let level = b.bits(8)?;
        if !(b'1' as u32..=b'9' as u32).contains(&level) {
            return Err(corrupt("bad block size"));
        }
        let block_max = (level - b'0' as u32) as usize * 100_000;
        let mut combined = 0u32;
        loop {
            let magic = ((b.bits(24)? as u64) << 24) | b.bits(24)? as u64;
            if magic == END_MAGIC {
                if b.bits(32)? != combined {
                    return Err(corrupt("stream CRC mismatch"));
                }
                b.align();
                break;
            }
            if magic != BLOCK_MAGIC {
                return Err(corrupt("bad block signature"));
            }
            let crc = b.bits(32)?;
            if block(&mut b, block_max, limit, out, &table)? != crc {
                return Err(corrupt("block CRC mismatch"));
            }
            if out.len() >= limit {
                return Ok(());
            }
            combined = combined.rotate_left(1) ^ crc;
        }
    }
    Ok(())
}

/// One block, appended to `out` (up to `limit`); its CRC, of all its bytes.
fn block<R: Read>(b: &mut Bits<R>, block_max: usize, limit: usize, out: &mut Vec<u8>, table: &[u32; 256]) -> Result<u32> {
    if b.bit()? {
        return Err(PreviewError::new("unsupported_format", "Randomised bzip2 blocks (bzip2 0.9.0) are not supported"));
    }
    let orig_ptr = b.bits(24)? as usize;
    // The bytes used, in order.
    let mut used = Vec::new();
    let groups16 = b.bits(16)?;
    for i in 0..16 {
        if groups16 & (0x8000 >> i) != 0 {
            let bits = b.bits(16)?;
            for j in 0..16 {
                if bits & (0x8000 >> j) != 0 {
                    used.push((i * 16 + j) as u8);
                }
            }
        }
    }
    if used.is_empty() {
        return Err(corrupt("no symbols"));
    }
    let alpha = used.len() + 2;
    let n_groups = b.bits(3)? as usize;
    if !(2..=MAX_GROUPS).contains(&n_groups) {
        return Err(corrupt("bad number of Huffman tables"));
    }
    let n_selectors = b.bits(15)? as usize;
    if n_selectors == 0 {
        return Err(corrupt("no selectors"));
    }
    // Selectors: move-to-front coded, in unary.
    let mut order: Vec<u8> = (0..n_groups as u8).collect();
    let mut selectors = Vec::with_capacity(n_selectors.min(MAX_SELECTORS));
    for i in 0..n_selectors {
        let mut j = 0;
        while b.bit()? {
            j += 1;
            if j >= n_groups {
                return Err(corrupt("bad selector"));
            }
        }
        let v = order[j];
        order.copy_within(0..j, 1);
        order[0] = v;
        if i < MAX_SELECTORS {
            selectors.push(v);
        }
    }
    // Code lengths of each table, as deltas.
    let mut codes = Vec::with_capacity(n_groups);
    for _ in 0..n_groups {
        let mut len = b.bits(5)? as i32;
        let mut lengths = vec![0u8; alpha];
        for l in lengths.iter_mut() {
            loop {
                if !(1..=MAX_CODE_LEN as i32).contains(&len) {
                    return Err(corrupt("bad code length"));
                }
                if !b.bit()? {
                    break;
                }
                len += if b.bit()? { -1 } else { 1 };
            }
            *l = len as u8;
        }
        codes.push(Code::new(&lengths));
    }

    // Symbols: runs of the front byte (RUNA, RUNB), move-to-front indices,
    // end of block.
    let eob = (alpha - 1) as u16;
    let mut mtf: Vec<u8> = (0..used.len()).map(|i| i as u8).collect();
    let mut tt: Vec<u32> = Vec::new();
    let mut counts = [0usize; 256];
    let (mut run, mut run_bit) = (0usize, 1usize);
    let (mut group, mut left) = (0usize, 0usize);
    loop {
        if left == 0 {
            if group >= selectors.len() {
                return Err(corrupt("out of selectors"));
            }
            group += 1;
            left = GROUP_SIZE;
        }
        left -= 1;
        let sym = codes[selectors[group - 1] as usize].decode(b)?;
        if sym <= 1 {
            run += (sym as usize + 1) * run_bit;
            run_bit <<= 1;
            if run > block_max {
                return Err(corrupt("run longer than the block"));
            }
            continue;
        }
        if run > 0 {
            if tt.len() + run > block_max {
                return Err(corrupt("block longer than declared"));
            }
            let byte = used[mtf[0] as usize];
            counts[byte as usize] += run;
            tt.extend(std::iter::repeat_n(byte as u32, run));
            (run, run_bit) = (0, 1);
        }
        if sym == eob {
            break;
        }
        let idx = sym as usize - 1;
        if idx >= mtf.len() {
            return Err(corrupt("bad symbol"));
        }
        let v = mtf[idx];
        mtf.copy_within(0..idx, 1);
        mtf[0] = v;
        if tt.len() >= block_max {
            return Err(corrupt("block longer than declared"));
        }
        let byte = used[v as usize];
        counts[byte as usize] += 1;
        tt.push(byte as u32);
    }
    let mut crc = !0u32;
    if tt.is_empty() {
        return Ok(!crc);
    }
    if orig_ptr >= tt.len() {
        return Err(corrupt("bad block origin"));
    }
    // Inverse Burrows–Wheeler: each entry keeps its byte (low 8 bits) and
    // the position of the next one.
    let mut next = [0usize; 256];
    let mut sum = 0;
    for (c, n) in counts.iter().enumerate() {
        next[c] = sum;
        sum += n;
    }
    for i in 0..tt.len() {
        let c = (tt[i] & 0xFF) as usize;
        tt[next[c]] |= (i as u32) << 8;
        next[c] += 1;
    }
    // Then the runs of four equal bytes and a count. Every byte goes into
    // the CRC; those past `limit` are not kept.
    let mut emit = |x: u8, n: usize, out: &mut Vec<u8>| {
        for _ in 0..n {
            crc = (crc << 8) ^ table[((crc >> 24) as u8 ^ x) as usize];
        }
        out.extend(std::iter::repeat_n(x, n.min(limit.saturating_sub(out.len()))));
    };
    let mut pos = (tt[orig_ptr] >> 8) as usize;
    let (mut last, mut same) = (-1i32, 0);
    for _ in 0..tt.len() {
        let e = tt[pos];
        let byte = (e & 0xFF) as u8;
        pos = (e >> 8) as usize;
        if same == 4 {
            emit(last as u8, byte as usize, out);
            (last, same) = (-1, 0);
        } else {
            if byte as i32 == last {
                same += 1;
            } else {
                (last, same) = (byte as i32, 1);
            }
            emit(byte, 1, out);
        }
    }
    Ok(!crc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unbz(data: &[u8], limit: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        decompress(data, limit, &mut out)?;
        Ok(out)
    }

    #[test]
    fn known_streams() {
        // `printf 'hello hello hello\n' | bzip2 -9`
        let hello = include_bytes!("testdata/hello.bz2");
        assert_eq!(unbz(hello, 1 << 20).unwrap(), b"hello hello hello\n");
        // Two streams one after the other; the limit stops early.
        let two = [hello.as_slice(), hello].concat();
        assert_eq!(unbz(&two, 1 << 20).unwrap(), b"hello hello hello\nhello hello hello\n");
        assert_eq!(unbz(&two, 7).unwrap(), b"hello h");
        // The CRC is checked in the block the output stops in.
        let mut bad = hello.to_vec();
        bad[16] ^= 1;
        assert!(unbz(&bad, 5).is_err());
        // An empty input (`bzip2 < /dev/null`).
        assert_eq!(unbz(b"BZh9\x17\x72\x45\x38\x50\x90\0\0\0\0", 10).unwrap(), b"");
    }

    #[test]
    fn damaged_streams_fail_cleanly() {
        let hello = include_bytes!("testdata/hello.bz2");
        for cut in 0..hello.len() {
            assert!(unbz(&hello[..cut], 1 << 20).is_err() || cut == 0, "cut at {cut}");
        }
        for i in 4..hello.len() {
            for bit in 0..8 {
                let mut m = hello.to_vec();
                m[i] ^= 1 << bit;
                // Never a panic; almost always an error (the CRC).
                let _ = unbz(&m, 1 << 20);
            }
        }
        assert!(unbz(b"BZh0", 10).is_err());
        assert!(unbz(b"PK\x03\x04", 10).is_err());
    }
}
