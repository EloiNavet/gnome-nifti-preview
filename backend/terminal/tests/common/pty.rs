//! nitty in a pseudo-terminal: the test plays the terminal (it answers the
//! queries it was told to answer), types keys, resizes, sends signals, and
//! reads the screen back through the strict terminal of `vt`.

#![allow(dead_code)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::vt::Term;

/// What the pretend terminal answers.
#[derive(Debug, Clone, Default)]
pub struct Answers {
    /// DA1, e.g. "?62;22c" (None: no answer at all).
    pub da1: Option<&'static str>,
    pub version: Option<&'static str>,
    pub kitty: bool,
    pub cell: Option<(u32, u32)>,
    /// Text area in pixels (width, height), answer to `CSI 14 t`.
    pub window: Option<(u32, u32)>,
    pub registers: Option<u32>,
}

impl Answers {
    /// An xterm without graphics.
    pub fn xterm() -> Answers {
        Answers { da1: Some("?62;22c"), version: Some("XTerm(390)"), ..Answers::default() }
    }
}

pub struct Pty {
    /// The test's side. The terminal side is open in nitty only, so that
    /// reading here ends (EIO) once nitty and its descriptors are gone.
    master: File,
    pub child: Child,
    out: Receiver<Vec<u8>>,
    /// What nitty wrote to stderr so far (read as it comes, so that a full
    /// pipe never blocks it).
    err: Arc<Mutex<Vec<u8>>>,
    err_reader: Option<JoinHandle<()>>,
    /// The terminal's settings before nitty started.
    pub initial: libc::termios,
    pub term: Term,
    pub answers: Answers,
    pub raw: Vec<u8>,
    pub cols: usize,
    pub rows: usize,
    /// After a resize: output is dropped up to the next whole frame drawn
    /// for the new size (what was drawn for the old one may still be on
    /// its way).
    skip_to_clear: bool,
}

/// Whether a pseudo-terminal can be opened here (some containers have no
/// /dev/ptmx). When not, the tests that need one are skipped, unless
/// NITTY_REQUIRE_PTY is set (CI).
pub fn available() -> bool {
    // SAFETY: opens and closes one descriptor.
    let m = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if m >= 0 {
        unsafe { libc::close(m) };
        return true;
    }
    let e = std::io::Error::last_os_error();
    assert!(std::env::var_os("NITTY_REQUIRE_PTY").is_none(), "NITTY_REQUIRE_PTY is set but no pseudo-terminal opens: {e}");
    eprintln!("no pseudo-terminal here ({e}): skipped");
    false
}

fn check(r: i32, what: &str) {
    assert!(r >= 0, "{what}: {}", std::io::Error::last_os_error());
}

pub fn set_size(fd: i32, cols: usize, rows: usize) {
    let ws = libc::winsize { ws_row: rows as u16, ws_col: cols as u16, ws_xpixel: (cols * 9) as u16, ws_ypixel: (rows * 18) as u16 };
    // SAFETY: TIOCSWINSZ reads one struct winsize.
    check(unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) }, "TIOCSWINSZ");
}

impl Pty {
    pub fn spawn(args: &[&str], env: &[(&str, &str)], cols: usize, rows: usize, answers: Answers) -> Pty {
        // SAFETY: the usual openpty sequence over posix_openpt; descriptors owned by Files.
        let (master, slave_path) = unsafe {
            let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
            check(m, "posix_openpt");
            check(libc::grantpt(m), "grantpt");
            check(libc::unlockpt(m), "unlockpt");
            let mut name = [0 as libc::c_char; 128];
            check(libc::ptsname_r(m, name.as_mut_ptr(), name.len()), "ptsname_r");
            (File::from_raw_fd(m), std::ffi::CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned())
        };
        set_size(master.as_raw_fd(), cols, rows);
        let initial = termios_of(master.as_raw_fd());
        let slave = std::fs::OpenOptions::new().read(true).write(true).open(&slave_path).expect("open the pty");
        let mut c = Command::new(super::BIN);
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        // SAFETY: async-signal-safe calls between fork and exec: a new
        // session whose controlling terminal is the pty (stdin).
        unsafe {
            c.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = c.spawn().expect("nitty starts");
        // Only nitty holds the terminal side now.
        drop(c);
        drop(slave);
        let err = Arc::new(Mutex::new(Vec::new()));
        let err_reader = child.stderr.take().map(|mut e| {
            let err = Arc::clone(&err);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n @ 1..) = e.read(&mut buf) {
                    err.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            })
        });
        let mut reader = master.try_clone().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1 << 16];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EIO once the program is gone
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Pty { master, child, out: rx, err, err_reader, initial, term: Term::new(cols, rows, false).raw(), answers, raw: Vec::new(), cols, rows, skip_to_clear: false }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }

    /// Answer what was asked since the last call.
    fn answer(&mut self) {
        let queries = std::mem::take(&mut self.term.queries);
        let mut reply = Vec::new();
        for q in queries {
            let a = &self.answers;
            match q {
                "kitty" if a.kitty => reply.extend_from_slice(b"\x1b_Gi=31;OK\x1b\\"),
                "version" => {
                    if let Some(v) = a.version {
                        reply.extend_from_slice(format!("\x1bP>|{v}\x1b\\").as_bytes());
                    }
                }
                "cell" => {
                    if let Some((w, h)) = a.cell {
                        reply.extend_from_slice(format!("\x1b[6;{h};{w}t").as_bytes());
                    }
                }
                "window" => {
                    if let Some((w, h)) = a.window {
                        reply.extend_from_slice(format!("\x1b[4;{h};{w}t").as_bytes());
                    }
                }
                "registers" => {
                    if let Some(n) = a.registers {
                        reply.extend_from_slice(format!("\x1b[?1;0;{n}S").as_bytes());
                    }
                }
                "da1" => {
                    if let Some(d) = a.da1 {
                        reply.extend_from_slice(format!("\x1b[{d}").as_bytes());
                    }
                }
                _ => {}
            }
        }
        if !reply.is_empty() {
            self.send(&reply);
        }
    }

    /// Play the output until `pred` holds (true) or `secs` pass (false).
    pub fn until(&mut self, secs: f64, pred: impl Fn(&Term) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        loop {
            if pred(&self.term) {
                return true;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.out.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(bytes) => {
                    self.raw.extend_from_slice(&bytes);
                    if self.skip_to_clear && !self.skip_old_frames() {
                        continue;
                    }
                    // Sequences may be cut between reads: play whole ones only.
                    let cut = whole(&self.raw);
                    let ready: Vec<u8> = self.raw.drain(..cut).collect();
                    self.term.feed(&ready);
                    self.answer();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return pred(&self.term),
            }
        }
    }

    /// Play the output until the screen holds `s`; panic with the screen otherwise.
    pub fn expect(&mut self, s: &str) {
        let s2 = s.to_string();
        if !self.until(10.0, move |t| t.text().contains(&s2)) {
            panic!("{s:?} never shown; the screen:\n{}", self.term.text());
        }
    }

    /// The terminal changed size: the program redraws everything. What it
    /// drew for the old size is dropped, up to a whole frame of the new size
    /// (a redraw, Ctrl-L, is asked too).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        set_size(self.master.as_raw_fd(), cols, rows);
        self.cols = cols;
        self.rows = rows;
        self.term = Term::new(cols, rows, false).raw();
        self.skip_to_clear = true;
        self.send(b"\x0c");
    }

    /// While `skip_to_clear`: drop the output up to the next whole frame
    /// (`?2026h` ... `?2026l` around a full redraw, `2J`) whose cursor
    /// moves all fall inside the new size. False while none has come yet.
    fn skip_old_frames(&mut self) -> bool {
        loop {
            let Some(clear) = find(&self.raw, b"\x1b[2J") else {
                let keep = self.raw.len().saturating_sub(3);
                self.raw.drain(..keep);
                return false;
            };
            let Some(end) = find(&self.raw[clear..], b"\x1b[?2026l").map(|e| clear + e) else {
                self.raw.drain(..clear);
                return false; // the frame is not whole yet
            };
            if fits(&self.raw[clear..end], self.cols, self.rows) {
                self.raw.drain(..clear);
                self.skip_to_clear = false;
                return true;
            }
            self.raw.drain(..end);
        }
    }

    /// The settings of the terminal (to see that they were put back).
    pub fn termios(&self) -> libc::termios {
        termios_of(self.master.as_raw_fd())
    }

    pub fn signal(&self, sig: i32) {
        // SAFETY: a signal to our child.
        unsafe { libc::kill(self.pid() as i32, sig) };
    }

    /// Wait for the program to end (and play what it wrote last).
    pub fn wait(&mut self, secs: u64) -> std::process::ExitStatus {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                self.until(0.3, |_| false);
                return s;
            }
            assert!(Instant::now() < end, "nitty still running after {secs} s; the screen:\n{}", self.term.text());
            self.until(0.05, |_| false);
        }
    }

    /// What nitty wrote to stderr (all of it once it has ended).
    pub fn stderr(&mut self) -> String {
        if self.child.try_wait().ok().flatten().is_some() {
            if let Some(t) = self.err_reader.take() {
                let _ = t.join();
            }
        }
        String::from_utf8_lossy(&self.err.lock().unwrap()).into_owned()
    }

    /// A line of /proc/<pid>/status.
    pub fn status_field(&self, key: &str) -> Option<String> {
        let s = std::fs::read_to_string(format!("/proc/{}/status", self.pid())).ok()?;
        s.lines().find_map(|l| l.strip_prefix(key).map(|v| v.trim_start_matches(':').trim().to_string()))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The settings of the terminal whose test side is `master` (on Linux, the
/// ioctl on a master reads its terminal side).
fn termios_of(master: i32) -> libc::termios {
    // SAFETY: tcgetattr fills one termios.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    check(unsafe { libc::tcgetattr(master, &mut t) }, "tcgetattr");
    t
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Every cursor move (`CSI row;col H`) of `frame` falls inside cols × rows.
fn fits(frame: &[u8], cols: usize, rows: usize) -> bool {
    let mut i = 0;
    while let Some(at) = find(&frame[i..], b"\x1b[") {
        let start = i + at + 2;
        let Some(len) = frame[start..].iter().position(|b| (0x40..=0x7e).contains(b)) else { break };
        if frame[start + len] == b'H' {
            let params = std::str::from_utf8(&frame[start..start + len]).unwrap_or("");
            let mut n = params.split(';').map(|x| x.parse::<usize>().unwrap_or(1).max(1));
            let (r, c) = (n.next().unwrap_or(1), n.next().unwrap_or(1));
            if r > rows || c > cols {
                return false;
            }
        }
        i = start + len + 1;
    }
    true
}

/// Bytes of `b` that end where no sequence or character is cut.
fn whole(b: &[u8]) -> usize {
    let mut i = 0;
    let mut safe = 0;
    while i < b.len() {
        if b[i] == 0x1b {
            let Some(&next) = b.get(i + 1) else { break };
            let end = match next {
                b'[' => (i + 2..b.len()).find(|&j| (0x40..=0x7e).contains(&b[j])).map(|j| j + 1),
                b'_' | b'P' | b']' => {
                    let tmux = next == b'P' && b[i + 2..].starts_with(b"tmux;");
                    let mut j = i + 2;
                    let mut found = None;
                    while j + 1 < b.len() {
                        if tmux && b[j] == 0x1b && b[j + 1] == 0x1b {
                            j += 2;
                            continue;
                        }
                        if b[j] == 0x1b && b[j + 1] == b'\\' {
                            found = Some(j + 2);
                            break;
                        }
                        if next == b']' && b[j] == 0x07 {
                            found = Some(j + 1);
                            break;
                        }
                        j += 1;
                    }
                    if found.is_none() && next == b']' && b.last() == Some(&0x07) {
                        found = Some(b.len());
                    }
                    found
                }
                _ => Some(i + 2),
            };
            match end {
                Some(e) => {
                    i = e;
                    safe = i;
                }
                None => break,
            }
        } else {
            let n = match b[i] {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => 1,
            };
            if i + n > b.len() {
                break;
            }
            i += n;
            safe = i;
        }
    }
    safe
}
