//! `nifti-preview thumbnail`: size and physical aspect, label colours never
//! blended, limits and failures, and a run inside the same bubblewrap sandbox
//! as GNOME's thumbnail factory (only /usr, the input and the output).

mod common;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{gz, tmpdir, Nifti};

const BIN: &str = env!("CARGO_BIN_EXE_nifti-preview");

/// Width, height and RGB pixels of a PNG written by our encoder (filter 0).
fn decode(png: &[u8]) -> (usize, usize, Vec<u8>) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let (mut pos, mut w, mut h, mut idat) = (8, 0, 0, Vec::new());
    while pos + 8 <= png.len() {
        let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = &png[pos + 4..pos + 8];
        let body = &png[pos + 8..pos + 8 + len];
        if kind == b"IHDR" {
            w = u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize;
            h = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
        } else if kind == b"IDAT" {
            idat.extend_from_slice(body);
        }
        pos += 12 + len;
    }
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&idat[..]).read_to_end(&mut raw).unwrap();
    let mut rgb = Vec::new();
    for row in raw.chunks(w * 3 + 1) {
        assert_eq!(row[0], 0, "filter type");
        rgb.extend_from_slice(&row[1..]);
    }
    (w, h, rgb)
}

fn thumbnail(input: &Path, out: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(BIN).arg("thumbnail").args(extra).arg(input).arg(out).output().unwrap()
}

/// A 3D image with the given voxel sizes, values `f(i, j, k)` (int16).
fn image(dir: &Path, name: &str, dims: [i64; 3], zooms: [f64; 3], f: impl Fn(usize, usize, usize) -> i16) -> PathBuf {
    let mut n = Nifti::new(&dims, 4);
    n.pixdim[1..4].copy_from_slice(&zooms);
    for (r, z) in zooms.iter().enumerate() {
        n.srow[r][r] = *z;
    }
    let [x, y, z] = dims.map(|d| d as usize);
    let mut data = Vec::new();
    for k in 0..z {
        for j in 0..y {
            for i in 0..x {
                data.extend_from_slice(&f(i, j, k).to_le_bytes());
            }
        }
    }
    let p = dir.join(name);
    std::fs::write(&p, gz(&n.file(&data))).unwrap();
    p
}

#[test]
fn thumbnails_keep_the_physical_aspect() {
    let dir = tmpdir("thumb-aspect");
    // 40 × 20 voxels of 1 × 2 mm: a square axial slice.
    let p = image(&dir, "a.nii.gz", [40, 20, 6], [1.0, 2.0, 1.0], |i, j, _| (i + j) as i16);
    let out = dir.join("a.png");
    assert!(thumbnail(&p, &out, &["-s", "256"]).status.success());
    let (w, h, rgb) = decode(&std::fs::read(&out).unwrap());
    assert_eq!((w, h), (256, 256));
    assert_eq!(rgb.len(), w * h * 3);
    // Coronal would be 40 voxels of 1 mm by 6 of 1 mm, a strip: the
    // acquired (axial, square) plane is shown instead.
    assert!(thumbnail(&p, &out, &["-s", "200", "--view", "coronal"]).status.success());
    let (w, h, _) = decode(&std::fs::read(&out).unwrap());
    assert_eq!((w, h), (200, 200));
    // A view that is not a strip keeps its physical aspect: sagittal is
    // 20 voxels of 2 mm by 6 of 1 mm (40 × 6 mm, a strip too: axial again);
    // a thicker image shows its coronal plane, 40 × 24 mm.
    let q = image(&dir, "b.nii.gz", [40, 20, 24], [1.0, 2.0, 1.0], |i, j, _| (i + j) as i16);
    assert!(thumbnail(&q, &out, &["-s", "200", "--view", "coronal"]).status.success());
    let (w, h, _) = decode(&std::fs::read(&out).unwrap());
    assert_eq!((w, h), (200, 120));
    // A line of values stays a line (no view of a single voxel).
    let line = image(&dir, "c.nii.gz", [30, 1, 1], [1.0, 1.0, 1.0], |i, _, _| (i * 100) as i16);
    assert!(thumbnail(&line, &out, &["-s", "200"]).status.success());
    let (w, h, _) = decode(&std::fs::read(&out).unwrap());
    assert!(w == 200 && h < 20, "{w} × {h}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn label_colours_are_never_blended() {
    let dir = tmpdir("thumb-labels");
    // Five labels in stripes, enlarged and shrunk.
    let p = image(&dir, "seg.nii.gz", [50, 50, 9], [1.0, 1.0, 1.0], |i, _, _| (i / 10) as i16);
    let out = dir.join("seg.png");
    for size in ["32", "512"] {
        assert!(thumbnail(&p, &out, &["-s", size]).status.success());
        let (_, _, rgb) = decode(&std::fs::read(&out).unwrap());
        let colours: std::collections::HashSet<&[u8]> = rgb.chunks(3).collect();
        assert!(colours.len() <= 5, "{} colours at {size} px: labels were blended", colours.len());
    }
    // In grayscale (--colormap rgb on a non-RGB image) they may be smoothed.
    assert!(thumbnail(&p, &out, &["-s", "32", "--colormap", "gray"]).status.success());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn failures_and_limits() {
    let dir = tmpdir("thumb-fail");
    let out = dir.join("x.png");
    let bad = dir.join("bad.nii.gz");
    std::fs::write(&bad, gz(b"not an image")).unwrap();
    let r = thumbnail(&bad, &out, &["-s", "128"]);
    assert_eq!(r.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&r.stderr).contains("invalid_image"));
    // Too much compressed data for the limit.
    // 4 MiB of noise: incompressible, so about 4 MB of gzip to read.
    let noise = |i: usize, j: usize, k: usize| {
        let mut x = ((i as u64) << 32) ^ ((j as u64) << 16) ^ k as u64 ^ 0x9e37_79b9_7f4a_7c15;
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x as i16
    };
    let p = image(&dir, "big.nii.gz", [128, 128, 128], [1.0, 1.0, 1.0], noise);
    let r = thumbnail(&p, &out, &["-s", "128", "--max-file-mb", "1"]);
    assert_eq!(r.status.code(), Some(2), "{}", String::from_utf8_lossy(&r.stderr));
    assert!(thumbnail(&p, &out, &["-s", "128"]).status.success());
    // The same for data compressed inside the file (NRRD gzip encoding).
    let data: Vec<u8> = (0..128 * 128 * 128).flat_map(|n| noise(n % 128, n / 128 % 128, n / 16384).to_le_bytes()).collect();
    let nrrd = dir.join("big.nrrd");
    std::fs::write(&nrrd, [b"NRRD0004\ntype: short\ndimension: 3\nsizes: 128 128 128\nencoding: gzip\n\n".to_vec(), gz(&data)].concat()).unwrap();
    let r = thumbnail(&nrrd, &out, &["-s", "128", "--max-file-mb", "1"]);
    assert_eq!(r.status.code(), Some(2), "{}", String::from_utf8_lossy(&r.stderr));
    assert!(thumbnail(&nrrd, &out, &["-s", "128"]).status.success());
    // Bad options and an output that cannot be written.
    for args in [&["-s", "x"][..], &["--view", "oblique"], &["--max-file-mb", "0"]] {
        assert_eq!(thumbnail(&p, &out, args).status.code(), Some(64), "{args:?}");
    }
    assert_eq!(thumbnail(&p, &dir.join("missing/x.png"), &[]).status.code(), Some(1));
    std::fs::remove_dir_all(&dir).ok();
}

/// GNOME's thumbnail sandbox (gnome-desktop-thumbnail-script.c): only /usr
/// (and the loader cache), private /tmp with the input and the output, no
/// network, no home. Our program is placed outside /usr for the test.
#[test]
fn runs_in_the_gnome_thumbnail_sandbox() {
    let works = Command::new("bwrap").args(["--ro-bind", "/", "/", "--unshare-all", "true"]).status();
    if !matches!(works, Ok(s) if s.success()) {
        eprintln!("bubblewrap unavailable: skipped");
        return;
    }
    let dir = tmpdir("thumb-bwrap");
    let p = image(&dir, "in.nii.gz", [30, 30, 10], [1.0, 1.0, 1.0], |i, j, k| (i * j + k) as i16);
    let tmp = dir.join("tmp");
    std::fs::create_dir(&tmp).unwrap();
    let mut cmd = Command::new("bwrap");
    cmd.args(["--ro-bind", "/usr", "/usr", "--ro-bind-try", "/etc/ld.so.cache", "/etc/ld.so.cache"]);
    for (link, target) in [("/bin", "usr/bin"), ("/lib", "usr/lib"), ("/lib64", "usr/lib64"), ("/sbin", "usr/sbin")] {
        if Path::new(link).is_symlink() {
            cmd.args(["--symlink", target, link]);
        } else if Path::new(link).exists() {
            cmd.args(["--ro-bind", link, link]);
        }
    }
    cmd.args(["--proc", "/proc", "--dev", "/dev", "--chdir", "/", "--setenv", "GIO_USE_VFS", "local", "--unshare-all", "--die-with-parent"]);
    cmd.arg("--bind").arg(&tmp).arg("/tmp");
    cmd.arg("--ro-bind").arg(&p).arg("/tmp/gnome-desktop-file-to-thumbnail.gz");
    cmd.arg("--ro-bind").arg(BIN).arg("/np-thumbnailer");
    cmd.args(["/np-thumbnailer", "thumbnail", "-s", "256", "/tmp/gnome-desktop-file-to-thumbnail.gz", "/tmp/gnome-desktop-thumbnailer.png"]);
    let r = cmd.output().unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let (w, h, _) = decode(&std::fs::read(tmp.join("gnome-desktop-thumbnailer.png")).unwrap());
    assert_eq!((w, h), (256, 256));
    std::fs::remove_dir_all(&dir).ok();
}
