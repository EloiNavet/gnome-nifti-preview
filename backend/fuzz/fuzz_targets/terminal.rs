//! The terminal viewer on any bytes: what the terminal sends (keys, mouse
//! reports, answers to queries) gives the same events however it is split
//! between reads; the Sixel and half-block encoders, at every colour depth,
//! write well-formed output for any picture. Must never crash, hang or keep
//! growing.
//!
//!   cargo +nightly fuzz run terminal CORPUS -- -max_total_time=600

#![no_main]

use libfuzzer_sys::fuzz_target;
use nitty::color::Depth;
use nitty::input::{Event, Parser};
use nitty::picture::Picture;
use nitty::screen::{Rect, Screen};

/// Every event of `chunks`, read one after the other, then the end of input.
fn events(chunks: &[&[u8]]) -> Vec<Event> {
    let mut p = Parser::new();
    let mut all = Vec::new();
    for c in chunks {
        all.extend(p.feed(c));
    }
    all.extend(p.flush());
    assert!(!p.pending());
    all
}

/// Output for the terminal: UTF-8, no control byte but ESC.
fn well_formed(out: &[u8]) {
    assert!(std::str::from_utf8(out).is_ok(), "not UTF-8");
    assert!(out.iter().all(|&b| b >= 0x20 || b == 0x1b), "a control byte");
}

fuzz_target!(|data: &[u8]| {
    let Some((&split, rest)) = data.split_first() else { return };
    // Input: the same events whole, in two reads cut anywhere, and byte by
    // byte (a sequence longer than the parser keeps may be dropped in parts,
    // so only inputs below that size are compared).
    let whole = events(&[rest]);
    if rest.len() <= 4096 {
        let cut = (split as usize).min(rest.len());
        assert_eq!(events(&[&rest[..cut], &rest[cut..]]), whole, "cut at {cut}");
        let bytes: Vec<&[u8]> = rest.chunks(1).collect();
        assert_eq!(events(&bytes), whole, "byte by byte");
    }

    // A picture of up to 40 × 40 pixels from the bytes.
    if rest.len() < 2 {
        return;
    }
    let (w, h) = ((rest[0] % 40) as usize + 1, (rest[1] % 40) as usize + 1);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| rest.get(2 + i % (rest.len() - 2).max(1)).copied().unwrap_or(0)).collect();
    let pic = Picture { w, h, rgb };
    let registers = 2 + (split as usize * 7 + rest.len()) % 1023;
    let s = nitty::graphics::sixel::encode(&pic, registers);
    assert!(s.starts_with(b"\x1bP") && s.ends_with(b"\x1b\\"));
    assert!(s[2..s.len() - 2].iter().all(|&b| (0x20..0x7f).contains(&b) || b == b'\n'), "sixel body");

    // Half blocks at every depth: all at once, then only what changed when
    // the picture is drawn again shifted by one pixel.
    let depth = [Depth::TrueColor, Depth::Ansi256, Depth::Ansi16, Depth::Mono][split as usize % 4];
    let area = Rect { x: 0, y: 0, w, h: h.div_ceil(2) };
    let mut first = Screen::new(w, h.div_ceil(2));
    nitty::blocks::half_blocks(&pic, depth, &mut first, area);
    let mut out = Vec::new();
    first.diff(None, depth, &mut out);
    well_formed(&out);
    let mut shifted = pic.rgb.clone();
    shifted.rotate_left(3);
    let mut second = Screen::new(w, h.div_ceil(2));
    nitty::blocks::half_blocks(&Picture { w, h, rgb: shifted }, depth, &mut second, area);
    let mut out = Vec::new();
    second.diff(Some(&first), depth, &mut out);
    well_formed(&out);
});
