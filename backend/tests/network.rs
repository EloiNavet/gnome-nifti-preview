//! Files on a network file system (here every file, through
//! NIFTI_PREVIEW_ASSUME_NETWORK=1, set by each test of this binary): never
//! mapped, read in blocks on a helper thread while `load` reports progress,
//! nothing read before the client asks (`defer_network`), other volumes of a
//! 4D image read in the background, no DICOM folder scan, no thumbnail
//! unless asked. The values must be those of the same file read locally.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{gz, tmpdir, Nifti};
use gnome_nifti_preview::server::{respond, Session};
use gnome_nifti_preview::volume::Volume;
use serde_json::{json, Value};

fn remote() {
    // Every test of this binary sets the same value.
    std::env::set_var("NIFTI_PREVIEW_ASSUME_NETWORK", "1");
}

fn request(s: &mut Session, req: Value) -> Value {
    respond(s, req.to_string().as_bytes()).0
}

fn value(ijk: [usize; 3], t: usize) -> i64 {
    let i = ijk[0] + 40 * (ijk[1] + 30 * (ijk[2] + 20 * t));
    ((i * 7919) % 3001) as i64 - 1000
}

/// A 4D int16 image of 40 × 30 × 20 voxels and 3 volumes.
fn image(dir: &Path, gzipped: bool) -> std::path::PathBuf {
    let dims = [40i64, 30, 20, 3];
    let n = (40 * 30 * 20 * 3) as usize;
    let data: Vec<u8> = (0..n).flat_map(|i| (((i * 7919) % 3001) as i16 - 1000).to_le_bytes()).collect();
    let bytes = Nifti::new(&dims, 4).file(&data);
    let p = dir.join(if gzipped { "img.nii.gz" } else { "img.nii" });
    std::fs::write(&p, if gzipped { gz(&bytes) } else { bytes }).unwrap();
    p
}

/// `load` with `async` until the data are there; the replies seen.
fn load_async(s: &mut Session) -> (Value, usize) {
    let start = Instant::now();
    let mut polls = 0;
    loop {
        let r = request(s, json!({"op": "load", "async": true}));
        assert_eq!(r["ok"], true, "{r}");
        if r["ready"] == json!(false) {
            let p = r["progress"].as_f64().unwrap();
            assert!((0.0..=1.0).contains(&p), "{r}");
            polls += 1;
            assert!(start.elapsed() < Duration::from_secs(30), "load never ended");
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        return (r, polls);
    }
}

#[test]
fn deferred_then_background_load() {
    remote();
    let dir = tmpdir("net-load");
    for gzipped in [false, true] {
        let p = image(&dir, gzipped);
        let mut s = Session::new(1 << 20);
        let r = request(&mut s, json!({"op": "open", "path": p}));
        assert_eq!(r["header"]["storage"]["network"], true, "{r}");
        assert_eq!(r["header"]["storage"]["fs"], "test");
        let bytes = r["header"]["storage"]["bytes_to_read"].as_u64().unwrap();
        // Nothing is read before the client asks.
        let r = request(&mut s, json!({"op": "load", "defer_network": true, "window_percentiles": [1, 99]}));
        assert_eq!((r["ready"].clone(), r["deferred"].clone()), (json!(false), json!(true)), "{r}");
        assert_eq!(r["bytes_to_read"].as_u64(), Some(bytes));
        if !gzipped {
            assert_eq!(bytes, 40 * 30 * 20 * 2, "volume 0 of an uncompressed file");
        }
        // While the data are read, only `load` (and `open`, `close`) answer.
        let (r, _) = load_async(&mut s);
        assert_eq!(r["ready"], true, "{r}");
        assert_eq!(r["stats"]["window"]["status"], "ok", "{r}");
        for ijk in [[0, 0, 0], [39, 29, 19], [17, 3, 11]] {
            let a = request(&mut s, json!({"op": "value", "ijk": ijk}));
            assert_eq!(a["value"], json!(value(ijk, 0)), "{ijk:?}");
        }
        // Values of the other volumes, read in the background.
        let mut ready = false;
        for _ in 0..3000 {
            let v = request(&mut s, json!({"op": "set_volume", "volume": 2}));
            assert_eq!(v["ok"], true, "{v}");
            if v["ready"] == true {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(ready);
        for ijk in [[0, 0, 0], [39, 29, 19], [17, 3, 11]] {
            let a = request(&mut s, json!({"op": "value", "ijk": ijk}));
            assert_eq!(a["value"], json!(value(ijk, 2)), "{ijk:?}");
        }
    }
}

#[test]
fn requests_while_loading_and_cancel() {
    remote();
    let dir = tmpdir("net-busy");
    let p = image(&dir, true);
    let mut s = Session::new(1 << 20);
    request(&mut s, json!({"op": "open", "path": p}));
    let r = request(&mut s, json!({"op": "load", "async": true}));
    if r["ready"] == false {
        let busy = request(&mut s, json!({"op": "slice", "view": "axial", "index": 0}));
        // Either still loading, or already done (a fast machine).
        if busy["ok"] == false {
            assert!(busy["error"]["message"].as_str().unwrap().contains("still being read"), "{busy}");
        }
    }
    // Another file while loading: the load is given up, the new file opens.
    let q = image(&tmpdir("net-other"), false);
    let r = request(&mut s, json!({"op": "open", "path": q}));
    assert_eq!(r["ok"], true, "{r}");
    let (r, _) = load_async(&mut s);
    assert_eq!(r["ready"], true);
    assert_eq!(request(&mut s, json!({"op": "close"}))["ok"], true);
}

#[test]
fn no_thumbnail_unless_asked() {
    remote();
    let dir = tmpdir("net-thumb");
    let p = image(&dir, false);
    let mut v = Volume::open(&p).unwrap();
    let opts = gnome_nifti_preview::thumbnail::Options::default();
    let e = gnome_nifti_preview::thumbnail::render(&mut v, &opts).err().expect("refused");
    assert!(e.message.contains("network"), "{e}");
    let opts = gnome_nifti_preview::thumbnail::Options { network: true, ..Default::default() };
    assert!(gnome_nifti_preview::thumbnail::render(&mut v, &opts).is_ok());
}
