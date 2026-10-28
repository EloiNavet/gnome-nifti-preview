//! Minimal RGB8 PNG encoder for the debugging CLI.

use std::io::Write;

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    let len = u32::try_from(payload.len()).expect("PNG chunk under 4 GiB (slices are bounded)");
    out.extend_from_slice(&len.to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(payload);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

pub fn encode_rgb(width: usize, height: usize, rgb: &[u8]) -> Vec<u8> {
    encode_rgb_with(width, height, rgb, 6)
}

/// `encode_rgb` with zlib compression `level` (0 to 9; 1 is the fastest
/// that still compresses, for pictures sent at once to a terminal).
pub fn encode_rgb_with(width: usize, height: usize, rgb: &[u8], level: u32) -> Vec<u8> {
    let stride = width * 3;
    let mut raw = Vec::with_capacity((stride + 1) * height);
    for row in rgb.chunks(stride) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(level.min(9)));
    z.write_all(&raw).expect("in-memory write");
    let idat = z.finish().expect("in-memory write");
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    // Slices are at most MAX_SLICE_SIDE pixels wide and high.
    ihdr.extend_from_slice(&u32::try_from(width).expect("bounded width").to_be_bytes());
    ihdr.extend_from_slice(&u32::try_from(height).expect("bounded height").to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}
