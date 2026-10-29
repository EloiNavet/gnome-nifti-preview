//! DICOM: every transfer syntax a test can write (implicit and explicit VR,
//! both byte orders, deflated, RLE, lossless JPEG) decodes to the same
//! values; geometry (LPS to RAS, oblique, series rebuilt from a folder,
//! 4D, missing slices, gantry tilt), multi-frame and enhanced images,
//! mosaics, colour, MONOCHROME1, 1-bit images, DICOMDIR, the series
//! requests of the protocol, and hostile files.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};

use common::{tmpdir, within};
use gnome_nifti_preview::formats::dicom::Options;
use gnome_nifti_preview::server::{respond, Session};
use gnome_nifti_preview::source::ReadOptions;
use gnome_nifti_preview::volume::Volume;
use serde_json::{json, Value};

// ------------------------------------------------------------ a writer

#[derive(Clone)]
enum V {
    Str(String),
    U16(Vec<u16>),
    U32(Vec<u32>),
    Raw(Vec<u8>),
    Seq(Vec<Vec<El>>),
    Fragments(Vec<Vec<u8>>),
}

#[derive(Clone)]
struct El {
    tag: u32,
    vr: [u8; 2],
    v: V,
}

const fn t(g: u16, e: u16) -> u32 {
    ((g as u32) << 16) | e as u32
}

fn s(tag: u32, vr: &[u8; 2], v: &str) -> El {
    El { tag, vr: *vr, v: V::Str(v.to_string()) }
}
fn us(tag: u32, v: &[u16]) -> El {
    El { tag, vr: *b"US", v: V::U16(v.to_vec()) }
}
fn ul(tag: u32, v: &[u32]) -> El {
    El { tag, vr: *b"UL", v: V::U32(v.to_vec()) }
}
fn ds(tag: u32, v: &[f64]) -> El {
    s(tag, b"DS", &v.iter().map(|x| format!("{x}")).collect::<Vec<_>>().join("\\"))
}
fn is(tag: u32, v: i64) -> El {
    s(tag, b"IS", &v.to_string())
}
fn ui(tag: u32, v: &str) -> El {
    s(tag, b"UI", v)
}
fn sq(tag: u32, items: Vec<Vec<El>>) -> El {
    El { tag, vr: *b"SQ", v: V::Seq(items) }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Ts {
    ImplicitLe,
    ExplicitLe,
    ExplicitBe,
    Deflated,
    Rle,
    JpegLossless,
}

impl Ts {
    fn uid(self) -> &'static str {
        match self {
            Ts::ImplicitLe => "1.2.840.10008.1.2",
            Ts::ExplicitLe => "1.2.840.10008.1.2.1",
            Ts::ExplicitBe => "1.2.840.10008.1.2.2",
            Ts::Deflated => "1.2.840.10008.1.2.1.99",
            Ts::Rle => "1.2.840.10008.1.2.5",
            Ts::JpegLossless => "1.2.840.10008.1.2.4.70",
        }
    }
}

fn long(vr: &[u8; 2]) -> bool {
    matches!(vr, b"OB" | b"OW" | b"OF" | b"OD" | b"SQ" | b"UN" | b"UT" | b"UC" | b"UR" | b"OL" | b"OV")
}

fn value_bytes(v: &V, vr: &[u8; 2], big: bool) -> Vec<u8> {
    match v {
        V::Str(x) => {
            let mut b = x.as_bytes().to_vec();
            if b.len() % 2 == 1 {
                b.push(if vr == b"UI" { 0 } else { b' ' });
            }
            b
        }
        V::U16(x) => x.iter().flat_map(|v| if big { v.to_be_bytes() } else { v.to_le_bytes() }).collect(),
        V::U32(x) => x.iter().flat_map(|v| if big { v.to_be_bytes() } else { v.to_le_bytes() }).collect(),
        V::Raw(x) => {
            let mut b = x.clone();
            if b.len() % 2 == 1 {
                b.push(0);
            }
            b
        }
        _ => unreachable!(),
    }
}

fn encode(els: &[El], explicit: bool, big: bool, out: &mut Vec<u8>) {
    let mut els = els.to_vec();
    els.sort_by_key(|e| e.tag);
    let u16b = |v: u16| if big { v.to_be_bytes() } else { v.to_le_bytes() };
    let u32b = |v: u32| if big { v.to_be_bytes() } else { v.to_le_bytes() };
    for e in &els {
        out.extend_from_slice(&u16b((e.tag >> 16) as u16));
        out.extend_from_slice(&u16b(e.tag as u16));
        let (body, undefined) = match &e.v {
            V::Seq(items) => {
                let mut b = Vec::new();
                for item in items {
                    let mut ib = Vec::new();
                    encode(item, explicit, big, &mut ib);
                    b.extend_from_slice(&u16b(0xFFFE));
                    b.extend_from_slice(&u16b(0xE000));
                    b.extend_from_slice(&u32b(ib.len() as u32));
                    b.extend_from_slice(&ib);
                }
                (b, false)
            }
            V::Fragments(frags) => {
                let mut b = Vec::new();
                for f in std::iter::once(&Vec::new()).chain(frags) {
                    let mut f = f.clone();
                    if f.len() % 2 == 1 {
                        f.push(0);
                    }
                    b.extend_from_slice(&u16b(0xFFFE));
                    b.extend_from_slice(&u16b(0xE000));
                    b.extend_from_slice(&u32b(f.len() as u32));
                    b.extend_from_slice(&f);
                }
                b.extend_from_slice(&u16b(0xFFFE));
                b.extend_from_slice(&u16b(0xE0DD));
                b.extend_from_slice(&[0; 4]);
                (b, true)
            }
            v => (value_bytes(v, &e.vr, big), false),
        };
        let len = if undefined { 0xFFFF_FFFF } else { body.len() as u32 };
        if explicit {
            out.extend_from_slice(&e.vr);
            if long(&e.vr) {
                out.extend_from_slice(&[0, 0]);
                out.extend_from_slice(&u32b(len));
            } else {
                out.extend_from_slice(&u16b(len as u16));
            }
        } else {
            out.extend_from_slice(&u32b(len));
        }
        out.extend_from_slice(&body);
    }
}

/// A Part 10 file.
fn file(ts: Ts, els: &[El]) -> Vec<u8> {
    let mut meta = Vec::new();
    encode(&[El { tag: t(2, 1), vr: *b"OB", v: V::Raw(vec![0, 1]) }, ui(t(2, 2), "1.2.840.10008.5.1.4.1.1.4"), ui(t(2, 0x10), ts.uid())], true, false, &mut meta);
    let mut out = vec![0u8; 128];
    out.extend_from_slice(b"DICM");
    let mut group = Vec::new();
    encode(&[ul(t(2, 0), &[meta.len() as u32])], true, false, &mut group);
    out.extend_from_slice(&group);
    out.extend_from_slice(&meta);
    let (explicit, big) = match ts {
        Ts::ImplicitLe => (false, false),
        Ts::ExplicitBe => (true, true),
        _ => (true, false),
    };
    let mut body = Vec::new();
    encode(els, explicit, big, &mut body);
    if ts == Ts::Deflated {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&body).unwrap();
        body = e.finish().unwrap();
    }
    out.extend_from_slice(&body);
    out
}

// --------------------------------------------------------- the images

const ROWS: usize = 5;
const COLS: usize = 7;

fn value(i: usize, j: usize, k: usize, tt: usize) -> i16 {
    (i + 10 * j + 100 * k + 1000 * tt) as i16 - 500
}

#[derive(Clone)]
struct Slice {
    k: usize,
    t: usize,
    ipp: [f64; 3],
    iop: [f64; 6],
    series: String,
    instance: String,
}

impl Slice {
    fn new(k: usize, tt: usize) -> Slice {
        Slice {
            k,
            t: tt,
            ipp: [10.0, 20.0, 30.0 + 3.0 * k as f64],
            iop: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            series: "1.2.3.4.5".into(),
            instance: format!("1.2.3.4.5.{k}.{tt}"),
        }
    }
}

fn pixels_le(k: usize, tt: usize) -> Vec<u8> {
    let mut b = Vec::new();
    for j in 0..ROWS {
        for i in 0..COLS {
            b.extend_from_slice(&value(i, j, k, tt).to_le_bytes());
        }
    }
    b
}

fn header(sl: &Slice) -> Vec<El> {
    vec![
        ui(t(8, 0x16), "1.2.840.10008.5.1.4.1.1.4"),
        ui(t(8, 0x18), &sl.instance),
        s(t(8, 0x60), b"CS", "MR"),
        s(t(8, 0x70), b"LO", "ACME"),
        s(t(8, 0x103E), b"LO", "T1 test"),
        s(t(0x10, 0x10), b"PN", "Doe^Jane"),
        ui(t(0x20, 0x0D), "1.2.3.4"),
        ui(t(0x20, 0x0E), &sl.series),
        is(t(0x20, 0x11), 3),
        is(t(0x20, 0x12), 1 + sl.t as i64),
        is(t(0x20, 0x13), (1 + sl.k + 10 * sl.t) as i64),
        ds(t(0x20, 0x32), &sl.ipp),
        ds(t(0x20, 0x37), &sl.iop),
        us(t(0x28, 0x02), &[1]),
        s(t(0x28, 0x04), b"CS", "MONOCHROME2"),
        us(t(0x28, 0x10), &[ROWS as u16]),
        us(t(0x28, 0x11), &[COLS as u16]),
        ds(t(0x28, 0x30), &[0.5, 0.25]),
        us(t(0x28, 0x100), &[16]),
        us(t(0x28, 0x101), &[16]),
        us(t(0x28, 0x102), &[15]),
        us(t(0x28, 0x103), &[1]),
        ds(t(0x18, 0x50), &[3.0]),
        ds(t(0x18, 0x80), &[2000.0]),
        ds(t(0x28, 0x1050), &[100.0]),
        ds(t(0x28, 0x1051), &[401.0]),
    ]
}

/// PackBits with literal runs only.
fn packbits(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in b.chunks(128) {
        out.push((chunk.len() - 1) as u8);
        out.extend_from_slice(chunk);
    }
    out
}

fn rle_frame(samples_le16: &[u8]) -> Vec<u8> {
    let hi: Vec<u8> = samples_le16.chunks(2).map(|c| c[1]).collect();
    let lo: Vec<u8> = samples_le16.chunks(2).map(|c| c[0]).collect();
    let (a, b) = (packbits(&hi), packbits(&lo));
    let mut header = [0u32; 16];
    header[0] = 2;
    header[1] = 64;
    header[2] = 64 + a.len() as u32;
    let mut out: Vec<u8> = header.iter().flat_map(|v| v.to_le_bytes()).collect();
    out.extend_from_slice(&a);
    out.extend_from_slice(&b);
    out
}

/// Lossless JPEG (process 14, predictor 1, 16 bits, one component): every
/// difference category coded on 5 bits.
fn jpeg_lossless(width: usize, height: usize, samples: &[u16]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    let mut counts = [0u8; 16];
    counts[4] = 17;
    out.extend_from_slice(&[0xFF, 0xC4, 0, 2 + 1 + 16 + 17, 0x00]);
    out.extend_from_slice(&counts);
    out.extend(0..17u8);
    out.extend_from_slice(&[0xFF, 0xC3, 0, 11, 16, 0, height as u8, 0, width as u8, 1, 1, 0x11, 0]);
    out.extend_from_slice(&[0xFF, 0xDA, 0, 8, 1, 1, 0x00, 1, 0, 0]);
    let mut bits: Vec<bool> = Vec::new();
    let px = |x: usize, y: usize| samples[y * width + x] as i32;
    for y in 0..height {
        for x in 0..width {
            let pred = if x == 0 && y == 0 { 1 << 15 } else if y == 0 { px(x - 1, y) } else if x == 0 { px(x, y - 1) } else { px(x - 1, y) };
            let d = (px(x, y) - pred).rem_euclid(65536);
            let d = if d >= 32768 { d - 65536 } else { d };
            let cat = if d == -32768 { 16 } else if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() };
            for k in (0..5).rev() {
                bits.push((cat >> k) & 1 == 1);
            }
            if cat < 16 {
                let v = if d < 0 { d + (1 << cat) - 1 } else { d } as u32;
                for k in (0..cat).rev() {
                    bits.push((v >> k) & 1 == 1);
                }
            }
        }
    }
    while bits.len() % 8 != 0 {
        bits.push(true);
    }
    for chunk in bits.chunks(8) {
        let b = chunk.iter().fold(0u8, |a, &bit| (a << 1) | bit as u8);
        out.push(b);
        if b == 0xFF {
            out.push(0);
        }
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

fn slice_file(sl: &Slice, ts: Ts) -> Vec<u8> {
    let mut els = header(sl);
    let le = pixels_le(sl.k, sl.t);
    let pixel = match ts {
        Ts::ExplicitBe => El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(le.chunks(2).flat_map(|c| [c[1], c[0]]).collect()) },
        Ts::Rle => El { tag: t(0x7FE0, 0x10), vr: *b"OB", v: V::Fragments(vec![rle_frame(&le)]) },
        Ts::JpegLossless => {
            let samples: Vec<u16> = le.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            El { tag: t(0x7FE0, 0x10), vr: *b"OB", v: V::Fragments(vec![jpeg_lossless(COLS, ROWS, &samples)]) }
        }
        _ => El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(le) },
    };
    els.push(pixel);
    file(ts, &els)
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn open(path: &Path, series: bool) -> gnome_nifti_preview::error::Result<Volume> {
    Volume::open_with(path, 1 << 30, ReadOptions { dicom: Options { series } })
}

fn request(s: &mut Session, req: Value) -> Value {
    respond(s, req.to_string().as_bytes()).0
}

fn assert_values(v: &mut Volume, k: usize, kk: usize, tt: usize) {
    for j in 0..ROWS {
        for i in 0..COLS {
            let got = v.value_at([i as i64, j as i64, kk as i64]).unwrap().unwrap();
            assert_eq!(got, value(i, j, k, tt) as f64, "voxel ({i}, {j}, {kk}) of slice {k}, volume {tt}");
        }
    }
}

// ---------------------------------------------------------------- tests

#[test]
fn every_syntax_decodes_the_same() {
    let dir = tmpdir("dicom-syntaxes");
    for ts in [Ts::ImplicitLe, Ts::ExplicitLe, Ts::ExplicitBe, Ts::Deflated, Ts::Rle, Ts::JpegLossless] {
        let p = write(&dir, &format!("{ts:?}.dcm"), &slice_file(&Slice::new(2, 0), ts));
        let mut v = open(&p, false).unwrap_or_else(|e| panic!("{ts:?}: {e}"));
        assert_eq!(v.header.shape, vec![COLS, ROWS, 1], "{ts:?}");
        assert_eq!(v.header.dtype.label(), "int16");
        assert_values(&mut v, 2, 0, 0);
        let info = v.header_info();
        assert_eq!(info["format"], "DICOM");
        // LPS (10, 20, 36) → RAS (-10, -20, 36); columns 0.25 mm, rows 0.5 mm.
        assert_eq!(v.affine[0], [-0.25, 0.0, 0.0, -10.0], "{ts:?}");
        assert_eq!(v.affine[1], [0.0, -0.5, 0.0, -20.0]);
        assert_eq!(v.affine[2][3], 36.0);
        assert_eq!(info["dicom"]["window"], json!([-100.5, 299.5]));
        assert_eq!(info["orientation"], "LPS");
        // The patient is never among the details shown by default.
        assert!(!info["format_fields"].to_string().contains("Doe"));
        assert!(info["dicom"]["patient"].to_string().contains("Doe Jane"));
    }
}

#[test]
fn series_rebuilt_from_the_folder() {
    let dir = tmpdir("dicom-series");
    // 4 slices × 2 volumes, written in a shuffled order, a copy of one
    // instance under another name, and a file of another series.
    let mut clicked = PathBuf::new();
    for (n, (k, tt)) in [(2, 1), (0, 0), (3, 0), (1, 1), (0, 1), (2, 0), (1, 0), (3, 1)].into_iter().enumerate() {
        let p = write(&dir, &format!("IM{n:03}"), &slice_file(&Slice::new(k, tt), Ts::ExplicitLe));
        if (k, tt) == (2, 1) {
            clicked = p;
        }
    }
    write(&dir, "copy.dcm", &slice_file(&Slice::new(3, 1), Ts::ImplicitLe));
    let mut other = Slice::new(0, 0);
    other.series = "9.9.9".into();
    other.instance = "9.9.9.1".into();
    write(&dir, "other.dcm", &slice_file(&other, Ts::ExplicitLe));
    write(&dir, "notes.txt", b"not a DICOM file");
    let clicked2 = clicked.clone();
    within(30, "series", move || {
        let mut v = open(&clicked2, true).unwrap();
        assert_eq!(v.header.shape, vec![COLS, ROWS, 4, 2]);
        assert_eq!(v.header.zooms[2], 3.0);
        assert_eq!(v.header.repetition_time, Some(2.0));
        // Opens on the file chosen: slice 2 of volume 1.
        assert_eq!(v.current_volume(), 1);
        assert_eq!(v.header.dicom.as_ref().unwrap().start, Some(([COLS / 2, ROWS / 2, 2], 1)));
        assert_values(&mut v, 2, 2, 1);
        for k in 0..4 {
            assert_values(&mut v, k, k, 1);
        }
        while !v.set_volume(0).unwrap().0 {}
        for k in 0..4 {
            assert_values(&mut v, k, k, 0);
        }
        assert_eq!(v.header_info()["dicom"]["files"], 8);
    });
    // Through the protocol: the cursor on the file chosen, the series list,
    // another series.
    let mut s = Session::new(1 << 20);
    let r = request(&mut s, json!({"op": "open", "path": clicked}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["volume"], 1);
    assert_eq!(r["cursor"][2], 2);
    let l = request(&mut s, json!({"op": "series_list"}));
    assert_eq!(l["series"].as_array().unwrap().len(), 2, "{l}");
    assert_eq!(l["origin"], "folder");
    let current: Vec<&Value> = l["series"].as_array().unwrap().iter().filter(|x| x["current"] == true).collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0]["images"], 9);
    let r = request(&mut s, json!({"op": "open_series", "uid": "9.9.9"}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["header"]["shape"], json!([COLS, ROWS, 1]));
    let r = request(&mut s, json!({"op": "open_series", "uid": "nope"}));
    assert_eq!(r["error"]["code"], "not_found");
    // The file alone when asked.
    let r = request(&mut s, json!({"op": "open", "path": clicked, "dicom_series": false}));
    assert_eq!(r["header"]["shape"], json!([COLS, ROWS, 1]));
    let r = request(&mut s, json!({"op": "load", "dicom_window": "auto"}));
    assert_eq!(r["display_window"]["source"], "auto", "{r}");
    let r = request(&mut s, json!({"op": "load", "dicom_window": "file"}));
    assert_eq!(r["display_window"]["source"], "file");
    assert_eq!(r["display_window"]["low"], -100.5);
}

#[test]
fn a_folder_opens_its_longest_series() {
    let dir = tmpdir("dicom-folder");
    for (n, (k, tt)) in [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)].into_iter().enumerate() {
        write(&dir, &format!("IM{n:03}"), &slice_file(&Slice::new(k, tt), Ts::ExplicitLe));
    }
    let mut other = Slice::new(0, 0);
    other.series = "9.9.9".into();
    other.instance = "9.9.9.1".into();
    write(&dir, "other.dcm", &slice_file(&other, Ts::ExplicitLe));
    write(&dir, "notes.txt", b"not a DICOM file");
    let d = dir.clone();
    within(30, "folder", move || {
        let mut v = gnome_nifti_preview::view::open_folder(&d, 1 << 30).unwrap();
        assert_eq!(v.header.shape, vec![COLS, ROWS, 3, 2], "the series of 6 images, not the single one");
        assert_values(&mut v, 1, 1, 0);
        // The other series of the folder are listed.
        let (catalog, current) = gnome_nifti_preview::view::series_catalog(&mut v).unwrap();
        assert_eq!(catalog.related(current.as_deref()).len(), 2);
    });
    // A folder without DICOM images.
    let empty = tmpdir("dicom-folder-empty");
    std::fs::write(empty.join("readme.txt"), b"nothing").unwrap();
    let e = gnome_nifti_preview::view::open_folder(&empty, 1 << 30).err().unwrap();
    assert_eq!(e.code, "not_found");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&empty).ok();
}

#[test]
fn missing_slices_irregular_spacing_and_tilt() {
    let dir = tmpdir("dicom-gaps");
    // Slices at z = 0, 3, 9 (one missing), the stack sheared in y (tilt).
    for (n, k) in [0usize, 1, 3].into_iter().enumerate() {
        let mut sl = Slice::new(k, 0);
        sl.ipp = [10.0, 20.0 + 0.5 * k as f64, 3.0 * k as f64];
        write(&dir, &format!("s{n}.dcm"), &slice_file(&sl, Ts::ExplicitLe));
    }
    let v = open(&dir.join("s0.dcm"), true).unwrap();
    assert_eq!(v.header.shape, vec![COLS, ROWS, 3]);
    let warnings = v.header.warnings.join(" ");
    assert!(warnings.contains("Irregular slice spacing"), "{warnings}");
    assert!(warnings.contains("gantry tilt"), "{warnings}");
    // The affine follows the positions (sheared), in RAS.
    assert_eq!(v.affine[1][2], -0.75);
    assert_eq!(v.affine[2][2], 4.5);
}

#[test]
fn oblique_orientation() {
    let dir = tmpdir("dicom-oblique");
    let (c, sn) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
    let mut sl = Slice::new(0, 0);
    sl.iop = [c, sn, 0.0, -sn, c, 0.0];
    let p = write(&dir, "o.dcm", &slice_file(&sl, Ts::ExplicitLe));
    let v = open(&p, false).unwrap();
    let a = v.affine;
    // Column 0: the row direction × 0.25 mm, column 1: the column direction × 0.5 mm, in RAS.
    for (got, want) in [(a[0][0], -c * 0.25), (a[1][0], -sn * 0.25), (a[0][1], sn * 0.5), (a[1][1], -c * 0.5)] {
        assert!((got - want).abs() < 1e-9, "{a:?}");
    }
    assert!(v.header_info()["obliquity_deg"].as_f64().unwrap() > 29.0);
}

fn base_multiframe(frames: usize) -> Vec<El> {
    vec![
        ui(t(8, 0x16), "1.2.840.10008.5.1.4.1.1.3.1"),
        ui(t(8, 0x18), "1.2.3.9"),
        s(t(8, 0x60), b"CS", "US"),
        ui(t(0x20, 0x0E), "1.2.3.9.1"),
        is(t(0x28, 0x08), frames as i64),
        us(t(0x28, 0x02), &[1]),
        s(t(0x28, 0x04), b"CS", "MONOCHROME2"),
        us(t(0x28, 0x10), &[ROWS as u16]),
        us(t(0x28, 0x11), &[COLS as u16]),
        us(t(0x28, 0x100), &[16]),
        us(t(0x28, 0x101), &[16]),
        us(t(0x28, 0x102), &[15]),
        us(t(0x28, 0x103), &[1]),
    ]
}

#[test]
fn multiframe_cine_rtdose_and_enhanced() {
    let dir = tmpdir("dicom-multiframe");
    // A cine: frames in time.
    let mut els = base_multiframe(3);
    els.push(ds(t(0x18, 0x1063), &[40.0]));
    let px: Vec<u8> = (0..3).flat_map(|f| pixels_le(0, f)).collect();
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(px.clone()) });
    let p = write(&dir, "cine.dcm", &file(Ts::ExplicitLe, &els));
    let mut v = open(&p, true).unwrap();
    assert_eq!(v.header.shape, vec![COLS, ROWS, 1, 3]);
    assert_eq!(v.header.repetition_time, Some(0.04));
    assert!(!v.orientation_known);
    while !v.set_volume(2).unwrap().0 {}
    assert_values(&mut v, 0, 0, 2);

    // RT dose: frames along the normal at the grid offsets, scaled.
    let mut els = base_multiframe(3);
    els.push(ds(t(0x20, 0x32), &[0.0, 0.0, 0.0]));
    els.push(ds(t(0x20, 0x37), &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0]));
    els.push(ds(t(0x28, 0x30), &[2.0, 2.0]));
    els.push(ds(t(0x3004, 0x0C), &[0.0, 5.0, 10.0]));
    els.push(ds(t(0x3004, 0x0E), &[0.5]));
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(px.clone()) });
    let p = write(&dir, "dose.dcm", &file(Ts::ExplicitLe, &els));
    let mut v = open(&p, true).unwrap();
    assert_eq!(v.header.shape, vec![COLS, ROWS, 3]);
    assert_eq!(v.affine[2][2], 5.0);
    assert_eq!(v.header.slope, 0.5);
    assert_eq!(v.value_at([1, 2, 2]).unwrap(), Some(value(1, 2, 0, 2) as f64 * 0.5));

    // Enhanced: per-frame positions (frames stored out of order), two
    // temporal positions, a rescale per frame.
    let mut els = base_multiframe(4);
    let frame = |z: f64, tpos: u32, slope: f64| {
        vec![
            sq(t(0x20, 0x9111), vec![vec![El { tag: t(0x20, 0x9157), vr: *b"UL", v: V::U32(vec![1, tpos]) }]]),
            sq(t(0x20, 0x9113), vec![vec![ds(t(0x20, 0x32), &[0.0, 0.0, z])]]),
            sq(t(0x28, 0x9145), vec![vec![ds(t(0x28, 0x1052), &[0.0]), ds(t(0x28, 0x1053), &[slope])]]),
        ]
    };
    // Stored: (z=4, t=2), (z=0, t=1), (z=0, t=2), (z=4, t=1).
    els.push(sq(t(0x5200, 0x9230), vec![frame(4.0, 2, 2.0), frame(0.0, 1, 1.0), frame(0.0, 2, 2.0), frame(4.0, 1, 1.0)]));
    els.push(sq(
        t(0x5200, 0x9229),
        vec![vec![
            sq(t(0x20, 0x9116), vec![vec![ds(t(0x20, 0x37), &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0])]]),
            sq(t(0x28, 0x9110), vec![vec![ds(t(0x28, 0x30), &[1.0, 1.0]), ds(t(0x18, 0x50), &[4.0])]]),
        ]],
    ));
    els.push(sq(
        t(0x20, 0x9222),
        vec![
            vec![El { tag: t(0x20, 0x9165), vr: *b"AT", v: V::U16(vec![0x0020, 0x9056]) }],
            vec![El { tag: t(0x20, 0x9165), vr: *b"AT", v: V::U16(vec![0x0020, 0x9128]) }],
        ],
    ));
    // Frame n holds slice k = n.
    let px: Vec<u8> = (0..4).flat_map(|f| pixels_le(f, 0)).collect();
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(px) });
    let p = write(&dir, "enhanced.dcm", &file(Ts::ExplicitLe, &els));
    let mut v = open(&p, true).unwrap();
    assert_eq!(v.header.shape, vec![COLS, ROWS, 2, 2]);
    assert_eq!(v.header.dtype.label(), "float32", "per-frame rescales");
    // Volume 0 (t=1): z=0 is frame 1, z=4 is frame 3 (slope 1).
    assert_eq!(v.value_at([1, 1, 0]).unwrap(), Some(value(1, 1, 1, 0) as f64));
    assert_eq!(v.value_at([1, 1, 1]).unwrap(), Some(value(1, 1, 3, 0) as f64));
    while !v.set_volume(1).unwrap().0 {}
    // Volume 1 (t=2): frames 2 and 0, slope 2.
    assert_eq!(v.value_at([1, 1, 0]).unwrap(), Some(2.0 * value(1, 1, 2, 0) as f64));
    assert_eq!(v.value_at([1, 1, 1]).unwrap(), Some(2.0 * value(1, 1, 0, 0) as f64));
}

#[test]
fn siemens_mosaic() {
    let dir = tmpdir("dicom-mosaic");
    // 3 slices of ROWS × COLS in a 2 × 2 mosaic.
    let (mr, mc) = (2 * ROWS, 2 * COLS);
    let mut px = vec![0u8; mr * mc * 2];
    for k in 0..3 {
        let (tr, tc) = (k / 2 * ROWS, k % 2 * COLS);
        for j in 0..ROWS {
            for i in 0..COLS {
                let o = ((tr + j) * mc + tc + i) * 2;
                px[o..o + 2].copy_from_slice(&value(i, j, k, 0).to_le_bytes());
            }
        }
    }
    let mut els: Vec<El> = header(&Slice::new(0, 0)).into_iter().filter(|e| e.tag != t(0x28, 0x10) && e.tag != t(0x28, 0x11)).collect();
    els.push(us(t(0x28, 0x10), &[mr as u16]));
    els.push(us(t(0x28, 0x11), &[mc as u16]));
    els.push(s(t(8, 8), b"CS", "ORIGINAL\\PRIMARY\\M\\ND\\MOSAIC"));
    els.push(s(t(0x19, 0x10), b"LO", "SIEMENS MR HEADER"));
    els.push(us(t(0x19, 0x100A), &[3]));
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(px) });
    let p = write(&dir, "mosaic.dcm", &file(Ts::ExplicitLe, &els));
    let mut v = open(&p, true).unwrap();
    assert_eq!(v.header.shape, vec![COLS, ROWS, 3]);
    for k in 0..3 {
        assert_values(&mut v, k, k, 0);
    }
    // The position given is the mosaic's corner: the first tile is centred.
    assert_eq!(v.affine[0][3], -(10.0 + 0.25 * COLS as f64 / 2.0));
    assert_eq!(v.affine[1][3], -(20.0 + 0.5 * ROWS as f64 / 2.0));
    assert_eq!(v.affine[2][2], 3.0);
}

#[test]
fn colour_palette_monochrome1_and_bits() {
    let dir = tmpdir("dicom-colour");
    let base = |photometric: &str, spp: u16, bits: u16, frames: usize| {
        vec![
            ui(t(8, 0x18), "1.2.3.7"),
            ui(t(0x20, 0x0E), "1.2.3.7.1"),
            is(t(0x28, 0x08), frames as i64),
            us(t(0x28, 0x02), &[spp]),
            s(t(0x28, 0x04), b"CS", photometric),
            us(t(0x28, 0x10), &[2]),
            us(t(0x28, 0x11), &[3]),
            us(t(0x28, 0x100), &[bits]),
            us(t(0x28, 0x101), &[bits]),
            us(t(0x28, 0x102), &[bits - 1]),
            us(t(0x28, 0x103), &[0]),
        ]
    };
    // RGB, planar configuration 1 (all reds, then greens, then blues).
    let mut els = base("RGB", 3, 8, 1);
    els.push(us(t(0x28, 0x06), &[1]));
    let planar: Vec<u8> = (0..3u8).flat_map(|c| (0..6u8).map(move |p| 10 * c + p)).collect();
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OB", v: V::Raw(planar) });
    let mut v = open(&write(&dir, "rgb.dcm", &file(Ts::ExplicitLe, &els)), false).unwrap();
    assert_eq!(v.header.dtype.label(), "RGB");
    assert_eq!(v.rgb_at([2, 1, 0]).unwrap(), Some([5, 15, 25]));

    // Palette colour: index → (index, 255 − index, 7) in 16-bit entries.
    let mut els = base("PALETTE COLOR", 1, 8, 1);
    let lut = |f: &dyn Fn(u16) -> u16| (0..256u16).map(|i| f(i) << 8).collect::<Vec<u16>>();
    for (d, data, f) in [(0x1101u16, 0x1201u16, &(|i| i) as &dyn Fn(u16) -> u16), (0x1102, 0x1202, &|i| 255 - i), (0x1103, 0x1203, &|_| 7)] {
        els.push(us(t(0x28, d), &[256, 0, 16]));
        els.push(El { tag: t(0x28, data), vr: *b"OW", v: V::U16(lut(f)) });
    }
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OB", v: V::Raw(vec![0, 10, 20, 30, 40, 250]) });
    let mut v = open(&write(&dir, "palette.dcm", &file(Ts::ExplicitLe, &els)), false).unwrap();
    assert_eq!(v.rgb_at([1, 1, 0]).unwrap(), Some([40, 215, 7]));

    // MONOCHROME1: the lowest value is drawn white.
    let mut els = base("MONOCHROME1", 1, 8, 1);
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OB", v: V::Raw(vec![0, 50, 100, 150, 200, 250]) });
    let mut v = open(&write(&dir, "mono1.dcm", &file(Ts::ExplicitLe, &els)), false).unwrap();
    assert!(v.inverted());
    assert_eq!(v.header_info()["dicom"]["inverted"], true);
    let plans = v.plans("stored").unwrap();
    let plan = plans.iter().find(|p| p.view == "axial").unwrap();
    let (rgb, _) = gnome_nifti_preview::render::render_slice(&mut v, plan, 0, "gray", &mut Vec::new()).unwrap();
    let grey: Vec<u8> = rgb.chunks(3).map(|c| c[0]).collect();
    // Rows from the top (DICOM order): the first pixel, value 0, is the brightest.
    assert_eq!(grey, vec![255, 205, 153, 102, 50, 0]);

    // 1 bit per pixel, frames packed across byte boundaries (6 pixels each).
    let mut els = base("MONOCHROME2", 1, 1, 3);
    let pixels: Vec<u8> = (0..18).map(|i| ((i * 7) % 3 == 0) as u8).collect();
    let mut packed = vec![0u8; 3];
    for (i, &b) in pixels.iter().enumerate() {
        packed[i / 8] |= b << (i % 8);
    }
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OB", v: V::Raw(packed) });
    let mut v = open(&write(&dir, "bits.dcm", &file(Ts::ExplicitLe, &els)), false).unwrap();
    assert_eq!(v.header.shape, vec![3, 2, 1, 3]);
    for f in 0..3 {
        while !v.set_volume(f as i64).unwrap().0 {}
        for p in 0..6 {
            assert_eq!(v.value_at([(p % 3) as i64, (p / 3) as i64, 0]).unwrap(), Some(pixels[6 * f + p] as f64));
        }
    }
}

/// A DICOMDIR (records in order, no offsets) for files below `root`.
fn dicomdir(root: &Path, records: Vec<Vec<El>>) -> PathBuf {
    let mut meta_els = vec![
        El { tag: t(2, 1), vr: *b"OB", v: V::Raw(vec![0, 1]) },
        ui(t(2, 2), "1.2.840.10008.1.3.10"),
        ui(t(2, 0x10), Ts::ExplicitLe.uid()),
    ];
    let mut meta = Vec::new();
    encode(&meta_els, true, false, &mut meta);
    meta_els.insert(0, ul(t(2, 0), &[meta.len() as u32]));
    let mut out = vec![0u8; 128];
    out.extend_from_slice(b"DICM");
    encode(&meta_els, true, false, &mut out);
    encode(&[s(t(4, 0x1130), b"CS", "TEST"), sq(t(4, 0x1220), records)], true, false, &mut out);
    write(root, "DICOMDIR", &out)
}

#[test]
fn dicomdir_lists_and_opens_series() {
    let root = tmpdir("dicom-dicomdir");
    std::fs::create_dir_all(root.join("dicom/s1")).unwrap();
    std::fs::create_dir_all(root.join("dicom/s2")).unwrap();
    for k in 0..3 {
        write(&root.join("dicom/s1"), &format!("i{k}"), &slice_file(&Slice::new(k, 0), Ts::ExplicitLe));
    }
    let mut other = Slice::new(0, 0);
    other.series = "7.7.7".into();
    other.instance = "7.7.7.1".into();
    write(&root.join("dicom/s2"), "i0", &slice_file(&other, Ts::ExplicitLe));
    write(&root, "secret", b"outside");
    let rec = |kind: &str, extra: Vec<El>| {
        let mut r = vec![s(t(4, 0x1430), b"CS", kind)];
        r.extend(extra);
        r
    };
    let image = |path: &str| rec("IMAGE", vec![s(t(4, 0x1500), b"CS", path)]);
    let records = vec![
        rec("PATIENT", vec![s(t(0x10, 0x10), b"PN", "Doe^Jane")]),
        rec("STUDY", vec![ui(t(0x20, 0x0D), "1.2.3.4"), s(t(8, 0x1030), b"LO", "Head")]),
        rec("SERIES", vec![ui(t(0x20, 0x0E), "1.2.3.4.5"), s(t(8, 0x60), b"CS", "MR"), is(t(0x20, 0x11), 3)]),
        // Upper case names on the disc, lower case on this disk.
        image("DICOM\\S1\\I0"),
        image("DICOM\\S1\\I1"),
        image("DICOM\\S1\\I2"),
        // Never outside the DICOMDIR's folder.
        image("..\\secret"),
        rec("SERIES", vec![ui(t(0x20, 0x0E), "7.7.7"), s(t(8, 0x60), b"CS", "MR"), is(t(0x20, 0x11), 4)]),
        image("DICOM\\S2\\I0"),
    ];
    let p = dicomdir(&root, records);
    let mut s = Session::new(1 << 20);
    let r = request(&mut s, json!({"op": "open", "path": p}));
    assert_eq!(r["ok"], true, "{r}");
    // The series with the most images opens first.
    assert_eq!(r["header"]["shape"], json!([COLS, ROWS, 3]));
    assert_eq!(r["header"]["dicom"]["dicomdir"], true);
    let l = request(&mut s, json!({"op": "series_list"}));
    assert_eq!(l["origin"], "dicomdir");
    let series = l["series"].as_array().unwrap();
    assert_eq!(series.len(), 2, "{l}");
    assert_eq!(series[0]["images"], 3);
    assert_eq!(series[0]["study_description"], "Head");
    let r = request(&mut s, json!({"op": "open_series", "uid": "7.7.7"}));
    assert_eq!(r["header"]["shape"], json!([COLS, ROWS, 1]), "{r}");
}

#[test]
fn hostile_files_fail_cleanly() {
    let dir = tmpdir("dicom-hostile");
    let good = slice_file(&Slice::new(0, 0), Ts::ExplicitLe);
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    // Truncated at every few bytes of the header, and in the pixels.
    for cut in (130..good.len()).step_by(37) {
        cases.push(("truncated", good[..cut].to_vec()));
    }
    // Absurd lengths, a huge image, nested sequences, garbage fragments.
    let mut huge = header(&Slice::new(0, 0));
    huge.retain(|e| e.tag != t(0x28, 0x10));
    huge.push(us(t(0x28, 0x10), &[65535]));
    huge.push(is(t(0x28, 0x08), 1 << 30));
    huge.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(vec![0; 64]) });
    cases.push(("huge", file(Ts::ExplicitLe, &huge)));
    let mut deep = vec![s(t(8, 0x60), b"CS", "MR")];
    for _ in 0..40 {
        deep = vec![sq(t(8, 0x1140), vec![deep])];
    }
    cases.push(("deep", file(Ts::ExplicitLe, &deep)));
    let mut bad_frag = header(&Slice::new(0, 0));
    bad_frag.push(El { tag: t(0x7FE0, 0x10), vr: *b"OB", v: V::Fragments(vec![b"\xFF\xD8\xFF\xC3garbage".to_vec(), vec![0xFF; 40]]) });
    cases.push(("fragments", file(Ts::JpegLossless, &bad_frag)));
    let mut bad_rle = header(&Slice::new(0, 0));
    bad_rle.push(El { tag: t(0x7FE0, 0x10), vr: *b"OB", v: V::Fragments(vec![vec![15, 0, 0, 0, 255, 255, 255, 255]]) });
    cases.push(("rle", file(Ts::Rle, &bad_rle)));
    let mut long = good.clone();
    // The first element after the meta group claims 4 GB.
    let at = long.windows(4).position(|w| w == [0x08, 0x00, 0x16, 0x00]).unwrap();
    long[at + 4..at + 8].copy_from_slice(b"UN\0\0");
    long.splice(at + 8..at + 8, [0xF0, 0xFF, 0xFF, 0x7F]);
    cases.push(("long", long));
    for (i, (what, bytes)) in cases.into_iter().enumerate() {
        let p = write(&dir, &format!("h{i}.dcm"), &bytes);
        within(20, what, move || {
            if let Ok(mut v) = open(&p, false) {
                let _ = v.stats();
            }
        });
    }
}

/// Encapsulated pixel data of one frame, for a stream of `testdata/`.
fn encapsulated_file(ts_uid: &str, rows: u16, cols: u16, bits: u16, spp: u16, photometric: &str, stream: &[u8]) -> Vec<u8> {
    let mut els = vec![
        ui(t(8, 0x18), "1.2.3.8"),
        ui(t(0x20, 0x0E), "1.2.3.8.1"),
        us(t(0x28, 0x02), &[spp]),
        s(t(0x28, 0x04), b"CS", photometric),
        us(t(0x28, 0x10), &[rows]),
        us(t(0x28, 0x11), &[cols]),
        us(t(0x28, 0x100), &[bits]),
        us(t(0x28, 0x101), &[bits]),
        us(t(0x28, 0x102), &[bits - 1]),
        us(t(0x28, 0x103), &[0]),
    ];
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OB", v: V::Fragments(vec![stream.to_vec()]) });
    let mut f = file(Ts::ExplicitLe, &els);
    // Swap the transfer syntax written for the one wanted (same length).
    let old = Ts::ExplicitLe.uid().as_bytes();
    let at = f.windows(old.len()).position(|w| w == old).unwrap();
    let mut new = ts_uid.as_bytes().to_vec();
    if new.len() % 2 == 1 {
        new.push(0);
    }
    let len_at = at - 2;
    f.splice(at..at + old.len() + 1, new.iter().copied());
    f[len_at..len_at + 2].copy_from_slice(&(new.len() as u16).to_le_bytes());
    // Group length.
    let meta_end = f.windows(2).skip(132).position(|w| w == [0x08, 0x00]).unwrap() + 132;
    f[140..144].copy_from_slice(&((meta_end - 144) as u32).to_le_bytes());
    f
}

#[test]
fn codec_streams_in_dicom() {
    let dir = tmpdir("dicom-codecs");
    let jls = encapsulated_file("1.2.840.10008.1.2.4.80", 8, 8, 8, 1, "MONOCHROME2", include_bytes!("../src/formats/dicom/testdata/gray8.jls"));
    let mut v = open(&write(&dir, "jls.dcm", &jls), false).unwrap();
    for i in 0..64usize {
        assert_eq!(v.value_at([(i % 8) as i64, (i / 8) as i64, 0]).unwrap(), Some(((i * 37) % 251) as f64));
    }
    let rgb = encapsulated_file("1.2.840.10008.1.2.4.81", 8, 8, 8, 3, "RGB", include_bytes!("../src/formats/dicom/testdata/rgb_ilv2.jls"));
    let mut v = open(&write(&dir, "rgb.dcm", &rgb), false).unwrap();
    let a = (9 * 37 % 251) as u8;
    assert_eq!(v.rgb_at([1, 1, 0]).unwrap(), Some([a, 255 - a, a / 2]));
    assert!(v.header_info()["format_fields"].to_string().contains("JPEG-LS Near-Lossless (lossy)"));
    let jpeg = encapsulated_file("1.2.840.10008.1.2.4.50", 8, 8, 8, 3, "YBR_FULL_422", include_bytes!("../src/formats/dicom/testdata/rgb420.jpg"));
    let mut v = open(&write(&dir, "jpeg.dcm", &jpeg), false).unwrap();
    let px = v.rgb_at([1, 1, 0]).unwrap().unwrap();
    assert!(px.iter().zip([a, 255 - a, a / 2]).all(|(&g, w)| (g as i32 - w as i32).abs() < 60), "{px:?}");
    let j2k = encapsulated_file("1.2.840.10008.1.2.4.91", 64, 64, 16, 1, "MONOCHROME2", include_bytes!("../src/formats/dicom/testdata/ramp16.j2k"));
    // Without OpenJPEG the syntax is refused when the pixels are read.
    match open(&write(&dir, "j2k.dcm", &j2k), false).and_then(|mut v| v.value_at([10, 20, 0])) {
        Ok(got) => {
            let got = got.unwrap();
            assert!(cfg!(feature = "openjpeg") && (got - 900.0).abs() < 200.0, "{got}");
        }
        Err(e) => assert!(cfg!(not(feature = "openjpeg")) && e.code == "unsupported_format", "{e}"),
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

#[test]
fn mutated_dicom_never_panics() {
    let dir = tmpdir("dicom-mutated");
    let mut seeds: Vec<Vec<u8>> = [Ts::ImplicitLe, Ts::ExplicitLe, Ts::ExplicitBe, Ts::Deflated, Ts::Rle, Ts::JpegLossless]
        .iter()
        .map(|&ts| slice_file(&Slice::new(1, 0), ts))
        .collect();
    seeds.push(encapsulated_file("1.2.840.10008.1.2.4.80", 8, 8, 8, 3, "RGB", include_bytes!("../src/formats/dicom/testdata/rgb_ilv2.jls")));
    seeds.push(encapsulated_file("1.2.840.10008.1.2.4.81", 8, 8, 16, 1, "MONOCHROME1", include_bytes!("../src/formats/dicom/testdata/near16.jls")));
    seeds.push(encapsulated_file("1.2.840.10008.1.2.4.50", 8, 8, 8, 3, "YBR_FULL_422", include_bytes!("../src/formats/dicom/testdata/rgb420.jpg")));
    seeds.push(encapsulated_file("1.2.840.10008.1.2.4.91", 64, 64, 16, 1, "MONOCHROME2", include_bytes!("../src/formats/dicom/testdata/ramp16.j2k")));
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut n = 0;
    for seed in &seeds {
        for round in 0..60 {
            let mut m = seed.clone();
            match round % 4 {
                0 => {
                    for _ in 0..1 + rng.below(4) {
                        let i = 132 + rng.below(m.len() - 132);
                        m[i] = rng.next() as u8;
                    }
                }
                1 => m.truncate(132 + rng.below(m.len() - 131)),
                2 => {
                    // An extreme length somewhere.
                    let i = 132 + rng.below(m.len() - 136);
                    let v: [u8; 4] = [[0xFF; 4], [0xFE, 0xFF, 0xFF, 0x7F], [0, 0, 0, 0x80], [0xFF, 0xFF, 0, 0]][rng.below(4)];
                    m[i..i + 4].copy_from_slice(&v);
                }
                _ => {
                    // A piece repeated.
                    let i = 132 + rng.below(m.len() - 132);
                    let len = rng.below(64).min(m.len() - i);
                    let piece = m[i..i + len].to_vec();
                    m.splice(i..i, piece);
                }
            }
            let sub = dir.join(format!("m{n}"));
            std::fs::create_dir_all(&sub).unwrap();
            let p = write(&sub, "f.dcm", &m);
            n += 1;
            within(20, &format!("mutant {n}"), move || {
                let Ok(mut v) = open(&p, true) else { return };
                let _ = v.header_info();
                if v.stats().is_err() {
                    return;
                }
                for mode in ["neurological", "stored"] {
                    let Ok(plans) = v.plans(mode) else { continue };
                    for plan in plans.iter() {
                        for cmap in ["auto", "gray", "rgb"] {
                            let _ = gnome_nifti_preview::render::render_slice(&mut v, plan, plan.n_slices / 2, cmap, &mut Vec::new());
                        }
                    }
                }
                let c = v.center();
                let _ = v.value_at([c[0] as i64, c[1] as i64, c[2] as i64]);
            });
        }
    }
}

#[test]
fn series_menu_lists_the_same_study() {
    let dir = tmpdir("dicom-study");
    // Two series of one exam, and an unrelated exam in the same folder.
    let a = write(&dir, "a.dcm", &slice_file(&Slice::new(0, 0), Ts::ExplicitLe));
    let mut b = Slice::new(0, 0);
    b.series = "1.2.3.4.6".into();
    b.instance = "1.2.3.4.6.1".into();
    write(&dir, "b.dcm", &slice_file(&b, Ts::ExplicitLe));
    let mut c = header(&Slice { series: "5.5.5".into(), instance: "5.5.5.1".into(), ..Slice::new(0, 0) });
    c.retain(|e| e.tag != t(0x20, 0x0D));
    c.push(ui(t(0x20, 0x0D), "9.8.7"));
    c.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(pixels_le(0, 0)) });
    write(&dir, "c.dcm", &file(Ts::ExplicitLe, &c));
    let mut s = Session::new(1 << 20);
    let r = request(&mut s, json!({"op": "open", "path": a}));
    assert_eq!(r["header"]["dicom"]["series_count"], 2, "{r}");
    let l = request(&mut s, json!({"op": "series_list"}));
    let uids: Vec<&str> = l["series"].as_array().unwrap().iter().map(|x| x["uid"].as_str().unwrap()).collect();
    assert_eq!(uids, vec!["1.2.3.4.5", "1.2.3.4.6"], "{l}");
}

#[test]
fn dicom_images_are_not_label_maps() {
    let dir = tmpdir("dicom-nolabels");
    // A few distinct integer values, as in a small or quantised image.
    let mut els = header(&Slice::new(0, 0));
    let px: Vec<u8> = (0..ROWS * COLS).flat_map(|i| ((i % 4) as i16 * 100).to_le_bytes()).collect();
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(px) });
    let mut v = open(&write(&dir, "few.dcm", &file(Ts::ExplicitLe, &els)), false).unwrap();
    assert!(!v.stats().unwrap().labels.is_labels);
}

#[test]
fn truncated_pixel_data_are_read_up_to_the_end() {
    let dir = tmpdir("dicom-cut");
    let full = slice_file(&Slice::new(0, 0), Ts::ExplicitLe);
    // The last two rows and one byte are missing.
    let cut = 2 * COLS * 2 + 1;
    let p = write(&dir, "cut.dcm", &full[..full.len() - cut]);
    let mut s = Session::new(1 << 20);
    assert_eq!(request(&mut s, json!({"op": "open", "path": p, "dicom_series": false}))["ok"], true);
    let load = request(&mut s, json!({"op": "load"}));
    let notes = load["stats"]["notes"].to_string();
    assert!(notes.contains(&format!("the last {cut} bytes")), "{load}");
    let mut v = open(&p, false).unwrap();
    v.stats().unwrap();
    for j in 0..ROWS {
        for i in [0, COLS - 1] {
            let got = v.value_at([i as i64, j as i64, 0]).unwrap().unwrap();
            let want = if j < ROWS - 2 { value(i, j, 0, 0) as f64 } else { 0.0 };
            // The voxel cut in half keeps its low byte.
            if (j, i) != (ROWS - 3, COLS - 1) {
                assert_eq!(got, want, "({i}, {j})");
            }
        }
    }
    // Compressed data cut short are still refused.
    let rle = slice_file(&Slice::new(0, 0), Ts::Rle);
    let p = write(&dir, "cut-rle.dcm", &rle[..rle.len() - 40]);
    let mut v = open(&p, false).unwrap();
    assert_eq!(v.stats().err().map(|e| e.code), Some("corrupt_data"));
}

#[test]
fn files_without_image_are_described() {
    let dir = tmpdir("dicom-about");
    let code = |meaning: &str| vec![s(t(8, 0x100), b"SH", "X1"), s(t(8, 0x104), b"LO", meaning)];
    let item = |kind: &str, name: &str, mut rest: Vec<El>| {
        let mut v = vec![s(t(0x40, 0xA010), b"CS", "CONTAINS"), s(t(0x40, 0xA040), b"CS", kind), sq(t(0x40, 0xA043), vec![code(name)])];
        v.append(&mut rest);
        v
    };
    let report = vec![
        ui(t(8, 0x16), "1.2.840.10008.5.1.4.1.1.88.11"),
        s(t(8, 0x60), b"CS", "SR"),
        s(t(8, 0x103E), b"LO", "Reports"),
        s(t(0x10, 0x10), b"PN", "Doe^Jane"),
        is(t(0x20, 0x11), 7),
        s(t(0x40, 0xA040), b"CS", "CONTAINER"),
        sq(t(0x40, 0xA043), vec![code("Radiology report")]),
        sq(t(0x40, 0xA730), vec![
            item("TEXT", "Finding", vec![s(t(0x40, 0xA160), b"UT", "No mass.\nNormal ventricles.")]),
            item("PNAME", "Observer", vec![s(t(0x40, 0xA123), b"PN", "Smith^John")]),
            item("NUM", "Diameter", vec![sq(t(0x40, 0xA300), vec![vec![
                sq(t(0x40, 0x08EA), vec![vec![s(t(8, 0x100), b"SH", "mm"), s(t(8, 0x104), b"LO", "millimeter")]]),
                ds(t(0x40, 0xA30A), &[12.5]),
            ]])]),
            item("CONTAINER", "Impression", vec![sq(t(0x40, 0xA730), vec![item("CODE", "Severity", vec![sq(t(0x40, 0xA168), vec![code("Mild")])])])]),
        ]),
    ];
    let structures = vec![
        ui(t(8, 0x16), "1.2.840.10008.5.1.4.1.1.481.3"),
        s(t(8, 0x60), b"CS", "RTSTRUCT"),
        s(t(0x3006, 0x02), b"SH", "Plan A"),
        sq(t(0x3006, 0x20), (1..=3).map(|n| vec![is(t(0x3006, 0x22), n), s(t(0x3006, 0x26), b"LO", &format!("ROI {n}"))]).collect()),
    ];
    let ecg = vec![
        ui(t(8, 0x16), "1.2.840.10008.5.1.4.1.1.9.1.1"),
        s(t(8, 0x60), b"CS", "ECG"),
        sq(t(0x5400, 0x100), vec![vec![
            us(t(0x3A, 0x05), &[2]),
            ul(t(0x3A, 0x10), &[5000]),
            ds(t(0x3A, 0x1A), &[500.0]),
            s(t(0x3A, 0x20), b"SH", "RHYTHM"),
            sq(t(0x3A, 0x200), ["Lead I", "Lead II"].iter().map(|l| vec![sq(t(0x3A, 0x208), vec![code(l)])]).collect()),
        ]]),
    ];
    for ts in [Ts::ImplicitLe, Ts::ExplicitLe] {
        for (name, els, expected) in [
            ("report", &report, vec!["Basic text report", "Radiology report", "Finding: No mass. / Normal ventricles.", "Diameter: 12.5 mm", "    Severity: Mild", "7 · Reports"]),
            ("structures", &structures, vec!["Radiotherapy structure set", "Structures · 3", "Plan A", "ROI 3"]),
            ("ecg", &ecg, vec!["12-lead ECG waveform", "2 channels, 5000 samples at 500 Hz (10 s)", "Lead I, Lead II"]),
        ] {
            let p = write(&dir, &format!("{name}-{ts:?}.dcm"), &file(ts, els));
            let mut session = Session::new(1 << 20);
            let r = request(&mut session, json!({"op": "open", "path": p}));
            assert_eq!(r["error"]["code"], "unsupported_format", "{r}");
            let about = r["error"]["about"].to_string();
            for e in expected {
                assert!(about.contains(&e.replace('\n', "\\n")), "{name} {ts:?}: {e:?} in {about}");
            }
            assert!(!about.contains("Doe") && !about.contains("Smith"), "no names: {about}");
        }
    }
}

#[test]
fn windows_of_float_data_and_linear_exact() {
    let dir = tmpdir("dicom-window");
    // Float pixel data (a parametric map): centre 0.5 and width 1 is 0 to 1.
    let mut els: Vec<El> = header(&Slice::new(0, 0)).into_iter().filter(|e| ![t(0x28, 0x100), t(0x28, 0x101), t(0x28, 0x102), t(0x28, 0x103), t(0x28, 0x1050), t(0x28, 0x1051)].contains(&e.tag)).collect();
    els.push(us(t(0x28, 0x100), &[32]));
    els.push(ds(t(0x28, 0x1050), &[0.5]));
    els.push(ds(t(0x28, 0x1051), &[1.0]));
    let floats: Vec<u8> = (0..ROWS * COLS).flat_map(|i| (i as f32 / 40.0).to_le_bytes()).collect();
    els.push(El { tag: t(0x7FE0, 0x08), vr: *b"OF", v: V::Raw(floats) });
    let p = write(&dir, "float.dcm", &file(Ts::ExplicitLe, &els));
    let info = open(&p, false).unwrap().header_info();
    assert_eq!(info["dicom"]["window"], json!([0.0, 1.0]), "{info}");
    // Integers: LINEAR (half-step shifts) unless LINEAR_EXACT.
    let mut els = header(&Slice::new(0, 0));
    els.push(s(t(0x28, 0x1056), b"CS", "LINEAR_EXACT"));
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(pixels_le(0, 0)) });
    let p = write(&dir, "exact.dcm", &file(Ts::ImplicitLe, &els));
    let info = open(&p, false).unwrap().header_info();
    assert_eq!(info["dicom"]["window"], json!([-100.5, 300.5]), "{info}");
}

/// An enhanced file whose frames all lie at one position: frame n has the
/// dimension index values `keys[n]` for the `pointers` (StackID and the
/// temporal position first), and the pixels of `pixels_le(0, n)`.
fn enhanced_at_one_position(pointers: &[(u16, u16)], keys: &[Vec<u32>]) -> Vec<u8> {
    let mut els = base_multiframe(keys.len());
    els.push(ds(t(0x18, 0x80), &[1500.0]));
    let frame = |values: &Vec<u32>| {
        vec![
            sq(t(0x20, 0x9111), vec![vec![El { tag: t(0x20, 0x9157), vr: *b"UL", v: V::U32(values.clone()) }]]),
            sq(t(0x20, 0x9113), vec![vec![ds(t(0x20, 0x32), &[0.0, 0.0, 0.0])]]),
        ]
    };
    els.push(sq(t(0x5200, 0x9230), keys.iter().map(frame).collect()));
    els.push(sq(
        t(0x5200, 0x9229),
        vec![vec![
            sq(t(0x20, 0x9116), vec![vec![ds(t(0x20, 0x37), &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0])]]),
            sq(t(0x28, 0x9110), vec![vec![ds(t(0x28, 0x30), &[1.0, 1.0]), ds(t(0x18, 0x50), &[4.0])]]),
        ]],
    ));
    let pointers = pointers.iter().map(|&(g, e)| vec![El { tag: t(0x20, 0x9165), vr: *b"AT", v: V::U16(vec![g, e]) }]).collect();
    els.push(sq(t(0x20, 0x9222), pointers));
    let px: Vec<u8> = (0..keys.len()).flat_map(|f| pixels_le(0, f)).collect();
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(px) });
    file(Ts::ExplicitLe, &els)
}

/// Echoes are not time points: the volumes of an acquisition with several
/// echoes are kept, in order, but get no times (k × TR would put the echoes
/// of one time point at different times) and a warning says why.
#[test]
fn echoes_are_not_time_points() {
    let dir = tmpdir("dicom-echoes");
    let (stack, temporal, echo) = ((0x0020, 0x9056), (0x0020, 0x9128), (0x0018, 0x9082));
    // Enhanced, 2 time points × 2 echoes (frames t1e1, t1e2, t2e1, t2e2).
    let keys = vec![vec![1, 1, 1], vec![1, 1, 2], vec![1, 2, 1], vec![1, 2, 2]];
    let p = write(&dir, "echoes.dcm", &enhanced_at_one_position(&[stack, temporal, echo], &keys));
    let mut v = open(&p, true).unwrap();
    assert_eq!(v.header.shape, vec![COLS, ROWS, 1, 4]);
    assert_eq!(v.header.repetition_time, None, "no times for echoes");
    let warnings = v.header.warnings.join(" ");
    assert!(warnings.contains("2 time points × 2 echoes"), "{warnings}");
    for k in 0..4 {
        while !v.set_volume(k as i64).unwrap().0 {}
        assert_values(&mut v, 0, 0, k);
    }
    // The same file without the echoes: two time points, 1.5 s apart.
    let keys = vec![vec![1, 1], vec![1, 2]];
    let p = write(&dir, "times.dcm", &enhanced_at_one_position(&[stack, temporal], &keys));
    let v = open(&p, true).unwrap();
    assert_eq!(v.header.shape, vec![COLS, ROWS, 1, 2]);
    assert_eq!(v.header.repetition_time, Some(1.5));
    assert!(v.header.warnings.is_empty(), "{:?}", v.header.warnings);
    // Only echoes: no times either.
    let keys = vec![vec![1, 1, 1], vec![1, 1, 2]];
    let p = write(&dir, "two-echoes.dcm", &enhanced_at_one_position(&[stack, temporal, echo], &keys));
    let v = open(&p, true).unwrap();
    assert_eq!(v.header.repetition_time, None);
    assert!(v.header.warnings.join(" ").contains("2 echoes"), "{:?}", v.header.warnings);

    // A series of single images: two slices, two echoes each (same
    // acquisition, EchoNumbers 1 and 2).
    let series = tmpdir("dicom-echo-series");
    for k in 0..2 {
        for e in 0..2 {
            let mut els: Vec<El> = header(&Slice::new(k, e)).into_iter().filter(|x| x.tag != t(0x20, 0x12)).collect();
            els.push(is(t(0x20, 0x12), 1));
            els.push(is(t(0x18, 0x86), 1 + e as i64));
            els.push(ds(t(0x18, 0x81), &[10.0 + 20.0 * e as f64]));
            els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(pixels_le(k, e)) });
            write(&series, &format!("e{k}{e}.dcm"), &file(Ts::ExplicitLe, &els));
        }
    }
    let mut v = open(&series.join("e00.dcm"), true).unwrap();
    assert_eq!(v.header.shape, vec![COLS, ROWS, 2, 2]);
    assert_eq!(v.header.repetition_time, None, "no times for echoes");
    assert!(v.header.warnings.join(" ").contains("2 echoes"), "{:?}", v.header.warnings);
    while !v.set_volume(1).unwrap().0 {}
    assert_values(&mut v, 1, 1, 1);
}

/// Slices whose pixels have another size are left out of the series, like
/// slices of another size or orientation: one affine cannot place both.
#[test]
fn slices_of_another_pixel_spacing_are_left_out() {
    let dir = tmpdir("dicom-spacing");
    for k in 0..3 {
        let mut els = header(&Slice::new(k, 0));
        if k == 1 {
            els.retain(|e| e.tag != t(0x28, 0x30));
            els.push(ds(t(0x28, 0x30), &[1.0, 0.5]));
        }
        els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(pixels_le(k, 0)) });
        write(&dir, &format!("s{k}.dcm"), &file(Ts::ExplicitLe, &els));
    }
    let mut v = open(&dir.join("s0.dcm"), true).unwrap();
    assert_eq!(v.header.shape, vec![COLS, ROWS, 2], "slice 1 left out");
    let warnings = v.header.warnings.join(" ");
    assert!(warnings.contains("1 images of the series have another size, pixel spacing or orientation"), "{warnings}");
    assert_values(&mut v, 2, 1, 0);
    // What assembling the series found is shown to the user (warnings), not
    // listed with the repairs of a header (header_fixes).
    let mut s = Session::new(1 << 20);
    let r = request(&mut s, json!({"op": "open", "path": dir.join("s0.dcm")}));
    assert!(r["header"]["warnings"].to_string().contains("pixel spacing"), "{r}");
    assert_eq!(r["header"]["header_fixes"], json!([]), "{r}");
    // Spacings equal but for rounding stay together.
    let dir = tmpdir("dicom-spacing-rounded");
    for k in 0..3 {
        let mut els = header(&Slice::new(k, 0));
        els.retain(|e| e.tag != t(0x28, 0x30));
        els.push(ds(t(0x28, 0x30), &[if k == 1 { 0.46875 } else { 0.4688 }, 0.25]));
        els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(pixels_le(k, 0)) });
        write(&dir, &format!("s{k}.dcm"), &file(Ts::ExplicitLe, &els));
    }
    let v = open(&dir.join("s0.dcm"), true).unwrap();
    assert_eq!(v.header.shape, vec![COLS, ROWS, 3]);
}

/// Double float pixel data keep the rescale of each image (they were all
/// given the first one's).
#[test]
fn double_float_slices_keep_their_own_rescale() {
    let dir = tmpdir("dicom-f64");
    for (k, (slope, intercept)) in [(1.0, 0.0), (2.0, 100.0)].into_iter().enumerate() {
        let drop = [t(0x28, 0x100), t(0x28, 0x101), t(0x28, 0x102), t(0x28, 0x103), t(0x28, 0x1050), t(0x28, 0x1051)];
        let mut els: Vec<El> = header(&Slice::new(k, 0)).into_iter().filter(|e| !drop.contains(&e.tag)).collect();
        els.push(us(t(0x28, 0x100), &[64]));
        els.push(ds(t(0x28, 0x1052), &[intercept]));
        els.push(ds(t(0x28, 0x1053), &[slope]));
        let px: Vec<u8> = (0..ROWS * COLS).flat_map(|_| 14f64.to_le_bytes()).collect();
        els.push(El { tag: t(0x7FE0, 0x09), vr: *b"OD", v: V::Raw(px) });
        write(&dir, &format!("d{k}.dcm"), &file(Ts::ExplicitLe, &els));
    }
    let mut v = open(&dir.join("d0.dcm"), true).unwrap();
    assert_eq!(v.header.shape, vec![COLS, ROWS, 2]);
    assert_eq!(v.header.dtype.label(), "float64");
    assert_eq!(v.value_at([0, 0, 0]).unwrap(), Some(14.0));
    assert_eq!(v.value_at([0, 0, 1]).unwrap(), Some(128.0), "14 × 2 + 100");
}

/// An 8-bit image with a VOI LUT and no window. `entries` are the LUT's
/// 16-bit outputs for the stored values 0 to 255.
fn voi_lut_file(entries: &[u16], extra: Vec<El>) -> Vec<u8> {
    let drop = [t(0x28, 0x100), t(0x28, 0x101), t(0x28, 0x102), t(0x28, 0x103), t(0x28, 0x1050), t(0x28, 0x1051)];
    let mut els: Vec<El> = header(&Slice::new(0, 0)).into_iter().filter(|e| !drop.contains(&e.tag)).collect();
    els.extend([us(t(0x28, 0x100), &[8]), us(t(0x28, 0x101), &[8]), us(t(0x28, 0x102), &[7]), us(t(0x28, 0x103), &[0])]);
    els.push(sq(
        t(0x28, 0x3010),
        vec![vec![us(t(0x28, 0x3002), &[entries.len() as u16, 0, 16]), El { tag: t(0x28, 0x3006), vr: *b"OW", v: V::U16(entries.to_vec()) }]],
    ));
    els.extend(extra);
    let px: Vec<u8> = (0..ROWS * COLS).map(|i| (i * 7) as u8).collect();
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(px) });
    file(Ts::ExplicitLe, &els)
}

/// A VOI LUT given instead of a window: a linear one over the whole output
/// range is that window, exactly; another one is not applied, and a warning
/// says that the window shown is not the file's. SIGMOID windows are shown
/// linear, with a warning.
#[test]
fn voi_lut_is_applied_when_it_is_a_window() {
    let dir = tmpdir("dicom-voi");
    // The identity of DICOM's VOI LUT test file vlut_04: 0, 257, ... 65535.
    let linear: Vec<u16> = (0..256u32).map(|i| (i * 257) as u16).collect();
    let p = write(&dir, "linear.dcm", &voi_lut_file(&linear, vec![]));
    let info = open(&p, false).unwrap().header_info();
    assert_eq!(info["dicom"]["window"], json!([0.0, 255.0]), "{info}");
    assert!(info["warnings"].as_array().unwrap().is_empty(), "{info}");
    let mut session = Session::new(1 << 20);
    assert_eq!(request(&mut session, json!({"op": "open", "path": p}))["ok"], true);
    let r = request(&mut session, json!({"op": "load"}));
    assert_eq!(r["display_window"]["source"], "file", "{r}");
    assert_eq!((r["display_window"]["low"].clone(), r["display_window"]["high"].clone()), (json!(0.0), json!(255.0)), "{r}");
    // A gamma curve: not a window.
    let gamma: Vec<u16> = (0..256u32).map(|i| (65535.0 * (i as f64 / 255.0).powf(0.5)).round() as u16).collect();
    let p = write(&dir, "gamma.dcm", &voi_lut_file(&gamma, vec![]));
    let info = open(&p, false).unwrap().header_info();
    assert_eq!(info["dicom"]["window"], Value::Null, "{info}");
    let warnings = info["warnings"].to_string();
    assert!(warnings.contains("VOI LUT"), "{warnings}");
    // A window with the SIGMOID function.
    let mut els = header(&Slice::new(0, 0));
    els.push(s(t(0x28, 0x1056), b"CS", "SIGMOID"));
    els.push(El { tag: t(0x7FE0, 0x10), vr: *b"OW", v: V::Raw(pixels_le(0, 0)) });
    let p = write(&dir, "sigmoid.dcm", &file(Ts::ExplicitLe, &els));
    let info = open(&p, false).unwrap().header_info();
    assert!(info["dicom"]["window"].is_array(), "{info}");
    assert!(info["warnings"].to_string().contains("SIGMOID"), "{info}");
}

/// A DICOMDIR's image that is a link to a file outside its folder is not
/// listed.
#[test]
fn dicomdir_images_behind_links_stay_in_the_folder() {
    let top = tmpdir("dicom-dicomdir-links");
    let root = top.join("disc");
    std::fs::create_dir_all(&root).unwrap();
    for k in 0..2 {
        write(&root, &format!("i{k}"), &slice_file(&Slice::new(k, 0), Ts::ExplicitLe));
    }
    write(&top, "elsewhere", &slice_file(&Slice::new(2, 0), Ts::ExplicitLe));
    std::os::unix::fs::symlink(top.join("elsewhere"), root.join("i2")).unwrap();
    std::os::unix::fs::symlink("i1", root.join("i1link")).unwrap();
    let rec = |kind: &str, extra: Vec<El>| {
        let mut r = vec![s(t(4, 0x1430), b"CS", kind)];
        r.extend(extra);
        r
    };
    let image = |path: &str| rec("IMAGE", vec![s(t(4, 0x1500), b"CS", path)]);
    let records = vec![
        rec("PATIENT", vec![]),
        rec("STUDY", vec![ui(t(0x20, 0x0D), "1.2.3.4")]),
        rec("SERIES", vec![ui(t(0x20, 0x0E), "1.2.3.4.5")]),
        image("I0"),
        image("I1LINK"),
        image("I2"),
    ];
    let p = dicomdir(&root, records);
    let mut s = Session::new(1 << 20);
    assert_eq!(request(&mut s, json!({"op": "open", "path": p}))["ok"], true);
    let l = request(&mut s, json!({"op": "series_list"}));
    assert_eq!(l["series"][0]["images"], 2, "the link inside the folder only: {l}");
}
