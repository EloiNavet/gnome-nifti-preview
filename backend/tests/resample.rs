//! Oblique images in the anatomical modes are resampled onto world axes: a
//! linear function of the world coordinates comes out of the trilinear
//! interpolation unchanged, colours are interpolated per channel, and the
//! stored mode keeps the voxel grid.

mod common;

use common::{tmpdir, Nifti};
use gnome_nifti_preview::orientation::voxel_to_world;
use gnome_nifti_preview::volume::Volume;

const SHAPE: [usize; 3] = [24, 20, 12];

/// 20° about the left-right axis, voxels of 1.2 × 1.0 × 3 mm.
fn srow() -> [[f64; 4]; 3] {
    let (c, s) = (20f64.to_radians().cos(), 20f64.to_radians().sin());
    [[1.2, 0.0, 0.0, -14.0], [0.0, c, -3.0 * s, -9.0], [0.0, s, 3.0 * c, -12.0]]
}

/// The affine as read back (srow is stored as float32 in NIfTI-1).
fn affine() -> [[f64; 4]; 4] {
    let r = srow();
    let mut a = [[0.0, 0.0, 0.0, 1.0]; 4];
    for i in 0..3 {
        for j in 0..4 {
            a[i][j] = r[i][j] as f32 as f64;
        }
    }
    a[3] = [0.0, 0.0, 0.0, 1.0];
    a
}

fn linear(w: [f64; 3]) -> f64 {
    0.5 * w[0] - 0.25 * w[1] + 0.75 * w[2] + 100.0
}

fn write(name: &str, datatype: i16, voxel: impl Fn([usize; 3]) -> Vec<u8>) -> std::path::PathBuf {
    let mut n = Nifti::new(&SHAPE.map(|x| x as i64), datatype);
    n.srow = srow();
    let mut data = Vec::new();
    for k in 0..SHAPE[2] {
        for j in 0..SHAPE[1] {
            for i in 0..SHAPE[0] {
                data.extend(voxel([i, j, k]));
            }
        }
    }
    let path = tmpdir("resample").join(name);
    std::fs::write(&path, n.file(&data)).unwrap();
    path
}

#[test]
fn linear_function_survives_resampling() {
    let a = affine();
    let path = write("linear.nii", 16, |v| {
        (linear(voxel_to_world(&a, v.map(|x| x as f64))) as f32).to_le_bytes().to_vec()
    });
    let mut vol = Volume::open(&path).unwrap();
    vol.ensure_data().unwrap();
    let plans = vol.plans("neurological").unwrap();
    let grid = &plans[0].grid;
    assert!(grid.resampled);
    assert!((grid.affine[0][0] - 1.0).abs() < 1e-6, "as fine as the smallest voxel");
    assert_eq!(grid.steps, [1, 1, 3], "one scroll step is one 3 mm slice");
    let mut out = Vec::new();
    let mut outside_any = 0;
    for plan in plans.iter() {
        assert!(["Coronal", "Sagittal", "Axial"].contains(&plan.title.as_str()), "{}", plan.title);
        let index = plan.n_slices / 2;
        vol.slice_values(plan, index, false, &mut out).unwrap();
        assert_eq!(out.len(), plan.width * plan.height);
        let (mut inside, mut outside) = (0, 0);
        for row in 0..plan.height {
            for col in 0..plan.width {
                let x = out[row * plan.width + col];
                let g = plan.grid_point(index, col, row);
                let v = grid.to_voxel(g);
                let within = (0..3).all(|c| v[c] >= -0.5 && v[c] <= SHAPE[c] as f64 - 0.5);
                assert_eq!(x.is_finite(), within, "{} {col},{row}: {x} at voxel {v:?}", plan.view);
                if !within {
                    outside += 1;
                    continue;
                }
                inside += 1;
                // Inside the voxel centres, interpolation is exact for a linear function.
                if (0..3).all(|c| v[c] >= 0.0 && v[c] <= (SHAPE[c] - 1) as f64) {
                    let want = linear(voxel_to_world(&grid.affine, g.map(|x| x as f64)));
                    assert!((x as f64 - want).abs() < 1e-3, "{} {col},{row}: {x} != {want}", plan.view);
                }
            }
        }
        assert!(inside > 0, "{}: nothing inside", plan.view);
        outside_any += outside;
    }
    assert!(outside_any > 0, "the corners of the world grid are outside the tilted image");

    // Stored mode: the voxel grid, values as stored.
    let stored = vol.plans("stored").unwrap();
    assert!(!stored[0].grid.resampled);
    let axial = &stored[2];
    assert_eq!((axial.width, axial.height, axial.n_slices), (SHAPE[0], SHAPE[1], SHAPE[2]));
    vol.slice_values(axial, 3, false, &mut out).unwrap();
    let g = axial.grid_point(3, 5, 7);
    let want = linear(voxel_to_world(&a, g.map(|x| x as f64))) as f32;
    assert_eq!(out[7 * axial.width + 5], want);
}

#[test]
fn nearest_voxel_for_labels_and_colours_per_channel() {
    // Labels: every resampled value is one of the stored labels.
    let path = write("labels.nii", 2, |v| vec![(1 + (v[0] / 6 + 4 * (v[2] / 3)) % 7) as u8]);
    let mut vol = Volume::open(&path).unwrap();
    vol.ensure_data().unwrap();
    let plans = vol.plans("radiological").unwrap();
    assert!(plans[0].grid.resampled);
    let mut out = Vec::new();
    for plan in plans.iter() {
        vol.slice_values(plan, plan.n_slices / 2, true, &mut out).unwrap();
        assert!(out.iter().all(|&x| x.is_nan() || (x == x.round() && (1.0..=7.0).contains(&x))), "{}", plan.view);
    }

    // RGB: R is linear in the first voxel index, so it is interpolated exactly.
    let path = write("rgb.nii", 128, |v| vec![(10 + 8 * v[0]) as u8, 200, (v[2] * 20) as u8]);
    let mut vol = Volume::open(&path).unwrap();
    vol.ensure_data().unwrap();
    let plans = vol.plans("neurological").unwrap();
    let grid = &plans[0].grid;
    for plan in plans.iter() {
        let index = plan.n_slices / 2;
        let mut rgb = Vec::new();
        vol.slice_rgb(plan, index, &mut rgb).unwrap();
        assert_eq!(rgb.len(), plan.width * plan.height * 3);
        for row in 0..plan.height {
            for col in 0..plan.width {
                let p = &rgb[(row * plan.width + col) * 3..][..3];
                let v = grid.to_voxel(plan.grid_point(index, col, row));
                if (0..3).all(|c| v[c] >= 0.0 && v[c] <= (SHAPE[c] - 1) as f64) {
                    assert!((p[0] as f64 - (10.0 + 8.0 * v[0])).abs() <= 0.5 + 1e-9, "{p:?} at {v:?}");
                    assert_eq!(p[1], 200);
                } else if !(0..3).all(|c| v[c] >= -0.5 && v[c] <= SHAPE[c] as f64 - 0.5) {
                    assert_eq!(p, [0, 0, 0], "black outside the image");
                }
            }
        }
    }
    // Stored mode: the bytes of the file.
    let stored = vol.plans("stored").unwrap();
    let mut rgb = Vec::new();
    vol.slice_rgb(&stored[2], 4, &mut rgb).unwrap();
    let p = &rgb[(2 * stored[2].width + 3) * 3..][..3];
    let g = stored[2].grid_point(4, 3, 2);
    assert_eq!(p, [(10 + 8 * g[0]) as u8, 200, (g[2] * 20) as u8]);
    assert_eq!(vol.rgb_at(g).unwrap(), Some([(10 + 8 * g[0]) as u8, 200, (g[2] * 20) as u8]));
}

#[test]
fn flat_and_straight_images_are_not_resampled() {
    let mut n = Nifti::new(&[30, 20, 1], 2);
    n.srow = srow();
    let path = tmpdir("resample").join("flat.nii");
    std::fs::write(&path, n.file(&vec![1u8; 600])).unwrap();
    let vol = Volume::open(&path).unwrap();
    assert!(!vol.plans("neurological").unwrap()[0].grid.resampled, "a single tilted slice keeps its grid");

    let path = tmpdir("resample").join("straight.nii");
    std::fs::write(&path, Nifti::new(&[8, 9, 10], 2).file(&[1u8; 720])).unwrap();
    let vol = Volume::open(&path).unwrap();
    let plans = vol.plans("neurological").unwrap();
    assert!(!plans[0].grid.resampled);
    assert!(plans.iter().all(|p| p.slice_step == 1));
}

#[test]
fn protocol_grid_cursor_and_values() {
    use gnome_nifti_preview::server::{respond, Session};
    use serde_json::{json, Value};
    let a = affine();
    let path = write("protocol.nii", 16, |v| {
        (linear(voxel_to_world(&a, v.map(|x| x as f64))) as f32).to_le_bytes().to_vec()
    });
    let mut s = Session::new(0);
    let mut ask = |req: Value| respond(&mut s, req.to_string().as_bytes()).0;
    let open = ask(json!({"id": 1, "op": "open", "path": path}));
    assert_eq!(open["grid"]["resampled"], true, "{open}");
    let shape: Vec<u64> = open["grid"]["shape"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).collect();
    assert_eq!(open["cursor"], json!(shape.iter().map(|n| n / 2).collect::<Vec<_>>()));
    assert_eq!(open["plans"]["axial"]["slice_step"], 3);
    assert!(open["header"]["warnings"][0].as_str().unwrap().contains("resampled"), "{open}");

    let corner = ask(json!({"id": 2, "op": "value", "ijk": [0, 0, 0]}));
    assert_eq!((corner["voxel"].clone(), corner["value"].clone()), (Value::Null, Value::Null), "{corner}");
    let center = ask(json!({"id": 3, "op": "value", "ijk": open["cursor"]}));
    assert!(center["voxel"].is_array() && center["value"].is_f64(), "{center}");
    let out = ask(json!({"id": 4, "op": "value", "ijk": shape}));
    assert_eq!(out["error"]["code"], "bad_request", "{out}");

    // The cursor is carried over: same world point, within half a voxel.
    let world = |reply: &Value, c: &Value| -> [f64; 3] {
        let g = reply["grid"]["affine"].as_array().unwrap();
        let c: Vec<f64> = c.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [0, 1, 2].map(|r| {
            let row: Vec<f64> = g[r].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
            row[0] * c[0] + row[1] * c[1] + row[2] * c[2] + row[3]
        })
    };
    let from = world(&open, &open["cursor"]);
    let stored = ask(json!({"id": 5, "op": "set_display", "mode": "stored", "cursor": open["cursor"]}));
    assert_eq!(stored["grid"]["resampled"], false);
    let to = world(&stored, &stored["cursor"]);
    assert!((0..3).all(|i| (from[i] - to[i]).abs() <= 1.6), "{from:?} -> {to:?}");
    let same = ask(json!({"id": 6, "op": "set_display", "colormap": "gray", "cursor": [3, 4, 5]}));
    assert_eq!(same["cursor"], json!([3, 4, 5]), "same grid, same cursor");
    let clamped = ask(json!({"id": 7, "op": "set_display", "cursor": [-4, 999, 2]}));
    assert_eq!(clamped["cursor"], json!([0, SHAPE[1] - 1, 2]));
    for bad in [json!([1, 2]), json!([1, 2, "3"]), json!([1.5, 2, 3]), json!("x")] {
        let r = ask(json!({"id": 8, "op": "set_display", "cursor": bad}));
        assert_eq!(r["error"]["code"], "bad_request", "{bad}: {r}");
    }
    let load = ask(json!({"id": 9, "op": "load"}));
    assert_eq!(load["colormaps"], json!(["gray"]));
}
