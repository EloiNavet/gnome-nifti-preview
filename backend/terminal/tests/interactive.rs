//! The interactive viewer in a pseudo-terminal: what it asks the terminal,
//! what it draws, how keys, the mouse, resizing and signals act, and that
//! the terminal is always given back as it was.

mod common;

use common::pty::{self, Answers, Pty};
use common::vt::{Colour, Term};
use common::{head, tmpdir, within, Nifti};

fn start(file: &std::path::Path, cols: usize, rows: usize, answers: Answers, args: &[&str]) -> Pty {
    let mut a: Vec<&str> = args.to_vec();
    a.push(file.to_str().unwrap());
    Pty::spawn(&a, &[], cols, rows, answers)
}

/// The bottom line (renderer, help) once the image is shown.
fn shown(p: &mut Pty) {
    p.expect("? help");
}

/// The voxel the status line shows.
fn voxel_shown(t: &Term) -> Option<[usize; 3]> {
    let text = t.text();
    let rest = &text[text.find("voxel ")? + 6..];
    let n: Vec<usize> = rest.split_whitespace().take(3).map(|x| x.parse().ok()).collect::<Option<_>>()?;
    n.try_into().ok()
}

/// The value the status line shows (what comes before the voxel).
fn value_shown(t: &Term) -> String {
    let text = t.text();
    let line = text.lines().find(|l| l.contains("voxel ")).unwrap_or("");
    line[..line.find("voxel").unwrap_or(0)].trim().to_string()
}

/// The cells with a colour behind them (the image, in half blocks):
/// first and last column, first and last row.
fn image_box(t: &Term) -> (usize, usize, usize, usize) {
    let mut b = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..t.h {
        for x in 0..t.w {
            if matches!(t.cell(x, y).bg, Colour::Rgb(_)) {
                b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
    }
    assert!(b.0 <= b.2 && b.1 <= b.3, "no image on\n{}", t.text());
    b
}

fn brightness(c: Colour) -> u32 {
    match c {
        Colour::Rgb([r, g, b]) => r as u32 + g as u32 + b as u32,
        _ => 0,
    }
}

/// An SGR mouse click (press and release) on cell (x, y), from 0.
fn click(p: &mut Pty, x: usize, y: usize) {
    p.send(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

#[test]
fn draws_moves_and_gives_the_terminal_back() {
    if !pty::available() {
        return;
    }
    within(60, "interactive session", || {
        let dir = tmpdir("tui-basic");
        let f = head(&dir, "head.nii.gz");
        let mut p = start(&f, 140, 44, Answers::xterm(), &["--colormap", "gray"]);
        let before = p.initial;
        shown(&mut p);
        let t = &p.term;
        assert_eq!(t.modes.get("1049"), Some(&true), "alternate screen");
        assert_eq!(t.modes.get("25"), Some(&false), "cursor hidden");
        assert!(t.text().contains("Coronal 24/47") && t.text().contains("Sagittal 20/39") && t.text().contains("Axial 18/35"), "{}", t.text());
        assert!(t.text().contains("voxel 20 24 18"), "{}", t.text());
        assert!(t.text().contains("blocks truecolor"), "{}", t.text());
        // The terminal is raw while the viewer runs.
        assert_eq!(p.termios().c_lflag & libc::ICANON, 0);

        // Right in the axial view (active): +x. Page Up: the next slice up.
        p.send(b"\x1b[C");
        p.expect("voxel 21 24 18");
        p.send(b"\x1b[5~");
        p.expect("Axial 19/35");
        p.send(b"\x1b[1;2D"); // Shift+Left: 10 voxels
        p.expect("voxel 11 24 19");
        // The active view changes with V; Down in the coronal view goes inferior.
        p.send(b"v");
        p.send(b"\x1b[B");
        p.expect("voxel 11 24 18");
        // O: radiological, then the stored axes.
        p.send(b"o");
        p.expect("radiological");
        p.send(b"o");
        p.expect("· stored ·");
        p.send(b"o");
        p.expect("neurological");
        // C: back to the centre.
        p.send(b"c");
        p.expect("voxel 20 24 18");
        // The panel, the help, and back.
        p.send(b"\t");
        p.expect("Dimensions");
        p.send(b"?");
        p.expect("any key returns to the image");
        // Ctrl-L redraws, the help stays.
        p.send(b"\x0c");
        p.until(0.5, |_| false);
        assert!(p.term.text().contains("any key returns to the image"), "{}", p.term.text());
        p.send(b" ");
        p.expect("? help");
        p.send(b"q");
        assert!(p.wait(10).success());
        let after = p.termios();
        assert_eq!((after.c_lflag, after.c_iflag, after.c_oflag), (before.c_lflag, before.c_iflag, before.c_oflag), "settings put back");
        let t = &p.term;
        assert_eq!(t.modes.get("1049"), Some(&false), "main screen again");
        assert_eq!(t.modes.get("25"), Some(&true), "cursor shown again");
        assert_eq!(t.modes.get("1000"), Some(&false), "mouse given back");
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn mouse_resize_tiny_terminals_and_signals() {
    if !pty::available() {
        return;
    }
    within(60, "interactive resize", || {
        let dir = tmpdir("tui-resize");
        let f = head(&dir, "head.nii.gz");
        let mut p = start(&f, 120, 40, Answers::xterm(), &["--colormap", "gray", "--view", "axial"]);
        shown(&mut p);
        // A click places the cursor on the voxel under it. Neurological, RAS
        // stored: the first column of the image is the left (i = 0), its top
        // row the front (j = 47), its last cell the right back corner.
        let (x0, y0, x1, y1) = image_box(&p.term);
        click(&mut p, x0, y0);
        assert!(p.until(5.0, |t| voxel_shown(t) == Some([0, 47, 18])), "{:?}\n{}", voxel_shown(&p.term), p.term.text());
        click(&mut p, x1, y1);
        assert!(p.until(5.0, |t| voxel_shown(t) == Some([39, 0, 18])), "{:?}\n{}", voxel_shown(&p.term), p.term.text());
        // The brightest cells are the bright block (400), right hemisphere.
        let t = &p.term;
        let cells: Vec<(usize, usize)> = (y0..=y1).flat_map(|y| (x0..=x1).map(move |x| (x, y))).collect();
        let light = |&(x, y): &(usize, usize)| brightness(t.pixel(x, 2 * y)).min(brightness(t.pixel(x, 2 * y + 1)));
        let top = cells.iter().map(light).max().unwrap();
        let bright: Vec<_> = cells.iter().filter(|c| light(c) == top).copied().collect();
        let (bx, by) = bright[bright.len() / 2];
        click(&mut p, bx, by);
        assert!(p.until(5.0, |t| value_shown(t) == "400"), "{:?} at {bx},{by}\n{}", value_shown(&p.term), p.term.text());
        let v = voxel_shown(&p.term).unwrap();
        assert!((28..34).contains(&v[0]) && (18..30).contains(&v[1]) && v[2] == 18, "{v:?}");
        // A terminal that ignores mode 1006 reports in the X10 form: a click,
        // not keys (column 81 is the byte 'q', which would quit).
        p.send(&[0x1b, b'[', b'M', 32, 32 + x0 as u8 + 1, 32 + y0 as u8 + 1, 0x1b, b'[', b'M', 35, 32 + x0 as u8 + 1, 32 + y0 as u8 + 1]);
        assert!(p.until(5.0, |t| voxel_shown(t) == Some([0, 47, 18])), "{}", p.term.text());
        p.send(&[0x1b, b'[', b'M', 32, b'q', 33, 0x1b, b'[', b'M', 35, b'q', 33]);
        p.until(0.5, |_| false);
        assert!(p.child.try_wait().unwrap().is_none(), "an X10 report read as q");
        // The wheel changes slices.
        p.send(format!("\x1b[<64;{};{}M", x0 + 5, y0 + 4).as_bytes());
        p.expect("Axial 19/35");
        // Smaller, tiny, bigger: no crash, the layout follows.
        // (The kernel signals the program when the size changes.)
        p.resize(60, 20);
        p.expect("Axial 19/35");
        p.resize(12, 5);
        p.expect("oo small");
        p.resize(200, 60);
        p.expect("Axial 19/35");
        // The sandbox holds while it runs. (Where seccomp filters are not
        // available, only NITTY_REQUIRE_SANDBOX, set in CI, makes it a failure.)
        assert_eq!(p.status_field("NoNewPrivs").as_deref(), Some("1"));
        let seccomp = p.status_field("Seccomp");
        if std::env::var_os("NITTY_REQUIRE_SANDBOX").is_some() || seccomp.as_deref() == Some("2") {
            assert_eq!(seccomp.as_deref(), Some("2"));
        } else {
            eprintln!("no seccomp filter here ({seccomp:?}): not checked");
        }
        // SIGTERM: quits, terminal given back.
        p.signal(libc::SIGTERM);
        assert!(p.wait(10).success());
        assert_eq!(p.term.modes.get("1049"), Some(&false));
        assert!(p.stderr().is_empty(), "{}", p.stderr());
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn image_protocols_are_detected() {
    if !pty::available() {
        return;
    }
    within(60, "detection", || {
        let dir = tmpdir("tui-detect");
        let f = head(&dir, "head.nii.gz");
        // Kitty answers its query: Kitty images, sized in cells.
        let kitty = Answers { kitty: true, version: Some("kitty(0.39.1)"), cell: Some((10, 20)), ..Answers::xterm() };
        let mut p = start(&f, 120, 40, kitty, &[]);
        shown(&mut p);
        assert!(p.term.text().contains("kitty truecolor"), "{}", p.term.text());
        let sent = |t: &common::vt::Term| t.images.iter().filter(|i| i.kind == "kitty" && i.body.starts_with(b"Ga=T")).count();
        assert!(p.until(5.0, |t| sent(t) >= 3));
        // The crosshair is in the margins, in its colour: moving the cursor
        // in the axial plane sends only the sagittal view again.
        let blue = common::vt::Colour::Rgb([0x61, 0xad, 0xff]);
        assert!(p.term.cells.iter().flatten().any(|c| c.bg == blue), "margin marks\n{}", p.term.text());
        p.until(0.5, |_| false);
        p.term.images.clear();
        p.send(b"\x1b[C");
        assert!(p.until(5.0, |t| sent(t) >= 1));
        p.until(0.5, |_| false);
        assert_eq!(sent(&p.term), 1, "one view sent again, not three");
        p.send(b"q");
        assert!(p.wait(10).success());
        // Sixel in DA1.
        let mut p = start(&f, 120, 40, Answers { da1: Some("?62;4;22c"), registers: Some(256), ..Answers::default() }, &[]);
        shown(&mut p);
        assert!(p.term.text().contains("sixel"), "{}", p.term.text());
        assert!(p.until(5.0, |t| t.images.iter().filter(|i| i.kind == "sixel").count() >= 3));
        p.send(b"q");
        assert!(p.wait(10).success());
        // A terminal that answers nothing: text cells, after the wait.
        let t0 = std::time::Instant::now();
        let mut p = start(&f, 120, 40, Answers::default(), &[]);
        shown(&mut p);
        assert!(p.term.text().contains("blocks"), "{}", p.term.text());
        assert!(t0.elapsed().as_secs_f64() < 5.0);
        p.send(b"q");
        assert!(p.wait(10).success());
        // Forced: no query at all.
        let mut p = start(&f, 120, 40, Answers::default(), &["--protocol", "blocks", "--colors", "256"]);
        shown(&mut p);
        assert!(p.term.text().contains("blocks 256"));
        p.send(b"\x03"); // Ctrl-C
        assert!(p.wait(10).success());
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn volumes_errors_and_suspend() {
    if !pty::available() {
        return;
    }
    within(60, "4D and errors", || {
        let dir = tmpdir("tui-4d");
        let mut n = Nifti::new(&[8, 9, 7, 5], 2);
        n.pixdim[4] = 2.0;
        let data: Vec<u8> = (0..8 * 9 * 7 * 5).map(|i| (i / (8 * 9 * 7) * 40 + i % 7) as u8).collect();
        let f = dir.join("bold.nii.gz");
        std::fs::write(&f, common::gz(&n.file(&data))).unwrap();
        let mut p = start(&f, 100, 30, Answers::xterm(), &[]);
        shown(&mut p);
        assert!(p.term.text().contains("Volume 0 / 4"), "{}", p.term.text());
        p.send(b".");
        p.expect("Volume 1 / 4 · 2 s");
        p.send(b"\x1b[F"); // End
        p.expect("Volume 4 / 4");
        // The panel: the time course of the voxel.
        p.send(b"i");
        p.expect("Time course");
        // Ctrl-Z gives the terminal back and takes it again (here, outside
        // job control, the stop is ignored by the kernel: it resumes at once).
        let since = p.term.mode_changes.len();
        p.send(b"\x1a");
        assert!(p.until(5.0, |t| t.left_and_came_back("1049", since)), "{:?}", &p.term.mode_changes[since..]);
        assert_eq!(p.termios().c_lflag & libc::ICANON, 0, "raw again");
        // So does a SIGTSTP from elsewhere (kill -TSTP).
        let since = p.term.mode_changes.len();
        p.signal(libc::SIGTSTP);
        assert!(p.until(5.0, |t| t.left_and_came_back("1049", since)), "{:?}", &p.term.mode_changes[since..]);
        p.expect("Volume 4 / 4");
        p.send(b"q");
        assert!(p.wait(10).success());

        // A file that cannot be shown: said in the viewer, q quits.
        let bad = dir.join("bad.nii");
        std::fs::write(&bad, b"not an image at all, but long enough to be read as one maybe").unwrap();
        let mut p = start(&bad, 80, 24, Answers::xterm(), &[]);
        p.expect("q quits");
        // No help there: ? does nothing, and q still quits.
        p.send(b"?q");
        assert!(p.wait(10).success());
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn signals_while_asking_and_running() {
    if !pty::available() {
        return;
    }
    within(60, "signals", || {
        let dir = tmpdir("tui-signals");
        let f = head(&dir, "head.nii.gz");
        // SIGTERM while the terminal is asked (it answers nothing): nitty
        // ends at once with the usual status, the terminal given back.
        let mut p = start(&f, 100, 30, Answers::default(), &[]);
        let before = p.initial;
        // (The wait for answers lasts 1 s and more.)
        p.until(0.4, |_| false);
        p.signal(libc::SIGTERM);
        let status = p.wait(5);
        assert_eq!(status.code(), Some(128 + libc::SIGTERM), "{status:?}");
        assert_eq!(p.termios().c_lflag, before.c_lflag);
        // SIGQUIT while it runs: quits like q.
        let mut p = start(&f, 100, 30, Answers::xterm(), &[]);
        shown(&mut p);
        p.signal(libc::SIGQUIT);
        assert!(p.wait(10).success());
        assert_eq!(p.term.modes.get("1049"), Some(&false));
        std::fs::remove_dir_all(&dir).ok();
    });
}
