//! The interactive viewer: the views around a cursor, a side panel, a
//! status bar, keys and mouse. Keys follow the Sushi viewer where it has
//! them (O, L, C, V, `,` `.`, Home End, `[` `]`).
//!
//! Each frame is drawn into a grid of cells and only the cells that changed
//! are written, between the begin and end of a synchronized update (no
//! tearing where the terminal supports it); a picture is computed again only
//! when what it shows changed, and images sent with a protocol are sent
//! again only then. Keys that arrive together are handled before drawing,
//! so that a held key never queues frames. The image is read on a helper
//! thread, so that the viewer answers (and quits) while a large or remote
//! file is read.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gnome_nifti_preview::error::Result;
use gnome_nifti_preview::orientation::MODES;
use gnome_nifti_preview::panel::{self, Section};
use gnome_nifti_preview::render;
use gnome_nifti_preview::view::{self, LabelList, LoadState, Loading, Readout};
use gnome_nifti_preview::window::PALETTES;

use crate::args::Options;
use crate::caps::Caps;
use crate::draw;
use crate::graphics::{self, kitty};
use crate::input::{Event, Key, Mods, MouseKind, Parser};
use crate::layout::{self, Extent, ViewBox};
use crate::picture::Picture;
use crate::screen::{Rect, Screen, Style};
use crate::text;
use crate::tty::{Tty, Wait};
use crate::viewer::{self, Viewer};

/// Rows of the status bar.
const STATUS_ROWS: usize = 2;
/// How long a message stays in the status bar.
const MESSAGE: Duration = Duration::from_secs(4);
/// Polling while something runs in the background (a load, a volume being
/// decompressed).
const TICK: Duration = Duration::from_millis(100);
/// An escape alone is the Esc key when nothing follows within this time.
const ESCAPE: Duration = Duration::from_millis(80);

const HELP: &[(&str, &str)] = &[
    ("← → ↑ ↓", "move the cursor in the active view (Shift: 10 voxels)"),
    ("PgUp PgDn  z x", "next / previous slice of the active view (Shift: 10)"),
    ("mouse", "click: place the cursor · wheel: slices · Shift+wheel: volumes"),
    ("v", "next view becomes the active one"),
    ("1 2 3 / 0", "coronal, sagittal or axial view alone / all three"),
    ("o", "neurological → radiological → stored voxel axes"),
    ("l", "labels or colour ↔ grey levels"),
    ("p", "palette of grey levels: gray, hot, inferno, viridis"),
    ("w W / b B / r", "window narrower, wider / darker, brighter / automatic"),
    (", .  Home End", "previous / next volume, first / last (4D)"),
    ("[ ]", "previous / next DICOM series"),
    ("n N", "next / previous file"),
    ("c", "back to the centre"),
    ("#", "crosshair shown or hidden"),
    ("Tab  i", "side panel: metadata, labels, time course"),
    ("?", "this help"),
    ("q  Esc", "quit · Ctrl-Z suspends · Ctrl-L redraws"),
];

/// Lines of text and their style.
type Lines = Vec<(String, Style)>;

/// The time course of the voxel under the cursor.
struct Timecourse {
    values: Vec<Option<f64>>,
    /// Every volume was decompressed when it was read.
    complete: bool,
    error: Option<String>,
}

/// What a picture shows (when it changes, the picture is computed again).
#[derive(Debug, Clone, Copy, PartialEq)]
struct PicKey {
    plan: usize,
    index: usize,
    volume: usize,
    mode: &'static str,
    generation: u64,
    size: (usize, usize),
    cross: Option<(usize, usize)>,
}

enum State {
    /// Header read, data being read on a helper thread.
    Loading { job: Loading, path: PathBuf, started: Instant },
    Failed { name: String, message: String, about: Vec<Section> },
    Ready(Box<Viewer>),
}

pub struct App<'a> {
    o: &'a Options,
    caps: Caps,
    tty: Tty,
    files: Vec<PathBuf>,
    file: usize,
    state: State,
    /// First time a file is shown: the options place the cursor.
    placed: bool,
    /// Display mode and colormap asked for, kept from file to file.
    mode: &'static str,
    colormap: &'static str,
    /// The image shown while another series is read, shown again if that
    /// series cannot be.
    fallback: Option<Box<Viewer>>,
    /// The header as the protocol sends it, of the image shown.
    header: Option<serde_json::Value>,
    /// The panel's warnings and lines, for (generation, volume).
    sections: Option<(u64, usize, Vec<String>, Lines)>,
    active: usize,
    single: Option<usize>,
    panel: bool,
    help: bool,
    crosshair: bool,
    message: Option<(String, Instant)>,
    /// Changes whenever what pictures show changes beyond their key.
    generation: u64,
    volume_target: usize,
    volume_progress: Option<f64>,
    readout: Option<Readout>,
    labels: Option<(u64, usize, LabelList)>,
    timecourse: Option<([usize; 3], usize, Timecourse)>,
    boxes: Vec<(ViewBox, usize)>,
    pictures: [Option<(PicKey, Picture)>; 3],
    drawn: [Option<(Rect, PicKey)>; 3],
    /// The image protocol's bytes of each picture, to draw it again (after a
    /// full redraw) without encoding it again.
    encoded: [Option<(Rect, PicKey, Vec<u8>)>; 3],
    prev: Option<Screen>,
    quit: bool,
    /// NITTY_TIMING: the time and bytes of each frame on stderr.
    timing: bool,
}

impl<'a> App<'a> {
    pub fn new(o: &'a Options, tty: Tty, caps: Caps) -> App<'a> {
        App {
            o,
            caps,
            tty,
            files: o.files.clone(),
            file: 0,
            state: State::Failed { name: String::new(), message: String::new(), about: Vec::new() },
            placed: false,
            mode: o.mode,
            colormap: o.colormap,
            fallback: None,
            header: None,
            sections: None,
            active: 2,
            single: None,
            panel: false,
            help: false,
            crosshair: true,
            message: None,
            generation: 0,
            volume_target: 0,
            volume_progress: None,
            readout: None,
            labels: None,
            timecourse: None,
            boxes: Vec::new(),
            pictures: [None, None, None],
            drawn: [None, None, None],
            encoded: [None, None, None],
            prev: None,
            quit: false,
            timing: std::env::var_os("NITTY_TIMING").is_some(),
        }
    }

    fn say(&mut self, m: impl Into<String>) {
        self.message = Some((m.into(), Instant::now()));
    }

    /// What the pictures show changed (palette, window, colormap, mode,
    /// volume, file): every picture is computed again.
    fn changed(&mut self) {
        self.generation += 1;
        self.readout = None;
        self.timecourse = None;
    }

    /// Only the cursor moved: the readout follows; a picture is computed
    /// again only if its slice (or, in text cells, its crosshair) changed.
    fn moved(&mut self) {
        self.readout = None;
    }

    /// Open file number `k` and start reading it.
    fn open(&mut self, k: usize) {
        self.file = k;
        self.placed = false;
        self.fallback = None;
        self.header = None;
        let path = self.files[k].clone();
        let name = text::clean(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        // Something on screen at once: opening a DICOM folder takes a moment.
        self.state = State::Failed { name: name.clone(), message: "Opening…".into(), about: Vec::new() };
        let _ = self.draw();
        self.state = match viewer::open_volume(&path, self.o) {
            Ok(vol) => self.start_loading(vol, path),
            Err(e) => State::Failed { name, message: text::clean(&e.message), about: viewer::about_of(&e) },
        };
        self.changed();
    }

    fn start_loading(&mut self, vol: gnome_nifti_preview::volume::Volume, path: PathBuf) -> State {
        match Loading::start(Box::new(vol)) {
            Ok(job) => State::Loading { job, path, started: Instant::now() },
            // No thread could be started: read here.
            Err(mut vol) => match vol.ensure_data().and_then(|_| vol.stats().map(|_| ())) {
                Ok(()) => self.ready(*vol, path),
                Err(e) => State::Failed { name: path.display().to_string(), message: e.message, about: Vec::new() },
            },
        }
    }

    fn ready(&mut self, vol: gnome_nifti_preview::volume::Volume, path: PathBuf) -> State {
        let mut v = match Viewer::new(vol, &path, self.mode, self.colormap) {
            Ok(v) => v,
            Err(e) => return State::Failed { name: path.display().to_string(), message: e.message, about: Vec::new() },
        };
        if !self.placed {
            self.placed = true;
            if let Err(e) = v.place_cursor(self.o) {
                self.say(e.message);
            }
            let views = v.views(self.o.view);
            self.single = (views.len() == 1).then(|| views[0]);
            self.active = self.single.unwrap_or(2);
            self.volume_target = match self.o.volume {
                Some(k) => k.min(v.vol.n_volumes().saturating_sub(1)),
                None => v.vol.current_volume(),
            };
        } else {
            // Another series: it opens on its own volume.
            self.volume_target = v.vol.current_volume();
        }
        self.volume_progress = None;
        self.header = Some(v.header());
        self.fallback = None;
        State::Ready(Box::new(v))
    }

    /// Poll the background work: the load, a volume being decompressed.
    fn poll(&mut self) -> bool {
        let mut changed = false;
        if let State::Loading { job, path, .. } = &self.state {
            match job.poll() {
                LoadState::Running => changed = true, // the progress moved
                LoadState::Done(vol, result) => {
                    let path = path.clone();
                    self.state = match (result, self.fallback.take()) {
                        (Ok(()), _) => self.ready(*vol, path),
                        // A series that cannot be read: back to the one shown.
                        (Err(e), Some(previous)) => {
                            self.say(text::clean(&e.message));
                            State::Ready(previous)
                        }
                        (Err(e), None) => State::Failed { name: path.display().to_string(), message: text::clean(&e.message), about: viewer::about_of(&e) },
                    };
                    self.changed();
                    self.prev = None;
                    changed = true;
                }
                LoadState::Lost => {
                    let message = "Internal error while reading the image.";
                    self.state = match self.fallback.take() {
                        Some(previous) => {
                            self.say(message);
                            State::Ready(previous)
                        }
                        None => State::Failed { name: path.display().to_string(), message: message.into(), about: Vec::new() },
                    };
                    self.redraw_all();
                    changed = true;
                }
            }
        }
        let target = self.volume_target;
        if let State::Ready(v) = &mut self.state {
            if v.vol.browsable() && target != v.vol.current_volume() {
                let before = v.vol.current_volume();
                match v.vol.set_volume(target as i64) {
                    Ok((true, _)) => {
                        let now = v.vol.current_volume();
                        // (A volume past the last one is clamped to it.)
                        self.volume_target = now;
                        self.volume_progress = None;
                        if now != before {
                            self.changed();
                        }
                    }
                    Ok((false, p)) => self.volume_progress = Some(p),
                    Err(e) => {
                        let shown = v.vol.current_volume();
                        self.volume_target = shown;
                        self.volume_progress = None;
                        self.say(e.message);
                    }
                }
                changed = true;
            }
        }
        if self.message.as_ref().is_some_and(|(_, t)| t.elapsed() > MESSAGE) {
            self.message = None;
            changed = true;
        }
        // The time course is read again until every volume is in.
        changed |= self.panel && self.timecourse.as_ref().is_some_and(|(_, _, t)| !t.complete);
        changed
    }

    fn busy(&self) -> bool {
        let decompressing = self.panel && self.timecourse.as_ref().is_some_and(|(_, _, t)| !t.complete);
        matches!(self.state, State::Loading { .. }) || self.volume_progress.is_some() || self.message.is_some() || decompressing
    }

    /// Run until the user quits.
    pub fn run(mut self, early: Vec<Event>) -> std::io::Result<()> {
        self.tty.raw()?;
        self.tty.enter(!self.o.no_mouse)?;
        self.open(0);
        let mut parser = Parser::new();
        for e in early {
            self.event(e);
        }
        let mut dirty = true;
        while !self.quit {
            if dirty {
                self.draw()?;
                dirty = false;
            }
            let timeout = if parser.pending() {
                Some(ESCAPE)
            } else if self.busy() {
                Some(TICK)
            } else {
                None
            };
            match self.tty.wait(timeout)? {
                Wait::Input(bytes) => {
                    for e in parser.feed(&bytes) {
                        self.event(e);
                    }
                    dirty = true;
                }
                Wait::Signals(signals) => {
                    for s in signals {
                        match s {
                            libc::SIGTERM | libc::SIGHUP | libc::SIGINT | libc::SIGQUIT => self.quit = true,
                            // Stopped from outside (kill -TSTP): as Ctrl-Z.
                            libc::SIGTSTP => {
                                self.tty.suspend(!self.o.no_mouse)?;
                                self.redraw_all();
                            }
                            // Continued after a stop from outside: take the terminal again.
                            libc::SIGCONT => {
                                self.tty.raw()?;
                                self.tty.enter(!self.o.no_mouse)?;
                                self.redraw_all();
                            }
                            _ => self.redraw_all(), // SIGWINCH
                        }
                    }
                    dirty = true;
                }
                Wait::Timeout => {
                    for e in parser.flush() {
                        self.event(e);
                    }
                    dirty |= self.poll();
                }
            }
            dirty |= self.poll();
        }
        Ok(())
    }

    /// Forget what the terminal shows: everything is written again.
    fn redraw_all(&mut self) {
        self.prev = None;
        self.drawn = [None, None, None];
    }

    // ------------------------------------------------------------ events

    fn event(&mut self, e: Event) {
        match e {
            Event::Key(k, m) => self.key(k, m),
            Event::Mouse { kind, x, y, mods } => self.mouse(kind, x, y, mods),
            Event::Reply(_) => {}
        }
    }

    fn key(&mut self, k: Key, m: Mods) {
        // Any key but q and the Ctrl ones (quit, suspend, redraw) closes the help.
        if self.help && !m.ctrl && k != Key::Char('q') {
            self.help = false;
            return;
        }
        let step = if m.shift { 10 } else { 1 };
        match (k, m.ctrl) {
            (Key::Char('c'), true) | (Key::Char('q'), false) | (Key::Esc, _) => self.quit = true,
            (Key::Char('z'), true) => {
                if self.tty.suspend(!self.o.no_mouse).is_err() {
                    self.quit = true;
                }
                self.redraw_all();
            }
            (Key::Char('l'), true) => self.redraw_all(),
            (_, true) => {}
            (Key::Char('?'), _) => self.help = matches!(self.state, State::Ready(_)),
            (Key::Tab | Key::Char('i'), _) => {
                self.panel = !self.panel;
                self.redraw_all();
            }
            (Key::Char('n'), _) if self.files.len() > 1 => self.open((self.file + 1) % self.files.len()),
            (Key::Char('N'), _) if self.files.len() > 1 => self.open((self.file + self.files.len() - 1) % self.files.len()),
            _ => {
                if let Err(e) = self.viewer_key(k, step) {
                    self.say(text::clean(&e.message));
                }
            }
        }
    }

    fn viewer_key(&mut self, k: Key, step: i64) -> Result<()> {
        let State::Ready(v) = &mut self.state else { return Ok(()) };
        let active = self.active;
        let cursor = v.cursor;
        let mut note = None;
        match k {
            Key::Left => move_cursor(v, active, -step, 0),
            Key::Right => move_cursor(v, active, step, 0),
            Key::Up => move_cursor(v, active, 0, -step),
            Key::Down => move_cursor(v, active, 0, step),
            Key::PageUp | Key::Char('z') => scroll(v, active, step),
            Key::PageDown | Key::Char('x') => scroll(v, active, -step),
            Key::Char('Z') => scroll(v, active, 10),
            Key::Char('X') => scroll(v, active, -10),
            Key::Char('c') => v.cursor = v.grid().center(),
            Key::Char('v') => {
                let shown = self.shown();
                let i = shown.iter().position(|&p| p == active).map_or(0, |i| (i + 1) % shown.len());
                self.active = shown[i];
                if self.single.is_some() {
                    self.single = Some(self.active);
                    self.redraw_all();
                }
                return Ok(());
            }
            Key::Char(c @ '1'..='3') => {
                let p = c as usize - '1' as usize;
                self.single = Some(p);
                self.active = p;
                self.redraw_all();
                return Ok(());
            }
            Key::Char('0') => {
                self.single = None;
                self.redraw_all();
                return Ok(());
            }
            Key::Char('o') => {
                let i = MODES.iter().position(|m| *m == v.mode).unwrap_or(0);
                v.set_mode(MODES[(i + 1) % MODES.len()])?;
                let m = v.effective_mode();
                let asked = v.mode;
                self.mode = asked;
                self.redraw_all();
                self.changed();
                if m != asked {
                    self.say("No orientation in this file: the stored voxel axes are shown.");
                }
                return Ok(());
            }
            Key::Char('l') => {
                let choices = render::colormap_choices(&mut v.vol)?;
                let shown = v.effective_colormap()?;
                if choices.len() < 2 {
                    self.say("Grey levels are the only colours of this image.");
                    return Ok(());
                }
                let i = choices.iter().position(|c| *c == shown).unwrap_or(0);
                v.colormap = choices[(i + 1) % choices.len()];
                self.colormap = v.colormap;
            }
            Key::Char('p') => {
                let i = PALETTES.iter().position(|p| *p == v.vol.palette).unwrap_or(0);
                v.vol.palette = PALETTES[(i + 1) % PALETTES.len()];
                v.cache.clear();
                let grey = if v.effective_colormap()? != "gray" { " (shown on grey levels)" } else { "" };
                note = Some(format!("Palette {}{grey}", v.vol.palette));
            }
            Key::Char(c @ ('w' | 'W' | 'b' | 'B')) => {
                let w = v.vol.display_window()?;
                let width = if w.high > w.low { w.high - w.low } else { w.low.abs().max(1.0) * 0.1 };
                let centre = (w.low + w.high) / 2.0;
                let (centre, width) = match c {
                    'w' => (centre, width * 0.8),
                    'W' => (centre, width * 1.25),
                    'b' => (centre + 0.1 * width, width),
                    _ => (centre - 0.1 * width, width),
                };
                v.vol.set_window(Some((centre - width / 2.0, centre + width / 2.0)))?;
                v.cache.clear();
            }
            Key::Char('r') => {
                v.vol.set_window(None)?;
                v.vol.set_use_file_window(true);
                v.cache.clear();
            }
            Key::Char(',') | Key::Char('.') | Key::Home | Key::End => {
                if !v.vol.browsable() {
                    return Ok(());
                }
                let last = v.vol.n_volumes() - 1;
                self.volume_target = match k {
                    Key::Char(',') => self.volume_target.saturating_sub(step as usize),
                    Key::Char('.') => (self.volume_target + step as usize).min(last),
                    Key::Home => 0,
                    _ => last,
                };
                return Ok(());
            }
            Key::Char('[') | Key::Char(']') => return self.step_series(if k == Key::Char('[') { -1 } else { 1 }),
            Key::Char('#') => {
                self.crosshair = !self.crosshair;
                return Ok(());
            }
            _ => return Ok(()),
        }
        let now = v.cursor;
        if let Some(n) = note {
            self.say(n);
        }
        if now != cursor {
            self.moved();
        } else if !matches!(k, Key::Left | Key::Right | Key::Up | Key::Down | Key::PageUp | Key::PageDown | Key::Char('z' | 'x' | 'Z' | 'X' | 'c')) {
            self.changed();
        }
        Ok(())
    }

    fn step_series(&mut self, d: i64) -> Result<()> {
        let State::Ready(v) = &mut self.state else { return Ok(()) };
        if v.vol.header.dicom.is_none() {
            self.say("Not a DICOM image: no other series.");
            return Ok(());
        }
        let (catalog, current) = view::series_catalog(&mut v.vol)?;
        let list = catalog.related(current.as_deref());
        if list.len() < 2 {
            self.say("No other series here.");
            return Ok(());
        }
        let i = list.iter().position(|s| Some(s.uid.as_str()) == current.as_deref()).unwrap_or(0) as i64;
        let next = list[(i + d).rem_euclid(list.len() as i64) as usize];
        let label = panel::series_label(next, false);
        let mut vol = view::open_series_with(&v.vol, &next.uid, self.o.max_bytes)?;
        viewer::apply_options(&mut vol, self.o)?;
        let path = v.path.clone();
        self.say(format!("Series {}", text::clean(&label)));
        let shown = std::mem::replace(&mut self.state, State::Failed { name: String::new(), message: String::new(), about: Vec::new() });
        if let State::Ready(previous) = shown {
            self.fallback = Some(previous);
        }
        self.state = self.start_loading(vol, path);
        self.changed();
        Ok(())
    }

    fn mouse(&mut self, kind: MouseKind, x: usize, y: usize, mods: Mods) {
        let Some(&(b, plan)) = self.boxes.iter().find(|(b, _)| b.frame.contains(x, y)) else { return };
        let State::Ready(v) = &mut self.state else { return };
        match kind {
            MouseKind::Press(0) | MouseKind::Drag(0) if b.image.contains(x, y) => {
                self.active = plan;
                let p = &v.plans[plan];
                let col = (((x - b.image.x) as f64 + 0.5) / b.image.w as f64 * p.width as f64) as usize;
                let row = (((y - b.image.y) as f64 + 0.5) / b.image.h as f64 * p.height as f64) as usize;
                v.cursor = p.grid_point(v.index(plan), col.min(p.width - 1), row.min(p.height - 1));
            }
            MouseKind::Press(0) => self.active = plan,
            MouseKind::WheelUp | MouseKind::WheelDown => {
                let d = if kind == MouseKind::WheelUp { 1 } else { -1 };
                if mods.shift && v.vol.browsable() {
                    let last = v.vol.n_volumes() - 1;
                    self.volume_target = (self.volume_target as i64 + d).clamp(0, last as i64) as usize;
                    return;
                }
                scroll(v, plan, d);
            }
            _ => return,
        }
        self.moved();
    }

    /// The views shown (plan indices).
    fn shown(&self) -> Vec<usize> {
        match (&self.state, self.single) {
            (_, Some(p)) => vec![p],
            (State::Ready(v), None) => v.views(None),
            _ => vec![0, 1, 2],
        }
    }

    // ------------------------------------------------------------ drawing

    fn draw(&mut self) -> std::io::Result<()> {
        let started = Instant::now();
        let (cols, rows, _, _) = self.tty.size();
        let mut s = Screen::new(cols, rows);
        let mut images: Vec<(usize, Rect, PicKey)> = Vec::new();
        let mut boxes = Vec::new();
        match &self.state {
            State::Loading { job, path, started } => {
                let name = text::clean(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                let p = job.fraction();
                let rate = job.rate().map(|r| format!(" · {}/s", panel::format_bytes(r as u64))).unwrap_or_default();
                let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][(started.elapsed().as_millis() / 100) as usize % 10];
                let lines = [name, format!("{spin} Reading… {} %{rate}", (p * 100.0).floor()), String::new(), "q quits".into()];
                centre_lines(&mut s, &lines);
            }
            State::Failed { name, message, about } => {
                let mut lines = vec![(name.clone(), Style::default().bold()), (message.clone(), Style::default())];
                lines.push((String::new(), Style::default()));
                lines.extend(draw::section_lines(about));
                lines.push((String::new(), Style::default()));
                lines.push((if self.files.len() > 1 { "n N other files · q quits" } else { "q quits" }.into(), Style::default().dim()));
                for (y, (l, st)) in lines.iter().enumerate().take(rows) {
                    s.put(1, y, &text::fit(l, cols.saturating_sub(2)), *st, cols);
                }
            }
            State::Ready(_) if self.help => {
                let w = HELP.iter().map(|(k, _)| text::str_width(k)).max().unwrap_or(0);
                s.put(1, 0, "nitty — keys", Style::default().bold(), cols);
                for (y, (k, d)) in HELP.iter().enumerate() {
                    s.put(2, y + 2, k, Style::default().bold(), cols);
                    s.put(4 + w, y + 2, d, Style::default(), cols.saturating_sub(4 + w));
                }
                s.put(1, HELP.len() + 3, "any key returns to the image", Style::default().dim(), cols);
            }
            State::Ready(_) => {
                if let Err(e) = self.draw_viewer(&mut s, &mut images, &mut boxes) {
                    s = Screen::new(cols, rows);
                    s.put(1, 0, &text::clean(&e.message), Style::default(), cols.saturating_sub(1));
                    images.clear();
                    boxes.clear();
                }
            }
        }
        // A new layout: everything is drawn again (and Kitty's images removed).
        let rects: Vec<Rect> = boxes.iter().map(|(b, _): &(ViewBox, usize)| b.image).collect();
        let old: Vec<Rect> = self.boxes.iter().map(|(b, _)| b.image).collect();
        if rects != old {
            self.redraw_all();
        }
        self.boxes = boxes;
        let mut out = b"\x1b[?2026h".to_vec();
        if self.prev.is_none() && self.caps.graphics == crate::caps::Graphics::Kitty {
            graphics::put(&kitty::delete_all(), &self.caps, &mut out);
        }
        s.diff(self.prev.as_ref(), self.caps.depth, &mut out);
        for (plan, r, key) in images {
            if self.drawn[plan] == Some((r, key)) {
                continue;
            }
            let Some((_, pic)) = &self.pictures[plan] else { continue };
            out.extend_from_slice(format!("\x1b[{};{}H", r.y + 1, r.x + 1).as_bytes());
            let id = plan as u32 + 1;
            if self.caps.graphics == crate::caps::Graphics::Kitty && self.drawn[plan].is_some() {
                graphics::put(&kitty::delete(id), &self.caps, &mut out);
            }
            if !matches!(&self.encoded[plan], Some((er, ek, _)) if *er == r && *ek == key) {
                self.encoded[plan] = Some((r, key, graphics::encode(pic, r, &self.caps, id)));
            }
            if let Some((_, _, bytes)) = &self.encoded[plan] {
                out.extend_from_slice(bytes);
            }
            self.drawn[plan] = Some((r, key));
        }
        out.extend_from_slice(b"\x1b[?2026l");
        self.prev = Some(s);
        let computed = started.elapsed();
        self.tty.write(&out)?;
        if self.timing {
            eprintln!("nitty: frame {:.2} ms to compute, {:.2} ms in all, {} bytes", computed.as_secs_f64() * 1e3, started.elapsed().as_secs_f64() * 1e3, out.len());
        }
        Ok(())
    }

    fn draw_viewer(&mut self, s: &mut Screen, images: &mut Vec<(usize, Rect, PicKey)>, boxes: &mut Vec<(ViewBox, usize)>) -> Result<()> {
        let (cols, rows) = (s.w, s.h);
        let shown = self.shown();
        if !shown.contains(&self.active) {
            self.active = shown[0];
        }
        let State::Ready(v) = &mut self.state else { return Ok(()) };
        if self.readout.is_none() {
            self.readout = Some(v.readout()?);
        }
        // A single-voxel spectrum has no views: its plot.
        if v.vol.mrs.is_some() && v.vol.shape3 == [1, 1, 1] {
            let panel_w = if self.panel { 48.min(cols) } else { 0 };
            let area = Rect { x: 0, y: 0, w: cols.saturating_sub(panel_w + usize::from(self.panel)), h: rows.saturating_sub(STATUS_ROWS) };
            if let Some(b) = layout::arrange_in(area, &[Extent { w: 4.0, h: 3.0 }], self.caps.cell_aspect(), &layout::arrangements(1)) {
                let b = b[0];
                let size = draw::picture_size(&self.caps, b.image, (1024, 768));
                let key = PicKey { plan: 0, index: 0, volume: 0, mode: "spectrum", generation: self.generation, size, cross: None };
                if self.pictures[0].as_ref().map(|(k, _)| *k) != Some(key) {
                    let opts = gnome_nifti_preview::thumbnail::Options { size: size.0.clamp(16, 2048), ..Default::default() };
                    let p = gnome_nifti_preview::thumbnail::render(&mut v.vol, &opts)?;
                    let rgb = gnome_nifti_preview::thumbnail::resample(&p.rgb, p.width, p.height, size.0, size.1, false)?;
                    self.pictures[0] = Some((key, Picture { w: size.0, h: size.1, rgb }));
                }
                s.put(b.frame.x, b.frame.y, " Spectrum ", Style::default().bold(), b.frame.w);
                if let Some((_, pic)) = &self.pictures[0] {
                    draw::view_cells(s, &self.caps, b.image, pic);
                }
                images.push((0, b.image, key));
                boxes.push((b, 0));
            }
            if self.panel {
                self.draw_panel(s, Rect { x: cols - panel_w, y: 0, w: panel_w, h: rows.saturating_sub(STATUS_ROWS) })?;
            }
            self.draw_status(s)?;
            if !self.caps.graphics.is_image() {
                images.clear();
            }
            return Ok(());
        }
        let panel_w = if self.panel { (cols / 3).clamp(30, 48).min(cols) } else { 0 };
        let full_panel = self.panel && cols.saturating_sub(panel_w) < 30;
        let area = Rect { x: 0, y: 0, w: if full_panel { 0 } else { cols.saturating_sub(panel_w + usize::from(self.panel)) }, h: rows.saturating_sub(STATUS_ROWS) };
        let extents: Vec<Extent> = shown
            .iter()
            .map(|&p| {
                let pl = &v.plans[p];
                let size = |x: f64| if x.is_finite() && x > 0.0 { x } else { 1.0 };
                Extent { w: pl.width as f64 * size(pl.pixel_w), h: pl.height as f64 * size(pl.pixel_h) }
            })
            .collect();
        let arranged = if area.w > 0 { layout::arrange_in(area, &extents, self.caps.cell_aspect(), &layout::arrangements(shown.len())) } else { None };
        match arranged {
            None if !full_panel => {
                let m = if cols >= 40 { format!("The terminal is too small ({cols} × {rows}).") } else { format!("Too small {cols}×{rows}") };
                s.put(0, 0, &text::fit(&m, cols), Style::default(), cols);
            }
            None => {}
            Some(list) => {
                let mode = v.effective_mode();
                let volume = v.vol.current_volume();
                for b in list {
                    let plan = shown[b.view];
                    let index = v.index(plan);
                    draw::view_frame(s, &b, &v.plans[plan], index, plan == self.active && shown.len() > 1);
                    let (pw, ph) = (v.plans[plan].width, v.plans[plan].height);
                    let size = draw::picture_size(&self.caps, b.image, (pw, ph));
                    // With an image protocol the crosshair is drawn in the
                    // margins, so that a picture only changes with its slice.
                    let images_drawn = self.caps.graphics.is_image();
                    let burnt = self.crosshair && !images_drawn;
                    let cross = burnt.then(|| v.plans[plan].screen_point(v.cursor));
                    let key = PicKey { plan, index, volume, mode, generation: self.generation, size, cross };
                    if self.pictures[plan].as_ref().map(|(k, _)| *k) != Some(key) {
                        let pic = v.picture(plan, size.0, size.1, burnt)?;
                        self.pictures[plan] = Some((key, pic));
                    }
                    if let Some((_, pic)) = &self.pictures[plan] {
                        draw::view_cells(s, &self.caps, b.image, pic);
                    }
                    if self.crosshair && images_drawn {
                        let p = &v.plans[plan];
                        draw::margin_marks(s, b.image, p, p.screen_point(v.cursor), self.caps.depth);
                    }
                    images.push((plan, b.image, key));
                    boxes.push((b, plan));
                }
            }
        }
        if self.panel {
            let (x, w) = if full_panel { (0, cols) } else { (cols - panel_w, panel_w) };
            self.draw_panel(s, Rect { x, y: 0, w, h: rows.saturating_sub(STATUS_ROWS) })?;
        }
        self.draw_status(s)?;
        if !self.caps.graphics.is_image() {
            images.clear();
        }
        Ok(())
    }

    /// The side panel. A part that cannot be computed (a time course of too
    /// many volumes...) says so in its place; the rest is shown.
    fn draw_panel(&mut self, s: &mut Screen, r: Rect) -> Result<()> {
        let State::Ready(v) = &mut self.state else { return Ok(()) };
        let volume = v.vol.current_volume();
        let note = |e: gnome_nifti_preview::error::PreviewError| vec![(text::clean(&e.message), Style::default().dim())];
        let fresh = self.sections.as_ref().is_some_and(|(g, k, _, _)| *g == self.generation && *k == volume);
        if !fresh {
            let warnings = v.warnings().unwrap_or_default();
            let lines = v.sections(self.o.patient).map(|x| draw::section_lines(&x)).unwrap_or_else(note);
            self.sections = Some((self.generation, volume, warnings, lines));
        }
        // Labels: counted once per volume and display (not per cursor move).
        let stale = self.labels.as_ref().is_none_or(|(g, k, _)| *g != self.generation || *k != volume);
        if stale {
            match view::label_rows(&mut v.vol) {
                Ok(list) => self.labels = Some((self.generation, volume, list)),
                Err(e) => {
                    self.labels = None;
                    self.say(text::clean(&e.message));
                    return Ok(());
                }
            }
        }
        let State::Ready(v) = &mut self.state else { return Ok(()) };
        let mut y = 0;
        let w = r.w.saturating_sub(1);
        // The warnings first, as in the GNOME viewer, cut to the panel's width.
        let warnings = self.sections.as_ref().map(|(_, _, x, _)| draw::warning_lines(x, w)).unwrap_or_default();
        for (l, st) in warnings.iter().chain(self.sections.iter().flat_map(|(_, _, _, lines)| lines)) {
            if y >= r.h {
                break;
            }
            s.put(r.x + 1, r.y + y, &text::fit(l, w), *st, w);
            y += 1;
        }
        // A 4D image: the values of the voxel under the cursor over time,
        // read again while volumes are still being decompressed.
        if v.vol.browsable() && y + 3 < r.h {
            let voxel = v.grid().voxel_at(v.cursor);
            let fresh = self.timecourse.as_ref().is_some_and(|(c, k, t)| Some(*c) == voxel && *k == volume && t.complete);
            if !fresh {
                if let Some(p) = voxel {
                    self.timecourse = Some((p, volume, match v.vol.timecourse(p) {
                        Ok((values, complete)) => Timecourse { values, complete, error: None },
                        Err(e) => Timecourse { values: Vec::new(), complete: true, error: Some(text::clean(&e.message)) },
                    }));
                }
            }
            if let Some((_, _, t)) = &self.timecourse {
                s.put(r.x + 1, r.y + y, "Time course", Style::default().bold(), w);
                if let Some(e) = &t.error {
                    s.put(r.x + 1, r.y + y + 1, &text::fit(e, w), Style::default().dim(), w);
                } else {
                    let line = sparkline(&t.values, w, volume);
                    s.put(r.x + 1, r.y + y + 1, &line.0, Style::default(), w);
                    s.put(r.x + 1 + line.1, r.y + y + 2, "^", Style::default().bold(), 1);
                }
                y += 3;
            }
        }
        if let Some((_, _, list)) = &self.labels {
            if !list.rows.is_empty() && y + 2 < r.h {
                s.put(r.x + 1, r.y + y, "Labels", Style::default().bold(), w);
                y += 1;
                let here = self.readout.as_ref().and_then(|x| x.value).map(|v| v.label_id());
                let room = r.h - y;
                let at = here.and_then(|h| list.rows.iter().position(|row| row.value.label_id() == h)).unwrap_or(0);
                let first = at.saturating_sub(room / 2).min(list.rows.len().saturating_sub(room));
                for row in list.rows.iter().skip(first).take(room) {
                    let highlight = Some(row.value.label_id()) == here;
                    draw::label_line(s, r.x + 1, r.y + y, w, row, self.caps.depth, highlight);
                    y += 1;
                }
            }
        }
        Ok(())
    }

    fn draw_status(&mut self, s: &mut Screen) -> Result<()> {
        let (cols, rows) = (s.w, s.h);
        if rows < STATUS_ROWS {
            return Ok(());
        }
        let State::Ready(v) = &mut self.state else { return Ok(()) };
        let (cmap, (lo, hi, src)) = v.shown()?;
        let r = self.readout.clone();
        let mut first = String::new();
        if let Some(r) = &r {
            first = panel::readout_text(r, cmap, true, false);
            if let Some(p) = r.voxel {
                first += &format!("   voxel {} {} {}", p[0], p[1], p[2]);
            }
            if v.vol.orientation_known && r.voxel.is_some() {
                let w: Vec<String> = r.world_mm.iter().map(|c| if c.is_finite() { panel::to_fixed(*c, 1) } else { "?".into() }).collect();
                first += &format!("   {} mm", w.join(", "));
            }
        }
        if let Some((m, _)) = &self.message {
            first = m.clone();
        }
        s.put(0, rows - 2, &text::fit(&text::clean(&first), cols), Style::default().bold(), cols);
        let mut parts = vec![v.name(), v.effective_mode().to_string()];
        parts.push(if cmap == "gray" && v.vol.palette != "gray" { format!("gray/{}", v.vol.palette) } else { cmap.to_string() });
        if cmap != "rgb" && cmap != "labels" {
            let how = match src {
                "manual" => " (set)",
                "file" => " (file)",
                _ => "",
            };
            parts.push(format!("window {} – {}{how}", panel::format_number(lo, 4), panel::format_number(hi, 4)));
        }
        if v.vol.browsable() {
            let progress = self.volume_progress.filter(|_| self.volume_target != v.vol.current_volume());
            if let Some(h) = &self.header {
                parts.push(panel::volume_text(h, self.volume_target, v.vol.current_volume(), progress));
            }
        }
        parts.push(format!("{} {}", self.caps.graphics.name(), self.caps.depth.name()));
        parts.push("? help".into());
        s.put(0, rows - 1, &text::fit(&parts.join(" · "), cols), Style::default().dim(), cols);
        Ok(())
    }
}

/// Move the cursor in the plane of view `plan`, by image pixels.
fn move_cursor(v: &mut Viewer, plan: usize, dx: i64, dy: i64) {
    let p = &v.plans[plan];
    let (col, row) = p.screen_point(v.cursor);
    let col = (col as i64 + dx).clamp(0, p.width as i64 - 1) as usize;
    let row = (row as i64 + dy).clamp(0, p.height as i64 - 1) as usize;
    v.cursor = p.grid_point(v.index(plan), col, row);
}

/// Move the slice of view `plan` by `steps` (positive: towards the letter
/// of `slice_up_label`); a step is one voxel of a resampled grid.
fn scroll(v: &mut Viewer, plan: usize, steps: i64) {
    let p = &v.plans[plan];
    let axis = p.slice_axis;
    let next = v.cursor[axis] as i64 + steps * p.slice_step as i64 * p.slice_up as i64;
    v.cursor[axis] = next.clamp(0, p.n_slices as i64 - 1) as usize;
}

fn centre_lines(s: &mut Screen, lines: &[String]) {
    let top = s.h.saturating_sub(lines.len()) / 2;
    for (k, l) in lines.iter().enumerate() {
        let w = text::str_width(l).min(s.w);
        s.put((s.w - w) / 2, top + k, &text::fit(l, s.w), Style::default(), s.w);
    }
}

/// A line of `▁▂▃▄▅▆▇█` for the values (gaps for missing ones) at most
/// `width` wide, and the column of volume `current`.
fn sparkline(values: &[Option<f64>], width: usize, current: usize) -> (String, usize) {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let n = values.len().max(1);
    let w = width.min(n).max(1);
    let finite = values.iter().flatten().copied().filter(|x| x.is_finite());
    let (lo, hi) = finite.fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| (a.min(x), b.max(x)));
    let line: String = (0..w)
        .map(|i| {
            let k = i * n / w;
            match values.get(k).copied().flatten() {
                Some(x) if hi > lo => BARS[(((x - lo) / (hi - lo)) * 7.0).round().clamp(0.0, 7.0) as usize],
                Some(_) => BARS[3],
                None => ' ',
            }
        })
        .collect();
    (line, (current * w / n).min(w - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparklines() {
        let v: Vec<Option<f64>> = (0..8).map(|x| Some(x as f64)).collect();
        assert_eq!(sparkline(&v, 20, 3), ("▁▂▃▄▅▆▇█".to_string(), 3));
        let (l, at) = sparkline(&v, 4, 7);
        assert_eq!((l.chars().count(), at), (4, 3));
        assert_eq!(sparkline(&[None, Some(1.0), Some(1.0)], 10, 0).0, " ▄▄");
    }
}
