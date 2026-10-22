//! Hostile files: tiny inputs that claim huge sizes, special files and racy
//! paths. Each must give an error code (or a correct preview) quickly, with
//! bounded memory, and never abort the process or hang.

mod common;

use std::path::{Path, PathBuf};

use common::{gz, tmpdir, within, Nifti};
use gnome_nifti_preview::render::render_slice;
use gnome_nifti_preview::volume::Volume;

/// Open, load and render the centre slices; the first error code, or "ok".
fn outcome(path: &Path, max_bytes: u64) -> String {
    let path = path.to_path_buf();
    within(60, &path.display().to_string(), move || {
        let mut v = match Volume::open_with_limit(&path, max_bytes) {
            Ok(v) => v,
            Err(e) => return e.code.to_string(),
        };
        let _ = v.header_info();
        if let Err(e) = v.stats() {
            return e.code.to_string();
        }
        let plans = v.plans("neurological").unwrap();
        let mut scratch = Vec::new();
        for plan in plans.iter() {
            if let Err(e) = render_slice(&mut v, plan, plan.n_slices / 2, "auto", &mut scratch) {
                return e.code.to_string();
            }
        }
        "ok".to_string()
    })
}

const LIMIT: u64 = 3 << 30;

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn huge_vox_offset_in_a_tiny_gzip_is_refused() {
    let dir = tmpdir("offset");
    for off in [1e9f64, 1e12, 1e18, 3.0e38] {
        let mut n = Nifti::new(&[4, 4, 4], 2);
        n.vox_offset = Some(off);
        let p = write(&dir, "off.nii.gz", &gz(&n.file(&[1u8; 64])));
        assert_eq!(outcome(&p, LIMIT), "corrupt_data", "vox_offset {off}");
        let p = write(&dir, "off.nii", &n.file(&[1u8; 64]));
        assert_eq!(outcome(&p, LIMIT), "corrupt_data", "vox_offset {off} (.nii)");
    }
    // NIfTI-2 stores vox_offset as an int64.
    let mut n = Nifti::new(&[4, 4, 4], 2).v2();
    n.vox_offset = Some(9.0e18);
    let p = write(&dir, "off2.nii.gz", &gz(&n.file(&[1u8; 64])));
    assert_eq!(outcome(&p, LIMIT), "corrupt_data");
    // Large but allowed offset, with the stream ending early: truncated.
    let mut n = Nifti::new(&[4, 4, 4], 2);
    n.vox_offset = Some(200.0 * 1048576.0);
    let p = write(&dir, "off3.nii.gz", &gz(&n.file(&[1u8; 64])));
    assert_eq!(outcome(&p, LIMIT), "corrupt_data");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_extension_before_the_data_is_skipped() {
    let dir = tmpdir("ext");
    let mut n = Nifti::new(&[4, 4, 4], 2);
    n.vox_offset = Some(352.0 + 4096.0);
    let mut body = vec![0xAAu8; 4096];
    body.extend((0..64).map(|i| i as u8));
    for (name, bytes) in [("e.nii", n.file(&body)), ("e.nii.gz", gz(&n.file(&body)))] {
        let p = write(&dir, name, &bytes);
        let mut v = Volume::open(&p).unwrap();
        assert_eq!(v.value_at([3, 3, 3]).unwrap(), Some(63.0), "{name}");
        assert_eq!(v.value_at([1, 0, 0]).unwrap(), Some(1.0), "{name}");
    }
    // Same through the streaming path (4D).
    let mut n4 = Nifti::new(&[4, 4, 4, 2], 2);
    n4.vox_offset = n.vox_offset;
    let mut body4 = body.clone();
    body4.extend([7u8; 64]);
    let p = write(&dir, "e4.nii.gz", &gz(&n4.file(&body4)));
    let mut v = Volume::open(&p).unwrap();
    assert_eq!(v.value_at([3, 3, 3]).unwrap(), Some(63.0));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn giant_slices_are_refused_before_any_allocation() {
    let dir = tmpdir("flat");
    for dims in [vec![32767, 32767, 1], vec![32767, 32767], vec![20000, 2, 2], vec![5000, 5000, 3], vec![2, 4097, 4097]] {
        let n = Nifti::new(&dims, 2);
        let p = write(&dir, "flat.nii.gz", &gz(&n.file(&[0u8; 1024])));
        assert_eq!(outcome(&p, LIMIT), "too_large", "{dims:?}");
        let p = write(&dir, "flat2.nii.gz", &gz(&n.clone().v2().file(&[0u8; 1024])));
        assert_eq!(outcome(&p, LIMIT), "too_large", "{dims:?} NIfTI-2");
    }
    // At the limit: accepted (data missing, so corrupt rather than too large).
    let n = Nifti::new(&[4096, 4096, 1], 2);
    let p = write(&dir, "edge.nii.gz", &gz(&n.file(&[0u8; 1024])));
    assert_eq!(outcome(&p, LIMIT), "corrupt_data");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn uncompressed_files_obey_the_size_limit() {
    let dir = tmpdir("sparse");
    // 8 GiB claimed, sparse file: nothing is read or mapped.
    let n = Nifti::new(&[2048, 2048, 2048], 2);
    let p = dir.join("sparse.nii");
    std::fs::write(&p, n.header()).unwrap();
    std::fs::OpenOptions::new().write(true).open(&p).unwrap().set_len(352 + (8u64 << 30)).unwrap();
    assert_eq!(outcome(&p, LIMIT), "too_large");
    // Under a lower limit, a 64 MiB image is too large as well.
    let n = Nifti::new(&[256, 256, 1024], 2);
    let p = dir.join("mid.nii");
    std::fs::write(&p, n.header()).unwrap();
    std::fs::OpenOptions::new().write(true).open(&p).unwrap().set_len(352 + (64u64 << 20)).unwrap();
    assert_eq!(outcome(&p, 32 << 20), "too_large");
    assert_eq!(outcome(&p, LIMIT), "ok");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn large_uncompressed_files_are_mapped_and_read_correctly() {
    let dir = tmpdir("mapped");
    // 300 MiB (above the read-into-memory threshold), sparse zeros + markers.
    let n = Nifti::new(&[1000, 1000, 300], 2);
    let p = dir.join("big.nii");
    std::fs::write(&p, n.header()).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.set_len(352 + 300_000_000).unwrap();
    use std::os::unix::fs::FileExt;
    let last = 352 + 300_000_000 - 1;
    f.write_all_at(&[42], last).unwrap();
    f.write_all_at(&[7], 352 + 1000 * 1000 * 150 + 1000 * 500 + 500).unwrap();
    drop(f);
    let mut v = Volume::open(&p).unwrap();
    assert_eq!(v.value_at([999, 999, 299]).unwrap(), Some(42.0));
    assert_eq!(v.value_at([500, 500, 150]).unwrap(), Some(7.0));
    assert_eq!(outcome(&p, LIMIT), "ok");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn truncated_uncompressed_file_is_corrupt() {
    let dir = tmpdir("trunc");
    let n = Nifti::new(&[16, 16, 16], 4);
    let full = n.file(&vec![1u8; 16 * 16 * 16 * 2]);
    let p = write(&dir, "t.nii", &full[..full.len() - 1]);
    assert_eq!(outcome(&p, LIMIT), "corrupt_data");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn special_files_are_refused_without_blocking() {
    let dir = tmpdir("special");
    let fifo = dir.join("fifo.nii.gz");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    assert_eq!(outcome(&fifo, LIMIT), "not_a_file");
    let zero = dir.join("zero.nii.gz");
    std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
    assert_eq!(outcome(&zero, LIMIT), "not_a_file");
    let sub = dir.join("dir.nii");
    std::fs::create_dir(&sub).unwrap();
    assert_eq!(outcome(&sub, LIMIT), "not_a_file");
    let empty = write(&dir, "empty.nii", b"");
    assert_eq!(outcome(&empty, LIMIT), "invalid_nifti");
    assert_eq!(outcome(&dir.join("missing.nii"), LIMIT), "not_found");
    // A symlink to a real file is followed.
    let n = Nifti::new(&[4, 4, 4], 2);
    let real = write(&dir, "real.nii", &n.file(&[3u8; 64]));
    let link = dir.join("link.nii");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(outcome(&link, LIMIT), "ok");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn replacing_the_path_after_open_has_no_effect() {
    let dir = tmpdir("swap");
    let n = Nifti::new(&[4, 4, 4], 2);
    for name in ["s.nii", "s.nii.gz"] {
        let bytes = if name.ends_with(".gz") { gz(&n.file(&[5u8; 64])) } else { n.file(&[5u8; 64]) };
        let p = write(&dir, name, &bytes);
        let mut v = Volume::open(&p).unwrap();
        std::fs::remove_file(&p).unwrap();
        assert!(std::process::Command::new("mkfifo").arg(&p).status().unwrap().success());
        let value = within(20, name, move || v.value_at([1, 1, 1]).unwrap());
        assert_eq!(value, Some(5.0), "{name}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn gzip_trailers_and_bombs_do_not_matter() {
    let dir = tmpdir("gzip");
    let n = Nifti::new(&[8, 8, 8], 2);
    let data: Vec<u8> = (0..512).map(|i| (i % 200) as u8).collect();
    let good = gz(&n.file(&data));
    // ISIZE claiming exactly the expected size, but the stream is cut short.
    let mut short = gz(&n.file(&data[..300]));
    let k = short.len();
    short[k - 4..].copy_from_slice(&((352 + 512) as u32).to_le_bytes());
    let p = write(&dir, "short.nii.gz", &short);
    assert_eq!(outcome(&p, LIMIT), "corrupt_data");
    // Megabytes of garbage after a valid stream: ignored.
    let mut trailing = good.clone();
    trailing.extend(std::iter::repeat_n(0x5Au8, 5 << 20));
    let p = write(&dir, "trailing.nii.gz", &trailing);
    assert_eq!(outcome(&p, LIMIT), "ok");
    let mut v = Volume::open(&p).unwrap();
    assert_eq!(v.value_at([7, 7, 7]).unwrap(), Some(f64::from(data[511])));
    // A bomb: 200 MB of zeros after a small image; only the image is inflated.
    let mut body = n.file(&data);
    body.resize(body.len() + (200 << 20), 0);
    let p = write(&dir, "bomb.nii.gz", &gz(&body));
    let start = std::time::Instant::now();
    assert_eq!(outcome(&p, LIMIT), "ok");
    assert!(start.elapsed().as_secs() < 10);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn non_finite_sizes_and_affines_give_a_usable_display() {
    let dir = tmpdir("nonfinite");
    let mut n = Nifti::new(&[6, 5, 4, 3], 4);
    n.pixdim[1] = f64::NAN;
    n.pixdim[3] = f64::INFINITY;
    n.pixdim[4] = f64::NAN;
    n.sform_code = 0;
    n.qform_code = 1;
    let p = write(&dir, "pixdim.nii", &n.file(&vec![1u8; 6 * 5 * 4 * 3 * 2]));
    let v = Volume::open(&p).unwrap();
    let info = v.header_info();
    assert_eq!(info["zooms"], serde_json::json!([1.0, 1.0, 1.0, 0.0]));
    assert!(info["header_fixes"].to_string().contains("finite"), "{}", info["header_fixes"]);
    assert_eq!(info["orientation_known"], true);
    // A NaN translation: orientation unknown, finite pixel sizes everywhere.
    let mut n = Nifti::new(&[6, 5, 4], 4);
    n.srow[1][3] = f64::NAN;
    n.srow[0][0] = f64::INFINITY;
    let p = write(&dir, "affine.nii", &n.file(&[1u8; 6 * 5 * 4 * 2]));
    let mut v = Volume::open(&p).unwrap();
    assert_eq!(v.header_info()["orientation"], "unknown");
    for mode in ["neurological", "stored"] {
        for plan in v.plans(mode).unwrap().iter() {
            assert!(plan.pixel_w.is_finite() && plan.pixel_w > 0.0 && plan.pixel_h.is_finite() && plan.pixel_h > 0.0);
        }
    }
    assert_eq!(v.value_at([1, 1, 1]).unwrap(), Some(257.0));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn header_text_cannot_fake_lines_or_reorder_the_panel() {
    let dir = tmpdir("descrip");
    let mut n = Nifti::new(&[4, 4, 4], 2);
    n.descrip = b"T1\x1b[31m\nfake row\xe2\x80\xae evil\x7f end\t".to_vec();
    for v2 in [false, true] {
        let n = if v2 { n.clone().v2() } else { n.clone() };
        let p = write(&dir, "d.nii", &n.file(&[0u8; 64]));
        let v = Volume::open(&p).unwrap();
        assert_eq!(v.header_info()["descrip"], "T1[31m fake row evil end");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn huge_extra_dimensions_do_not_overflow() {
    let dir = tmpdir("dims");
    let n = Nifti::new(&[4, 4, 4, i64::MAX / 2, 1 << 40, 1 << 40], 2).v2();
    let p = write(&dir, "dims.nii.gz", &gz(&n.file(&[9u8; 64])));
    let mut v = Volume::open(&p).unwrap();
    assert_eq!(v.header.n_volumes(), usize::MAX);
    assert_eq!(v.value_at([3, 3, 3]).unwrap(), Some(9.0));
    std::fs::remove_dir_all(&dir).ok();
}
