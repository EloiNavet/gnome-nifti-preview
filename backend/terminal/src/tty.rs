//! The terminal device, written directly over the C library (`libc`)
//! without a TUI crate: its size, raw mode, the alternate screen, input and
//! signals.
//!
//! The controlling terminal (`/dev/tty`) is opened once, before the sandbox
//! is applied: Landlock decides at open time, so the descriptor keeps its
//! ioctls (size, raw mode) afterwards. Signals reach the main loop through a
//! pipe (not a socket pair, which the sandbox's seccomp filter refuses).
//! The terminal is put back as it was however the program ends: on return,
//! on a panic, on SIGTERM, SIGHUP or SIGINT; Ctrl-Z restores it before the
//! process stops and takes it again when it continues.

use std::io;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Size of the terminal on `fd`: columns, rows, and the width and height of
/// the text area in pixels (0 when the terminal does not tell).
pub fn winsize(fd: i32) -> Option<(usize, usize, usize, usize)> {
    // SAFETY: TIOCGWINSZ writes one struct winsize, which `ws` is.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0;
    (ok && ws.ws_col > 0 && ws.ws_row > 0).then_some((ws.ws_col as usize, ws.ws_row as usize, ws.ws_xpixel as usize, ws.ws_ypixel as usize))
}

/// Whether `fd` is a terminal.
pub fn is_tty(fd: i32) -> bool {
    // SAFETY: isatty only reads its integer argument.
    unsafe { libc::isatty(fd) == 1 }
}

/// The cell size in pixels the kernel reports, when the terminal sets it.
pub fn cell_px(fd: i32) -> Option<(u32, u32)> {
    let (c, r, x, y) = winsize(fd)?;
    (x >= c && y >= r && x > 0 && y > 0).then(|| ((x / c) as u32, (y / r) as u32))
}

/// Write end of the signal pipe (-1 before `Tty::open`).
static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

/// What the panic hook puts back: the terminal, its settings, whether the
/// alternate screen is on, and the thread that drives the terminal.
struct Restore {
    fd: i32,
    termios: libc::termios,
    screen: bool,
    owner: std::thread::ThreadId,
}

static RESTORE: Mutex<Option<Restore>> = Mutex::new(None);

/// Panic messages of other threads while the terminal is taken (the library
/// catches those panics and reports an error): printed once it is given back.
static HELD_PANICS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// What `leave` writes: mouse off, wrapping and cursor back, main screen.
const LEAVE: &[u8] = b"\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2026l\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l";

#[cfg(any(target_os = "linux", target_os = "android"))]
fn errno() -> *mut libc::c_int {
    // SAFETY: the thread's errno.
    unsafe { libc::__errno_location() }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn errno() -> *mut libc::c_int {
    // SAFETY: the thread's errno.
    unsafe { libc::__error() }
}

extern "C" fn on_signal(sig: libc::c_int) {
    let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        let b = sig as u8;
        // SAFETY: write(2) is async-signal-safe; errno is saved and restored.
        unsafe {
            let saved = *errno();
            libc::write(fd, &b as *const u8 as *const libc::c_void, 1);
            *errno() = saved;
        }
    }
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

/// What `Tty::wait` saw.
#[derive(Debug, PartialEq, Eq)]
pub enum Wait {
    Input(Vec<u8>),
    Signals(Vec<i32>),
    Timeout,
}

/// The signals caught while a `Tty` is open (SIGTSTP from outside: the
/// terminal is given back before stopping, as for Ctrl-Z).
const CAUGHT: [libc::c_int; 7] = [libc::SIGWINCH, libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGCONT, libc::SIGTSTP];

/// Largest screen drawn (a terminal claiming more is drawn this size).
pub const MAX_COLS: usize = 4096;
pub const MAX_ROWS: usize = 2048;

pub struct Tty {
    fd: i32,
    saved: libc::termios,
    raw: bool,
    screen: bool,
    signals_r: i32,
    /// The handlers before ours, put back when closed.
    previous: Vec<(libc::c_int, libc::sigaction)>,
    /// Input read buffer, kept between reads.
    buf: Vec<u8>,
}

impl Tty {
    /// Open the controlling terminal and catch the signals the viewer
    /// handles (SIGWINCH, SIGTERM, SIGHUP, SIGINT, SIGCONT).
    pub fn open() -> io::Result<Tty> {
        // SAFETY: a NUL-terminated path and flags; the descriptor is ours.
        let fd = unsafe { libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(last_error());
        }
        // SAFETY: tcgetattr fills one termios.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            let e = last_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }
        let mut p = [0i32; 2];
        // SAFETY: pipe2 writes two descriptors into `p`.
        if unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            let e = last_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }
        SIGNAL_PIPE.store(p[1], Ordering::Relaxed);
        let mut previous = Vec::new();
        for sig in CAUGHT {
            // SAFETY: a handler that only writes a byte to a pipe; the
            // previous action is kept to be put back.
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
                sa.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut sa.sa_mask);
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, &sa, &mut old) == 0 {
                    previous.push((sig, old));
                }
            }
        }
        Ok(Tty { fd, saved, raw: false, screen: false, signals_r: p[0], previous, buf: vec![0; 1 << 16] })
    }

    pub fn fd(&self) -> i32 {
        self.fd
    }

    /// Columns, rows, and pixels of the text area (0 when unknown), the
    /// cells bounded by `MAX_COLS` × `MAX_ROWS`.
    pub fn size(&self) -> (usize, usize, usize, usize) {
        let (c, r, x, y) = winsize(self.fd).unwrap_or((80, 24, 0, 0));
        (c.min(MAX_COLS), r.min(MAX_ROWS), x, y)
    }

    /// Drop what the terminal sent and was not read yet (answers to the
    /// queries that came too late), so that the shell does not read them.
    pub fn flush_input(&self) {
        // SAFETY: discards the input queue of our own terminal descriptor.
        unsafe { libc::tcflush(self.fd, libc::TCIFLUSH) };
    }

    fn arm_restore(&self) {
        *RESTORE.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(Restore { fd: self.fd, termios: self.saved, screen: self.screen, owner: std::thread::current().id() });
    }

    /// Raw mode: keys arrive as typed, nothing is echoed, Ctrl-C and Ctrl-Z
    /// are bytes like any other.
    pub fn raw(&mut self) -> io::Result<()> {
        let mut t = self.saved;
        // SAFETY: cfmakeraw edits the termios given; tcsetattr reads it.
        unsafe { libc::cfmakeraw(&mut t) };
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &t) } != 0 {
            return Err(last_error());
        }
        self.raw = true;
        self.arm_restore();
        install_panic_hook();
        Ok(())
    }

    /// The settings the terminal had.
    pub fn cooked(&mut self) {
        if self.raw {
            // SAFETY: puts back the termios read at open.
            unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
            self.raw = false;
        }
    }

    /// The alternate screen, cursor hidden, no wrapping at the right edge,
    /// and the mouse reported (SGR) when `mouse`.
    pub fn enter(&mut self, mouse: bool) -> io::Result<()> {
        let mut s = b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[H\x1b[2J".to_vec();
        if mouse {
            s.extend_from_slice(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h");
        }
        self.screen = true;
        self.arm_restore();
        self.write(&s)
    }

    /// Back to the main screen as it was.
    pub fn leave(&mut self) {
        if self.screen {
            let _ = self.write(LEAVE);
            self.screen = false;
            if self.raw {
                self.arm_restore();
            }
        }
    }

    pub fn write(&self, mut b: &[u8]) -> io::Result<()> {
        while !b.is_empty() {
            // SAFETY: writes from a live slice.
            let n = unsafe { libc::write(self.fd, b.as_ptr() as *const libc::c_void, b.len()) };
            if n < 0 {
                let e = last_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            b = &b[n as usize..];
        }
        Ok(())
    }

    /// Wait for input or a signal, at most `timeout` (None: no limit).
    pub fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Wait> {
        let mut fds = [
            libc::pollfd { fd: self.signals_r, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 },
        ];
        let ms = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as i32);
        // SAFETY: two pollfd structures.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
        if n < 0 {
            let e = last_error();
            return if e.kind() == io::ErrorKind::Interrupted { Ok(Wait::Signals(Vec::new())) } else { Err(e) };
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 64];
            // SAFETY: reads into a local buffer from our non-blocking pipe.
            let n = unsafe { libc::read(self.signals_r, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n > 0 {
                return Ok(Wait::Signals(buf[..n as usize].iter().map(|&s| s as i32).collect()));
            }
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            // SAFETY: reads into our buffer, at most its length.
            let n = unsafe { libc::read(self.fd, self.buf.as_mut_ptr() as *mut libc::c_void, self.buf.len()) };
            if n < 0 {
                let e = last_error();
                return if e.kind() == io::ErrorKind::Interrupted { Ok(Wait::Signals(Vec::new())) } else { Err(e) };
            }
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the terminal closed"));
            }
            return Ok(Wait::Input(self.buf[..n as usize].to_vec()));
        }
        Ok(Wait::Timeout)
    }

    /// Ctrl-Z: give the terminal back, stop, and take it again once
    /// continued (the caller redraws everything).
    pub fn suspend(&mut self, mouse: bool) -> io::Result<()> {
        let screen = self.screen;
        self.leave();
        self.cooked();
        // SAFETY: stops this process with SIGTSTP's default action, then
        // catches SIGTSTP again once continued.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut sa.sa_mask);
            let mut ours: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGTSTP, &sa, &mut ours);
            libc::raise(libc::SIGTSTP);
            libc::sigaction(libc::SIGTSTP, &ours, std::ptr::null_mut());
        }
        self.raw()?;
        if screen {
            self.enter(mouse)?;
        }
        Ok(())
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        self.leave();
        self.cooked();
        *RESTORE.lock().unwrap_or_else(|p| p.into_inner()) = None;
        for message in std::mem::take(&mut *HELD_PANICS.lock().unwrap_or_else(|p| p.into_inner())) {
            eprintln!("{message}");
        }
        for (sig, old) in &self.previous {
            // SAFETY: puts back the action read when the terminal was opened.
            unsafe { libc::sigaction(*sig, old, std::ptr::null_mut()) };
        }
        let w = SIGNAL_PIPE.swap(-1, Ordering::Relaxed);
        // SAFETY: our own descriptors, closed once.
        unsafe {
            if w >= 0 {
                libc::close(w);
            }
            libc::close(self.signals_r);
            libc::close(self.fd);
        }
    }
}

/// On a panic of the thread that drives the terminal, the terminal is put
/// back before the message is printed. A panic of another thread (the
/// library catches those on its readers and decoders, and reports an error)
/// leaves the terminal alone: its message is kept and printed when the
/// terminal is given back.
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if on_panic(|| info.to_string()) {
                previous(info);
            }
        }));
    });
}

/// What the hook does before the usual message; false when that message
/// must wait (another thread's panic while the terminal is taken).
fn on_panic(message: impl Fn() -> String) -> bool {
    let mut restore = RESTORE.lock().unwrap_or_else(|p| p.into_inner());
    match restore.as_ref() {
        Some(r) if r.owner != std::thread::current().id() => {
            let name = std::thread::current().name().unwrap_or("<unnamed>").to_string();
            HELD_PANICS.lock().unwrap_or_else(|p| p.into_inner()).push(format!("thread '{name}' {}", message()));
            false
        }
        Some(_) => {
            let r = restore.take().expect("checked");
            // SAFETY: the terminal descriptor and its saved settings.
            unsafe {
                if r.screen {
                    libc::write(r.fd, LEAVE.as_ptr() as *const libc::c_void, LEAVE.len());
                }
                libc::tcsetattr(r.fd, libc::TCSANOW, &r.termios);
            }
            true
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panics_of_other_threads_leave_the_terminal_alone() {
        let mut p = [0i32; 2];
        // SAFETY: a pipe standing for the terminal.
        assert_eq!(unsafe { libc::pipe(p.as_mut_ptr()) }, 0);
        // SAFETY: zeroed settings (tcsetattr on a pipe fails harmlessly).
        let termios: libc::termios = unsafe { std::mem::zeroed() };
        let arm = |owner| *RESTORE.lock().unwrap() = Some(Restore { fd: p[1], termios, screen: true, owner });
        // Another thread's panic: kept for later, nothing written.
        arm(std::thread::current().id());
        let other = std::thread::spawn(|| on_panic(|| "panicked at the decoder".into())).join().unwrap();
        assert!(!other);
        assert!(RESTORE.lock().unwrap().is_some(), "the terminal is still ours");
        assert!(HELD_PANICS.lock().unwrap().iter().any(|m| m.contains("panicked at the decoder")));
        // The driving thread's panic: the terminal is given back first.
        assert!(on_panic(|| "panicked here".into()));
        assert!(RESTORE.lock().unwrap().is_none());
        let mut buf = [0u8; 64];
        // SAFETY: reads what the hook wrote to the pipe.
        let n = unsafe { libc::read(p[0], buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        assert_eq!(&buf[..n as usize], LEAVE);
        HELD_PANICS.lock().unwrap().clear();
        // SAFETY: our pipe, closed once.
        unsafe {
            libc::close(p[0]);
            libc::close(p[1]);
        }
    }
}
