//! Pictures drawn with text cells, which every terminal shows: half blocks
//! (two pixels per cell, the upper one as the background colour and the
//! lower one as the foreground colour of `▄`), or characters of increasing
//! ink without colour for terminals that have none.

use crate::color::{Color, Depth};
use crate::picture::Picture;
use crate::screen::{Cell, Glyph, Rect, Screen, Style};

/// Pixels a picture needs to fill `cells` with half blocks.
pub fn half_block_pixels(cells: Rect) -> (usize, usize) {
    (cells.w, cells.h * 2)
}

fn pixel(pic: &Picture, x: usize, y: usize) -> [u8; 3] {
    let o = (y * pic.w + x) * 3;
    [pic.rgb[o], pic.rgb[o + 1], pic.rgb[o + 2]]
}

/// Draw `pic` (`half_block_pixels(at)` in size) into the cells `at`.
pub fn half_blocks(pic: &Picture, depth: Depth, screen: &mut Screen, at: Rect) {
    for cy in 0..at.h.min(pic.h.div_ceil(2)) {
        for cx in 0..at.w.min(pic.w) {
            let top = depth.reduce(pixel(pic, cx, 2 * cy));
            let bottom = if 2 * cy + 1 < pic.h { depth.reduce(pixel(pic, cx, 2 * cy + 1)) } else { Color::Default };
            // One colour: a space on that background is shorter to write.
            let cell = if top == bottom {
                Cell { glyph: Glyph::Char(' '), style: Style { bg: top, ..Style::default() } }
            } else {
                Cell { glyph: Glyph::Char('▄'), style: Style { fg: bottom, bg: top, ..Style::default() } }
            };
            screen.set(at.x + cx, at.y + cy, cell);
        }
    }
}

/// Characters from no ink to full ink.
const RAMP: &[u8] = b" .:-=+*#%@";

/// Draw `pic` (one pixel per cell) as characters by brightness.
pub fn ascii(pic: &Picture, screen: &mut Screen, at: Rect) {
    for cy in 0..at.h.min(pic.h) {
        for cx in 0..at.w.min(pic.w) {
            let [r, g, b] = pixel(pic, cx, cy);
            let luma = (299 * r as u32 + 587 * g as u32 + 114 * b as u32) / 1000;
            let c = RAMP[(luma as usize * RAMP.len() / 256).min(RAMP.len() - 1)] as char;
            screen.set(at.x + cx, at.y + cy, Cell { glyph: Glyph::Char(c), style: Style::default() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pic(w: usize, h: usize, px: &[[u8; 3]]) -> Picture {
        Picture { w, h, rgb: px.iter().flatten().copied().collect() }
    }

    #[test]
    fn two_pixels_per_cell() {
        // 2 × 4 pixels: column 0 black over white, column 1 one colour.
        let p = pic(2, 4, &[[0, 0, 0], [9, 9, 9], [255, 255, 255], [9, 9, 9], [1, 2, 3], [7, 7, 7], [1, 2, 3], [7, 7, 7]]);
        let mut s = Screen::new(2, 2);
        half_blocks(&p, Depth::TrueColor, &mut s, Rect { x: 0, y: 0, w: 2, h: 2 });
        let mut out = Vec::new();
        s.lines(Depth::TrueColor, &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b[38;2;255;255;255;48;2;0;0;0m▄\x1b[39;48;2;9;9;9m \x1b[0m\n\x1b[48;2;1;2;3m \x1b[48;2;7;7;7m \x1b[0m\n"
        );
    }

    #[test]
    fn odd_heights_and_reduced_colours() {
        let p = pic(1, 3, &[[200, 0, 0], [0, 0, 200], [0, 200, 0]]);
        let mut s = Screen::new(1, 2);
        half_blocks(&p, Depth::Ansi256, &mut s, Rect { x: 0, y: 0, w: 1, h: 2 });
        assert_eq!(s.get(0, 0).style, Style { fg: Color::Index(20), bg: Color::Index(160), ..Style::default() });
        // The last pixel row alone: the lower half shows the terminal background.
        assert_eq!(s.get(0, 1).style, Style { fg: Color::Default, bg: Color::Index(40), ..Style::default() });
        let mut a = Screen::new(3, 1);
        ascii(&pic(3, 1, &[[0, 0, 0], [128, 128, 128], [255, 255, 255]]), &mut a, Rect { x: 0, y: 0, w: 3, h: 1 });
        let mut out = Vec::new();
        a.lines(Depth::TrueColor, &mut out);
        assert_eq!(out, b" +@\n");
    }
}
