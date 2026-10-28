//! `nitty`: medical images in the terminal (see `nitty --help`).

use std::io::Write;
use std::process::ExitCode;

use gnome_nifti_preview::error::PreviewError;
use gnome_nifti_preview::{sandbox, volume};

use nitty::args::{self, Command, Options};
use nitty::caps::{self, Caps, Env, Probed, Replies};
use nitty::draw;
use nitty::screen::Screen;
use nitty::app::App;
use nitty::{glance, tty, viewer};

/// Exit codes, as nifti-preview's: an image that cannot be shown, a usage error.
const EXIT_IMAGE: u8 = 2;
const EXIT_USAGE: u8 = 64;

/// A signal asking to stop that arrived while the terminal was asked: the
/// usual exit status of a process it ends (128 + its number).
fn stopped_by(p: &Probed) -> Option<ExitCode> {
    p.signals
        .iter()
        .find(|&&s| [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT].contains(&s))
        .map(|&s| ExitCode::from(128 + s as u8))
}

fn report(file: &std::path::Path, e: &PreviewError, depth: nitty::color::Depth) {
    let name = nitty::text::clean(&file.display().to_string());
    eprintln!("nitty: {name}: error [{}]: {}", e.code, nitty::text::clean(&e.message));
    // What could be read of a file that has no image (a DICOM report, a surface).
    let about = viewer::about_of(e);
    if !about.is_empty() {
        let lines = draw::section_lines(&about);
        let mut s = Screen::new(100, lines.len());
        for (y, (l, st)) in lines.iter().enumerate() {
            s.put(0, y, l, *st, 100);
        }
        let mut out = Vec::new();
        s.lines(depth, &mut out);
        let _ = std::io::stdout().write_all(&out);
    }
}

/// Whether the terminal must be asked: something is left to choose, or an
/// image protocol needs the size of a cell in pixels.
fn needs_probe(o: &Options) -> bool {
    o.graphics.is_none() || o.depth.is_none() || o.graphics.is_some_and(|g| g.is_image())
}

/// The cell size the kernel reports, else the text area the terminal
/// answered divided by the cells.
fn cell_hint(fd: i32, r: &Replies) -> Option<(u32, u32)> {
    tty::cell_px(fd).or_else(|| {
        let (cols, rows, _, _) = tty::winsize(fd)?;
        let (w, h) = r.window_px?;
        Some((w / cols as u32, h / rows as u32))
    })
}

fn quick_look(o: &Options, env: &Env) -> ExitCode {
    let stdout_tty = tty::is_tty(1);
    let size = if stdout_tty { tty::winsize(1) } else { None };
    sandbox::close_inherited_descriptors();
    // On a terminal, ask it what it can show (in raw mode, so that the
    // answers are not echoed), then give it back as it was.
    // (Keys typed meanwhile cannot be given back to the shell: they are lost.)
    let mut replies = Replies::default();
    if stdout_tty && !o.panel_json && needs_probe(o) {
        if let Ok(mut t) = tty::Tty::open() {
            if t.raw().is_ok() {
                let p = caps::probe(&mut t, env, caps::probe_timeout());
                if let Some(code) = stopped_by(&p) {
                    return code;
                }
                replies = p.replies;
            }
        }
    }
    let caps: Caps = caps::decide(env, &replies, o.graphics, o.depth, cell_hint(1, &replies));
    let cols = o.width.or(size.map(|s| s.0)).or_else(|| std::env::var("COLUMNS").ok()?.parse().ok()).unwrap_or(80).clamp(1, 10_000);
    let rows = size.map(|s| s.1);
    let report_sandbox = sandbox::apply(sandbox::data_limit(o.max_bytes, 0));
    if std::env::var_os("NIFTI_PREVIEW_DEBUG").is_some() {
        eprintln!("nitty: sandbox {report_sandbox:?}, terminal {replies:?}, caps {caps:?}");
    }
    let mut status = ExitCode::SUCCESS;
    let mut stdout = std::io::stdout().lock();
    for file in &o.files {
        let mut out = Vec::new();
        let result = glance::run(o, file, &caps, cols, rows, &mut out);
        if let Err(e) = stdout.write_all(&out).and_then(|_| stdout.flush()) {
            // A closed pipe (`nitty -p x | head`) ends the output quietly.
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                return status;
            }
            eprintln!("nitty: {e}");
            return ExitCode::from(1);
        }
        if let Err(e) = result {
            report(file, &e, caps.depth);
            status = ExitCode::from(EXIT_IMAGE);
        }
    }
    status
}

/// The interactive viewer, on the controlling terminal.
fn interactive(o: &Options, env: &Env) -> ExitCode {
    sandbox::close_inherited_descriptors();
    let mut t = match tty::Tty::open() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nitty: no terminal to show the image on ({e}); try -p");
            return ExitCode::from(1);
        }
    };
    // Ask the terminal what it can show, in raw mode (the answers are not echoed).
    let (replies, early) = if !needs_probe(o) {
        (Replies::default(), Vec::new())
    } else {
        if let Err(e) = t.raw() {
            eprintln!("nitty: {e}");
            return ExitCode::from(1);
        }
        let p = caps::probe(&mut t, env, caps::probe_timeout());
        if let Some(code) = stopped_by(&p) {
            return code;
        }
        (p.replies, p.events)
    };
    let caps = caps::decide(env, &replies, o.graphics, o.depth, cell_hint(t.fd(), &replies));
    let report = sandbox::apply(sandbox::data_limit(o.max_bytes, viewer::CACHE_BYTES as u64));
    let result = App::new(o, t, caps).run(early);
    if std::env::var_os("NIFTI_PREVIEW_DEBUG").is_some() {
        eprintln!("nitty: sandbox {report:?}, terminal {replies:?}, caps {caps:?}");
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nitty: {e}");
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut o = match args::parse(&raw, |k| std::env::var(k).ok()) {
        Ok(Command::Run(o)) => *o,
        // (A closed pipe, `nitty --help | head -1`, is not an error.)
        Ok(Command::Help) => {
            let _ = writeln!(std::io::stdout(), "{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Command::Version) => {
            let _ = writeln!(std::io::stdout(), "nitty {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            // An argument is a file name, which may hold anything.
            eprintln!("nitty: {}\n\n{}", nitty::text::clean(&e), args::USAGE);
            return ExitCode::from(EXIT_USAGE);
        }
    };
    // Read (and warned about) once, before the terminal is taken.
    o.max_bytes = volume::max_bytes_from_env();
    let env = Env::from_process();
    if env.dumb() && o.interactive {
        eprintln!("nitty: TERM=dumb: this terminal cannot show the viewer; use -p (or set TERM)");
        return ExitCode::from(EXIT_USAGE);
    }
    let wanted = !o.print && !o.panel_json && !env.dumb() && (o.interactive || (tty::is_tty(0) && tty::is_tty(1)));
    if wanted || (o.interactive && !o.panel_json) {
        return interactive(&o, &env);
    }
    quick_look(&o, &env)
}
