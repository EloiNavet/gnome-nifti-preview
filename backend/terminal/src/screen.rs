//! A grid of cells and how it reaches the terminal: as plain lines (the
//! quick look, which scrolls with the shell), or as the difference from the
//! previous frame (the interactive viewer: only the cells that changed are
//! written, with as few SGR parameters as possible).

use std::io::Write;

use crate::color::{self, Color, Depth};
use crate::text;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub reverse: bool,
}

impl Style {
    pub fn fg(c: Color) -> Style {
        Style { fg: c, ..Style::default() }
    }

    pub fn bold(self) -> Style {
        Style { bold: true, ..self }
    }

    pub fn dim(self) -> Style {
        Style { dim: true, ..self }
    }

    pub fn reverse(self) -> Style {
        Style { reverse: true, ..self }
    }
}

/// What a cell holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Char(char),
    /// The second cell of a wide character.
    Continuation,
    /// Under an image drawn with a graphics protocol: never written.
    Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub glyph: Glyph,
    pub style: Style,
}

pub const BLANK: Cell = Cell { glyph: Glyph::Char(' '), style: Style { fg: Color::Default, bg: Color::Default, bold: false, dim: false, reverse: false } };

/// A rectangle of cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    pub w: usize,
    pub h: usize,
    cells: Vec<Cell>,
}

impl Screen {
    pub fn new(w: usize, h: usize) -> Screen {
        Screen { w, h, cells: vec![BLANK; w * h] }
    }

    pub fn get(&self, x: usize, y: usize) -> Cell {
        self.cells[y * self.w + x]
    }

    pub fn set(&mut self, x: usize, y: usize, cell: Cell) {
        if x < self.w && y < self.h {
            self.cells[y * self.w + x] = cell;
        }
    }

    pub fn fill(&mut self, r: Rect, cell: Cell) {
        for y in r.y..(r.y + r.h).min(self.h) {
            for x in r.x..(r.x + r.w).min(self.w) {
                self.cells[y * self.w + x] = cell;
            }
        }
    }

    /// Write `s` from (x, y), at most `max` cells and not past the right
    /// edge; characters that may not be written are dropped. Returns the
    /// cells used.
    pub fn put(&mut self, x: usize, y: usize, s: &str, style: Style, max: usize) -> usize {
        if y >= self.h {
            return 0;
        }
        let limit = max.min(self.w.saturating_sub(x));
        let mut used = 0;
        for c in s.chars().filter(|&c| text::printable(c)) {
            let cw = text::width(c);
            if cw == 0 {
                continue;
            }
            if used + cw > limit {
                break;
            }
            self.cells[y * self.w + x + used] = Cell { glyph: Glyph::Char(c), style };
            if cw == 2 {
                self.cells[y * self.w + x + used + 1] = Cell { glyph: Glyph::Continuation, style };
            }
            used += cw;
        }
        used
    }

    /// The cells as lines of text, each ending with a reset and a line
    /// feed; cells under images are written as spaces, trailing blank cells
    /// are left out.
    pub fn lines(&self, depth: Depth, out: &mut Vec<u8>) {
        for y in 0..self.h {
            let row = &self.cells[y * self.w..(y + 1) * self.w];
            let end = row.iter().rposition(|c| *c != BLANK && c.glyph != Glyph::Image).map_or(0, |i| i + 1);
            let mut pen = Style::default();
            for cell in row[..end].iter().filter(|c| c.glyph != Glyph::Continuation) {
                write_cell(cell, &mut pen, depth, out);
            }
            if pen != Style::default() {
                out.extend_from_slice(b"\x1b[0m");
            }
            out.push(b'\n');
        }
    }

    /// The sequences that turn `prev` (what the terminal shows; None: unknown,
    /// so everything is written) into this screen. The cursor is left where
    /// the last cell was written, the pen reset.
    pub fn diff(&self, prev: Option<&Screen>, depth: Depth, out: &mut Vec<u8>) {
        let prev = prev.filter(|p| p.w == self.w && p.h == self.h);
        if prev.is_none() {
            out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
        }
        let mut pen = Style::default();
        let mut at: Option<(usize, usize)> = None;
        for y in 0..self.h {
            for x in 0..self.w {
                let cell = self.cells[y * self.w + x];
                let changed = match prev {
                    Some(p) => p.cells[y * self.w + x] != cell,
                    None => cell != BLANK,
                };
                if !changed || matches!(cell.glyph, Glyph::Image | Glyph::Continuation) {
                    continue;
                }
                if at != Some((x, y)) {
                    let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
                }
                write_cell(&cell, &mut pen, depth, out);
                let cw = match cell.glyph {
                    Glyph::Char(c) => text::width(c).max(1),
                    _ => 1,
                };
                at = Some((x + cw, y));
            }
        }
        if pen != Style::default() {
            out.extend_from_slice(b"\x1b[0m");
        }
    }
}

/// Write one cell, changing the pen first if needed.
fn write_cell(cell: &Cell, pen: &mut Style, depth: Depth, out: &mut Vec<u8>) {
    let s = cell.style;
    if s != *pen && depth != Depth::Mono {
        let mut p = Vec::new();
        let reset = (pen.bold && !s.bold) || (pen.dim && !s.dim) || (pen.reverse && !s.reverse);
        let base = if reset { Style::default() } else { *pen };
        if reset {
            p.push(b'0');
        }
        let add = |p: &mut Vec<u8>, f: &dyn Fn(&mut Vec<u8>)| {
            if !p.is_empty() {
                p.push(b';');
            }
            f(p);
        };
        if s.bold && !base.bold {
            add(&mut p, &|p| p.push(b'1'));
        }
        if s.dim && !base.dim {
            add(&mut p, &|p| p.push(b'2'));
        }
        if s.reverse && !base.reverse {
            add(&mut p, &|p| p.push(b'7'));
        }
        if s.fg != base.fg {
            add(&mut p, &|p| color::sgr(s.fg, true, depth, p));
        }
        if s.bg != base.bg {
            add(&mut p, &|p| color::sgr(s.bg, false, depth, p));
        }
        out.extend_from_slice(b"\x1b[");
        out.extend_from_slice(&p);
        out.push(b'm');
        *pen = s;
    }
    match cell.glyph {
        Glyph::Char(c) => {
            let mut b = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
        }
        Glyph::Image | Glyph::Continuation => out.push(b' '),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[u8]) -> String {
        String::from_utf8(v.to_vec()).unwrap()
    }

    #[test]
    fn diff_writes_only_what_changed() {
        let mut a = Screen::new(10, 3);
        a.put(0, 0, "abc", Style::default(), 10);
        let mut out = Vec::new();
        a.diff(None, Depth::TrueColor, &mut out);
        assert_eq!(s(&out), "\x1b[0m\x1b[H\x1b[2J\x1b[1;1Habc");
        let mut b = a.clone();
        b.put(1, 0, "X", Style::fg(Color::Rgb(255, 0, 0)), 10);
        b.put(5, 2, "yz", Style::default().bold(), 10);
        out.clear();
        b.diff(Some(&a), Depth::TrueColor, &mut out);
        assert_eq!(s(&out), "\x1b[1;2H\x1b[38;2;255;0;0mX\x1b[3;6H\x1b[1;39myz\x1b[0m");
        out.clear();
        b.diff(Some(&b), Depth::TrueColor, &mut out);
        assert!(out.is_empty(), "nothing changed, nothing written");
    }

    #[test]
    fn hostile_text_and_wide_characters() {
        let mut a = Screen::new(6, 1);
        assert_eq!(a.put(0, 0, "\x1b]0;x\x07山a", Style::default(), 6), 6);
        let mut out = Vec::new();
        a.lines(Depth::TrueColor, &mut out);
        assert_eq!(s(&out), "]0;x山\n");
        // A wide character that does not fit is not written in half.
        let mut b = Screen::new(3, 1);
        assert_eq!(b.put(0, 0, "ab山", Style::default(), 3), 2);
    }

    #[test]
    fn lines_trim_and_reset() {
        let mut a = Screen::new(5, 2);
        a.set(1, 0, Cell { glyph: Glyph::Char('▄'), style: Style { fg: Color::Index(196), bg: Color::Index(16), ..Style::default() } });
        let mut out = Vec::new();
        a.lines(Depth::Ansi256, &mut out);
        assert_eq!(s(&out), " \x1b[38;5;196;48;5;16m▄\x1b[0m\n\n");
    }
}
