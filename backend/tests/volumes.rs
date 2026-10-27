//! The other volumes of 4D images: uncompressed reads and mappings, the
//! background inflater of gzipped files (all kept, or decoded towards the
//! wanted volume within a budget), truncation, time courses and protocol.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{gz, tmpdir, within, Nifti};
use gnome_nifti_preview::server::{respond, Session};
use gnome_nifti_preview::volume::Volume;
use serde_json::{json, Value};

/// A 4D int16 image whose voxel (i, j, k) in volume t holds 100 t + i + j + k.
fn series(dims: [i64; 4]) -> (Nifti, Vec<u8>) {
    let n = Nifti::new(&dims, 4);
    let [x, y, z, t] = dims.map(|d| d as usize);
    let mut data = Vec::with_capacity(x * y * z * t * 2);
    for v in 0..t {
        for k in 0..z {
            for j in 0..y {
                for i in 0..x {
                    data.extend_from_slice(&((100 * v + i + j + k) as i16).to_le_bytes());
                }
            }
        }
    }
    (n, data)
}

/// Show volume `k`, asking again while it is being decompressed.
fn show(v: &mut Volume, k: i64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (ready, progress) = v.set_volume(k).unwrap();
        assert!((0.0..=1.0).contains(&progress), "progress {progress}");
        if ready {
            return true;
        }
        assert!(Instant::now() < deadline, "volume {k} never became ready");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn value(v: &mut Volume, ijk: [i64; 3]) -> f64 {
    v.value_at(ijk).unwrap().unwrap()
}

fn timecourse(v: &mut Volume, voxel: [usize; 3]) -> Vec<Option<f64>> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (values, complete) = v.timecourse(voxel).unwrap();
        if complete {
            return values;
        }
        assert!(Instant::now() < deadline, "time course never complete");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn open(path: &Path, limit: u64) -> Volume {
    Volume::open_with_limit(path, limit).unwrap()
}

const LIMIT: u64 = 256 << 20;

#[test]
fn every_volume_of_an_uncompressed_series() {
    let dir = tmpdir("vol-raw");
    let (n, data) = series([6, 5, 4, 7]);
    let p = dir.join("s.nii");
    std::fs::write(&p, n.file(&data)).unwrap();
    let mut v = open(&p, LIMIT);
    assert!(v.browsable());
    for k in [3, 6, 1, 0, 6] {
        assert!(show(&mut v, k));
        assert_eq!(v.current_volume(), k as usize);
        assert_eq!(value(&mut v, [1, 2, 3]), (100 * k + 6) as f64);
    }
    // Clamped like slice indices.
    assert!(show(&mut v, 99));
    assert_eq!(v.current_volume(), 6);
    assert!(show(&mut v, -4));
    assert_eq!(v.current_volume(), 0);
    let tc = timecourse(&mut v, [2, 1, 0]);
    assert_eq!(tc, (0..7).map(|t| Some((100 * t + 3) as f64)).collect::<Vec<_>>());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn every_volume_of_a_gzipped_series_kept_in_memory() {
    within(60, "gzip series", || {
        let dir = tmpdir("vol-gz");
        let (n, data) = series([8, 6, 5, 9]);
        let p = dir.join("s.nii.gz");
        std::fs::write(&p, gz(&n.file(&data))).unwrap();
        let mut v = open(&p, LIMIT);
        assert_eq!(value(&mut v, [0, 0, 0]), 0.0);
        for k in [8, 2, 5, 0, 8] {
            assert!(show(&mut v, k));
            assert_eq!(value(&mut v, [7, 5, 4]), (100 * k + 16) as f64);
        }
        let tc = timecourse(&mut v, [1, 1, 1]);
        assert_eq!(tc, (0..9).map(|t| Some((100 * t + 3) as f64)).collect::<Vec<_>>());
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn a_series_above_the_limit_is_decoded_towards_the_wanted_volume() {
    within(60, "budget", || {
        let dir = tmpdir("vol-budget");
        // 1 MiB volumes, 24 MiB in all, under a 16 MiB limit: they cannot all
        // be kept, so the decoder goes to each volume asked (and back).
        let (n, data) = series([64, 64, 128, 24]);
        let p = dir.join("big.nii.gz");
        std::fs::write(&p, gz(&n.file(&data))).unwrap();
        let mut v = open(&p, 16 << 20);
        for k in [20, 3, 23, 1, 12] {
            assert!(show(&mut v, k));
            assert_eq!(value(&mut v, [10, 20, 30]), ((100 * k + 60) as i16) as f64);
        }
        // Not every volume can be kept: the time course stays incomplete.
        let (tc, complete) = v.timecourse([0, 0, 0]).unwrap();
        assert!(!complete);
        assert_eq!(tc.len(), 24);
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn a_truncated_series_keeps_its_complete_volumes() {
    let dir = tmpdir("vol-trunc");
    let (n, data) = series([6, 5, 4, 5]);
    let full = n.file(&data);
    let vol = 6 * 5 * 4 * 2;
    let cut = &full[..full.len() - vol - 7];
    let p = dir.join("t.nii");
    std::fs::write(&p, cut).unwrap();
    let mut v = open(&p, LIMIT);
    assert!(show(&mut v, 2));
    assert_eq!(v.set_volume(4).unwrap_err().code, "corrupt_data");
    assert_eq!(v.current_volume(), 2, "the volume shown does not change");
    let (tc, complete) = v.timecourse([0, 0, 0]).unwrap();
    assert!(!complete);
    assert_eq!(tc[..3], [Some(0.0), Some(100.0), Some(200.0)]);
    assert_eq!(tc[4], None);

    let pz = dir.join("t.nii.gz");
    std::fs::write(&pz, &gz(&full)[..gz(&full).len() / 2]).unwrap();
    let mut v = open(&pz, LIMIT);
    let deadline = Instant::now() + Duration::from_secs(20);
    let err = loop {
        match v.set_volume(4) {
            Err(e) => break e,
            Ok((ready, _)) => assert!(!ready, "a truncated volume cannot be shown"),
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(err.code, "corrupt_data");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn large_uncompressed_series_are_mapped() {
    let dir = tmpdir("vol-map");
    // 96 MiB volumes, 4 of them: above the read limit, mapped.
    let n = Nifti::new(&[512, 512, 192, 4], 4);
    let p = dir.join("big.nii");
    let header = n.header();
    let mut f = std::fs::File::create(&p).unwrap();
    use std::io::Write;
    f.write_all(&header).unwrap();
    let vol = 512u64 * 512 * 192 * 2;
    f.set_len(header.len() as u64 + 4 * vol).unwrap();
    // One marked voxel in volume 3.
    use std::os::unix::fs::FileExt;
    f.write_all_at(&1234i16.to_le_bytes(), header.len() as u64 + 3 * vol + 2 * (5 + 512 * 6)).unwrap();
    drop(f);
    let mut v = open(&p, 1 << 30);
    assert!(show(&mut v, 3));
    assert_eq!(value(&mut v, [5, 6, 0]), 1234.0);
    let tc = timecourse(&mut v, [5, 6, 0]);
    assert_eq!(tc, vec![Some(0.0), Some(0.0), Some(0.0), Some(1234.0)]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn three_d_images_have_one_volume() {
    let dir = tmpdir("vol-3d");
    let (n, data) = series([6, 5, 4, 1]);
    let p = dir.join("one.nii.gz");
    std::fs::write(&p, gz(&n.file(&data))).unwrap();
    let mut v = open(&p, LIMIT);
    assert!(!v.browsable());
    assert!(show(&mut v, 3));
    assert_eq!(v.current_volume(), 0);
    assert_eq!(timecourse(&mut v, [1, 1, 1]), vec![Some(3.0)]);
    std::fs::remove_dir_all(&dir).ok();
}

fn request(session: &mut Session, req: Value) -> Value {
    respond(session, req.to_string().as_bytes()).0
}

#[test]
fn protocol_set_volume_and_timecourse() {
    within(60, "protocol", || {
        let dir = tmpdir("vol-proto");
        let (mut n, data) = series([6, 5, 4, 3]);
        n.pixdim[4] = 2.5;
        let p = dir.join("s.nii.gz");
        std::fs::write(&p, gz(&n.file(&data))).unwrap();
        let mut s = Session::new(1 << 20);
        let open = request(&mut s, json!({"id": 1, "op": "open", "path": p}));
        assert_eq!(open["header"]["n_volumes"], 3);
        assert!(open["header"]["warnings"].as_array().unwrap().is_empty(), "{open}");
        assert_eq!(request(&mut s, json!({"id": 2, "op": "load"}))["volume"], 0);
        let deadline = Instant::now() + Duration::from_secs(20);
        let r = loop {
            let r = request(&mut s, json!({"id": 3, "op": "set_volume", "volume": 2}));
            assert_eq!(r["ok"], true, "{r}");
            assert_eq!(r["volume"], 2);
            assert_eq!(r["time"], 5.0);
            if r["ready"] == true {
                break r;
            }
            assert_eq!(r["shown"], 0);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(r["shown"], 2);
        assert_eq!(r["stats"]["min"], 200, "{r}");
        let value = request(&mut s, json!({"id": 4, "op": "value", "ijk": [1, 1, 1]}));
        assert_eq!(value["value"], 203);
        let tc = request(&mut s, json!({"id": 5, "op": "timecourse", "ijk": [1, 1, 1]}));
        assert_eq!(tc["values"], json!([3.0, 103.0, 203.0]), "{tc}");
        assert_eq!(tc["repetition_time"], 2.5);
        for bad in [json!({"op": "set_volume"}), json!({"op": "set_volume", "volume": "1"}), json!({"op": "timecourse", "ijk": [9, 9, 9]})] {
            assert_eq!(request(&mut s, bad)["error"]["code"], "bad_request");
        }
        std::fs::remove_dir_all(&dir).ok();
    });
}
