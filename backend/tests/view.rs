//! The viewer helpers shared by the Sushi protocol and the terminal viewer
//! (`view`), called directly: start and label cursors, readouts, the list of
//! labels, cursors carried across display modes, single-slice images, the
//! time of a volume, loading on a helper thread and the slice cache.

mod common;

use std::sync::Arc;

use common::{gz, tmpdir, within, Nifti};
use gnome_nifti_preview::view::{self, LoadState, Loading, SliceCache};
use gnome_nifti_preview::volume::Volume;
use gnome_nifti_preview::voxel::VoxelValue;

/// Blocks of labels along x (8 voxels each), 2 mm isotropic voxels.
fn segmentation(dir: &std::path::Path, ids: &[i16]) -> std::path::PathBuf {
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

/// A ramp (value = x + 10 y + 100 z) on an affine rotated by 30° about z.
fn oblique(dir: &std::path::Path) -> std::path::PathBuf {
    let mut n = Nifti::new(&[10, 10, 6], 16);
    let (c, s) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
    n.srow = [[c, -s, 0.0, 0.0], [s, c, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
    let mut data = Vec::new();
    for k in 0..6 {
        for j in 0..10 {
            for i in 0..10 {
                data.extend_from_slice(&((i + 10 * j + 100 * k) as f32).to_le_bytes());
            }
        }
    }
    let p = dir.join("oblique.nii");
    std::fs::write(&p, n.file(&data)).unwrap();
    p
}

#[test]
fn readout_names_the_structure_and_its_volume() {
    let dir = tmpdir("view-readout");
    let mut vol = Volume::open(&segmentation(&dir, &[0, 2, 17, 41, 53])).unwrap();
    let grid = vol.plans("stored").unwrap()[0].grid.clone();
    let r = view::readout(&mut vol, &grid, [8 * 2 + 3, 2, 2]).unwrap();
    assert_eq!(r.value, Some(VoxelValue::Integer(17)));
    assert!(r.categorical);
    assert_eq!(r.voxel, Some([19, 2, 2]));
    assert_eq!(r.world_mm, [38.0, 4.0, 4.0]);
    let label = r.label.expect("a label");
    assert_eq!(label.name.as_deref(), Some("Left-Hippocampus"));
    assert_eq!(label.voxels, 8 * 6 * 5);
    assert_eq!(label.volume_mm3, Some(8.0 * 240.0));
    // The background has no structure; outside the grid is refused.
    let r = view::readout(&mut vol, &grid, [1, 1, 1]).unwrap();
    assert_eq!((r.value, r.label), (Some(VoxelValue::Integer(0)), None));
    // Outside the grid: the protocol's error, word for word.
    for ijk in [[40, 0, 0], [-1, 0, 0]] {
        let e = view::readout(&mut vol, &grid, ijk).unwrap_err();
        assert_eq!(e.code, "bad_request");
        assert_eq!(e.message, format!("Voxel {ijk:?} outside image [40, 6, 5]"));
        assert_eq!(view::voxel_under(&grid, ijk).unwrap_err().message, e.message);
    }

    let list = view::label_rows(&mut vol).unwrap();
    assert_eq!((list.complete, list.scheme), (true, "freesurfer"));
    let names: Vec<_> = list.rows.iter().map(|r| r.name.as_deref().unwrap_or("")).collect();
    assert_eq!(names, ["Left-Cerebral-White-Matter", "Left-Hippocampus", "Right-Cerebral-White-Matter", "Right-Hippocampus"]);
    assert_eq!(list.rows[1].rgb, [220, 216, 20]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn intensity_images_have_no_labels_and_resampled_corners_no_voxel() {
    let dir = tmpdir("view-oblique");
    let mut vol = Volume::open(&oblique(&dir)).unwrap();
    assert_eq!(view::label_rows(&mut vol).unwrap(), view::LabelList { rows: Vec::new(), complete: true, scheme: "" });
    let plans = vol.plans("neurological").unwrap();
    let grid = plans[0].grid.clone();
    assert!(grid.resampled);
    // A corner of the world-aligned grid lies outside the rotated image.
    let r = view::readout(&mut vol, &grid, [0, 0, 0]).unwrap();
    assert_eq!((r.voxel, r.value, r.label), (None, None, None));
    // The centre of the grid is a voxel of the image, with its value.
    let c = grid.center().map(|x| x as i64);
    let r = view::readout(&mut vol, &grid, c).unwrap();
    let v = r.voxel.expect("inside");
    assert_eq!(r.value, Some(VoxelValue::Float((v[0] + 10 * v[1] + 100 * v[2]) as f64)));
    assert!(!r.categorical);
    // The nearest voxel to the grid point's own voxel coordinates.
    let vc = grid.to_voxel(grid.center());
    assert_eq!(v, vc.map(|x| x.round() as usize));

    // Carried to the stored grid and back, the cursor stays at the same place.
    let stored = vol.plans("stored").unwrap()[0].grid.clone();
    let on_stored = view::carry_cursor(&grid, &stored, c);
    assert_eq!(Some(on_stored), grid.voxel_at(grid.center()));
    let back = view::carry_cursor(&stored, &grid, on_stored.map(|x| x as i64));
    assert!((0..3).all(|a| back[a].abs_diff(grid.center()[a]) <= 1), "{back:?} vs {:?}", grid.center());
    // Same kind of grid: unchanged, clamped to the grid.
    assert_eq!(view::carry_cursor(&stored, &stored, [3, 4, 5]), [3, 4, 5]);
    assert_eq!(view::carry_cursor(&stored, &stored, [-7, 99, 2]), [0, 9, 2]);
    assert_eq!(view::start_cursor(&vol, &stored), [5, 5, 3]);
    assert_eq!(view::label_cursor(&mut vol, &stored).unwrap(), None);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn single_slices_and_volume_times() {
    let dir = tmpdir("view-single");
    let p = dir.join("flat.nii");
    std::fs::write(&p, Nifti::new(&[7, 5, 1], 2).file(&[9u8; 35])).unwrap();
    let vol = Volume::open(&p).unwrap();
    assert_eq!(view::single_view(&vol, &vol.plans("neurological").unwrap()), Some("axial"));
    let p = dir.join("cube.nii");
    std::fs::write(&p, Nifti::new(&[3, 3, 3], 2).file(&[1u8; 27])).unwrap();
    let vol = Volume::open(&p).unwrap();
    assert_eq!(view::single_view(&vol, &vol.plans("neurological").unwrap()), None);

    let mut n = Nifti::new(&[2, 2, 2, 4], 2);
    n.pixdim[4] = 2.5;
    let p = dir.join("bold.nii");
    std::fs::write(&p, n.file(&[0u8; 32])).unwrap();
    let vol = Volume::open(&p).unwrap();
    assert_eq!(view::volume_time(&vol.header, 3), Some(7.5));
    assert_eq!(view::volume_time(&Volume::open(&dir.join("cube.nii")).unwrap().header, 3), None);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn sparse_labels_and_loading_on_a_helper_thread() {
    let dir = tmpdir("view-load");
    let mut data = vec![0u8; 40 * 40 * 40];
    for k in 4..7 {
        for j in 30..33 {
            for i in 5..8 {
                data[i + 40 * (j + 40 * k)] = 1 + (i % 2) as u8;
            }
        }
    }
    let p = dir.join("sparse.nii.gz");
    std::fs::write(&p, gz(&Nifti::new(&[40, 40, 40], 2).file(&data))).unwrap();
    let vol = Box::new(Volume::open(&p).unwrap());
    let job = Loading::start(vol).unwrap_or_else(|_| panic!("no thread"));
    let mut vol = within(30, "loading", move || loop {
        match job.poll() {
            LoadState::Done(v, result) => {
                result.unwrap();
                break v;
            }
            LoadState::Running => std::thread::sleep(std::time::Duration::from_millis(2)),
            LoadState::Lost => panic!("load thread lost"),
        }
    });
    assert!(vol.data_loaded() && vol.stats_if_loaded().is_some());
    let grid = vol.plans("stored").unwrap()[0].grid.clone();
    assert_eq!(view::label_cursor(&mut vol, &grid).unwrap(), Some([6, 31, 5]));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn slice_cache_keeps_the_most_recent_within_its_budget() {
    let slice = |n: usize| (Arc::new(vec![0u8; n]), 1, 1);
    let mut c = SliceCache::new(3 * (1000 + 256));
    for i in 0..3 {
        c.put(("stored", "gray", "axial", i, 0), slice(1000));
    }
    assert_eq!(c.len(), 3);
    assert!(c.get(&("stored", "gray", "axial", 0, 0)).is_some()); // now the most recent
    c.put(("stored", "gray", "axial", 3, 0), slice(1000));
    assert_eq!(c.len(), 3);
    assert!(c.get(&("stored", "gray", "axial", 1, 0)).is_none(), "the least recent went first");
    assert!(c.get(&("stored", "gray", "axial", 0, 0)).is_some());
    c.put(("stored", "gray", "axial", 9, 0), slice(10_000)); // larger than the budget
    assert!(c.get(&("stored", "gray", "axial", 9, 0)).is_none());
    c.clear();
    assert!(c.is_empty());
}

#[test]
fn rgb_readouts_series_errors_and_volume_times() {
    let dir = tmpdir("view-misc");
    // RGB24: the colour of the voxel, not a label.
    let n = Nifti::new(&[2, 2, 1], 128);
    let rgb: Vec<u8> = (0..12).map(|i| (i * 20) as u8).collect();
    let p = dir.join("rgb.nii");
    std::fs::write(&p, n.file(&rgb)).unwrap();
    let mut vol = Volume::open(&p).unwrap();
    let grid = vol.plans("stored").unwrap()[0].grid.clone();
    let r = view::readout(&mut vol, &grid, [1, 1, 0]).unwrap();
    assert_eq!((r.rgb, r.categorical, r.label), (Some([180, 200, 220]), false, None));
    // Series of an image that is not DICOM, and before the catalog is read.
    let e = view::series_catalog(&mut vol).unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("bad_request", "Not a DICOM image"));
    assert_eq!(view::open_series(&vol, "1.2").err().unwrap().message, "Not a DICOM image");
    // A repetition time that is not a time.
    for tr in [0.0, -2.0, f64::NAN] {
        let mut n = Nifti::new(&[2, 2, 2, 3], 2);
        n.pixdim[4] = tr;
        let p = dir.join("tr.nii");
        std::fs::write(&p, n.file(&[0u8; 24])).unwrap();
        assert_eq!(view::volume_time(&Volume::open(&p).unwrap().header, 2), None, "TR {tr}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn loading_reports_its_progress() {
    let dir = tmpdir("view-progress");
    let p = dir.join("big.nii.gz");
    std::fs::write(&p, gz(&Nifti::new(&[64, 64, 64], 4).file(&vec![1u8; 64 * 64 * 64 * 2]))).unwrap();
    let job = Loading::start(Box::new(Volume::open(&p).unwrap())).unwrap_or_else(|_| panic!("no thread"));
    assert!(!job.cancelled());
    assert!(job.rate().is_none(), "no rate in the first 0.2 s");
    let done = within(30, "loading", move || loop {
        assert!((0.0..=1.0).contains(&job.fraction()));
        match job.poll() {
            LoadState::Done(v, r) => break (v, r),
            LoadState::Running => std::thread::sleep(std::time::Duration::from_millis(2)),
            LoadState::Lost => panic!("load thread lost"),
        }
    });
    done.1.unwrap();
    assert!(done.0.data_loaded());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn slice_cache_replaces_and_refuses_without_losing_others() {
    let slice = |n: usize| (Arc::new(vec![0u8; n]), 1, 1);
    let key = |i| ("stored", "gray", "axial", i, 0);
    let mut c = SliceCache::new(3 * (1000 + 256));
    c.put(key(0), slice(1000));
    c.put(key(1), slice(1000));
    // The same key again: replaced, its old size given back.
    c.put(key(0), slice(500));
    c.put(key(2), slice(400));
    assert_eq!(c.len(), 3, "756 + 1256 + 656 bytes fit");
    assert_eq!(c.get(&key(0)).unwrap().0.len(), 500);
    // Larger than the budget: refused, the others stay.
    c.put(key(9), slice(5000));
    assert_eq!(c.len(), 3);
    // One more slice: only the least recent (1) goes, had key 0 been
    // counted twice, two would have gone.
    c.put(key(3), slice(1000));
    assert!(c.get(&key(1)).is_none());
    assert!(c.get(&key(0)).is_some() && c.get(&key(2)).is_some() && c.get(&key(3)).is_some());
}
