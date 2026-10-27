//! Display options of the protocol: window percentiles, a window set by the
//! user (and back to automatic), palettes, and the window of 4D images (per
//! volume or that of the first).

mod common;

use std::time::{Duration, Instant};

use common::{gz, tmpdir, within, Nifti};
use gnome_nifti_preview::server::{respond, Session};
use serde_json::{json, Value};

fn request(session: &mut Session, req: Value) -> (Value, Option<Vec<u8>>) {
    let (header, payload) = respond(session, req.to_string().as_bytes());
    (header, payload.map(|p| p.to_vec()))
}

/// A 4D int16 series: voxel (i, j, k) of volume t is (i + 1) * (t + 1).
fn series(dir: &std::path::Path) -> std::path::PathBuf {
    let n = Nifti::new(&[40, 6, 5, 3], 4);
    let mut data = Vec::new();
    for t in 0..3 {
        for _k in 0..5 {
            for _j in 0..6 {
                for i in 0..40 {
                    data.extend_from_slice(&(((i + 1) * (t + 1)) as i16).to_le_bytes());
                }
            }
        }
    }
    let p = dir.join("s.nii.gz");
    std::fs::write(&p, gz(&n.file(&data))).unwrap();
    p
}

fn show(s: &mut Session, k: i64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (r, _) = request(s, json!({"op": "set_volume", "volume": k}));
        assert_eq!(r["ok"], true, "{r}");
        if r["ready"] == true {
            return r;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn percentiles_manual_window_and_palettes() {
    within(60, "display", || {
        let dir = tmpdir("display");
        let p = series(&dir);
        let mut s = Session::new(8 << 20);
        request(&mut s, json!({"op": "open", "path": p}));
        let (load, _) = request(&mut s, json!({"op": "load", "window_percentiles": [0, 100]}));
        assert_eq!(load["display_window"], json!({"low": 1.0, "high": 40.0, "source": "auto"}), "{load}");
        assert_eq!(load["palette"], "gray");
        // Integral stripes look like a segmentation: show them as intensities.
        request(&mut s, json!({"op": "set_display", "colormap": "gray"}));
        let (a, pixels) = request(&mut s, json!({"op": "slice", "view": "axial", "index": 2}));
        let gray = pixels.unwrap();
        assert_eq!(a["width"], 40);
        // A window set by hand: everything from 30 up is white.
        let (d, _) = request(&mut s, json!({"op": "set_display", "window": [20, 30]}));
        assert_eq!(d["display_window"]["source"], "manual");
        let (_, manual) = request(&mut s, json!({"op": "slice", "view": "axial", "index": 2}));
        let manual = manual.unwrap();
        assert_ne!(manual, gray, "the cached slice is not reused");
        let row: Vec<u8> = manual.chunks(3).take(40).map(|p| p[0]).collect();
        assert!(row[..19].iter().all(|&v| v == 0) && row[29..].iter().all(|&v| v == 255), "{row:?}");
        // Back to automatic, then a palette: the darkest voxel is viridis' first colour.
        let (d, _) = request(&mut s, json!({"op": "set_display", "window": null, "palette": "viridis"}));
        assert_eq!(d["display_window"]["source"], "auto");
        assert_eq!(d["palette"], "viridis");
        let (_, coloured) = request(&mut s, json!({"op": "slice", "view": "axial", "index": 2}));
        let coloured = coloured.unwrap();
        let first = &coloured[..3];
        assert!(first == [68, 1, 84] || first == [253, 231, 37], "{first:?}");
        for bad in [
            json!({"op": "set_display", "palette": "rainbow"}),
            json!({"op": "set_display", "window": [5, 1]}),
            json!({"op": "set_display", "window": "wide"}),
            json!({"op": "load", "window_percentiles": [50, 10]}),
            json!({"op": "load", "window_volume": "each"}),
        ] {
            assert_eq!(request(&mut s, bad.clone()).0["error"]["code"], "bad_request", "{bad}");
        }
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn window_of_the_first_volume_for_all() {
    within(60, "4d window", || {
        let dir = tmpdir("display-4d");
        let p = series(&dir);
        // Per volume (default): the window follows the volume.
        let mut s = Session::new(8 << 20);
        request(&mut s, json!({"op": "open", "path": p}));
        let (first, _) = request(&mut s, json!({"op": "load", "window_percentiles": [0, 100]}));
        let r = show(&mut s, 2);
        assert_eq!(r["display_window"], json!({"low": 3.0, "high": 120.0, "source": "auto"}), "{r}");
        // First: volume 0's window for every volume.
        let mut s = Session::new(8 << 20);
        request(&mut s, json!({"op": "open", "path": p}));
        request(&mut s, json!({"op": "load", "window_percentiles": [0, 100], "window_volume": "first"}));
        let r = show(&mut s, 2);
        assert_eq!(r["display_window"]["low"], first["display_window"]["low"]);
        assert_eq!(r["display_window"]["high"], first["display_window"]["high"]);
        assert_eq!(r["display_window"]["source"], "first");
        // The statistics stay those of the volume shown.
        assert_eq!(r["stats"]["max"], 120);
        std::fs::remove_dir_all(&dir).ok();
    });
}
