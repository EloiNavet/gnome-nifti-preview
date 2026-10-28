//! A small, strict terminal for the tests: it plays what nitty writes onto
//! a grid of cells and panics on any sequence it does not know, so that a
//! stray or malformed escape sequence fails a test. Image protocols are
//! recorded (where they were drawn, with what) rather than decoded.

#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Default,
    Rgb([u8; 3]),
    Index(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Colour,
    pub bg: Colour,
    pub bold: bool,
    pub dim: bool,
    pub reverse: bool,
}

const BLANK: Cell = Cell { ch: ' ', fg: Colour::Default, bg: Colour::Default, bold: false, dim: false, reverse: false };

/// An image drawn with a protocol: at which cell, and the sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawn {
    pub x: usize,
    pub y: usize,
    pub kind: &'static str,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Term {
    pub w: usize,
    pub h: usize,
    /// Rows grow when the cursor goes past the last one and `scroll` is off
    /// (a quick look printed to a file has no height).
    pub grow: bool,
    pub cells: Vec<Vec<Cell>>,
    pub x: usize,
    pub y: usize,
    pen: Cell,
    saved: (usize, usize),
    pub images: Vec<Drawn>,
    pub modes: std::collections::BTreeMap<String, bool>,
    /// Every mode set or reset, in order.
    pub mode_changes: Vec<(String, bool)>,
    /// A line feed also returns the cursor to the first column, as a
    /// terminal in cooked mode (ONLCR) does; not in raw mode.
    pub onlcr: bool,
    /// Lines scrolled off the top (fixed-height terminals).
    pub scrolled: usize,
    /// Queries the program sent, for the test to answer: "da1", "version",
    /// "cell", "registers", "kitty".
    pub queries: Vec<&'static str>,
}

impl Term {
    pub fn new(w: usize, h: usize, grow: bool) -> Term {
        Term {
            w,
            h,
            grow,
            cells: vec![vec![BLANK; w]; h],
            x: 0,
            y: 0,
            pen: BLANK,
            saved: (0, 0),
            images: Vec::new(),
            modes: Default::default(),
            mode_changes: Vec::new(),
            onlcr: true,
            scrolled: 0,
            queries: Vec::new(),
        }
    }

    /// A terminal in raw mode, as nitty's interactive viewer sets it.
    pub fn raw(mut self) -> Term {
        self.onlcr = false;
        self
    }

    /// Whether mode `m` was reset, then set again, among the changes from
    /// index `since`.
    pub fn left_and_came_back(&self, m: &str, since: usize) -> bool {
        let changes: Vec<bool> = self.mode_changes[since.min(self.mode_changes.len())..].iter().filter(|(k, _)| k == m).map(|&(_, on)| on).collect();
        changes.windows(2).any(|w| w == [false, true])
    }

    pub fn cell(&self, x: usize, y: usize) -> Cell {
        self.cells[y][x]
    }

    /// The text of row `y` (trailing spaces removed).
    pub fn row(&self, y: usize) -> String {
        self.cells[y].iter().filter(|c| c.ch != '\0').map(|c| c.ch).collect::<String>().trim_end().to_string()
    }

    pub fn text(&self) -> String {
        (0..self.cells.len()).map(|y| self.row(y)).collect::<Vec<_>>().join("\n")
    }

    /// (x, y) of the first occurrence of `s`.
    pub fn find(&self, s: &str) -> Option<(usize, usize)> {
        for y in 0..self.cells.len() {
            let row: Vec<char> = self.cells[y].iter().map(|c| c.ch).collect();
            let pat: Vec<char> = s.chars().collect();
            if let Some(x) = row.windows(pat.len()).position(|w| w == &pat[..]) {
                return Some((x, y));
            }
        }
        None
    }

    /// The colour of half-block pixel (x, py): row py / 2 of cells, upper
    /// half the background, lower half the foreground of `▄`.
    pub fn pixel(&self, x: usize, py: usize) -> Colour {
        let c = self.cells[py / 2][x];
        match (c.ch, py % 2) {
            ('▄', 1) => c.fg,
            _ => c.bg,
        }
    }

    fn newline(&mut self) {
        self.y += 1;
        if self.y >= self.cells.len() {
            if self.grow {
                self.cells.push(vec![BLANK; self.w]);
                self.h = self.cells.len();
            } else {
                self.cells.remove(0);
                self.cells.push(vec![BLANK; self.w]);
                self.scrolled += 1;
                self.y = self.cells.len() - 1;
            }
        }
    }

    fn put(&mut self, c: char) {
        let cw = nitty::text::width(c);
        if cw == 0 {
            return;
        }
        assert!(self.x + cw <= self.w, "text past the right edge at ({}, {}): {c:?}", self.x, self.y);
        self.cells[self.y][self.x] = Cell { ch: c, ..self.pen };
        if cw == 2 {
            self.cells[self.y][self.x + 1] = Cell { ch: '\0', ..self.pen };
        }
        self.x += cw;
    }

    fn sgr(&mut self, params: &[u32]) {
        let mut i = 0;
        let p = |i: usize| *params.get(i).unwrap_or_else(|| panic!("SGR cut short: {params:?}"));
        if params.is_empty() {
            self.pen = BLANK;
        }
        while i < params.len() {
            match p(i) {
                0 => self.pen = BLANK,
                1 => self.pen.bold = true,
                2 => self.pen.dim = true,
                7 => self.pen.reverse = true,
                n @ 30..=37 => self.pen.fg = Colour::Index((n - 30) as u8),
                n @ 90..=97 => self.pen.fg = Colour::Index((n - 90 + 8) as u8),
                n @ 40..=47 => self.pen.bg = Colour::Index((n - 40) as u8),
                n @ 100..=107 => self.pen.bg = Colour::Index((n - 100 + 8) as u8),
                39 => self.pen.fg = Colour::Default,
                49 => self.pen.bg = Colour::Default,
                n @ (38 | 48) => {
                    let c = match p(i + 1) {
                        2 => {
                            let c = Colour::Rgb([p(i + 2) as u8, p(i + 3) as u8, p(i + 4) as u8]);
                            assert!((2..5).all(|k| p(i + k) <= 255), "bad colour {params:?}");
                            i += 4;
                            c
                        }
                        5 => {
                            assert!(p(i + 2) <= 255, "bad colour {params:?}");
                            i += 2;
                            Colour::Index(p(i) as u8)
                        }
                        other => panic!("SGR {n};{other}"),
                    };
                    if n == 38 {
                        self.pen.fg = c;
                    } else {
                        self.pen.bg = c;
                    }
                }
                other => panic!("unknown SGR {other} in {params:?}"),
            }
            i += 1;
        }
    }

    /// Play `bytes`.
    pub fn feed(&mut self, bytes: &[u8]) {
        let s = std::str::from_utf8(bytes).expect("output is UTF-8");
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\n' => {
                    self.newline();
                    if self.onlcr {
                        self.x = 0;
                    }
                    i += 1;
                }
                b'\r' => {
                    self.x = 0;
                    i += 1;
                }
                0x1b => i = self.escape(b, i),
                c if c < 0x20 || c == 0x7f => panic!("control byte {c:#04x} written at {i}"),
                _ => {
                    let c = s[i..].chars().next().unwrap();
                    assert!(nitty::text::printable(c), "unprintable {c:?}");
                    self.put(c);
                    i += c.len_utf8();
                }
            }
        }
    }

    /// One escape sequence at `b[i]`; returns the index after it.
    fn escape(&mut self, b: &[u8], i: usize) -> usize {
        let next = *b.get(i + 1).expect("ESC at the end");
        match next {
            b'[' => {
                let mut j = i + 2;
                while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
                    j += 1;
                }
                let params = std::str::from_utf8(&b[i + 2..j]).unwrap();
                let fin = b[j] as char;
                self.csi(params, fin);
                j + 1
            }
            b'7' => {
                self.saved = (self.x, self.y);
                i + 2
            }
            b'8' => {
                (self.x, self.y) = self.saved;
                i + 2
            }
            b'_' | b'P' | b']' => {
                // APC, DCS, OSC: up to ST (ESC \) or, for OSC, BEL.
                let mut j = i + 2;
                let tmux = next == b'P' && b[j..].starts_with(b"tmux;");
                loop {
                    assert!(j < b.len(), "unterminated string sequence");
                    // Inside tmux's passthrough, ESC ESC is one escaped ESC.
                    if tmux && b[j] == 0x1b && b.get(j + 1) == Some(&0x1b) {
                        j += 2;
                        continue;
                    }
                    if b[j] == 0x1b && b.get(j + 1) == Some(&b'\\') {
                        break;
                    }
                    if next == b']' && b[j] == 0x07 {
                        break;
                    }
                    j += 1;
                }
                let body = b[i + 2..j].to_vec();
                let end = if b[j] == 0x07 { j + 1 } else { j + 2 };
                self.string(next, body);
                end
            }
            other => panic!("unknown escape ESC {:?}", other as char),
        }
    }

    fn string(&mut self, kind: u8, body: Vec<u8>) {
        let (x, y) = (self.x, self.y);
        match kind {
            b'_' => {
                assert!(body.starts_with(b"G"), "unknown APC");
                if body.windows(4).any(|w| w == b"a=q,") {
                    self.queries.push("kitty");
                } else {
                    self.images.push(Drawn { x, y, kind: "kitty", body });
                }
            }
            b'P' if body.starts_with(b"tmux;") => {
                // tmux passthrough: ESC doubled inside.
                let inner: Vec<u8> = {
                    let mut v = Vec::new();
                    let mut k = 5;
                    while k < body.len() {
                        if body[k] == 0x1b {
                            assert_eq!(body.get(k + 1), Some(&0x1b), "single ESC inside tmux passthrough");
                            k += 1;
                        }
                        v.push(body[k]);
                        k += 1;
                    }
                    v
                };
                self.images.push(Drawn { x, y, kind: "tmux", body: inner });
            }
            b'P' => {
                let q = body.iter().position(|&c| c == b'q').expect("DCS q (sixel)");
                assert!(body[..q].iter().all(|c| c.is_ascii_digit() || *c == b';'), "unknown DCS");
                self.images.push(Drawn { x, y, kind: "sixel", body });
            }
            b']' => {
                assert!(body.starts_with(b"1337;File="), "unknown OSC {}", String::from_utf8_lossy(&body[..body.len().min(20)]));
                self.images.push(Drawn { x, y, kind: "iterm2", body });
            }
            _ => unreachable!(),
        }
    }

    fn csi(&mut self, params: &str, fin: char) {
        match (params, fin) {
            ("", 'c') => return self.queries.push("da1"),
            (">0", 'q') => return self.queries.push("version"),
            ("16", 't') => return self.queries.push("cell"),
            ("14", 't') => return self.queries.push("window"),
            ("?1;1;0", 'S') => return self.queries.push("registers"),
            _ => {}
        }
        if let Some(p) = params.strip_prefix('?') {
            let on = match fin {
                'h' => true,
                'l' => false,
                _ => panic!("unknown private CSI ?{p}{fin}"),
            };
            for m in p.split(';') {
                assert!(["25", "1049", "2026", "7", "1000", "1002", "1006", "2004"].contains(&m), "unknown mode ?{m}");
                self.modes.insert(m.to_string(), on);
                self.mode_changes.push((m.to_string(), on));
            }
            return;
        }
        let n: Vec<u32> = if params.is_empty() { Vec::new() } else { params.split(';').map(|x| x.parse().unwrap_or_else(|_| panic!("CSI {params}{fin}"))).collect() };
        let a = |k: usize, d: u32| n.get(k).copied().filter(|&v| v != 0).unwrap_or(d) as usize;
        match fin {
            'm' => self.sgr(&n),
            'H' => {
                self.y = a(0, 1) - 1;
                self.x = a(1, 1) - 1;
                assert!(self.y < self.cells.len() && self.x < self.w, "cursor off screen: CSI {params}H");
            }
            'A' => self.y = self.y.checked_sub(a(0, 1)).expect("cursor above the top"),
            'B' => self.y = (self.y + a(0, 1)).min(self.cells.len() - 1),
            'C' => self.x = (self.x + a(0, 1)).min(self.w - 1),
            'J' => {
                assert_eq!(n, [2], "only CSI 2J");
                self.cells = vec![vec![BLANK; self.w]; self.cells.len()];
            }
            'K' => {
                assert!(n.is_empty() || n == [0] || n == [2], "CSI {params}K");
                for x in 0..self.w {
                    if n == [2] || x >= self.x {
                        self.cells[self.y][x] = Cell { ch: ' ', ..self.pen };
                    }
                }
            }
            other => panic!("unknown CSI {params}{other}"),
        }
    }
}
