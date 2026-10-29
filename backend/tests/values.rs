//! What the readout reports: label identifiers kept exact, coordinates in mm
//! whatever the unit of the header, and a backend that answers with an error
//! instead of aborting when memory is short.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use common::{tmpdir, Nifti, TestFile};
use gnome_nifti_preview::orientation::build_plans;
use gnome_nifti_preview::render::render_slice;
use gnome_nifti_preview::server::{respond, Session};
use gnome_nifti_preview::volume::Volume;
use serde_json::{json, Value};

fn write(name: &str, bytes: Vec<u8>) -> TestFile {
    let p = tmpdir("values").file(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

/// The physical values fit float32 even when the stored factors do not.
#[test]
fn scaling_compensates_before_reducing_display_precision() {
    for (version, dtype, raw_unit, base_slope) in [(1, 64, 1e40, 1e-40), (2, 64, 1e-300, 1e300), (2, 16, 1e-40, 1e40)] {
        for be in [false, true] {
            for sign in [-1.0, 1.0] {
                let slope = sign * base_slope;
                let mut n = Nifti::new(&[2, 2, 2], dtype);
                n.version = version;
                n.big_endian = be;
                n.slope = slope;
                n.inter = 10.0;
                // NIfTI-1 stores its slope as float32, NIfTI-2 as float64.
                let stored_slope = if version == 1 { slope as f32 as f64 } else { slope };
                let raw_value = |i| {
                    let v = i as f64 * raw_unit;
                    if dtype == 16 { v as f32 as f64 } else { v }
                };
                let expected: Vec<f32> = (1..=8).map(|i| (raw_value(i) * stored_slope + 10.0) as f32).collect();
                let data: Vec<u8> = (1..=8).flat_map(|i| {
                    let x = raw_value(i);
                    if dtype == 16 {
                        if be { (x as f32).to_be_bytes().to_vec() } else { (x as f32).to_le_bytes().to_vec() }
                    } else if be { x.to_be_bytes().to_vec() } else { x.to_le_bytes().to_vec() }
                }).collect();
                let path = write("compensated.nii", n.file(&data));
                let mut v = Volume::open(&path).unwrap();
                for (i, &want) in expected.iter().enumerate() {
                    let got = v.exact_value_at([(i % 2) as i64, ((i / 2) % 2) as i64, (i / 4) as i64]).unwrap().unwrap();
                    assert_eq!(got.as_f64(), want as f64);
                }
                let plans = v.plans("stored").unwrap();
                let mut samples = Vec::new();
                v.slice_values(&plans[0], 0, false, &mut samples).unwrap();
                assert!(samples.iter().all(|x| x.is_finite() && *x >= 2.0 && *x <= 18.0), "{samples:?}");
                let (pixels, _) = render_slice(&mut v, &plans[0], 0, "gray", &mut Vec::new()).unwrap();
                assert!(pixels.iter().any(|&x| x != 0));
                let mut session = Session::new(0);
                respond(&mut session, json!({"op":"open","path":*path}).to_string().as_bytes());
                let load = respond(&mut session, br#"{"op":"load"}"#).0;
                assert_eq!(load["stats"]["window"]["status"], "ok", "{load}");
                assert_eq!(load["stats"]["non_finite"], 0);
                assert_eq!(load["stats"]["min"].as_f64(), expected.iter().copied().reduce(f32::min).map(|x| x as f64));
                assert_eq!(load["stats"]["max"].as_f64(), expected.iter().copied().reduce(f32::max).map(|x| x as f64));
            }
        }
    }
}

/// Labels 2^24 and 2^24 + 1 are the same float32: they must stay distinct.
#[test]
fn large_label_identifiers_stay_distinct() {
    const A: u32 = 1 << 24;
    const B: u32 = (1 << 24) + 1;
    let mut data = Vec::new();
    for z in 0..4 {
        for _y in 0..4 {
            for x in 0..4 {
                let v: u32 = if z == 0 { 0 } else if x < 2 { A } else { B };
                data.extend(v.to_le_bytes());
            }
        }
    }
    let path = write("labels.nii", Nifti::new(&[4, 4, 4], 768).file(&data));
    let mut s = Session::new(0);
    let mut ask = |req: Value| respond(&mut s, req.to_string().as_bytes()).0;
    ask(json!({"id": 1, "op": "open", "path": *path}));
    let load = ask(json!({"id": 2, "op": "load"}));
    assert_eq!(load["stats"]["labels"]["n_labels"], 2, "{load}");
    let a = ask(json!({"id": 3, "op": "value", "ijk": [0, 0, 2]}));
    let b = ask(json!({"id": 4, "op": "value", "ijk": [3, 0, 2]}));
    assert_eq!((a["value"].as_f64(), b["value"].as_f64()), (Some(A as f64), Some(B as f64)), "{a} {b}");
    assert_eq!((a["label_voxels"].as_u64(), b["label_voxels"].as_u64()), (Some(24), Some(24)), "{a} {b}");
    assert_eq!(a["label_volume_mm3"], 24.0);

    // Each label keeps its own colour.
    let mut v = Volume::open(&path).unwrap();
    let plans = build_plans(&[[1.0, 0., 0., 0.], [0., 1.0, 0., 0.], [0., 0., 1.0, 0.], [0., 0., 0., 1.]], [4, 4, 4], "stored", true).unwrap();
    let axial = plans.iter().find(|p| p.view == "axial").unwrap();
    let (rgb, cmap) = render_slice(&mut v, axial, 2, "auto", &mut Vec::new()).unwrap();
    assert_eq!(cmap, "labels");
    let colours: std::collections::BTreeSet<[u8; 3]> = rgb.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    assert_eq!(colours.len(), 2, "{colours:?}");
}

/// Expectations are integer constants, not values obtained through the
/// reference's float conversions. Exercise both formats, byte orders and gzip.
#[test]
fn exact_scalar_labels_and_metadata() {
    for (dtype, a, b) in [(768, 1i128 << 24, (1 << 24) + 1),
        (1024, 1 << 53, (1 << 53) + 1),
        (1024, i64::MAX as i128 - 1, i64::MAX as i128),
        (1280, u64::MAX as i128 - 1, u64::MAX as i128),
        (64, 1 << 24, (1 << 24) + 1)] {
        for version in [1, 2] {
            for be in [false, true] {
                for compressed in [false, true] {
                    let mut n = Nifti::new(&[2, 2, 2], dtype);
                    n.version = version;
                    n.big_endian = be;
                    let data: Vec<u8> = (0..8).flat_map(|i| {
                        let v = if i % 2 == 0 { a } else { b };
                        match dtype {
                            768 => if be { (v as u32).to_be_bytes().to_vec() } else { (v as u32).to_le_bytes().to_vec() },
                            64 => if be { (v as f64).to_be_bytes().to_vec() } else { (v as f64).to_le_bytes().to_vec() },
                            _ => if be { (v as u64).to_be_bytes().to_vec() } else { (v as u64).to_le_bytes().to_vec() },
                        }
                    }).collect();
                    let bytes = n.file(&data);
                    let bytes = if compressed { common::gz(&bytes) } else { bytes };
                    let path = write(if compressed { "exact.nii.gz" } else { "exact.nii" }, bytes);
                    let wire = |v: i128| if v > (1 << 53) - 1 { json!(v.to_string()) } else if dtype == 64 { json!(v as f64) } else { json!(v as u64) };
                    let mut s = Session::new(0);
                    let mut ask = |req: Value| respond(&mut s, req.to_string().as_bytes()).0;
                    let opened = ask(json!({"id":1,"op":"open","path":*path,"mode":"stored"}));
                    assert_eq!(opened["ok"], true, "{opened}");
                    let load = ask(json!({"id":2,"op":"load"}));
                    assert_eq!(load["stats"]["labels"]["n_labels"], 2, "{load}");
                    assert_eq!(load["stats"]["min"], wire(a), "{load}");
                    assert_eq!(load["stats"]["max"], wire(b), "{load}");
                    assert!(!load["stats"]["notes"].as_array().unwrap().iter().any(|v| v.as_str().unwrap().contains("Constant volume")), "{load}");
                    for (x, v) in [(0, a), (1, b)] {
                        let reply = ask(json!({"id":3,"op":"value","ijk":[x,0,0]}));
                        assert_eq!(reply["value"], wire(v), "{reply}");
                        assert_eq!(reply["label_voxels"], 4, "{reply}");
                        assert_eq!(reply["label_volume_mm3"], 4.0, "{reply}");
                    }
                    let mut v = Volume::open(&path).unwrap();
                    let plans = v.plans("stored").unwrap();
                    let (rgb, cmap) = render_slice(&mut v, &plans[0], 0, "auto", &mut Vec::new()).unwrap();
                    assert_eq!(cmap, "labels");
                    let colours: std::collections::BTreeSet<_> = rgb.chunks_exact(3).collect();
                    assert_eq!(colours.len(), 2, "{dtype} v{version} be={be} gzip={compressed}");
                }
            }
        }
    }
}

#[test]
fn signed_integer_limits_and_float64_readout_are_exact() {
    use gnome_nifti_preview::voxel::VoxelValue;
    let values = [i64::MIN, i64::MIN + 1, -(1 << 53) - 1, -(1 << 53)];
    let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let path = write("negative.nii", Nifti::new(&[4, 1, 1], 1024).file(&data));
    let mut v = Volume::open(&path).unwrap();
    for (i, &expected) in values.iter().enumerate() {
        let actual = v.exact_value_at([i as i64, 0, 0]).unwrap().unwrap();
        assert_eq!(actual, VoxelValue::Integer(expected as i128));
        assert_eq!(actual.to_json(), expected.to_string());
        assert_eq!(v.count_equal(actual).unwrap(), 1);
    }
    let values = [1.0000000001f64, 1.0000000002];
    let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let path = write("float64.nii", Nifti::new(&[2, 1, 1], 64).file(&data));
    let mut v = Volume::open(&path).unwrap();
    for (i, &expected) in values.iter().enumerate() {
        let actual = v.exact_value_at([i as i64, 0, 0]).unwrap().unwrap();
        assert_eq!(actual.as_f64(), expected);
        assert_eq!(v.count_equal(actual).unwrap(), 1);
    }
}

/// The same grid in mm, m and µm: the readout gives the same position in mm.
#[test]
fn world_coordinates_are_in_mm() {
    let mut worlds = Vec::new();
    for (unit, per_mm) in [(2u8, 1.0), (1, 1e-3), (3, 1e3)] {
        let mut n = Nifti::new(&[3, 3, 3], 2);
        n.xyzt_units = unit | 8;
        n.srow = [[per_mm, 0., 0., 5.0 * per_mm], [0., per_mm, 0., 6.0 * per_mm], [0., 0., per_mm, 7.0 * per_mm]];
        n.pixdim[1..4].copy_from_slice(&[per_mm; 3]);
        let path = write(&format!("units{unit}.nii"), n.file(&[1u8; 27]));
        let mut s = Session::new(0);
        let mut ask = |req: Value| respond(&mut s, req.to_string().as_bytes()).0;
        ask(json!({"id": 1, "op": "open", "path": *path}));
        let r = ask(json!({"id": 2, "op": "value", "ijk": [1, 1, 1]}));
        let w: Vec<f64> = r["world"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        worlds.push(w);
    }
    for w in &worlds {
        assert!(w.iter().zip([6.0, 7.0, 8.0]).all(|(a, b)| (a - b).abs() < 1e-3), "{worlds:?}");
    }
}

#[test]
fn render_rejects_an_unknown_view() {
    let path = write("view.nii", Nifti::new(&[3, 3, 3], 2).file(&[1u8; 27]));
    let out = path.with_file_name("should-not-exist.png");
    let r = Command::new(env!("CARGO_BIN_EXE_nifti-preview"))
        .args(["render", path.to_str().unwrap(), "--view", "typo", "-o", out.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&r.stderr).contains("--view"));
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, ["view.nii"], "nothing written");
}

#[repr(C)]
struct Rlimit {
    cur: u64,
    max: u64,
}

extern "C" {
    fn setrlimit(resource: i32, rlim: *const Rlimit) -> i32;
}

/// Started under a memory limit tighter than its own (inherited from a
/// parent), the backend loading a 64 MiB image answers, with the data or an
/// error, and keeps answering: it does not abort on an auxiliary allocation.
#[test]
#[cfg(target_os = "linux")]
fn short_of_memory_answers_instead_of_aborting() {
    use std::os::unix::process::CommandExt;
    let path = write("big.nii", Nifti::new(&[256, 256, 1024], 2).file(&vec![7u8; 256 * 256 * 1024]));
    for mib in [72u64, 80, 88, 104] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nifti-preview"));
        cmd.arg("serve").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let limit = mib << 20;
        // SAFETY: setrlimit is async-signal-safe and only lowers this child's limit.
        unsafe {
            cmd.pre_exec(move || {
                let r = Rlimit { cur: limit, max: limit };
                if setrlimit(2, &r) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).unwrap(); // hello
        let mut ask = |req: Value| -> Value {
            writeln!(stdin, "{req}").unwrap();
            let mut line = String::new();
            out.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("{mib} MiB: no reply ({e}): {line:?}"))
        };
        ask(json!({"id": 1, "op": "open", "path": *path}));
        let load = ask(json!({"id": 2, "op": "load"}));
        assert!(load["ok"] == true || load["error"]["code"] == "too_large", "{mib} MiB: {load}");
        assert_eq!(ask(json!({"id": 3, "op": "ping"}))["ok"], true, "{mib} MiB");
        drop(stdin);
        let status = child.wait().unwrap();
        assert!(status.success(), "{mib} MiB: {status:?}");
    }
}

/// `dump` writes the stored values in little endian, as its JSON says,
/// whatever the byte order of the file (big-endian files were copied as they
/// are); each component of a complex value is swapped, RGB bytes are not.
#[test]
fn dump_writes_little_endian() {
    let floats = [1.5f32, -2.25, 1e30, 7.0, 0.0, -0.5, 3.0, 9.75];
    let shorts = [1i16, -2, 300, -30000, 7, 0, 12345, -1];
    let cases: Vec<(i16, Vec<u8>, Vec<u8>)> = vec![
        (16, floats.iter().flat_map(|x| x.to_be_bytes()).collect(), floats.iter().flat_map(|x| x.to_le_bytes()).collect()),
        (4, shorts.iter().flat_map(|x| x.to_be_bytes()).collect(), shorts.iter().flat_map(|x| x.to_le_bytes()).collect()),
        // complex64: (re, im) pairs of float32.
        (32, floats.iter().chain(&floats).flat_map(|x| x.to_be_bytes()).collect(), floats.iter().chain(&floats).flat_map(|x| x.to_le_bytes()).collect()),
        (128, (0..24u8).collect(), (0..24u8).collect()),
    ];
    for (datatype, stored, expected) in cases {
        for big in [true, false] {
            let mut n = Nifti::new(&[2, 2, 2], datatype);
            let data = if big {
                n = n.be();
                stored.clone()
            } else {
                expected.clone()
            };
            let path = write(&format!("dump-{datatype}-{big}.nii"), n.file(&data));
            let out = path.with_extension("raw");
            let r = Command::new(env!("CARGO_BIN_EXE_nifti-preview")).args(["dump"]).arg(&*path).arg(&out).output().unwrap();
            assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
            let json: Value = serde_json::from_slice(&r.stdout).unwrap();
            assert_eq!(json["endian"], "little");
            assert_eq!(std::fs::read(&out).unwrap(), expected, "datatype {datatype}, big endian {big}");
        }
    }
}
