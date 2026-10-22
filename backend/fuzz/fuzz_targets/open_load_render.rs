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

fuzz_target!(|data: &[u8]| {
    let path = std::env::temp_dir().join(format!("np-fuzz-{}.nii", std::process::id()));
    std::fs::write(&path, data).unwrap();
    let Ok(mut v) = Volume::open_with_limit(&path, 64 << 20) else { return };
    let _ = v.header_info();
    if v.stats().is_err() {
        return;
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
