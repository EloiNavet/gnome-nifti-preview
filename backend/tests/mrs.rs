//! NIfTI-MRS: detection, metadata, combination of transients, the
//! chemical-shift axis on synthetic FIDs with known peaks, and hostile
//! extensions.

mod common;

use common::{gz, tmpdir, Nifti};
use gnome_nifti_preview::mrs::Spectrum;
use gnome_nifti_preview::server::{respond, Session};
use gnome_nifti_preview::volume::Volume;
use serde_json::{json, Value};

const SF: f64 = 123.2; // MHz, 1H at 2.9 T
const DWELL: f64 = 1.0 / 2000.0;
/// NAA, creatine, choline.
const PEAKS: [(f64, f64); 3] = [(2.01, 1.0), (3.03, 0.6), (3.21, 0.5)];

/// FID of the peaks (ppm, amplitude) at `sf` MHz, around `centre` ppm.
fn fid(n: usize, sf: f64, centre: f64, peaks: &[(f64, f64)], phase: f64, gain: f64) -> Vec<(f64, f64)> {
    (0..n)
        .map(|t| {
            let time = t as f64 * DWELL;
            let (mut re, mut im) = (0.0, 0.0);
            for &(ppm, a) in peaks {
                let hz = (centre - ppm) * sf;
                let ang = 2.0 * std::f64::consts::PI * hz * time + phase;
                let decay = a * gain * (-time / 0.08).exp();
                re += decay * ang.cos();
                im += decay * ang.sin();
            }
            (re, im)
        })
        .collect()
}

fn c64(samples: &[(f64, f64)]) -> Vec<u8> {
    samples.iter().flat_map(|&(r, i)| [(r as f32).to_le_bytes(), (i as f32).to_le_bytes()].concat()).collect()
}

fn mrs_json(nucleus: &str, sf: f64, extra: Value) -> Vec<u8> {
    let mut j = json!({"SpectrometerFrequency": [sf], "ResonantNucleus": [nucleus], "EchoTime": 0.03});
    if let (Value::Object(a), Value::Object(b)) = (&mut j, extra) {
        a.extend(b);
    }
    j.to_string().into_bytes()
}

fn svs(dims: &[i64], json: Vec<u8>) -> Nifti {
    let mut n = Nifti::new(dims, 32); // complex64
    n.version = 2;
    n.pixdim[4] = DWELL;
    n.intent_name = b"mrs_v0_3".to_vec();
    n.extensions = vec![(44, json)];
    n.xyzt_units = 2 | 8;
    n
}

fn write(name: &str, bytes: Vec<u8>) -> std::path::PathBuf {
    let p = tmpdir("mrs").join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn axis(s: &Spectrum, i: usize) -> f64 {
    s.first + (s.last - s.first) * i as f64 / (s.real.len() - 1) as f64
}

#[test]
fn explicit_chemical_reference_places_zero_frequency_at_the_declared_ppm() {
    for nucleus in ["1H", "13C"] {
        for reference in [json!(0.0), json!(2.0), json!(-1.5), Value::Null, json!("invalid")] {
            let n = svs(&[1, 1, 1, 64], mrs_json(nucleus, 100.0, json!({"SpecFreqChemShift": reference})));
            let path = write("reference.nii", n.file(&c64(&[(1.0, 0.0); 64])));
            let s = Volume::open(&path).unwrap().spectrum([0, 0, 0]).unwrap();
            let peak = s.magnitude.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
            let expected = reference.as_f64().unwrap_or(if nucleus == "1H" { 4.65 } else { 0.0 });
            assert_eq!(peak, 32);
            assert!((axis(&s, peak) - expected).abs() < 1e-12, "{nucleus} {reference}");
        }
    }
}

#[test]
fn standard_nucleus_names_and_zero_echo_time_keep_their_metadata() {
    for (nucleus, gamma) in [("3HE", 32.434), ("7LI", 16.546), ("23NA", 11.262), ("129XE", 11.777), ("23Na", 11.262)] {
        let n = svs(&[1, 1, 1, 16], mrs_json(nucleus, 3.0 * gamma, json!({"EchoTime":0.0})));
        let path = write("nucleus.nii", n.file(&c64(&[(1.0, 0.0); 16])));
        let m = Volume::open(&path).unwrap().header_info()["mrs"].clone();
        assert!((m["field_t"].as_f64().unwrap() - 3.0).abs() < 1e-12, "{m}");
        assert_eq!(m["nucleus"], nucleus);
        assert_eq!(m["echo_time_s"], 0.0);
    }
    for te in [Value::Null, json!(-0.1), json!("invalid")] {
        let n = svs(&[1, 1, 1, 16], mrs_json("1H", SF, json!({"EchoTime":te})));
        let path = write("timing.nii", n.file(&c64(&[(1.0, 0.0); 16])));
        assert_eq!(Volume::open(&path).unwrap().header_info()["mrs"]["echo_time_s"], Value::Null);
    }
}

#[test]
fn metadata_counts_transients_without_materializing_indices() {
    let huge = 1i64 << (usize::BITS - 16);
    let cases = [
        // Invalid JSON uses the default coil/dynamic dimension tags, as in
        // the compressed NIfTI-2 input that failed the CI fuzzing job.
        (b"{".to_vec(), 2 * huge as usize),
        (mrs_json("1H", SF, json!({"dim_5": "DIM_COIL", "dim_6": "DIM_EDIT"})), huge as usize),
        (mrs_json("1H", SF, json!({"dim_5": "DIM_EDIT", "dim_6": "DIM_DYN"})), 2),
    ];
    for (extra, combined) in cases {
        for big_endian in [false, true] {
            for compressed in [false, true] {
                let mut n = svs(&[1, 1, 1, 64, huge, 2], extra.clone());
                n.big_endian = big_endian;
                // Only volume 0 exists. Its map can load; the full spectrum
                // must be refused by the configured data-size limit.
                let bytes = n.file(&[0; 8]);
                let path = write("large-dimensions.nii", if compressed { gz(&bytes) } else { bytes });
                let mut v = Volume::open_with_limit(&path, 64 << 20).unwrap();
                let info = v.header_info();
                assert_eq!(info["mrs"]["combined"], combined);
                assert_eq!(info["mrs"]["transients"], 2 * huge as usize);
                assert!(v.stats().is_ok());
                assert_eq!(v.spectrum([0, 0, 0]).unwrap_err().code, "too_large");
            }
        }
    }
}

#[test]
fn metadata_counts_saturate_on_dimension_product_overflow() {
    let huge = (usize::MAX / 2) as i64;
    let n = svs(&[1, 1, 1, 64, huge, huge, 2], mrs_json("1H", SF, json!({"dim_7": "DIM_MEAS"})));
    let path = write("overflow-dimensions.nii", n.file(&[0; 8]));
    let mut v = Volume::open_with_limit(&path, 64 << 20).unwrap();
    let info = v.header_info();
    assert_eq!(info["mrs"]["combined"], usize::MAX);
    assert_eq!(info["mrs"]["transients"], usize::MAX);
    assert_eq!(v.spectrum([0, 0, 0]).unwrap_err().code, "too_large");
}

/// The highest local maxima of `values` within [lo, hi], as axis values.
fn peaks(s: &Spectrum, values: &[f32], lo: f64, hi: f64, count: usize) -> Vec<f64> {
    let mut idx: Vec<usize> = (1..values.len() - 1)
        .filter(|&i| (lo..=hi).contains(&axis(s, i)) && values[i] > values[i - 1] && values[i] >= values[i + 1])
        .collect();
    idx.sort_by(|&a, &b| values[b].total_cmp(&values[a]));
    let mut out: Vec<f64> = idx.into_iter().take(count).map(|i| axis(s, i)).collect();
    out.sort_by(f64::total_cmp);
    out
}

#[test]
fn single_voxel_coils_and_dynamics_combine_into_upright_peaks() {
    // 4 coils with different phases and gains (two cancel out on a plain
    // average) x 3 dynamics.
    let (n, coils, dyns) = (1024, 4, 3);
    let phases = [0.3, 0.3 + std::f64::consts::PI, 1.9, -2.4];
    let gains = [1.0, 1.0, 0.5, 0.2];
    let mut data = Vec::new();
    for _ in 0..dyns {
        for c in 0..coils {
            data.extend(fid(n, SF, 4.65, &PEAKS, phases[c], gains[c]));
        }
    }
    let nifti = svs(&[1, 1, 1, n as i64, coils as i64, dyns as i64], mrs_json("1H", SF, json!({"dim_5": "DIM_COIL", "dim_6": "DIM_DYN"})));
    let path = write("svs.nii.gz", gz(&nifti.file(&c64(&data))));
    let mut v = Volume::open(&path).unwrap();
    let info = v.header_info();
    let m = &info["mrs"];
    assert_eq!(m["nucleus"], "1H", "{info}");
    assert_eq!(m["points"], n);
    assert_eq!(m["transients"], coils * dyns);
    assert_eq!(m["single_voxel"], true);
    assert_eq!(m["dims"], json!([{"tag": "DIM_COIL", "size": 4}, {"tag": "DIM_DYN", "size": 3}]));
    assert!((m["field_t"].as_f64().unwrap() - SF / 42.577478).abs() < 1e-9);
    assert!((m["bandwidth_hz"].as_f64().unwrap() - 2000.0).abs() < 1e-6);
    assert_eq!(info["repetition_time"], Value::Null, "pixdim[4] is the dwell time");
    assert!(info["warnings"].as_array().unwrap().is_empty(), "{}", info["warnings"]);

    let s = v.spectrum([0, 0, 0]).unwrap();
    assert_eq!((s.unit, s.default_part, s.transients), ("ppm", "real", 12));
    assert_eq!(s.default_range, Some([0.2, 4.2]));
    assert!(s.first > s.last, "high shifts on the left");
    assert!((s.first - (4.65 + 1000.0 / SF)).abs() < 1e-9);
    let found = peaks(&s, &s.real, 0.2, 4.2, 3);
    assert_eq!(found.len(), PEAKS.len(), "peaks {found:?}");
    for ((want, _), got) in PEAKS.iter().zip(&found) {
        assert!((want - got).abs() < 0.02, "peaks {found:?}");
    }
    // Phased: the peaks point up. Combined: the opposite coils do not cancel.
    let top = s.real.iter().cloned().fold(f32::MIN, f32::max);
    let bottom = s.real.iter().cloned().fold(f32::MAX, f32::min);
    assert!(top > 0.0 && top > -3.0 * bottom, "top {top}, bottom {bottom}");
    let single = {
        let one = svs(&[1, 1, 1, n as i64], mrs_json("1H", SF, json!({})));
        let p = write("one.nii", one.file(&c64(&fid(n, SF, 4.65, &PEAKS, 0.0, 1.0))));
        Volume::open(&p).unwrap().spectrum([0, 0, 0]).unwrap()
    };
    let single_top = single.real.iter().cloned().fold(f32::MIN, f32::max);
    assert!(top > 0.7 * single_top, "combined {top} vs one coil {single_top}");

    let stats = v.stats().unwrap();
    assert!(stats.notes.is_empty(), "{:?}", stats.notes);
}

#[test]
fn spectroscopic_imaging_gives_a_spectrum_per_voxel() {
    // 31P, 3 x 2 x 2 voxels: the PCr-like peak moves with the voxel.
    let (n, sf) = (256, 49.86);
    let mut data = vec![(0.0, 0.0); 3 * 2 * 2 * n];
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..3 {
                let s = x + 3 * (y + 2 * z);
                let peak = -10.0 + 1.5 * s as f64;
                for (t, v) in fid(n, sf, 0.0, &[(peak, 1.0)], 0.0, 1.0 + 0.1 * s as f64).into_iter().enumerate() {
                    data[s + 12 * t] = v;
                }
            }
        }
    }
    let mut nifti = svs(&[3, 2, 2, n as i64], mrs_json("31P", sf, json!({})));
    nifti.intent_name.clear(); // detected by the extension alone
    let path = write("mrsi.nii", nifti.file(&c64(&data)));
    let mut v = Volume::open(&path).unwrap();
    assert!(v.mrs.is_some());
    assert_eq!(v.header_info()["mrs"]["single_voxel"], false);
    for voxel in [[0, 0, 0], [2, 1, 1], [1, 0, 1]] {
        let s = v.spectrum(voxel).unwrap();
        assert_eq!((s.unit, s.default_part, s.default_range), ("ppm", "magnitude", None));
        let index = voxel[0] + 3 * (voxel[1] + 2 * voxel[2]);
        let want = -10.0 + 1.5 * index as f64;
        let got = peaks(&s, &s.magnitude, -30.0, 30.0, 1);
        assert!((got[0] - want).abs() < 0.3, "voxel {voxel:?}: {got:?}, want {want}");
    }
    // The maps show the first time point.
    let stats = v.stats().unwrap();
    assert_eq!(stats.notes, ["Maps show the magnitude of the first time point."]);
    assert!(v.spectrum([3, 0, 0]).is_err());
}

#[test]
fn axis_without_frequency_or_dwell_time() {
    let n = 64;
    let data = c64(&fid(n, SF, 4.65, &PEAKS, 0.0, 1.0));
    let path = write("hz.nii", svs(&[1, 1, 1, n as i64], b"{}".to_vec()).file(&data));
    let s = Volume::open(&path).unwrap().spectrum([0, 0, 0]).unwrap();
    assert_eq!((s.unit, s.default_part), ("hz", "magnitude"));
    assert!((s.first - 1000.0).abs() < 1e-9);
    let mut nodwell = svs(&[1, 1, 1, n as i64], b"{}".to_vec());
    nodwell.pixdim[4] = 0.0;
    let path = write("points.nii", nodwell.file(&data));
    let s = Volume::open(&path).unwrap().spectrum([0, 0, 0]).unwrap();
    assert_eq!((s.unit, s.first, s.last), ("points", 0.0, (n - 1) as f64));
}

#[test]
fn big_endian_complex128_without_orientation() {
    let n = 128;
    let mut nifti = svs(&[1, 1, 1, n as i64, 2], mrs_json("1H", SF, json!({"dim_5": "DIM_USER_0"})));
    nifti.datatype = 1792; // complex128
    nifti.bitpix = 128;
    nifti.sform_code = 0;
    nifti.qform_code = 0;
    nifti = nifti.be();
    let samples: Vec<(f64, f64)> = [fid(n, SF, 4.65, &PEAKS, 0.4, 1.0), fid(n, SF, 4.65, &PEAKS, 0.4, 1.0)].concat();
    let data: Vec<u8> = samples.iter().flat_map(|&(r, i)| [r.to_be_bytes(), i.to_be_bytes()].concat()).collect();
    let path = write("be.nii", nifti.file(&data));
    let mut v = Volume::open(&path).unwrap();
    let info = v.header_info();
    assert_eq!(info["mrs"]["dims"], json!([{"tag": "DIM_USER_0", "size": 2}]));
    // No orientation warning for a single voxel; only the user dimension's.
    let w = info["warnings"].as_array().unwrap();
    assert!(w.len() == 1 && w[0].as_str().unwrap().contains("index 0 of DIM_USER_0 (2)"), "{w:?}");
    let s = v.spectrum([0, 0, 0]).unwrap();
    let found = peaks(&s, &s.real, 0.2, 4.2, 1);
    assert!((found[0] - 2.01).abs() < 0.1, "{found:?}");
}

#[test]
fn not_spectroscopy_and_hostile_extensions() {
    // Real-valued data marked "mrs_v": an ordinary 4D image.
    let mut n = Nifti::new(&[2, 2, 2, 3], 16);
    n.intent_name = b"mrs_v0_3".to_vec();
    let path = write("real.nii", n.file(&[0u8; 2 * 2 * 2 * 3 * 4]));
    let v = Volume::open(&path).unwrap();
    assert!(v.mrs.is_none());
    assert!(v.browsable(), "an ordinary 4D image: its volumes can be browsed");

    let samples = c64(&fid(32, SF, 4.65, &PEAKS, 0.0, 1.0));
    // Invalid JSON: still spectroscopy (intent_name), without the metadata.
    let path = write("badjson.nii", svs(&[1, 1, 1, 32], b"{not json".to_vec()).file(&samples));
    let mut v = Volume::open(&path).unwrap();
    let m = v.mrs.clone().unwrap();
    assert_eq!((m.nucleus, m.spectrometer_mhz), (None, None));
    assert_eq!(v.spectrum([0, 0, 0]).unwrap().unit, "hz");

    // Extension sizes that lie: ignored, the data still load.
    for (esize, name) in [(17i32, "odd.nii"), (1 << 30, "huge.nii"), (-16, "negative.nii")] {
        let nifti = svs(&[1, 1, 1, 32], mrs_json("1H", SF, json!({})));
        let mut bytes = nifti.file(&samples);
        bytes[544..548].copy_from_slice(&esize.to_le_bytes());
        let mut v = Volume::open(&write(name, bytes)).unwrap();
        assert!(v.mrs.is_some(), "{name}");
        assert!(v.mrs.as_ref().unwrap().spectrometer_mhz.is_none(), "{name}");
        assert_eq!(v.spectrum([0, 0, 0]).unwrap().real.len(), 32, "{name}");
    }

    // Extensions running past the read limit: a warning, nothing else.
    let mut far = svs(&[1, 1, 1, 32], mrs_json("1H", SF, json!({})));
    far.vox_offset = Some((2 << 20) as f64);
    let mut bytes = far.header();
    bytes.resize(2 << 20, 0);
    bytes.extend_from_slice(&samples);
    let mut v = Volume::open(&write("far.nii", bytes)).unwrap();
    assert!(v.header.warnings.iter().any(|w| w.contains("extensions beyond")), "{:?}", v.header.warnings);
    assert_eq!(v.mrs.as_ref().unwrap().spectrometer_mhz, Some(SF));
    assert!(v.spectrum([0, 0, 0]).is_ok());
}

#[test]
fn limits() {
    // Data above the image limit: the map loads, the spectrum is refused.
    let n = 8192;
    let nifti = svs(&[2, 2, 2, n as i64, 4], mrs_json("1H", SF, json!({})));
    let data = vec![0u8; 2 * 2 * 2 * n * 4 * 8];
    let path = write("big.nii", nifti.file(&data));
    let mut v = Volume::open_with_limit(&path, 1 << 20).unwrap();
    assert!(v.stats().is_ok(), "the maps of volume 0 fit");
    let e = v.spectrum([0, 0, 0]).unwrap_err();
    assert_eq!(e.code, "too_large", "{}", e.message);
    assert!(e.message.contains("spectroscopy data"), "{}", e.message);

    // Truncated after volume 0: the map shows, the spectrum says why not.
    let nifti = svs(&[2, 2, 2, 64, 2], mrs_json("1H", SF, json!({})));
    let mut bytes = nifti.file(&vec![1u8; 2 * 2 * 2 * 64 * 2 * 8]);
    bytes.truncate(bytes.len() / 2);
    let mut v = Volume::open(&write("cut.nii", bytes)).unwrap();
    assert!(v.stats().is_ok());
    assert_eq!(v.spectrum([0, 0, 0]).unwrap_err().code, "corrupt_data");

    // Too many points for a spectrum.
    let long = svs(&[1, 1, 1, 1 << 17], mrs_json("1H", SF, json!({})));
    let path = write("long.nii", long.file(&vec![0u8; (1 << 17) * 8]));
    assert_eq!(Volume::open(&path).unwrap().spectrum([0, 0, 0]).unwrap_err().code, "too_large");

    // In the order of a preview (open, load, spectrum), a spectrum known to be
    // too long does not make load read every FID: 2 MiB of zeros, a few KiB
    // compressed.
    let long = svs(&[1, 1, 1, (1 << 16) + 1, 4], mrs_json("1H", SF, json!({})));
    let path = write("long.nii.gz", gz(&long.file(&vec![0u8; ((1 << 16) + 1) * 4 * 8])));
    let mut v = Volume::open(&path).unwrap();
    assert!(v.stats().is_ok());
    assert!(v.loaded_bytes().unwrap() < 64 << 10, "{:?} bytes read", v.loaded_bytes());
    let e = v.spectrum([0, 0, 0]).unwrap_err();
    assert!(e.code == "too_large" && e.message.contains("too long"), "{}", e.message);
}

#[test]
fn protocol() {
    let samples = c64(&fid(64, SF, 4.65, &PEAKS, 0.0, 1.0));
    let path = write("proto.nii", svs(&[1, 1, 1, 64], mrs_json("1H", SF, json!({}))).file(&samples));
    let t1 = write("t1.nii", Nifti::new(&[4, 4, 4], 2).file(&[1u8; 64]));
    let mut s = Session::new(0);
    let mut ask = |req: Value| respond(&mut s, req.to_string().as_bytes()).0;
    assert_eq!(ask(json!({"id": 1, "op": "open", "path": t1}))["ok"], true);
    assert_eq!(ask(json!({"id": 2, "op": "spectrum"}))["error"]["code"], "bad_request");
    let open = ask(json!({"id": 3, "op": "open", "path": path}));
    assert_eq!(open["header"]["mrs"]["nucleus"], "1H", "{open}");
    let r = ask(json!({"id": 4, "op": "spectrum"}));
    assert_eq!((r["ok"].clone(), r["unit"].clone(), r["points"].clone()), (json!(true), json!("ppm"), json!(64)), "{r}");
    assert!(r.get("bytes").is_none(), "a JSON reply, not a binary slice");
    assert_eq!(r["real"].as_array().unwrap().len(), 64);
    let again = ask(json!({"id": 5, "op": "spectrum", "ijk": [0, 0, 0]}));
    assert_eq!(again["real"], r["real"]);
    for bad in [json!([1, 0, 0]), json!([0, 0]), json!("x")] {
        assert_eq!(ask(json!({"id": 6, "op": "spectrum", "ijk": bad}))["error"]["code"], "bad_request", "{bad}");
    }
}

/// Edit conditions, ISIS steps, phase cycles, indirect and user dimensions
/// hold different signals (an edited spectrum is a difference): the spectrum
/// uses their index 0 instead of averaging them, and says so.
#[test]
fn encoded_dimensions_use_their_first_index() {
    let n = 512;
    // A = outer + target, B = outer - target: averaging keeps the outer signal.
    let a = fid(n, SF, 4.65, &[(3.03, 10.0), (2.01, 1.0)], 0.4, 1.0);
    let b = fid(n, SF, 4.65, &[(3.03, 10.0), (2.01, -1.0)], 0.4, 1.0);
    let spectrum = |dims: &[i64], json: Value, data: &[(f64, f64)]| {
        let path = write(&format!("enc{}.nii", data.len()), svs(dims, mrs_json("1H", SF, json)).file(&c64(data)));
        let mut v = Volume::open(&path).unwrap();
        let info = v.header_info();
        (v.spectrum([0, 0, 0]).unwrap(), info)
    };
    let (alone, _) = spectrum(&[1, 1, 1, n as i64], json!({}), &a);
    let same = |x: &Spectrum, y: &Spectrum| {
        let scale = y.real.iter().fold(0f32, |m, v| m.max(v.abs()));
        x.real.iter().zip(&y.real).all(|(p, q)| (p - q).abs() <= 1e-5 * scale)
    };
    let both: Vec<(f64, f64)> = a.iter().chain(&b).copied().collect();
    for tag in ["DIM_EDIT", "DIM_ISIS", "DIM_PHASE_CYCLE", "DIM_INDIRECT_0", "DIM_USER_0"] {
        let (s, info) = spectrum(&[1, 1, 1, n as i64, 2], json!({ "dim_5": tag }), &both);
        assert_eq!(s.transients, 1, "{tag}");
        assert!(same(&s, &alone), "{tag}: not the spectrum of index 0");
        let m = &info["mrs"];
        assert_eq!((m["transients"].as_u64(), m["combined"].as_u64()), (Some(2), Some(1)), "{tag}");
        assert_eq!(m["first_index_only"], json!([{"tag": tag, "size": 2}]));
        let w = info["warnings"].to_string();
        assert!(w.contains(&format!("index 0 of {tag} (2)")), "{w}");
    }
    // Coils are averaged: the same pair, as two coils, cancels the target.
    let (coils, info) = spectrum(&[1, 1, 1, n as i64, 2], json!({"dim_5": "DIM_COIL"}), &both);
    assert_eq!((coils.transients, info["mrs"]["combined"].as_u64()), (2, Some(2)));
    assert!(!same(&coils, &alone));
    assert!(info["warnings"].as_array().unwrap().is_empty(), "{}", info["warnings"]);
    // Coils x edit conditions: every coil of condition 0.
    let mut data = Vec::new();
    data.extend(a.iter().chain(&a).chain(&a)); // 3 coils, condition 0
    data.extend(b.iter().chain(&b).chain(&b)); // 3 coils, condition 1
    let (s, info) = spectrum(&[1, 1, 1, n as i64, 3, 2], json!({"dim_5": "DIM_COIL", "dim_6": "DIM_EDIT"}), &data);
    assert_eq!((s.transients, info["mrs"]["combined"].as_u64()), (3, Some(3)));
    assert!(same(&s, &alone));
}
