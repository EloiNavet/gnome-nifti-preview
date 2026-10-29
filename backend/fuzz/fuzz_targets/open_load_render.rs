//! Any file content: open it, load it, render every view and read values,
//! the way a preview does. Must never crash, hang or exhaust memory.
//!
//!   cargo +nightly fuzz run open_load_render CORPUS -- -max_total_time=600
//!
//! Seeds: python3 fuzz/make_seeds.py CORPUS

#![no_main]

use gnome_nifti_preview::orientation::voxel_to_world;
use gnome_nifti_preview::render::render_slice;
use gnome_nifti_preview::volume::Volume;
use libfuzzer_sys::fuzz_target;

/// Removes the input's folder when an input is done, whichever way it ends
/// (declared before the volume, so dropped after it).
struct Cleanup(std::path::PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fuzz_target!(|data: &[u8]| {
    // Alone in its folder: a DICOM file looks for the rest of its series there.
    let dir = std::env::temp_dir().join(format!("np-fuzz-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let _cleanup = Cleanup(dir.clone());
    let path = dir.join("input");
    std::fs::write(&path, data).unwrap();
    // 16 MiB of voxels: this build (sanitiser, coverage) runs about a
    // hundred times slower than the release, and a larger image (a 16384 ×
    // 832 JPEG 2000 frame) outlasts the time limit without being stuck.
    let Ok(mut v) = Volume::open_with_limit(&path, 16 << 20) else { return };
    let _ = v.header_info();
    if v.stats().is_err() {
        return;
    }
    // Another volume and a time course, as the 4D slider asks for them.
    if v.browsable() {
        for _ in 0..100 {
            if !matches!(v.set_volume(v.n_volumes() as i64 - 1), Ok((false, _))) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let _ = v.timecourse(v.center());
    }
    let mut scratch = Vec::new();
    for mode in ["neurological", "radiological", "stored"] {
        let Ok(plans) = v.plans(mode) else { continue };
        for plan in plans.iter() {
            for cmap in ["auto", "gray", "labels", "rgb"] {
                let _ = render_slice(&mut v, plan, plan.n_slices / 2, cmap, &mut scratch);
            }
        }
        // Values and cursor moves on the display grid (resampled or not).
        let g = &plans[0].grid;
        for p in [[0, 0, 0], g.center(), g.shape.map(|n| n - 1)] {
            if let Some(ijk) = g.nearest(g.to_voxel(p)) {
                let _ = v.value_at(ijk.map(|x| x as i64));
                let _ = v.rgb_at(ijk);
            }
            let _ = g.from_world(voxel_to_world(&g.affine, p.map(|x| x as f64)));
        }
    }
    // Spectroscopy: the spectrum of a voxel.
    if v.mrs.is_some() {
        let _ = v.spectrum(v.center());
    }
    let c = v.center();
    if let Ok(Some(x)) = v.value_at([c[0] as i64, c[1] as i64, c[2] as i64]) {
        let _ = v.count_equal(x);
    }
});
