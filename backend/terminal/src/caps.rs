//! What the terminal can show, from its environment and from its answers
//! to a few queries: an image protocol (Kitty, Sixel, iTerm2) or text cells,
//! how many colours, and the size of a cell in pixels.
//!
//! The queries are sent together, DA1 (primary device attributes) last:
//! every terminal answers it, so its reply ends the wait after one round
//! trip, even over a slow SSH link. Inside tmux the queries reach tmux, which
//! answers for itself: Sixel only when tmux draws it, never Kitty, so the
//! image protocols that tmux would not pass on are left alone unless asked.

use std::time::{Duration, Instant};

use crate::color::Depth;
use crate::input::{Event, Parser, Reply};
use crate::tty::{Tty, Wait};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Graphics {
    Kitty,
    Sixel,
    Iterm2,
    /// Half blocks `▄` with two colours per cell.
    Blocks,
    /// Characters by brightness, no colour (dumb terminals, logs).
    Ascii,
}

impl Graphics {
    pub const NAMES: [&'static str; 5] = ["kitty", "sixel", "iterm2", "blocks", "ascii"];

    pub fn from_name(s: &str) -> Option<Graphics> {
        match s {
            "kitty" => Some(Graphics::Kitty),
            "sixel" => Some(Graphics::Sixel),
            "iterm2" => Some(Graphics::Iterm2),
            "blocks" => Some(Graphics::Blocks),
            "ascii" => Some(Graphics::Ascii),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }

    /// Drawn as pixels by the terminal (not as text cells).
    pub fn is_image(self) -> bool {
        matches!(self, Graphics::Kitty | Graphics::Sixel | Graphics::Iterm2)
    }
}

/// The environment variables that tell about the terminal.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Env {
    pub term: String,
    pub colorterm: String,
    pub term_program: String,
    pub lc_terminal: String,
    pub kitty_window: bool,
    /// Inside tmux ($TMUX).
    pub tmux: bool,
    /// Inside GNU screen ($STY).
    pub screen: bool,
}

impl Env {
    pub fn from_process() -> Env {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        Env {
            term: var("TERM"),
            colorterm: var("COLORTERM"),
            term_program: var("TERM_PROGRAM"),
            lc_terminal: var("LC_TERMINAL"),
            kitty_window: std::env::var_os("KITTY_WINDOW_ID").is_some(),
            tmux: std::env::var_os("TMUX").is_some(),
            screen: std::env::var_os("STY").is_some(),
        }
    }

    pub fn dumb(&self) -> bool {
        self.term == "dumb"
    }

    /// GNU screen (and not tmux, which also uses TERM=screen*).
    pub fn in_screen(&self) -> bool {
        !self.tmux && (self.screen || self.term.starts_with("screen"))
    }

    /// The Linux console and other terminals that would print unknown
    /// sequences: only DA1 is asked.
    fn plain(&self) -> bool {
        self.term == "linux" || self.term.starts_with("vt") || self.term == "ansi" || self.in_screen()
    }
}

/// What the terminal answered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Replies {
    /// DA1 attributes (`4`: Sixel graphics).
    pub da1: Option<Vec<u32>>,
    /// XTVERSION: name and version of the terminal.
    pub version: Option<String>,
    /// The Kitty graphics query was answered OK.
    pub kitty: Option<bool>,
    /// Cell size in pixels (width, height), from `CSI 16 t`.
    pub cell_px: Option<(u32, u32)>,
    /// Text area in pixels (width, height), from `CSI 14 t`, for terminals
    /// that do not answer the cell size.
    pub window_px: Option<(u32, u32)>,
    /// Sixel colour registers, from XTSMGRAPHICS.
    pub registers: Option<u32>,
}

impl Replies {
    pub fn complete(&self) -> bool {
        self.da1.is_some()
    }
}

/// Kitty's own query id.
pub const KITTY_QUERY_ID: u32 = 31;

/// The queries to send (empty for a dumb terminal).
pub fn queries(env: &Env) -> Vec<u8> {
    if env.dumb() {
        return Vec::new();
    }
    let mut q = Vec::new();
    if !env.plain() {
        // A 1×1 image transmitted directly, only queried (`a=q`): nothing is shown.
        q.extend_from_slice(format!("\x1b_Gi={KITTY_QUERY_ID},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\").as_bytes());
        q.extend_from_slice(b"\x1b[>0q"); // XTVERSION
        q.extend_from_slice(b"\x1b[16t"); // cell size in pixels
        q.extend_from_slice(b"\x1b[14t"); // text area in pixels
        q.extend_from_slice(b"\x1b[?1;1;0S"); // XTSMGRAPHICS: colour registers
    }
    q.extend_from_slice(b"\x1b[c"); // DA1, answered by every terminal
    q
}

/// What `probe` gathered besides the answers.
#[derive(Debug, Default)]
pub struct Probed {
    pub replies: Replies,
    /// Keys and mouse events typed meanwhile, to be handled after.
    pub events: Vec<Event>,
    /// Signals received meanwhile (SIGTERM, SIGINT...).
    pub signals: Vec<i32>,
}

/// Send the queries and gather the answers until DA1 arrives or `timeout`
/// passes. When the answers are late, what still comes within a short grace
/// is read too and the rest dropped from the input queue, so that the shell
/// does not read it after the program ends.
pub fn probe(tty: &mut Tty, env: &Env, timeout: Duration) -> Probed {
    let mut p = Probed::default();
    let q = queries(env);
    if q.is_empty() || tty.write(&q).is_err() {
        return p;
    }
    let deadline = Instant::now() + timeout;
    let mut parser = Parser::new();
    let mut late = false;
    while !p.replies.complete() {
        let mut left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            if late {
                break;
            }
            late = true;
            left = Duration::from_millis(200);
        }
        match tty.wait(Some(left)) {
            Ok(Wait::Input(bytes)) => {
                for e in parser.feed(&bytes) {
                    let r = &mut p.replies;
                    match e {
                        Event::Reply(Reply::Attributes(a)) => r.da1 = Some(a),
                        Event::Reply(Reply::Version(v)) => r.version = Some(v),
                        Event::Reply(Reply::Kitty { id: KITTY_QUERY_ID, ok }) => r.kitty = Some(ok),
                        Event::Reply(Reply::CellSize(w, h)) => r.cell_px = Some((w, h)),
                        Event::Reply(Reply::WindowPx(w, h)) => r.window_px = Some((w, h)),
                        Event::Reply(Reply::Graphics(1, 0, n)) => r.registers = Some(n),
                        Event::Reply(_) => {}
                        other => p.events.push(other),
                    }
                }
            }
            Ok(Wait::Signals(s)) => p.signals.extend(s),
            Ok(Wait::Timeout) if !late => {}
            Ok(Wait::Timeout) | Err(_) => break,
        }
    }
    if !p.replies.complete() {
        tty.flush_input();
    }
    p
}

/// How long to wait for the answers (NITTY_PROBE_MS, 1000 ms by default).
pub fn probe_timeout() -> Duration {
    let ms = std::env::var("NITTY_PROBE_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(1000);
    Duration::from_millis(ms.min(10_000))
}

/// The capabilities chosen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caps {
    pub graphics: Graphics,
    pub depth: Depth,
    /// Size of a cell in pixels (width, height).
    pub cell_px: (u32, u32),
    /// Image protocols go through tmux (DCS passthrough).
    pub tmux: bool,
    /// Colours a Sixel image may use.
    pub registers: usize,
}

impl Caps {
    /// Width over height of a cell.
    pub fn cell_aspect(&self) -> f64 {
        self.cell_px.0 as f64 / self.cell_px.1 as f64
    }
}

/// A usual cell size when the terminal does not tell.
pub const DEFAULT_CELL_PX: (u32, u32) = (8, 16);

fn known_truecolor(version: &str) -> bool {
    ["kitty", "WezTerm", "foot", "XTerm", "iTerm2", "ghostty", "contour", "mintty", "Alacritty", "tmux", "Konsole", "WindowsTerminal"]
        .iter()
        .any(|n| version.starts_with(n))
}

/// Choose from the environment, the replies, what the options force and the
/// cell size the kernel reports (`TIOCGWINSZ`, when the terminal sets it).
pub fn decide(env: &Env, r: &Replies, graphics: Option<Graphics>, depth: Option<Depth>, winsize_cell_px: Option<(u32, u32)>) -> Caps {
    let version = r.version.as_deref().unwrap_or("");
    // Behind a multiplexer the environment may still name the terminal at
    // the far end (LC_TERMINAL is forwarded by SSH): only its answer counts.
    let multiplexed = env.tmux || env.term.starts_with("tmux") || env.term.starts_with("screen");
    let named = env.term_program == "iTerm.app" || env.lc_terminal == "iTerm2";
    let iterm2 = version.starts_with("iTerm2") || (named && !multiplexed);
    let sixel = r.da1.as_ref().is_some_and(|a| a.contains(&4));
    let graphics = graphics.unwrap_or(if env.dumb() {
        Graphics::Ascii
    } else if r.kitty == Some(true) && !env.tmux {
        Graphics::Kitty
    } else if iterm2 && !env.tmux {
        Graphics::Iterm2
    } else if sixel {
        Graphics::Sixel
    } else {
        Graphics::Blocks
    });
    let colorterm = env.colorterm.to_ascii_lowercase();
    let depth = depth.unwrap_or(if graphics == Graphics::Ascii {
        Depth::Mono
    } else if colorterm == "truecolor" || colorterm == "24bit" || known_truecolor(version) {
        Depth::TrueColor
    } else if env.term == "linux" || env.term.starts_with("vt") || env.term == "ansi" {
        Depth::Ansi16
    } else if env.term_program == "Apple_Terminal" || env.in_screen() {
        Depth::Ansi256
    } else {
        Depth::TrueColor
    });
    let valid = |p: &(u32, u32)| (1..=1024).contains(&p.0) && (1..=1024).contains(&p.1);
    let cell_px = r.cell_px.filter(valid).or(winsize_cell_px.filter(valid)).unwrap_or(DEFAULT_CELL_PX);
    let registers = r.registers.map_or(256, |n| (n as usize).clamp(2, 256));
    Caps { graphics, depth, cell_px, tmux: env.tmux, registers }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(term: &str) -> Env {
        Env { term: term.into(), ..Env::default() }
    }

    fn replies(da1: &[u32]) -> Replies {
        Replies { da1: Some(da1.to_vec()), ..Replies::default() }
    }

    #[test]
    fn protocols_from_replies() {
        let e = env("xterm-256color");
        assert_eq!(decide(&e, &replies(&[62, 22]), None, None, None).graphics, Graphics::Blocks);
        assert_eq!(decide(&e, &replies(&[62, 4, 22]), None, None, None).graphics, Graphics::Sixel);
        let kitty = Replies { kitty: Some(true), version: Some("kitty(0.39.1)".into()), ..replies(&[62, 4]) };
        assert_eq!(decide(&e, &kitty, None, None, None).graphics, Graphics::Kitty);
        let iterm = Replies { version: Some("iTerm2 3.5.4".into()), ..replies(&[62, 4]) };
        assert_eq!(decide(&e, &iterm, None, None, None).graphics, Graphics::Iterm2);
        // Nothing answered (or only DA1): text cells.
        assert_eq!(decide(&e, &Replies::default(), None, None, None).graphics, Graphics::Blocks);
        let dumb = decide(&env("dumb"), &Replies::default(), None, None, None);
        assert_eq!((dumb.graphics, dumb.depth), (Graphics::Ascii, Depth::Mono));
        // Forced.
        assert_eq!(decide(&e, &kitty, Some(Graphics::Blocks), None, None).graphics, Graphics::Blocks);
    }

    #[test]
    fn tmux_is_cautious() {
        let e = Env { term: "tmux-256color".into(), tmux: true, ..Env::default() };
        // Even if an outer Kitty answered through tmux, tmux would not pass images on.
        let r = Replies { kitty: Some(true), version: Some("tmux 3.4".into()), ..replies(&[62, 22]) };
        let c = decide(&e, &r, None, None, None);
        assert_eq!((c.graphics, c.depth, c.tmux), (Graphics::Blocks, Depth::TrueColor, true));
        // tmux built with Sixel says so in its own DA1.
        assert_eq!(decide(&e, &replies(&[62, 4, 22]), None, None, None).graphics, Graphics::Sixel);
        let e2 = Env { term_program: "iTerm.app".into(), ..e.clone() };
        assert_eq!(decide(&e2, &replies(&[62]), None, None, None).graphics, Graphics::Blocks);
        // A tmux reached over SSH (no $TMUX here), iTerm2 at the far end.
        let far = Env { term: "tmux-256color".into(), lc_terminal: "iTerm2".into(), ..Env::default() };
        assert_eq!(decide(&far, &replies(&[62]), None, None, None).graphics, Graphics::Blocks);
        let near = Env { term: "xterm-256color".into(), lc_terminal: "iTerm2".into(), ..Env::default() };
        assert_eq!(decide(&near, &replies(&[62]), None, None, None).graphics, Graphics::Iterm2);
        assert_eq!(decide(&e2, &replies(&[62]), Some(Graphics::Iterm2), None, None).graphics, Graphics::Iterm2);
    }

    #[test]
    fn colours_and_cells() {
        let r = Replies::default();
        assert_eq!(decide(&env("xterm-256color"), &r, None, None, None).depth, Depth::TrueColor);
        assert_eq!(decide(&env("linux"), &r, None, None, None).depth, Depth::Ansi16);
        assert_eq!(decide(&Env { screen: true, ..env("screen.xterm-256color") }, &r, None, None, None).depth, Depth::Ansi256);
        assert_eq!(decide(&Env { term_program: "Apple_Terminal".into(), ..env("xterm-256color") }, &r, None, None, None).depth, Depth::Ansi256);
        assert_eq!(decide(&Env { colorterm: "truecolor".into(), ..env("screen") }, &r, None, None, None).depth, Depth::TrueColor);
        assert_eq!(decide(&env("xterm"), &r, None, Some(Depth::Ansi256), None).depth, Depth::Ansi256);
        // Cell size: the terminal's answer, else the kernel's, else 8 × 16.
        let c = |r: &Replies, w| decide(&env("xterm"), r, None, None, w).cell_px;
        assert_eq!(c(&r, None), (8, 16));
        assert_eq!(c(&r, Some((10, 21))), (10, 21));
        assert_eq!(c(&Replies { cell_px: Some((9, 18)), ..r.clone() }, Some((10, 21))), (9, 18));
        assert_eq!(c(&Replies { cell_px: Some((0, 18)), ..r.clone() }, Some((0, 0))), (8, 16));
        assert_eq!(decide(&env("xterm"), &Replies { registers: Some(1024), ..r.clone() }, None, None, None).registers, 256);
        assert_eq!(decide(&env("xterm"), &Replies { registers: Some(16), ..r }, None, None, None).registers, 16);
    }

    #[test]
    fn queries_suit_the_terminal() {
        assert!(queries(&env("dumb")).is_empty());
        assert_eq!(queries(&env("linux")), b"\x1b[c");
        assert_eq!(queries(&Env { screen: true, ..env("screen") }), b"\x1b[c");
        let q = String::from_utf8(queries(&env("xterm-256color"))).unwrap();
        assert!(q.starts_with("\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\") && q.ends_with("\x1b[c"), "{q:?}");
    }
}
