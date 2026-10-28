//! Bytes read from the terminal, as events: keys (with Shift, Alt and
//! Ctrl), mouse buttons and wheel (SGR reports), and the terminal's answers
//! to queries (DA1, XTVERSION, Kitty graphics, cell size, XTSMGRAPHICS).
//! A sequence cut between two reads is kept until its end arrives; an
//! escape alone is a key only once no byte follows it (`flush`).

/// Longest sequence kept (a longer one is dropped).
const MAX_SEQUENCE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Insert,
    Delete,
    Tab,
    BackTab,
    Enter,
    Backspace,
    Esc,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Press(u8),
    Release,
    Drag(u8),
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Primary device attributes.
    Attributes(Vec<u32>),
    /// XTVERSION: the terminal's name and version.
    Version(String),
    /// Kitty graphics: the answer to image `id`, OK or not.
    Kitty { id: u32, ok: bool },
    /// Cell size in pixels (width, height), answer to `CSI 16 t`.
    CellSize(u32, u32),
    /// Text area size in pixels (width, height), answer to `CSI 14 t`.
    WindowPx(u32, u32),
    /// XTSMGRAPHICS: item, status, value.
    Graphics(u32, u32, u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(Key, Mods),
    /// Cell (column, row) from 0.
    Mouse { kind: MouseKind, x: usize, y: usize, mods: Mods },
    Reply(Reply),
}

#[derive(Debug, Default)]
pub struct Parser {
    buf: Vec<u8>,
}

fn mods_of(param: u32) -> Mods {
    // xterm: 1 + (shift 1, alt 2, ctrl 4).
    let m = param.saturating_sub(1);
    Mods { shift: m & 1 != 0, alt: m & 2 != 0, ctrl: m & 4 != 0 }
}

fn numbers(s: &[u8]) -> Vec<u32> {
    s.split(|&b| b == b';').map(|p| std::str::from_utf8(p).ok().and_then(|p| p.parse().ok()).unwrap_or(0)).collect()
}

/// Outcome of parsing at the start of a buffer.
enum Step {
    /// An event (or nothing worth reporting) and the bytes consumed.
    Done(Option<Event>, usize),
    /// The sequence is not complete yet.
    More,
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    /// Bytes are waiting for the rest of a sequence (an escape alone, so far).
    pub fn pending(&self) -> bool {
        !self.buf.is_empty()
    }

    /// Parse `bytes`, appended to what was left.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        let mut i = 0;
        while i < self.buf.len() {
            match parse(&self.buf[i..]) {
                Step::Done(e, n) => {
                    events.extend(e);
                    i += n.max(1);
                }
                Step::More => {
                    if self.buf.len() - i > MAX_SEQUENCE {
                        // A sequence that never ends: drop it.
                        i = self.buf.len();
                    }
                    break;
                }
            }
        }
        self.buf.drain(..i);
        events
    }

    /// No byte came after what is waiting: an escape alone is the Esc key
    /// (Alt + key once the key follows), anything else is dropped.
    pub fn flush(&mut self) -> Vec<Event> {
        let events = if self.buf == [0x1b] { vec![Event::Key(Key::Esc, Mods::default())] } else { Vec::new() };
        self.buf.clear();
        events
    }
}

fn key(k: Key) -> Option<Event> {
    Some(Event::Key(k, Mods::default()))
}

fn parse(b: &[u8]) -> Step {
    match b[0] {
        0x1b => parse_escape(b),
        b'\r' | b'\n' => Step::Done(key(Key::Enter), 1),
        b'\t' => Step::Done(key(Key::Tab), 1),
        0x7f | 0x08 => Step::Done(key(Key::Backspace), 1),
        c @ 0x01..=0x1a => Step::Done(Some(Event::Key(Key::Char((b'a' + c - 1) as char), Mods { ctrl: true, ..Mods::default() })), 1),
        c if c < 0x20 => Step::Done(None, 1),
        c if c < 0x80 => Step::Done(key(Key::Char(c as char)), 1),
        c => {
            // UTF-8: the length from the first byte, then the whole character.
            let n = match c {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => return Step::Done(None, 1),
            };
            if b.len() < n {
                return Step::More;
            }
            match std::str::from_utf8(&b[..n]).ok().and_then(|s| s.chars().next()) {
                Some(ch) => Step::Done(key(Key::Char(ch)), n),
                None => Step::Done(None, 1),
            }
        }
    }
}

fn parse_escape(b: &[u8]) -> Step {
    let Some(&next) = b.get(1) else { return Step::More };
    match next {
        b'[' => parse_csi(b),
        b'O' => {
            let Some(&f) = b.get(2) else { return Step::More };
            let k = match f {
                b'A' => Key::Up,
                b'B' => Key::Down,
                b'C' => Key::Right,
                b'D' => Key::Left,
                b'H' => Key::Home,
                b'F' => Key::End,
                b'P'..=b'S' => Key::F(f - b'P' + 1),
                _ => return Step::Done(None, 3),
            };
            Step::Done(key(k), 3)
        }
        b'P' | b'_' | b']' => parse_string(b),
        0x1b => Step::Done(key(Key::Esc), 1),
        _ => {
            // Alt + key.
            match parse(&b[1..]) {
                Step::Done(Some(Event::Key(k, m)), n) => Step::Done(Some(Event::Key(k, Mods { alt: true, ..m })), n + 1),
                Step::Done(_, n) => Step::Done(None, n + 1),
                Step::More => Step::More,
            }
        }
    }
}

/// DCS, APC and OSC strings, up to ST (or BEL for OSC).
fn parse_string(b: &[u8]) -> Step {
    let mut j = 2;
    loop {
        if j >= b.len() {
            return Step::More;
        }
        if b[j] == 0x1b {
            if j + 1 >= b.len() {
                return Step::More;
            }
            if b[j + 1] == b'\\' {
                break;
            }
        }
        if b[1] == b']' && b[j] == 0x07 {
            return Step::Done(None, j + 1);
        }
        j += 1;
    }
    let body = &b[2..j];
    let event = match b[1] {
        // XTVERSION: DCS > | text ST
        b'P' => body.strip_prefix(b">|").map(|v| Event::Reply(Reply::Version(crate::text::clean(&String::from_utf8_lossy(v))))),
        // Kitty: APC G key=value,…;message ST
        b'_' => body.strip_prefix(b"G").and_then(|g| {
            let (keys, message) = match g.iter().position(|&c| c == b';') {
                Some(p) => (&g[..p], &g[p + 1..]),
                None => (g, &[][..]),
            };
            let id = keys.split(|&c| c == b',').find_map(|kv| kv.strip_prefix(b"i=")).and_then(|v| std::str::from_utf8(v).ok()?.parse().ok())?;
            Some(Event::Reply(Reply::Kitty { id, ok: message == b"OK" }))
        }),
        _ => None,
    };
    Step::Done(event, j + 2)
}

/// A mouse report: `code` as xterm encodes it (buttons in bits 0–1, Shift,
/// Alt and Ctrl in bits 2–4, motion in bit 5, the wheel in bit 6, the extra
/// buttons in bit 7), cell (x, y) from 1. Horizontal scrolling and the extra
/// buttons (back, forward) are left out.
fn mouse(code: u32, x: u32, y: u32, release: bool) -> Option<Event> {
    let mods = Mods { shift: code & 4 != 0, alt: code & 8 != 0, ctrl: code & 16 != 0 };
    let button = (code & 3) as u8;
    let kind = if code & 128 != 0 {
        return None;
    } else if code & 64 != 0 {
        match button {
            0 => MouseKind::WheelUp,
            1 => MouseKind::WheelDown,
            _ => return None,
        }
    } else if release || button == 3 {
        MouseKind::Release
    } else if code & 32 != 0 {
        MouseKind::Drag(button)
    } else {
        MouseKind::Press(button)
    };
    Some(Event::Mouse { kind, x: x.saturating_sub(1) as usize, y: y.saturating_sub(1) as usize, mods })
}

fn parse_csi(b: &[u8]) -> Step {
    // X10 mouse report, from terminals that ignore the SGR mode (1006):
    // CSI M and three bytes, each 32 plus a value.
    if b.get(2) == Some(&b'M') {
        if b.len() < 6 {
            return Step::More;
        }
        let v = |i: usize| (b[i] as u32).saturating_sub(32);
        return Step::Done(mouse(v(3), v(4), v(5), false), 6);
    }
    let mut j = 2;
    while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
        if j > 64 {
            return Step::Done(None, j);
        }
        j += 1;
    }
    if j >= b.len() {
        return Step::More;
    }
    let fin = b[j];
    let params = &b[2..j];
    let n = j + 1;
    let event = match (params.first(), fin) {
        // SGR mouse: CSI < b ; x ; y M (press, drag, wheel) or m (release)
        (Some(b'<'), b'M' | b'm') => {
            let v = numbers(&params[1..]);
            mouse(*v.first().unwrap_or(&0), *v.get(1).unwrap_or(&1), *v.get(2).unwrap_or(&1), fin == b'm')
        }
        (Some(b'?'), b'c') => Some(Event::Reply(Reply::Attributes(numbers(&params[1..])))),
        (Some(b'?'), b'S') => {
            let v = numbers(&params[1..]);
            Some(Event::Reply(Reply::Graphics(*v.first().unwrap_or(&0), *v.get(1).unwrap_or(&0), *v.get(2).unwrap_or(&0))))
        }
        (_, b't') => {
            let v = numbers(params);
            match (v.first(), v.len() >= 3) {
                (Some(6), true) => Some(Event::Reply(Reply::CellSize(v[2], v[1]))),
                (Some(4), true) => Some(Event::Reply(Reply::WindowPx(v[2], v[1]))),
                _ => None,
            }
        }
        (_, b'~') => {
            let v = numbers(params);
            let k = match v.first().copied().unwrap_or(0) {
                1 | 7 => Some(Key::Home),
                2 => Some(Key::Insert),
                3 => Some(Key::Delete),
                4 | 8 => Some(Key::End),
                5 => Some(Key::PageUp),
                6 => Some(Key::PageDown),
                n @ 11..=15 => Some(Key::F((n - 10) as u8)),
                n @ 17..=21 => Some(Key::F((n - 11) as u8)),
                n @ 23..=24 => Some(Key::F((n - 12) as u8)),
                _ => None,
            };
            k.map(|k| Event::Key(k, mods_of(*v.get(1).unwrap_or(&1))))
        }
        (_, f) => {
            let k = match f {
                b'A' => Some(Key::Up),
                b'B' => Some(Key::Down),
                b'C' => Some(Key::Right),
                b'D' => Some(Key::Left),
                b'H' => Some(Key::Home),
                b'F' => Some(Key::End),
                b'Z' => Some(Key::BackTab),
                b'P'..=b'S' => Some(Key::F(f - b'P' + 1)),
                _ => None,
            };
            let v = numbers(params);
            k.map(|k| Event::Key(k, mods_of(*v.get(1).unwrap_or(&1))))
        }
    };
    Step::Done(event, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(key: Key) -> Event {
        Event::Key(key, Mods::default())
    }

    fn feed(b: &[u8]) -> Vec<Event> {
        Parser::new().feed(b)
    }

    #[test]
    fn keys() {
        assert_eq!(feed(b"q\x1b[A\x1bOB\x1b[5~\x1b[6~\x1b[H\x1b[4~\t\r\x7f"), vec![
            k(Key::Char('q')), k(Key::Up), k(Key::Down), k(Key::PageUp), k(Key::PageDown), k(Key::Home), k(Key::End),
            k(Key::Tab), k(Key::Enter), k(Key::Backspace),
        ]);
        let shift = Mods { shift: true, ..Mods::default() };
        assert_eq!(feed(b"\x1b[1;2C\x1b[5;5~"), vec![Event::Key(Key::Right, shift), Event::Key(Key::PageUp, Mods { ctrl: true, ..Mods::default() })]);
        assert_eq!(feed(b"\x03\x1a"), vec![
            Event::Key(Key::Char('c'), Mods { ctrl: true, ..Mods::default() }),
            Event::Key(Key::Char('z'), Mods { ctrl: true, ..Mods::default() }),
        ]);
        assert_eq!(feed("é山".as_bytes()), vec![k(Key::Char('é')), k(Key::Char('山'))]);
        assert_eq!(feed(b"\x1bx"), vec![Event::Key(Key::Char('x'), Mods { alt: true, ..Mods::default() })]);
        assert_eq!(feed(b"\x1b[Z\x1b[15~\x1bOP"), vec![k(Key::BackTab), k(Key::F(5)), k(Key::F(1))]);
    }

    #[test]
    fn split_reads_and_lone_escape() {
        let mut p = Parser::new();
        assert!(p.feed(b"\x1b").is_empty());
        assert!(p.pending());
        assert_eq!(p.feed(b"[A"), vec![k(Key::Up)]);
        assert!(p.feed(b"\x1b[1;").is_empty());
        assert_eq!(p.feed(b"5D"), vec![Event::Key(Key::Left, Mods { ctrl: true, ..Mods::default() })]);
        assert!(p.feed(&"山".as_bytes()[..2]).is_empty());
        assert_eq!(p.feed(&"山".as_bytes()[2..]), vec![k(Key::Char('山'))]);
        assert!(p.feed(b"\x1b").is_empty());
        assert_eq!(p.flush(), vec![k(Key::Esc)]);
        assert!(!p.pending());
    }

    #[test]
    fn mouse() {
        assert_eq!(feed(b"\x1b[<0;10;5M\x1b[<0;10;5m\x1b[<64;3;4M\x1b[<65;3;4M\x1b[<32;11;5M\x1b[<4;1;1M"), vec![
            Event::Mouse { kind: MouseKind::Press(0), x: 9, y: 4, mods: Mods::default() },
            Event::Mouse { kind: MouseKind::Release, x: 9, y: 4, mods: Mods::default() },
            Event::Mouse { kind: MouseKind::WheelUp, x: 2, y: 3, mods: Mods::default() },
            Event::Mouse { kind: MouseKind::WheelDown, x: 2, y: 3, mods: Mods::default() },
            Event::Mouse { kind: MouseKind::Drag(0), x: 10, y: 4, mods: Mods::default() },
            Event::Mouse { kind: MouseKind::Press(0), x: 0, y: 0, mods: Mods { shift: true, ..Mods::default() } },
        ]);
    }

    #[test]
    fn x10_mouse_and_other_buttons() {
        // A terminal without the SGR mode: CSI M, then 32 + button, column, row.
        assert_eq!(feed(b"\x1b[M\x20\x2a\x25\x1b[M\x23\x2a\x25q"), vec![
            Event::Mouse { kind: MouseKind::Press(0), x: 9, y: 4, mods: Mods::default() },
            Event::Mouse { kind: MouseKind::Release, x: 9, y: 4, mods: Mods::default() },
            k(Key::Char('q')),
        ]);
        let mut p = Parser::new();
        assert!(p.feed(b"\x1b[M\x60").is_empty(), "a report cut between reads waits");
        assert_eq!(p.feed(b"\x21\x21"), vec![Event::Mouse { kind: MouseKind::WheelUp, x: 0, y: 0, mods: Mods::default() }]);
        // Horizontal scrolling and the back/forward buttons are not slices nor clicks.
        assert!(feed(b"\x1b[<66;3;4M\x1b[<67;3;4M\x1b[<128;3;4M\x1b[<129;3;4M").is_empty());
    }

    #[test]
    fn replies() {
        let r = |r| Event::Reply(r);
        assert_eq!(feed(b"\x1b[?62;4;22c"), vec![r(Reply::Attributes(vec![62, 4, 22]))]);
        assert_eq!(feed(b"\x1bP>|kitty(0.39.1)\x1b\\"), vec![r(Reply::Version("kitty(0.39.1)".into()))]);
        assert_eq!(feed(b"\x1b_Gi=31;OK\x1b\\\x1b_Gi=31;ENOTSUPPORTED:x\x1b\\"), vec![r(Reply::Kitty { id: 31, ok: true }), r(Reply::Kitty { id: 31, ok: false })]);
        assert_eq!(feed(b"\x1b[6;18;9t"), vec![r(Reply::CellSize(9, 18))]);
        assert_eq!(feed(b"\x1b[4;720;1280t"), vec![r(Reply::WindowPx(1280, 720))]);
        assert_eq!(feed(b"\x1b[?1;0;1024S"), vec![r(Reply::Graphics(1, 0, 1024))]);
        // Answers cut between reads, other strings ignored.
        let mut p = Parser::new();
        assert!(p.feed(b"\x1bP>|WezTerm 2024").is_empty());
        assert_eq!(p.feed(b"\x1b\\q"), vec![r(Reply::Version("WezTerm 2024".into())), k(Key::Char('q'))]);
        assert_eq!(feed(b"\x1b]11;rgb:0000/0000/0000\x07x"), vec![k(Key::Char('x'))]);
        // A hostile version string is cleaned (a stray byte is not UTF-8: replaced).
        assert_eq!(feed(b"\x1bP>|evil\x07\xc2\x9b\x9b\x1b\\"), vec![r(Reply::Version("evil\u{fffd}".into()))]);
    }

    #[test]
    fn garbage_never_panics_nor_grows() {
        let mut p = Parser::new();
        let mut x = 0x2545f4914f6cdd1du64;
        for _ in 0..200_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let b = [(x & 0xff) as u8, ((x >> 8) & 0xff) as u8, 0x1b, b'['];
            let n = (x >> 16) as usize % 4 + 1;
            let _ = p.feed(&b[..n]);
            assert!(p.buf.len() <= MAX_SEQUENCE + 4);
        }
        // An endless string is dropped.
        let mut p = Parser::new();
        p.feed(b"\x1bP");
        p.feed(&vec![b'a'; MAX_SEQUENCE + 10]);
        assert!(!p.pending());
        assert_eq!(p.feed(b"q"), vec![k(Key::Char('q'))]);
    }
}
