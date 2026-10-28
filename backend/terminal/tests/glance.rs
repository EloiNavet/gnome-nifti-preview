//! The quick look (`nitty -p`, and any run whose output is not a
//! terminal), played on a strict terminal: what is drawn where, in which
//! colours, with which sequences; and how it fails.

mod common;

use common::vt::{Colour, Term};
use common::{aseg, head, nitty, stdout, tmpdir, Nifti};

fn play(out: &str, width: usize) -> Term {
    let mut t = Term::new(width, 1, true);
    t.feed(out.as_bytes());
    t
}

fn brightness(c: Colour) -> u32 {
    match c {
        Colour::Rgb([r, g, b]) => r as u32 + g as u32 + b as u32,
        Colour::Default => 0,
        Colour::Index(i) => panic!("index {i} in truecolor"),
    }
}

/// Mean brightness of the left and right halves of the image under the
/// title `title` (the image starts two rows below it, one column right).
fn halves(t: &Term, title: &str) -> (f64, f64, usize, usize) {
    let (tx, ty) = t.find(title).unwrap_or_else(|| panic!("no {title} in\n{}", t.text()));
    let (x0, y0) = (tx, ty + 2);
    // The image: up to the first column without colour.
    let w = (x0..t.w).take_while(|&x| t.cell(x, y0 + 2).bg != Colour::Default).count();
    let h = (y0..t.cells.len()).take_while(|&y| t.cell(x0 + w / 2, y).bg != Colour::Default).count();
    assert!(w > 4 && h > 4, "image {w} × {h} at {x0},{y0}\n{}", t.text());
    let mean = |xs: std::ops::Range<usize>| {
        let mut s = 0.0;
        let mut n = 0.0;
        for y in 2 * y0..2 * (y0 + h) {
            for x in xs.clone() {
                s += brightness(t.pixel(x, y)) as f64;
                n += 1.0;
            }
        }
        s / n
    };
    (mean(x0..x0 + w / 2), mean(x0 + w - w / 2..x0 + w), w, h)
}

#[test]
fn right_is_right_in_neurological_left_in_radiological() {
    let dir = tmpdir("glance-lr");
    let p = head(&dir, "head.nii.gz");
    // (Its few distinct values make it look like a label map: grey asked.)
    let o = nitty(&["-p", "--width", "100", "--view", "axial", "--colormap", "gray", p.to_str().unwrap()], &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let t = play(&stdout(&o), 100);
    let (left, right, w, h) = halves(&t, "Axial 18/35");
    assert!(right > 1.3 * left, "the bright right hemisphere on the right: {left} {right}\n{}", t.text());
    // The letters around the image: L left, R right, A on top, P below.
    let (tx, ty) = t.find("Axial").unwrap();
    let (x0, y0) = (tx, ty + 2);
    assert_eq!(t.cell(x0 - 1, y0 + h / 2).ch, 'L', "{}", t.text());
    assert_eq!(t.cell(x0 + w, y0 + h / 2).ch, 'R');
    assert_eq!(t.cell(x0 + w / 2, y0 - 1).ch, 'A');
    assert_eq!(t.cell(x0 + w / 2, y0 + h).ch, 'P');
    // The nose (front) is at the top.
    let top: u32 = (x0..x0 + w).map(|x| brightness(t.pixel(x, 2 * y0 + 1))).sum();
    let bottom: u32 = (x0..x0 + w).map(|x| brightness(t.pixel(x, 2 * (y0 + h) - 2))).sum();
    assert!(top > bottom, "front at the top: {top} {bottom}");

    let o = nitty(&["-p", "--width", "100", "--view", "axial", "--colormap", "gray", "--mode", "radiological", p.to_str().unwrap()], &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let t = play(&stdout(&o), 100);
    let (left, right, w, h) = halves(&t, "Axial 18/35");
    assert!(left > 1.3 * right, "radiological: the right hemisphere on the left: {left} {right}");
    let (tx, ty) = t.find("Axial").unwrap();
    assert_eq!(t.cell(tx - 1, ty + 2 + h / 2).ch, 'R');
    assert_eq!(t.cell(tx + w, ty + 2 + h / 2).ch, 'L');
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn three_views_one_scale_and_a_summary() {
    let dir = tmpdir("glance-three");
    let p = head(&dir, "head.nii.gz");
    let o = nitty(&["-p", "--width", "120", p.to_str().unwrap()], &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let t = play(&stdout(&o), 120);
    let first = t.row(0);
    assert!(first.starts_with("head.nii.gz") && first.contains("40 × 48 × 36") && first.contains("RAS") && first.contains("int16"), "{first}");
    let (cx, _) = t.find("Coronal 24/47").expect("coronal");
    let (sx, _) = t.find("Sagittal 20/39").expect("sagittal");
    let (ax, _) = t.find("Axial 18/35").expect("axial");
    assert!(cx < sx && sx < ax, "coronal, sagittal, axial from left to right");
    // The same scale: the axial view (40 mm wide) is as wide as the coronal (40 mm).
    let width_at = |x0: usize, y: usize| (x0..t.w).take_while(|&x| t.cell(x, y).bg != Colour::Default || t.cell(x, y).ch == '▄').count();
    let (_, ty) = t.find("Coronal").unwrap();
    let mid = ty + 2 + 5;
    let cw = width_at(cx, mid);
    let aw = width_at(ax, mid);
    assert!(cw.abs_diff(aw) <= 1, "{cw} {aw}\n{}", t.text());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn segmentations_in_freesurfer_colours_with_their_legend() {
    let dir = tmpdir("glance-aseg");
    let p = aseg(&dir, &[0, 2, 17, 41, 53]);
    let o = nitty(&["-p", "--width", "100", "--view", "axial", "--legend", p.to_str().unwrap()], &[]);
    assert!(o.status.success());
    let t = play(&stdout(&o), 100);
    let has = |c: [u8; 3]| t.cells.iter().flatten().any(|x| x.bg == Colour::Rgb(c) || x.fg == Colour::Rgb(c));
    assert!(has([220, 216, 20]), "Left-Hippocampus in its colour\n{}", t.text());
    assert!(t.text().contains("Left-Hippocampus") && t.text().contains("Right-Cerebral-White-Matter"), "{}", t.text());
    assert!(t.row(0).contains("4 labels"), "{}", t.row(0));
    let o = nitty(&["-p", "--width", "100", "--view", "axial", "--colors", "256", p.to_str().unwrap()], &[]);
    assert!(o.status.success());
    let t = play(&stdout(&o), 100);
    assert!(t.cells.iter().flatten().any(|x| x.bg == Colour::Index(184) || x.fg == Colour::Index(184)), "256 colours");
    assert!(!t.cells.iter().flatten().any(|x| matches!(x.bg, Colour::Rgb(_)) || matches!(x.fg, Colour::Rgb(_))));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn info_and_text_only_terminals() {
    let dir = tmpdir("glance-info");
    let p = head(&dir, "head.nii.gz");
    let o = nitty(&["-p", "--width", "90", "--info", "--colormap", "gray", p.to_str().unwrap()], &[("TERM", "dumb")]);
    assert!(o.status.success());
    let out = stdout(&o);
    assert!(!out.contains('\x1b'), "no escape sequence for a dumb terminal");
    assert!(out.contains("Geometry") && out.contains("Dimensions   40 × 48 × 36") && out.contains("Transform    sform (scanner)"), "{out}");
    assert!(out.contains('@') && out.contains('.'), "characters by brightness");
    // An unknown TERM is no obstacle: no terminfo is read.
    let o = nitty(&["-p", "--width", "90", p.to_str().unwrap()], &[("TERM", "frobnicator-9000")]);
    assert!(o.status.success());
    play(&stdout(&o), 90);
    // The Linux console: 16 colours, in front and behind.
    let o = nitty(&["-p", "--width", "90", p.to_str().unwrap()], &[("TERM", "linux"), ("COLORTERM", "")]);
    assert!(o.status.success());
    let t = play(&stdout(&o), 90);
    let sixteen = |c: Colour| !matches!(c, Colour::Rgb(_) | Colour::Index(16..));
    assert!(t.cells.iter().flatten().all(|c| sixteen(c.fg) && sixteen(c.bg)), "16 colours only");
    assert!(t.cells.iter().flatten().any(|c| c.ch == '▄' && matches!(c.fg, Colour::Index(_))), "the image in colours");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn image_protocols_when_asked() {
    let dir = tmpdir("glance-protocols");
    let p = head(&dir, "head.nii.gz");
    for (proto, kind, start) in [("kitty", "kitty", "Ga=T,f=100,t=d,q=2,C=1,"), ("sixel", "sixel", "0;1;0q\"1;1;"), ("iterm2", "iterm2", "1337;File=inline=1;")] {
        let o = nitty(&["-p", "--width", "100", "--protocol", proto, p.to_str().unwrap()], &[]);
        assert!(o.status.success(), "{proto}");
        let t = play(&stdout(&o), 100);
        assert_eq!(t.images.len(), 3, "{proto}: three pictures");
        for (img, title) in t.images.iter().zip(["Coronal", "Sagittal", "Axial"]) {
            assert_eq!(img.kind, kind);
            assert!(img.body.starts_with(start.as_bytes()), "{proto}: {}", String::from_utf8_lossy(&img.body[..40.min(img.body.len())]));
            // Drawn at the image's first cell: two rows under its title, one column right.
            let (tx, ty) = t.find(title).unwrap();
            assert_eq!((img.x, img.y), (tx, ty + 2), "{proto} {title}");
        }
        // Text cells are left empty under the pictures.
        assert!(!t.text().contains('▄'), "{proto}");
    }
    // Kitty: sized in cells; through tmux when inside it.
    let o = nitty(&["-p", "--width", "100", "--protocol", "kitty", "--view", "axial", p.to_str().unwrap()], &[("TMUX", "/tmp/x,1,0")]);
    assert!(o.status.success());
    let t = play(&stdout(&o), 100);
    assert!(t.images.iter().all(|i| i.kind == "tmux"), "{:?}", t.images.iter().map(|i| i.kind).collect::<Vec<_>>());
    assert!(t.images[0].body.starts_with(b"\x1b_Ga=T,f=100"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn errors_and_hostile_files() {
    let dir = tmpdir("glance-hostile");
    // Missing file, a folder of nothing, a truncated image: exit 2 and a message.
    let missing = dir.join("missing.nii");
    let o = nitty(&["-p", missing.to_str().unwrap()], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("missing.nii"));
    let o = nitty(&["-p", dir.to_str().unwrap()], &[]);
    assert_eq!(o.status.code(), Some(2));
    let full = Nifti::new(&[10, 10, 10], 2).file(&[7u8; 1000]);
    std::fs::write(dir.join("cut.nii"), &full[..600]).unwrap();
    let o = nitty(&["-p", dir.join("cut.nii").to_str().unwrap()], &[]);
    assert_eq!(o.status.code(), Some(2), "{}", stdout(&o));
    // Surface data: refused with what is known of the file.
    let mut n = Nifti::new(&[1, 1, 1, 1, 1, 500], 16).v2();
    n.intent = 3006;
    n.intent_name = b"ConnDenseScalar".to_vec();
    std::fs::write(dir.join("x.dscalar.nii"), n.file(&vec![0u8; 2000])).unwrap();
    let o = nitty(&["-p", dir.join("x.dscalar.nii").to_str().unwrap()], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("CIFTI-2"));
    assert!(stdout(&o).contains("Dimensions"), "{}", stdout(&o));
    // A header and a file name trying to send escape sequences.
    let mut n = Nifti::new(&[4, 4, 4], 2);
    n.descrip = b"\x1b]52;c;cm0gLXJmIH4=\x07evil\x1b[2J".to_vec();
    let evil = dir.join("a\x1b]0;pwned\x07.nii");
    std::fs::write(&evil, n.file(&[5u8; 64])).unwrap();
    let o = nitty(&["-p", "--info", "--width", "80", evil.to_str().unwrap()], &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = stdout(&o);
    assert!(!out.contains("\x1b]") && !out.contains("\x1b[2J") && !out.contains('\x07'), "{out:?}");
    play(&out, 80);
    assert!(out.contains("]52;c;cm0gLXJmIH4=evil"), "the text stays, without its controls");
    let o = nitty(&["-p", dir.join("nope\x1b[31m.nii").to_str().unwrap()], &[]);
    assert!(!String::from_utf8_lossy(&o.stderr).contains('\x1b'));
    // A terminal too narrow: said, not a crash.
    let p = head(&dir, "head.nii.gz");
    let o = nitty(&["-p", "--width", "5", p.to_str().unwrap()], &[]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("too narrow"), "{}", String::from_utf8_lossy(&o.stderr));
    // Several files: the good ones are shown, the exit status tells of the bad.
    let o = nitty(&["-p", "--width", "60", missing.to_str().unwrap(), p.to_str().unwrap()], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).contains("head.nii.gz"));
    // Options checked against the image.
    for args in [&["--at", "40,0,0"][..], &["--view", "axial", "--slice", "36"], &["--volume", "1"]] {
        let mut a = vec!["-p"];
        a.extend_from_slice(args);
        a.push(p.to_str().unwrap());
        let o = nitty(&a, &[]);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
    }
    let o = nitty(&["-p", "--bogus", p.to_str().unwrap()], &[]);
    assert_eq!(o.status.code(), Some(64));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_closed_pipe_ends_quietly() {
    let dir = tmpdir("glance-pipe");
    let p = head(&dir, "head.nii.gz");
    let mut c = std::process::Command::new(common::BIN)
        .args(["-p", "--width", "200", p.to_str().unwrap(), p.to_str().unwrap(), p.to_str().unwrap()])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(c.stdout.take());
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success() || o.status.code() == Some(1), "{:?}", o.status);
    assert!(!String::from_utf8_lossy(&o.stderr).contains("panicked"), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn panel_json_matches_the_panel() {
    let dir = tmpdir("glance-json");
    let p = aseg(&dir, &[0, 2, 17, 41, 53]);
    let o = nitty(&["-p", "--panel-json", "--at", "19,2,2", p.to_str().unwrap()], &[]);
    assert!(o.status.success());
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["sections"][0]["title"], "Geometry");
    assert_eq!(v["sections"][1]["rows"][1], serde_json::json!(["Labels", "4"]));
    assert_eq!(v["readout"], "label 17 · Left-Hippocampus · 1.92 cm³");
    assert_eq!(v["labels"][1], serde_json::json!(["Left-Hippocampus", "1.92 cm³"]));
    assert_eq!(v["volume"], serde_json::Value::Null, "one volume: no volume line");
    // A 4D image: the volume line, at the volume asked.
    let mut n = Nifti::new(&[4, 4, 3, 6], 2);
    n.pixdim[4] = 2.5;
    let f = dir.join("bold.nii.gz");
    std::fs::write(&f, common::gz(&n.file(&[1u8; 4 * 4 * 3 * 6]))).unwrap();
    let o = nitty(&["-p", "--panel-json", "--volume", "3", f.to_str().unwrap()], &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["volume"], "Volume 3 / 5 · 7.5 s");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn usage_edges() {
    let dir = tmpdir("glance-usage");
    // An option-looking file name with an escape sequence: the error is cleaned.
    let o = nitty(&["-p", "-\x1b]0;pwned\x07"], &[]);
    assert_eq!(o.status.code(), Some(64));
    assert!(!String::from_utf8_lossy(&o.stderr).contains('\x1b'));
    // The viewer is refused on a dumb terminal.
    let p = head(&dir, "head.nii.gz");
    let o = nitty(&["-i", p.to_str().unwrap()], &[("TERM", "dumb")]);
    assert_eq!(o.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&o.stderr).contains("TERM=dumb"));
    // A folder without DICOM images: said so (not "not a regular file").
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let o = nitty(&["-p", empty.to_str().unwrap()], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("No DICOM image in the folder"), "{}", String::from_utf8_lossy(&o.stderr));
    // --help into a pipe closed early: no panic.
    let mut c = std::process::Command::new(common::BIN)
        .arg("--help")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(c.stdout.take());
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success() && !String::from_utf8_lossy(&o.stderr).contains("panicked"));
    std::fs::remove_dir_all(&dir).ok();
}
