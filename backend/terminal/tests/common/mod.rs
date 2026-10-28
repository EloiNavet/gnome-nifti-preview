//! Helpers of nitty's tests: the backend's fixture builders, a strict
//! terminal (`vt`), synthetic images and a way to run the binary.

#![allow(dead_code)]

#[path = "../../../tests/common/mod.rs"]
pub mod backend;
pub mod pty;
pub mod vt;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[allow(unused_imports)] // each test file uses some
pub use backend::{gz, tmpdir, within, Nifti};

pub const BIN: &str = env!("CARGO_BIN_EXE_nitty");

/// nitty with a known, minimal environment (a truecolor xterm, no tmux).
pub fn nitty(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_default()).env("TERM", "xterm-256color").env("COLORTERM", "truecolor");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().expect("nitty runs")
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).expect("UTF-8 output")
}

/// A head of 40 × 48 × 36 voxels of 1 mm, stored RAS: an ellipsoid of 100
/// with a bright block (400) in the right hemisphere and a bright nose
/// (300) at the front, so that left/right and front/back can be told.
pub fn head(dir: &Path, name: &str) -> PathBuf {
    let (nx, ny, nz) = (40usize, 48usize, 36usize);
    let mut n = Nifti::new(&[nx as i64, ny as i64, nz as i64], 4);
    n.srow = [[1.0, 0.0, 0.0, -20.0], [0.0, 1.0, 0.0, -24.0], [0.0, 0.0, 1.0, -18.0]];
    let mut data = Vec::new();
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let (x, y, z) = ((i as f64 - 19.5) / 17.0, (j as f64 - 23.5) / 21.0, (k as f64 - 17.5) / 15.0);
                let mut v: i16 = if x * x + y * y + z * z < 1.0 { 100 } else { 0 };
                if (28..34).contains(&i) && (18..30).contains(&j) && (12..24).contains(&k) {
                    v = 400;
                }
                if (17..23).contains(&i) && (43..47).contains(&j) && (15..21).contains(&k) {
                    v = 300;
                }
                data.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    let p = dir.join(name);
    std::fs::write(&p, gz(&n.file(&data))).unwrap();
    p
}

/// A FreeSurfer-like segmentation: blocks of labels along x (8 voxels
/// each), 2 mm voxels.
pub fn aseg(dir: &Path, ids: &[i16]) -> PathBuf {
    let mut n = Nifti::new(&[8 * ids.len() as i64, 6, 5], 4);
    n.pixdim[1..4].copy_from_slice(&[2.0, 2.0, 2.0]);
    n.srow = [[2.0, 0.0, 0.0, 0.0], [0.0, 2.0, 0.0, 0.0], [0.0, 0.0, 2.0, 0.0]];
    let mut data = Vec::new();
    for _ in 0..5 * 6 {
        for &id in ids {
            for _ in 0..8 {
                data.extend_from_slice(&id.to_le_bytes());
            }
        }
    }
    let p = dir.join("aseg.nii.gz");
    std::fs::write(&p, gz(&n.file(&data))).unwrap();
    p
}
