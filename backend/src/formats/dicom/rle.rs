//! DICOM RLE Lossless (PS3.5 annex G): per frame, a 64-byte header with up
//! to 15 segment offsets, each segment a PackBits-coded plane of bytes:
//! for each sample, its most significant byte first.

use super::pixels::{native_colour, Frame, PixelDesc, Samples};
use crate::error::{bail, Result};

fn corrupt(what: &str) -> crate::error::PreviewError {
    crate::error::PreviewError::new("corrupt_data", format!("DICOM RLE data could not be decoded: {what}."))
}

/// Decode one PackBits segment into exactly `out.len()` bytes (missing bytes
/// are an error, extra ones ignored).
fn packbits(src: &[u8], out: &mut [u8]) -> Result<()> {
    let (mut i, mut o) = (0usize, 0usize);
    while o < out.len() {
        let Some(&h) = src.get(i) else { return Err(corrupt("segment too short")) };
        i += 1;
        let n = h as i8;
        if n >= 0 {
            let count = n as usize + 1;
            let Some(lit) = src.get(i..i + count) else { return Err(corrupt("literal run past the segment")) };
            let take = count.min(out.len() - o);
            out[o..o + take].copy_from_slice(&lit[..take]);
            o += take;
            i += count;
        } else if n != -128 {
            let count = (1 - n as i16) as usize;
            let Some(&b) = src.get(i) else { return Err(corrupt("repeat run past the segment")) };
            i += 1;
            let take = count.min(out.len() - o);
            out[o..o + take].fill(b);
            o += take;
        }
    }
    Ok(())
}

pub fn decode(d: &PixelDesc, data: &[u8]) -> Result<Frame> {
    if data.len() < 64 {
        return Err(corrupt("header missing"));
    }
    let word = |k: usize| u32::from_le_bytes([data[4 * k], data[4 * k + 1], data[4 * k + 2], data[4 * k + 3]]) as usize;
    let count = word(0);
    let n = d.pixels();
    if d.bits_allocated == 1 && count == 1 && data.len() >= 64 {
        // 1-bit images: encoders write one byte per pixel (pydicom,
        // highdicom), or the packed bits.
        let start = word(1).max(64).min(data.len());
        let mut bytes = vec![0u8; n];
        if packbits(&data[start..], &mut bytes).is_ok() {
            return Ok(Frame { samples: Samples::Int(bytes.iter().map(|&b| (b & 1) as u32).collect()), colour: native_colour(d.photometric) });
        }
    }
    let (bytes_per_sample, plane) = if d.bits_allocated == 1 { (1usize, n.div_ceil(8)) } else { (d.bits_allocated as usize / 8, n) };
    let expected = d.spp * bytes_per_sample;
    if count != expected || count > 15 {
        bail!("corrupt_data", "DICOM RLE data could not be decoded: {count} segments, {expected} expected.");
    }
    let mut offsets: Vec<usize> = (1..=count).map(word).collect();
    offsets.push(data.len());
    let mut planes = vec![0u8; count * plane];
    for s in 0..count {
        let (start, end) = (offsets[s], offsets[s + 1].max(offsets[s]).min(data.len()));
        if start < 64 || start > data.len() {
            return Err(corrupt("bad segment offset"));
        }
        packbits(&data[start..end], &mut planes[s * plane..(s + 1) * plane])?;
    }
    let samples = if d.bits_allocated == 1 {
        Samples::Int((0..n).map(|i| ((planes[i / 8] >> (i % 8)) & 1) as u32).collect())
    } else {
        let mut out = Vec::with_capacity(n * d.spp);
        for i in 0..n {
            for c in 0..d.spp {
                let mut v = 0u32;
                for b in 0..bytes_per_sample {
                    v = (v << 8) | planes[(c * bytes_per_sample + b) * plane + i] as u32;
                }
                out.push(v);
            }
        }
        Samples::Int(out)
    };
    Ok(Frame { samples, colour: native_colour(d.photometric) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits_runs() {
        let mut out = [0u8; 7];
        // 3 literals, then 4 × 9.
        packbits(&[2, 1, 2, 3, (-3i8) as u8, 9], &mut out).unwrap();
        assert_eq!(out, [1, 2, 3, 9, 9, 9, 9]);
        assert!(packbits(&[2, 1], &mut out).is_err());
        let mut out = [0u8; 2];
        packbits(&[0x80, 1, 5, 6], &mut out).unwrap();
        assert_eq!(out, [5, 6]);
    }
}
