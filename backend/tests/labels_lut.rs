//! Label colour tables: FreeSurfer's for FreeSurfer-like segmentations
//! (names in the readout and the list), the generic palette otherwise, a
//! user table in FreeSurferColorLUT format, and the list of labels with
//! their volumes.

mod common;

use common::{gz, tmpdir, Nifti};
use gnome_nifti_preview::server::{respond, Session};
use serde_json::{json, Value};

fn request(s: &mut Session, req: Value) -> (Value, Option<Vec<u8>>) {
    let (h, p) = respond(s, req.to_string().as_bytes());
    (h, p.map(|p| p.to_vec()))
}

/// Blocks of labels along x (8 voxels each), 2 mm isotropic voxels.
fn segmentation(dir: &std::path::Path, name: &str, ids: &[i16]) -> std::path::PathBuf {
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
    let p = dir.join(name);
    std::fs::write(&p, gz(&n.file(&data))).unwrap();
    p
}

#[test]
fn freesurfer_names_colours_and_list() {
    let dir = tmpdir("lut-fs");
    let p = segmentation(&dir, "aseg.nii.gz", &[0, 2, 17, 41, 53, 1035]);
    let mut s = Session::new(1 << 20);
    request(&mut s, json!({"op": "open", "path": p, "mode": "stored"}));
    let (load, _) = request(&mut s, json!({"op": "load"}));
    assert_eq!(load["stats"]["labels"]["scheme"], "freesurfer", "{load}");
    let (v, _) = request(&mut s, json!({"op": "value", "ijk": [8 * 2 + 3, 2, 2]}));
    assert_eq!(v["value"], 17);
    assert_eq!(v["label_name"], "Left-Hippocampus");
    // The pixels of label 17 have FreeSurfer's colour.
    let (_, px) = request(&mut s, json!({"op": "slice", "view": "axial", "index": 2}));
    let px = px.unwrap();
    assert!(px.chunks(3).any(|c| c == [220, 216, 20]), "Left-Hippocampus colour");
    let (list, _) = request(&mut s, json!({"op": "labels"}));
    let labels = list["labels"].as_array().unwrap();
    assert_eq!(labels.len(), 5);
    assert_eq!(labels[1], json!({"label": 17, "name": "Left-Hippocampus", "rgb": [220, 216, 20], "voxels": 8 * 6 * 5, "volume_mm3": 8.0 * 240.0}));
    assert_eq!(labels[4]["name"], "ctx-lh-insula");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn generic_palette_user_table_and_masks() {
    let dir = tmpdir("lut-user");
    // A tissue map (1 CSF, 2 GM, 3 WM) is not FreeSurfer.
    let p = segmentation(&dir, "tissues.nii.gz", &[0, 1, 2, 3]);
    let mut s = Session::new(1 << 20);
    request(&mut s, json!({"op": "open", "path": p}));
    assert_eq!(request(&mut s, json!({"op": "load"})).0["stats"]["labels"]["scheme"], "random");
    let (v, _) = request(&mut s, json!({"op": "value", "ijk": [12, 2, 2]}));
    assert_eq!(v["value"], 1);
    assert!(v.get("label_name").is_none());
    // The same with the user's table (names and colours).
    let lut = dir.join("tissues.txt");
    std::fs::write(&lut, "# tissues\n1 CSF 0 0 255 0\n2 Grey-matter 128 128 128 0\n3 White-matter 255 255 255 0\n").unwrap();
    let mut s = Session::new(1 << 20);
    request(&mut s, json!({"op": "open", "path": p}));
    let (load, _) = request(&mut s, json!({"op": "load", "label_lut": lut}));
    assert_eq!(load["stats"]["labels"]["scheme"], "custom", "{load}");
    assert_eq!(request(&mut s, json!({"op": "value", "ijk": [12, 2, 2]})).0["label_name"], "CSF");
    let (list, _) = request(&mut s, json!({"op": "labels"}));
    assert_eq!(list["labels"][2]["rgb"], json!([255, 255, 255]));
    // Forced schemes, and refusals.
    let mut s = Session::new(1 << 20);
    request(&mut s, json!({"op": "open", "path": p}));
    assert_eq!(request(&mut s, json!({"op": "load", "label_colors": "freesurfer"})).0["stats"]["labels"]["scheme"], "freesurfer");
    for bad in [json!({"op": "load", "label_colors": "pastel"}), json!({"op": "load", "label_lut": dir.join("missing.txt")})] {
        let (r, _) = request(&mut s, bad);
        assert_eq!(r["ok"], false);
    }
    // A mask stays white; an intensity image has no labels.
    let m = segmentation(&dir, "mask.nii.gz", &[0, 1]);
    let mut s = Session::new(1 << 20);
    request(&mut s, json!({"op": "open", "path": m}));
    assert_eq!(request(&mut s, json!({"op": "load"})).0["stats"]["labels"]["scheme"], "binary");
    assert_eq!(request(&mut s, json!({"op": "labels"})).0["labels"][0]["rgb"], json!([255, 255, 255]));
    std::fs::remove_dir_all(&dir).ok();
}

/// A small labelled blob far from the centre of a 40³ volume.
fn sparse(dir: &std::path::Path) -> std::path::PathBuf {
    let n = Nifti::new(&[40, 40, 40], 2);
    let mut data = vec![0u8; 40 * 40 * 40];
    for k in 4..7 {
        for j in 30..33 {
            for i in 5..8 {
                data[i + 40 * (j + 40 * k)] = 1 + (i % 2) as u8;
            }
        }
    }
    let p = dir.join("sparse.nii.gz");
    std::fs::write(&p, gz(&n.file(&data))).unwrap();
    p
}

#[test]
fn sparse_segmentations_start_at_their_labels() {
    let dir = tmpdir("lut-sparse");
    let p = sparse(&dir);
    let mut s = Session::new(1 << 20);
    request(&mut s, json!({"op": "open", "path": p, "mode": "stored"}));
    let (r, _) = request(&mut s, json!({"op": "load"}));
    assert_eq!(r["label_cursor"], json!([6, 31, 5]), "{r}");
    // Labels through the centre: the cursor stays.
    let q = segmentation(&dir, "dense.nii.gz", &[1, 2, 3, 4]);
    request(&mut s, json!({"op": "open", "path": q}));
    let (r, _) = request(&mut s, json!({"op": "load"}));
    assert!(r.get("label_cursor").is_none(), "{r}");
    // The thumbnail cuts through the labels.
    let mut v = gnome_nifti_preview::volume::Volume::open(&p).unwrap();
    let pic = gnome_nifti_preview::thumbnail::render(&mut v, &Default::default()).unwrap();
    assert!(pic.rgb.chunks(3).any(|c| c != [0, 0, 0]), "a label is in the thumbnail");
}

#[test]
fn surface_data_are_named() {
    let dir = tmpdir("lut-surface");
    // CIFTI-2 dense scalar: NIfTI-2, intent 3006, the values in the 6th dimension.
    let mut n = Nifti::new(&[1, 1, 1, 1, 1, 500], 16).v2();
    n.intent = 3006;
    n.intent_name = b"ConnDenseScalar".to_vec();
    let p = dir.join("x.dscalar.nii");
    std::fs::write(&p, n.file(&vec![0u8; 500 * 4])).unwrap();
    let e = gnome_nifti_preview::volume::Volume::open(&p).err().unwrap();
    assert_eq!(e.code, "unsupported_format");
    assert!(e.message.contains("CIFTI-2") && e.message.contains("ConnDenseScalar"), "{e}");
    // One value per vertex of a surface.
    let n = Nifti::new(&[2562, 1, 1], 16);
    let p = dir.join("lh.thickness.nii");
    std::fs::write(&p, n.file(&vec![0u8; 2562 * 4])).unwrap();
    let e = gnome_nifti_preview::volume::Volume::open(&p).err().unwrap();
    assert!(e.message.contains("2562 values"), "{e}");
    // FreeSurfer's layout of the 163842 vertices of fsaverage.
    let n = Nifti::new(&[27307, 1, 6], 16);
    let p = dir.join("pval.lh.nii");
    std::fs::write(&p, n.file(&vec![0u8; 163842 * 4])).unwrap();
    let e = gnome_nifti_preview::volume::Volume::open(&p).err().unwrap();
    assert!(e.message.contains("163842 values"), "{e}");
    // What is known of the file goes with the refusal, for the panel.
    let about = e.about.as_ref().expect("about").to_string();
    assert!(about.contains("\"Dimensions\",\"27307 × 1 × 6\"") && about.contains("float32"), "{about}");
}
