//! Sixel images: a palette of at most the terminal's colour registers,
//! then bands of six pixel rows, one pass per colour, run-length encoded.
//!
//! Grey levels, label colours and the 256-entry palettes usually fit the
//! registers exactly; otherwise the colours are reduced by median cut
//! (each box of colours split at its median along its widest channel).

use std::collections::HashMap;
use std::io::Write;

use crate::picture::Picture;

/// A palette of at most `max` colours for `pic`, and the palette index of
/// each pixel.
pub fn quantize(pic: &Picture, max: usize) -> (Vec<[u8; 3]>, Vec<u16>) {
    let max = max.clamp(1, 256);
    let mut counts: HashMap<[u8; 3], u32> = HashMap::new();
    for px in pic.rgb.chunks_exact(3) {
        *counts.entry([px[0], px[1], px[2]]).or_insert(0) += 1;
    }
    let mut colours: Vec<([u8; 3], u32)> = counts.into_iter().collect();
    colours.sort_unstable();
    let (palette, of) = if colours.len() <= max {
        let of: HashMap<[u8; 3], u16> = colours.iter().enumerate().map(|(i, (c, _))| (*c, i as u16)).collect();
        (colours.iter().map(|(c, _)| *c).collect(), of)
    } else {
        median_cut(&mut colours, max)
    };
    let index = pic.rgb.chunks_exact(3).map(|px| of[&[px[0], px[1], px[2]]]).collect();
    (palette, index)
}

fn median_cut(colours: &mut [([u8; 3], u32)], max: usize) -> (Vec<[u8; 3]>, HashMap<[u8; 3], u16>) {
    let range = |s: &[([u8; 3], u32)], ch: usize| {
        let (lo, hi) = s.iter().fold((255u8, 0u8), |(lo, hi), (c, _)| (lo.min(c[ch]), hi.max(c[ch])));
        hi.saturating_sub(lo)
    };
    let mut boxes: Vec<std::ops::Range<usize>> = Vec::new();
    boxes.push(0..colours.len());
    while boxes.len() < max {
        // The box with the widest channel; one colour cannot be split.
        let pick = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(i, b)| (i, (0..3).map(|ch| range(&colours[b.clone()], ch)).max().unwrap_or(0)))
            .max_by_key(|&(_, r)| r);
        let Some((i, r)) = pick else { break };
        if r == 0 {
            break;
        }
        let b = boxes.swap_remove(i);
        let s = &mut colours[b.clone()];
        let ch = (0..3).max_by_key(|&ch| range(s, ch)).unwrap_or(0);
        s.sort_unstable_by_key(|(c, _)| c[ch]);
        let total: u64 = s.iter().map(|(_, n)| *n as u64).sum();
        let mut acc = 0u64;
        let mut cut = 1;
        for (k, (_, n)) in s.iter().enumerate() {
            acc += *n as u64;
            if acc * 2 >= total {
                cut = (k + 1).clamp(1, s.len() - 1);
                break;
            }
        }
        boxes.push(b.start..b.start + cut);
        boxes.push(b.start + cut..b.end);
    }
    let mut palette = Vec::new();
    let mut of = HashMap::new();
    for b in boxes {
        let s = &colours[b];
        let total: u64 = s.iter().map(|(_, n)| *n as u64).sum::<u64>().max(1);
        let mean = |ch: usize| ((s.iter().map(|(c, n)| c[ch] as u64 * *n as u64).sum::<u64>() + total / 2) / total) as u8;
        let i = palette.len() as u16;
        palette.push([mean(0), mean(1), mean(2)]);
        for (c, _) in s {
            of.insert(*c, i);
        }
    }
    (palette, of)
}

/// The Sixel sequence of `pic` with at most `registers` colours.
pub fn encode(pic: &Picture, registers: usize) -> Vec<u8> {
    let (palette, index) = quantize(pic, registers);
    let (w, h) = (pic.w, pic.h);
    let mut out = Vec::with_capacity(w * h / 2);
    let _ = write!(out, "\x1bP0;1;0q\"1;1;{w};{h}");
    let percent = |v: u8| (v as u32 * 100 + 127) / 255;
    for (i, c) in palette.iter().enumerate() {
        let _ = write!(out, "#{i};2;{};{};{}", percent(c[0]), percent(c[1]), percent(c[2]));
    }
    let mut rows: Vec<Vec<u8>> = vec![Vec::new(); palette.len()];
    let mut used: Vec<usize> = Vec::new();
    for y0 in (0..h).step_by(6) {
        used.clear();
        for y in y0..(y0 + 6).min(h) {
            let bit = 1u8 << (y - y0);
            for x in 0..w {
                let c = index[y * w + x] as usize;
                if rows[c].is_empty() {
                    rows[c].resize(w, 0);
                    used.push(c);
                }
                rows[c][x] |= bit;
            }
        }
        used.sort_unstable();
        for (k, &c) in used.iter().enumerate() {
            let _ = write!(out, "#{c}");
            let row = &rows[c];
            // Trailing empty sixels need not be written.
            let end = row.iter().rposition(|&b| b != 0).map_or(0, |e| e + 1);
            let mut x = 0;
            while x < end {
                let b = row[x];
                let mut n = 1;
                while x + n < end && row[x + n] == b {
                    n += 1;
                }
                let ch = 63 + b;
                if n > 3 {
                    let _ = write!(out, "!{n}{}", ch as char);
                } else {
                    out.extend(std::iter::repeat_n(ch, n));
                }
                x += n;
            }
            out.push(if k + 1 < used.len() { b'$' } else { b'-' });
        }
        for &c in &used {
            rows[c].clear();
        }
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A decoder of what `encode` writes: (width, height, RGB in 0–100 %).
    pub fn decode(s: &[u8]) -> (usize, usize, Vec<[u32; 3]>) {
        let s = std::str::from_utf8(s).unwrap();
        let body = s.strip_prefix("\x1bP0;1;0q\"1;1;").unwrap().strip_suffix("\x1b\\").unwrap();
        let mut it = body.splitn(3, [';', '#']);
        let w: usize = it.next().unwrap().parse().unwrap();
        let rest = it.next().unwrap();
        let h: usize = rest.parse().unwrap();
        let body = &body[body.find('#').unwrap()..];
        let mut palette: HashMap<usize, [u32; 3]> = HashMap::new();
        let mut img = vec![[999u32; 3]; w * h];
        let (mut x, mut y0, mut colour) = (0usize, 0usize, 0usize);
        let b = body.as_bytes();
        let mut i = 0;
        let num = |i: &mut usize| {
            let start = *i;
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
            body[start..*i].parse::<usize>().unwrap()
        };
        while i < b.len() {
            match b[i] {
                b'#' => {
                    i += 1;
                    let n = num(&mut i);
                    if i < b.len() && b[i] == b';' {
                        i += 3; // ";2;"
                        let r = num(&mut i) as u32;
                        i += 1;
                        let g = num(&mut i) as u32;
                        i += 1;
                        let bl = num(&mut i) as u32;
                        palette.insert(n, [r, g, bl]);
                    } else {
                        colour = n;
                    }
                }
                b'$' => {
                    x = 0;
                    i += 1;
                }
                b'-' => {
                    x = 0;
                    y0 += 6;
                    i += 1;
                }
                b'!' => {
                    i += 1;
                    let n = num(&mut i);
                    for _ in 0..n {
                        paint(&mut img, w, h, x, y0, b[i] - 63, palette[&colour]);
                        x += 1;
                    }
                    i += 1;
                }
                c => {
                    paint(&mut img, w, h, x, y0, c - 63, palette[&colour]);
                    x += 1;
                    i += 1;
                }
            }
        }
        (w, h, img)
    }

    fn paint(img: &mut [[u32; 3]], w: usize, h: usize, x: usize, y0: usize, bits: u8, c: [u32; 3]) {
        for k in 0..6 {
            if bits & (1 << k) != 0 && y0 + k < h {
                assert!(x < w, "sixel past the right edge");
                img[(y0 + k) * w + x] = c;
            }
        }
    }

    fn ramp(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Picture {
        let mut rgb = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&f(x, y));
            }
        }
        Picture { w, h, rgb }
    }

    #[test]
    fn grey_levels_round_trip() {
        let pic = ramp(37, 13, |x, y| {
            let v = ((x * 7 + y * 3) % 256) as u8;
            [v, v, v]
        });
        let (w, h, img) = decode(&encode(&pic, 256));
        assert_eq!((w, h), (37, 13));
        for (k, px) in pic.rgb.chunks(3).enumerate() {
            let expect = (px[0] as u32 * 100 + 127) / 255;
            assert_eq!(img[k], [expect; 3], "pixel {k}");
        }
    }

    #[test]
    fn many_colours_fit_the_registers() {
        let pic = ramp(64, 20, |x, y| [(x * 4) as u8, (y * 12) as u8, ((x + y) * 3) as u8]);
        for registers in [16, 256] {
            let (palette, index) = quantize(&pic, registers);
            assert!(palette.len() <= registers && palette.len() > registers / 2, "{}", palette.len());
            // Every pixel is near its palette colour.
            let worst = pic.rgb.chunks(3).zip(&index).map(|(px, &i)| {
                let c = palette[i as usize];
                (0..3).map(|ch| (px[ch] as i32 - c[ch] as i32).abs()).max().unwrap()
            }).max().unwrap();
            assert!(worst < if registers == 16 { 90 } else { 40 }, "{registers}: {worst}");
            let (_, _, img) = decode(&encode(&pic, registers));
            assert!(img.iter().all(|p| p[0] <= 100), "every pixel painted");
        }
        // Long runs are compressed.
        let flat = ramp(500, 6, |_, _| [10, 20, 30]);
        let s = encode(&flat, 256);
        assert!(s.len() < 60, "{}", String::from_utf8_lossy(&s));
        assert!(String::from_utf8_lossy(&s).contains("!500~"));
    }
}
