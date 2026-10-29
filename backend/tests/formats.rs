//! MGH/MGZ, MRtrix and NRRD against NIfTI: the same array written in each
//! format, with every storage order the formats allow (permuted and reversed
//! axes, volumes interleaved with voxels, bits, gzip after a text header,
//! detached data files), must give the same grid, affine, slices, values and
//! time courses as the NIfTI file. Plus hostile headers.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{gz, tmpdir, within, Nifti};
use gnome_nifti_preview::server::{respond, Session};
use serde_json::{json, Value};

/// A 4D float32 test image: value(i, j, k, t) distinct everywhere.
const SHAPE: [usize; 4] = [7, 5, 4, 3];
fn value(i: usize, j: usize, k: usize, t: usize) -> f32 {
    (1 + i + 10 * j + 100 * k + 1000 * t) as f32
}
/// Values in Fortran order of the logical axes.
fn fortran() -> Vec<f32> {
    let [x, y, z, n] = SHAPE;
    let mut v = Vec::new();
    for t in 0..n {
        for k in 0..z {
            for j in 0..y {
                for i in 0..x {
                    v.push(value(i, j, k, t));
                }
            }
        }
    }
    v
}
/// An affine with rotation, scaling and offset (RAS).
const AFFINE: [[f64; 4]; 3] = [[0.0, -2.0, 0.0, 10.0], [1.5, 0.0, 0.0, -20.0], [0.0, 0.0, 3.0, 5.0]];

fn nifti(dir: &Path) -> PathBuf {
    let mut n = Nifti::new(&SHAPE.map(|d| d as i64), 16);
    n.srow = AFFINE;
    n.pixdim[1..4].copy_from_slice(&[1.5, 2.0, 3.0]);
    let bytes: Vec<u8> = fortran().iter().flat_map(|v| v.to_le_bytes()).collect();
    let p = dir.join("ref.nii");
    std::fs::write(&p, n.file(&bytes)).unwrap();
    p
}

/// Logical index of the element stored at position `pos` for a storage
/// order given as (logical axis, reversed) from fastest to slowest.
fn stored(order: &[(usize, bool)]) -> Vec<f32> {
    let dims: Vec<usize> = order.iter().map(|&(a, _)| SHAPE[a]).collect();
    let total: usize = dims.iter().product();
    let mut out = Vec::with_capacity(total);
    for pos in 0..total {
        let mut rest = pos;
        let mut idx = [0usize; 4];
        for (d, &(a, rev)) in order.iter().enumerate() {
            let c = rest % dims[d];
            rest /= dims[d];
            idx[a] = if rev { SHAPE[a] - 1 - c } else { c };
        }
        out.push(value(idx[0], idx[1], idx[2], idx[3]));
    }
    out
}

fn mif(dir: &Path, name: &str, layout: &str, order: &[(usize, bool)], compress: bool) -> PathBuf {
    // MRtrix transform: rotation (unit columns) and translation; vox scales.
    let vox = [1.5, 2.0, 3.0];
    let mut t = String::new();
    for row in AFFINE {
        let r: Vec<String> = (0..3).map(|c| format!("{}", row[c] / vox[c])).collect();
        t.push_str(&format!("transform: {},{}\n", r.join(","), row[3]));
    }
    let mut head = format!(
        "mrtrix image\ndim: 7,5,4,3\nvox: 1.5,2,3,nan\nlayout: {layout}\ndatatype: Float32LE\n{t}comments: SECRET PATIENT\nfile: . OFFSET\nEND\n"
    );
    let offset = head.len() + 16;
    head = head.replace("OFFSET", &format!("{offset:<6}"));
    let mut bytes = head.into_bytes();
    bytes.resize(offset + 4, b' ');
    bytes.truncate(offset);
    bytes.extend(stored(order).iter().flat_map(|v| v.to_le_bytes()));
    let p = dir.join(name);
    std::fs::write(&p, if compress { gz(&bytes) } else { bytes }).unwrap();
    p
}

/// NRRD in LPS (3D Slicer): directions and origin negated in x and y.
fn nrrd_header(sizes: &str, directions: &str, encoding: &str, extra: &str) -> String {
    format!(
        "NRRD0004\n# test\ntype: float\ndimension: 4\nspace: left-posterior-superior\nsizes: {sizes}\nspace directions: {directions}\n\
kinds: domain domain domain list\nendian: little\nencoding: {encoding}\nspace origin: (-10,20,5)\n{extra}"
    )
}
fn lps(c: usize) -> String {
    format!("({},{},{})", -AFFINE[0][c], -AFFINE[1][c], AFFINE[2][c])
}

fn request(s: &mut Session, req: Value) -> (Value, Option<Vec<u8>>) {
    let (h, p) = respond(s, req.to_string().as_bytes());
    (h, p.map(|p| p.to_vec()))
}

fn show(s: &mut Session, k: i64) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (r, _) = request(s, json!({"op": "set_volume", "volume": k}));
        assert_eq!(r["ok"], true, "{r}");
        if r["ready"] == true {
            return;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// World points (centres of NIfTI voxels) where the formats are compared.
fn world_points() -> Vec<[f64; 3]> {
    [[0usize, 0, 0], [3, 2, 1], [6, 4, 3], [1, 3, 2]]
        .iter()
        .map(|ijk| [0, 1, 2].map(|r| AFFINE[r][3] + (0..3).map(|c| AFFINE[r][c] * ijk[c] as f64).sum::<f64>()))
        .collect()
}

/// Grid point of world point `w` for a grid affine (4×4, JSON).
fn grid_point(affine: &Value, w: [f64; 3]) -> [i64; 3] {
    let a: Vec<Vec<f64>> = affine.as_array().unwrap().iter().map(|r| r.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect()).collect();
    let m = [[a[0][0], a[0][1], a[0][2]], [a[1][0], a[1][1], a[1][2]], [a[2][0], a[2][1], a[2][2]]];
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let inv = |r: usize, c: usize| {
        let (r1, r2) = ((c + 1) % 3, (c + 2) % 3);
        let (c1, c2) = ((r + 1) % 3, (r + 2) % 3);
        (m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]) / det
    };
    let d = [w[0] - a[0][3], w[1] - a[1][3], w[2] - a[2][3]];
    [0, 1, 2].map(|r| (inv(r, 0) * d[0] + inv(r, 1) * d[1] + inv(r, 2) * d[2]).round() as i64)
}

/// What the views show of `path` around the world points, in a form that
/// does not depend on how the voxels are stored.
fn render(path: &Path) -> Vec<Value> {
    let mut s = Session::new(16 << 20);
    let (open, _) = request(&mut s, json!({"op": "open", "path": path}));
    assert_eq!(open["ok"], true, "{}: {open}", path.display());
    request(&mut s, json!({"op": "load"}));
    let affine = open["grid"]["affine"].clone();
    let plans = &open["plans"];
    let mut out = Vec::new();
    for view in ["axial", "coronal", "sagittal"] {
        let p = &plans[view];
        out.push(json!([view, p["width"], p["height"], p["n_slices"], p["labels"], p["pixel_w"], p["pixel_h"]]));
    }
    let points: Vec<[i64; 3]> = world_points().into_iter().map(|w| grid_point(&affine, w)).collect();
    for k in 0..SHAPE[3] {
        show(&mut s, k as i64);
        for view in ["axial", "coronal", "sagittal"] {
            let axis = plans[view]["slice_axis"].as_u64().unwrap() as usize;
            for g in &points {
                let (r, px) = request(&mut s, json!({"op": "slice", "view": view, "index": g[axis], "colormap": "gray"}));
                assert_eq!(r["ok"], true, "{r}");
                out.push(json!([view, k, px.unwrap()]));
            }
        }
        for g in &points {
            out.push(request(&mut s, json!({"op": "value", "ijk": g})).0["value"].clone());
        }
    }
    let (tc, _) = request(&mut s, json!({"op": "timecourse", "ijk": points[1]}));
    out.push(tc["values"].clone());
    out
}

fn same_as_nifti(reference: &[Value], path: &Path) {
    let got = render(path);
    assert_eq!(got.len(), reference.len());
    for (i, (a, b)) in reference.iter().zip(&got).enumerate() {
        assert_eq!(a, b, "{}: item {i} differs", path.display());
    }
}

#[test]
fn mrtrix_layouts_match_nifti() {
    within(120, "mrtrix", || {
        let dir = tmpdir("fmt-mif");
        let reference = render(&nifti(&dir));
        // Canonical, reversed, permuted, interleaved (volumes fastest), gzip.
        for (name, layout, order, gzip) in [
            ("plain.mif", "+0,+1,+2,+3", vec![(0, false), (1, false), (2, false), (3, false)], false),
            ("reversed.mif", "-0,-1,+2,+3", vec![(0, true), (1, true), (2, false), (3, false)], false),
            ("permuted.mif", "+1,-2,+0,+3", vec![(2, false), (0, false), (1, true), (3, false)], false),
            ("interleaved.mif", "-1,-2,+3,+0", vec![(3, false), (0, true), (1, true), (2, false)], false),
            ("interleaved.mif.gz", "-1,-2,+3,+0", vec![(3, false), (0, true), (1, true), (2, false)], true),
            ("plain.mif.gz", "+0,+1,+2,+3", vec![(0, false), (1, false), (2, false), (3, false)], true),
        ] {
            same_as_nifti(&reference, &mif(&dir, name, layout, &order, gzip));
        }
        // A header alone (.mih, without END): the data in two files, the
        // second after a few bytes.
        let data: Vec<u8> = stored(&[(0, false), (1, false), (2, false), (3, false)]).iter().flat_map(|v| v.to_le_bytes()).collect();
        let half = data.len() / 2;
        std::fs::write(dir.join("first.dat"), &data[..half]).unwrap();
        std::fs::write(dir.join("second.dat"), [b"skip".as_slice(), &data[half..]].concat()).unwrap();
        let head: String = String::from_utf8_lossy(&std::fs::read(dir.join("plain.mif")).unwrap()).lines()
            .take_while(|l| !l.starts_with("file:")).map(|l| l.to_string() + "\n").collect();
        std::fs::write(dir.join("split.mih"), format!("{head}file: first.dat\nfile: second.dat 4\n")).unwrap();
        same_as_nifti(&reference, &dir.join("split.mih"));
        // Comments (patient details) are never shown.
        let mut s = Session::new(1 << 20);
        let (open, _) = request(&mut s, json!({"op": "open", "path": dir.join("plain.mif")}));
        assert!(!open.to_string().contains("SECRET"), "{open}");
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn nrrd_variants_match_nifti() {
    within(120, "nrrd", || {
        let dir = tmpdir("fmt-nrrd");
        let reference = render(&nifti(&dir));
        let canonical = stored(&[(0, false), (1, false), (2, false), (3, false)]);
        let bytes = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
        let dirs = format!("{} {} {} none", lps(0), lps(1), lps(2));
        // Attached raw, attached gzip.
        for (name, encoding) in [("raw.nrrd", "raw"), ("gzip.nrrd", "gzip")] {
            let mut f = nrrd_header("7 5 4 3", &dirs, encoding, "\n").into_bytes();
            let data = bytes(&canonical);
            f.extend(if encoding == "gzip" { gz(&data) } else { data });
            std::fs::write(dir.join(name), f).unwrap();
            same_as_nifti(&reference, &dir.join(name));
        }
        // Lines skipped after the header, longer than the bytes read with it.
        let mut f = nrrd_header("7 5 4 3", &dirs, "raw", "line skip: 3\n\n").into_bytes();
        for i in 0..3 {
            f.extend(format!("{}\n", format!("comment {i} ").repeat(60)).into_bytes());
        }
        f.extend(bytes(&canonical));
        std::fs::write(dir.join("lines.nrrd"), f).unwrap();
        same_as_nifti(&reference, &dir.join("lines.nrrd"));
        // The list axis first (3D Slicer DWI): volumes interleaved with voxels.
        let first = stored(&[(3, false), (0, false), (1, false), (2, false)]);
        let header = format!(
            "NRRD0005\ntype: float\ndimension: 4\nspace: left-posterior-superior\nsizes: 3 7 5 4\nspace directions: none {} {} {}\n\
kinds: list domain domain domain\nendian: little\nencoding: gzip\nspace origin: (-10,20,5)\n\n",
            lps(0), lps(1), lps(2)
        );
        let mut f = header.into_bytes();
        f.extend(gz(&bytes(&first)));
        std::fs::write(dir.join("list-first.nrrd"), f).unwrap();
        same_as_nifti(&reference, &dir.join("list-first.nrrd"));
        // Detached: one raw file; three gzip files (one volume each, a pattern);
        // a LIST; data at the end of a file (byte skip -1).
        std::fs::write(dir.join("one.raw"), bytes(&canonical)).unwrap();
        std::fs::write(dir.join("one.nhdr"), nrrd_header("7 5 4 3", &dirs, "raw", "data file: one.raw\n")).unwrap();
        same_as_nifti(&reference, &dir.join("one.nhdr"));
        let per = canonical.len() / 3;
        for t in 0..3 {
            std::fs::write(dir.join(format!("vol{t:02}.raw.gz")), gz(&bytes(&canonical[t * per..(t + 1) * per]))).unwrap();
        }
        std::fs::write(dir.join("pattern.nhdr"), nrrd_header("7 5 4 3", &dirs, "gzip", "data file: vol%02d.raw.gz 0 2 1 3\n")).unwrap();
        same_as_nifti(&reference, &dir.join("pattern.nhdr"));
        std::fs::write(dir.join("list.nhdr"), nrrd_header("7 5 4 3", &dirs, "gzip", "data file: LIST 3\nvol00.raw.gz\nvol01.raw.gz\nvol02.raw.gz\n")).unwrap();
        same_as_nifti(&reference, &dir.join("list.nhdr"));
        let mut tail = b"junk at the start".to_vec();
        tail.extend(bytes(&canonical));
        std::fs::write(dir.join("tail.raw"), tail).unwrap();
        std::fs::write(dir.join("tail.nhdr"), nrrd_header("7 5 4 3", &dirs, "raw", "byte skip: -1\ndata file: tail.raw\n")).unwrap();
        same_as_nifti(&reference, &dir.join("tail.nhdr"));
        // Text encodings: ascii (numbers on lines, commas), the list axis
        // first; hex of big-endian values; ascii in three files, a line skipped.
        let text = |v: &[f32], per_line: usize| -> String {
            v.chunks(per_line).map(|c| c.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", ")).collect::<Vec<_>>().join("\n")
        };
        let header = format!(
            "NRRD0005\ntype: float\ndimension: 4\nspace: left-posterior-superior\nsizes: 3 7 5 4\nspace directions: none {} {} {}\n\
kinds: list domain domain domain\nencoding: ascii\nspace origin: (-10,20,5)\n\n{}\n",
            lps(0), lps(1), lps(2), text(&first, 7)
        );
        std::fs::write(dir.join("ascii.nrrd"), header).unwrap();
        same_as_nifti(&reference, &dir.join("ascii.nrrd"));
        let hex: String = canonical.iter().flat_map(|x| x.to_be_bytes()).map(|b| format!("{b:02x}")).collect::<Vec<_>>().chunks(32).map(|c| c.concat()).collect::<Vec<_>>().join("\n");
        let f = nrrd_header("7 5 4 3", &dirs, "hex", "").replace("endian: little", "endian: big") + "\n" + &hex;
        std::fs::write(dir.join("hex.nrrd"), f).unwrap();
        same_as_nifti(&reference, &dir.join("hex.nrrd"));
        for t in 0..3 {
            std::fs::write(dir.join(format!("vol{t}.txt")), format!("# volume {t}\n{}", text(&canonical[t * per..(t + 1) * per], 5))).unwrap();
        }
        std::fs::write(dir.join("text.nhdr"), nrrd_header("7 5 4 3", &dirs, "ascii", "line skip: 1\ndata file: vol%d.txt 0 2 1\n")).unwrap();
        same_as_nifti(&reference, &dir.join("text.nhdr"));
        // bzip2 (with the reference tool, when installed): attached, and
        // detached after a few bytes.
        if let Some(packed) = bzip2_tool(&bytes(&canonical)) {
            let mut f = nrrd_header("7 5 4 3", &dirs, "bzip2", "\n").into_bytes();
            f.extend(&packed);
            std::fs::write(dir.join("bz2.nrrd"), f).unwrap();
            same_as_nifti(&reference, &dir.join("bz2.nrrd"));
            let skipped = bzip2_tool(&[b"12345".as_slice(), &bytes(&canonical)].concat()).unwrap();
            std::fs::write(dir.join("data.raw.bz2"), skipped).unwrap();
            std::fs::write(dir.join("bz2.nhdr"), nrrd_header("7 5 4 3", &dirs, "bz2", "byte skip: 5\ndata file: data.raw.bz2\n")).unwrap();
            same_as_nifti(&reference, &dir.join("bz2.nhdr"));
        }
        std::fs::remove_dir_all(&dir).ok();
    });
}

fn mgh(dir: &Path, name: &str, compress: bool) -> PathBuf {
    let mut b = vec![0u8; 284];
    let put_i = |b: &mut Vec<u8>, o: usize, v: i32| b[o..o + 4].copy_from_slice(&v.to_be_bytes());
    let put_f = |b: &mut Vec<u8>, o: usize, v: f64| b[o..o + 4].copy_from_slice(&(v as f32).to_be_bytes());
    put_i(&mut b, 0, 1);
    for (i, d) in SHAPE.iter().enumerate() {
        put_i(&mut b, 4 + 4 * i, *d as i32);
    }
    put_i(&mut b, 20, 3);
    b[28..30].copy_from_slice(&1i16.to_be_bytes());
    let vox = [1.5, 2.0, 3.0];
    for (i, v) in vox.iter().enumerate() {
        put_f(&mut b, 30 + 4 * i, *v);
    }
    for (r, row) in AFFINE.iter().enumerate() {
        for c in 0..3 {
            put_f(&mut b, 42 + 12 * c + 4 * r, row[c] / vox[c]);
        }
        // c_ras: the world position of voxel dims / 2.
        let centre: f64 = row[3] + (0..3).map(|c| row[c] * SHAPE[c] as f64 / 2.0).sum::<f64>();
        put_f(&mut b, 78 + 4 * r, centre);
    }
    b.extend(fortran().iter().flat_map(|v| v.to_be_bytes()));
    let p = dir.join(name);
    std::fs::write(&p, if compress { gz(&b) } else { b }).unwrap();
    p
}

#[test]
fn mgh_matches_nifti() {
    within(120, "mgh", || {
        let dir = tmpdir("fmt-mgh");
        let reference = render(&nifti(&dir));
        same_as_nifti(&reference, &mgh(&dir, "a.mgh", false));
        same_as_nifti(&reference, &mgh(&dir, "a.mgz", true));
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn mrtrix_bits() {
    let dir = tmpdir("fmt-bits");
    // 10 × 3 × 2 voxels, one bit each, most significant first: voxel i is set when i % 3 == 0.
    let n = 60;
    let mut packed = vec![0u8; n / 8 + 1];
    for i in (0..n).step_by(3) {
        packed[i / 8] |= 0x80 >> (i % 8);
    }
    let head = "mrtrix image\ndim: 10,3,2\nvox: 1,1,1\nlayout: +0,+1,+2\ndatatype: Bit\nfile: . 128\nEND\n";
    let mut b = head.as_bytes().to_vec();
    b.resize(128, b' ');
    b.extend(&packed);
    let p = dir.join("mask.mif");
    std::fs::write(&p, &b).unwrap();
    let mut s = Session::new(1 << 20);
    assert_eq!(request(&mut s, json!({"op": "open", "path": p})).0["ok"], true);
    let (load, _) = request(&mut s, json!({"op": "load"}));
    assert_eq!(load["stats"]["labels"]["binary"], true, "{load}");
    for (ijk, v) in [([0, 0, 0], 1), ([1, 0, 0], 0), ([3, 0, 0], 1), ([9, 2, 1], 0), ([7, 2, 1], 1)] {
        let i = ijk[0] + 10 * ijk[1] + 30 * ijk[2];
        assert_eq!(v, if i % 3 == 0 { 1 } else { 0 });
        assert_eq!(request(&mut s, json!({"op": "value", "ijk": ijk})).0["value"], v, "{ijk:?}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

fn outcome(path: &Path) -> String {
    let mut s = Session::new(1 << 20);
    let (open, _) = request(&mut s, json!({"op": "open", "path": path}));
    if open["ok"] != true {
        return open["error"]["code"].as_str().unwrap().to_string();
    }
    let (load, _) = request(&mut s, json!({"op": "load"}));
    if load["ok"] != true {
        return load["error"]["code"].as_str().unwrap().to_string();
    }
    "ok".into()
}

/// A MetaImage header for the test image (ITK: rows of TransformMatrix are
/// the LPS directions of the axes, spacing apart).
fn mha_header(extra: &str, data: &str) -> String {
    format!(
        "ObjectType = Image\nNDims = 4\nBinaryData = True\n{extra}TransformMatrix = 0 -1 0 0 1 0 0 0 0 0 1 0 0 0 0 1\n\
Offset = -10 20 5 0\nElementSpacing = 1.5 2 3 1\nDimSize = 7 5 4 3\nElementType = MET_FLOAT\nElementDataFile = {data}\n"
    )
}

#[test]
fn metaimage_variants_match_nifti() {
    within(120, "metaimage", || {
        use std::io::Write;
        let dir = tmpdir("fmt-mha");
        let reference = render(&nifti(&dir));
        let canonical = stored(&[(0, false), (1, false), (2, false), (3, false)]);
        let le: Vec<u8> = canonical.iter().flat_map(|x| x.to_le_bytes()).collect();
        let be: Vec<u8> = canonical.iter().flat_map(|x| x.to_be_bytes()).collect();
        let write = |name: &str, header: String, data: &[u8]| {
            let mut f = header.into_bytes();
            f.extend_from_slice(data);
            std::fs::write(dir.join(name), f).unwrap();
            dir.join(name)
        };
        // Attached raw, big endian, zlib.
        same_as_nifti(&reference, &write("raw.mha", mha_header("", "LOCAL"), &le));
        same_as_nifti(&reference, &write("be.mha", mha_header("BinaryDataByteOrderMSB = True\n", "LOCAL"), &be));
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&le).unwrap();
        let z = z.finish().unwrap();
        same_as_nifti(&reference, &write("zlib.mha", mha_header(&format!("CompressedData = True\nCompressedDataSize = {}\n", z.len()), "LOCAL"), &z));
        // Detached data, also at the end of a file (HeaderSize -1).
        std::fs::write(dir.join("data.raw"), &le).unwrap();
        same_as_nifti(&reference, &write("detached.mhd", mha_header("", "data.raw"), &[]));
        std::fs::write(dir.join("tail.raw"), [b"junk".to_vec(), le.clone()].concat()).unwrap();
        same_as_nifti(&reference, &write("tail.mhd", mha_header("HeaderSize = -1\n", "tail.raw"), &[]));
        // The volumes as channels, interleaved with the voxels.
        let channels = stored(&[(3, false), (0, false), (1, false), (2, false)]);
        let bytes: Vec<u8> = channels.iter().flat_map(|x| x.to_le_bytes()).collect();
        let header = mha_header("", "LOCAL")
            .replace("NDims = 4", "NDims = 3")
            .replace("TransformMatrix = 0 -1 0 0 1 0 0 0 0 0 1 0 0 0 0 1", "TransformMatrix = 0 -1 0 1 0 0 0 0 1")
            .replace("Offset = -10 20 5 0", "Offset = -10 20 5")
            .replace("ElementSpacing = 1.5 2 3 1", "ElementSpacing = 1.5 2 3")
            .replace("DimSize = 7 5 4 3", "DimSize = 7 5 4\nElementNumberOfChannels = 3");
        same_as_nifti(&reference, &write("channels.mha", header, &bytes));
        // Outside the header's folder: refused.
        let p = write("escape.mhd", mha_header("", "../data.raw"), &[]);
        assert_eq!(outcome(&p), "unsupported_format");
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn hostile_headers_fail_cleanly() {
    within(60, "hostile", || {
        let dir = tmpdir("fmt-hostile");
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            ("huge.mif", b"mrtrix image\ndim: 100000,100000,100000\ndatatype: Float32LE\nfile: . 100\nEND\n".to_vec(), "too_large"),
            ("short.mif", b"mrtrix image\ndim: 4,4,4\ndatatype: Float32LE\nfile: . 70\nEND\n".to_vec(), "corrupt_data"),
            ("noend.mif", [b"mrtrix image\ndim: 4,4,4\n".to_vec(), vec![b'x'; 5 << 20]].concat(), "invalid_image"),
            ("inside.mif", b"mrtrix image\ndim: 4,4,4\ndatatype: Float32LE\nfile: . 3\nEND\n".to_vec(), "invalid_image"),
            ("layout.mif", b"mrtrix image\ndim: 4,4,4\nlayout: +0,+1,+7\ndatatype: Float32LE\nfile: . 80\nEND\n".to_vec(), "invalid_image"),
            ("mih.mih", b"mrtrix image\ndim: 4,4,4\ndatatype: Float32LE\nfile: data.dat 0\nEND\n".to_vec(), "not_found"),
            ("mihdev.mih", b"mrtrix image\ndim: 4,4,4\ndatatype: Float32LE\nfile: /dev/zero\n".to_vec(), "unsupported_format"),
            ("mihparent.mih", b"mrtrix image\ndim: 4,4,4\ndatatype: Float32LE\nfile: ../x.dat\n".to_vec(), "unsupported_format"),
            ("mihshort.mih", b"mrtrix image\ndim: 4,4,4\ndatatype: Float32LE\nfile: mih.mih\n".to_vec(), "corrupt_data"),
            ("huge.nrrd", b"NRRD0004\ntype: float\ndimension: 3\nsizes: 99999 99999 99999\nencoding: raw\n\nxx".to_vec(), "too_large"),
            ("bomb.nrrd", [b"NRRD0004\ntype: uchar\ndimension: 3\nsizes: 2000 2000 500\nencoding: gzip\n\n".to_vec(), gz(&vec![0u8; 1000])].concat(), "corrupt_data"),
            ("missing.nhdr", b"NRRD0004\ntype: float\ndimension: 3\nsizes: 4 4 4\nencoding: raw\ndata file: nowhere.raw\n".to_vec(), "not_found"),
            ("device.nhdr", b"NRRD0004\ntype: float\ndimension: 3\nsizes: 4 4 4\nencoding: raw\ndata file: /dev/zero\n".to_vec(), "unsupported_format"),
            ("parent.nhdr", b"NRRD0004\ntype: uchar\ndimension: 3\nsizes: 4 4 4\nencoding: raw\ndata file: LIST\na.raw\n../../etc/hostname\n".to_vec(), "unsupported_format"),
            ("overflow.nhdr", b"NRRD0004\ntype: uchar\ndimension: 3\nsizes: 4 4 4\nencoding: raw\ndata file: x%d.raw 9223372036854775805 9223372036854775807 1\n".to_vec(), "not_found"),
            ("overflow2.nhdr", b"NRRD0004\ntype: uchar\ndimension: 3\nsizes: 4 4 4\nencoding: raw\ndata file: x%d.raw -9223372036854775808 9223372036854775807 1\n".to_vec(), "invalid_image"),
            ("many.nhdr", b"NRRD0004\ntype: float\ndimension: 3\nsizes: 4 4 4\nencoding: raw\ndata file: f%d.raw 0 99999999 1\n".to_vec(), "invalid_image"),
            ("noheaderend.nrrd", [b"NRRD0004\ntype: float\n".to_vec(), vec![b'a'; 5 << 20]].concat(), "invalid_image"),
            ("short.nrrd", b"NRRD0004\ntype: float\ndimension: 3\nsizes: 4 4 4\nencoding: ascii\n\n1 2 3\n".to_vec(), "corrupt_data"),
            ("words.nrrd", b"NRRD0004\ntype: short\ndimension: 1\nsizes: 3\nencoding: ascii\n\n1 two 3\n".to_vec(), "corrupt_data"),
            ("token.nrrd", [b"NRRD0004\ntype: short\ndimension: 1\nsizes: 3\nencoding: ascii\n\n".to_vec(), vec![b'7'; 1 << 20]].concat(), "corrupt_data"),
            ("bz2junk.nrrd", b"NRRD0004\ntype: uchar\ndimension: 1\nsizes: 3\nencoding: bzip2\n\nBZh9garbage".to_vec(), "corrupt_data"),
            ("bz2short.nrrd", [b"NRRD0004\ntype: uchar\ndimension: 1\nsizes: 30\nencoding: bzip2\n\n".to_vec(), include_bytes!("../src/formats/testdata/hello.bz2").to_vec()].concat(), "corrupt_data"),
            ("hexbad.nrrd", b"NRRD0004\ntype: uchar\ndimension: 1\nsizes: 3\nencoding: hex\n\n01 0g 03\n".to_vec(), "corrupt_data"),
            ("hugetext.nrrd", b"NRRD0004\ntype: float\ndimension: 3\nsizes: 99999 99999 99999\nencoding: ascii\n\n1\n".to_vec(), "too_large"),
            ("devtext.nhdr", b"NRRD0004\ntype: float\ndimension: 3\nsizes: 4 4 4\nencoding: ascii\ndata file: /dev/zero\n".to_vec(), "unsupported_format"),
            ("2d.mif", b"mrtrix image\ndim: 4,4\nvox: 1,1\ndatatype: UInt8\nlayout: +0,+1\nfile: . 128\nEND\n".to_vec(), "corrupt_data"),
            ("huge.mha", b"ObjectType = Image\nNDims = 3\nDimSize = 99999 99999 99999\nElementType = MET_FLOAT\nElementDataFile = LOCAL\n".to_vec(), "too_large"),
            ("bomb.mha", [b"ObjectType = Image\nNDims = 3\nDimSize = 2000 2000 500\nElementType = MET_UCHAR\nCompressedData = True\nElementDataFile = LOCAL\n".to_vec(), vec![0x78, 0x9c, 3, 0]].concat(), "corrupt_data"),
            ("type.mha", b"ObjectType = Image\nNDims = 3\nDimSize = 4 4 4\nElementType = MET_STRING\nElementDataFile = LOCAL\n".to_vec(), "unsupported_datatype"),
            ("short.mha", b"ObjectType = Image\nNDims = 3\nDimSize = 4 4 4\nElementType = MET_FLOAT\nElementDataFile = LOCAL\n".to_vec(), "corrupt_data"),
            ("noend.mha", [b"ObjectType = Image\nNDims = 3\n".to_vec(), vec![b'a'; 2 << 20]].concat(), "invalid_image"),
            ("mgh-dims.mgh", { let mut b = vec![0u8; 300]; b[3] = 1; b[7] = 0xff; b[6] = 0xff; b[5] = 0xff; b[4] = 0x7f; b[11] = 1; b[15] = 1; b[19] = 1; b[23] = 3; b }, "invalid_image"),
        ];
        for (name, bytes, code) in cases {
            let p = write(name, &bytes);
            assert_eq!(outcome(&p), code, "{name}");
        }
        std::fs::remove_dir_all(&dir).ok();
    });
}

/// `data` compressed by the bzip2 tool, if installed.
fn bzip2_tool(data: &[u8]) -> Option<Vec<u8>> {
    let mut child = std::process::Command::new("bzip2")
        .arg("-c")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let input = data.to_vec();
    let writer = std::thread::spawn(move || std::io::Write::write_all(&mut stdin, &input));
    let packed = child.wait_with_output().ok()?.stdout;
    writer.join().ok()?.ok()?;
    Some(packed)
}

/// The bzip2 decoder against the reference tool (skipped without it):
/// runs, all byte values, incompressible bytes, several blocks.
#[test]
fn bzip2_matches_the_reference_tool() {
    let Ok(_) = std::process::Command::new("bzip2").arg("--help").output() else {
        eprintln!("bzip2 not installed: skipped");
        return;
    };
    let mut seed = 12345u64;
    let mut rand = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as u8
    };
    let mut inputs: Vec<Vec<u8>> = vec![
        b"a".to_vec(),
        vec![7u8; 5000],
        (0..=255u8).cycle().take(70_000).collect(),
        (0..300_000).map(|_| rand()).collect(),
    ];
    // Runs of every length, words, and noise: 1.2 MB, several blocks.
    let mut mixed = Vec::new();
    for i in 0..40_000usize {
        mixed.extend(std::iter::repeat_n((i % 7) as u8 + b'a', i % 9));
        mixed.extend_from_slice(b"brain ");
        mixed.push(rand());
    }
    inputs.push(mixed);
    for (i, data) in inputs.iter().enumerate() {
        for level in ["-1", "-9"] {
            let mut child = std::process::Command::new("bzip2")
                .args(["-c", level])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let input = data.clone();
            let writer = std::thread::spawn(move || std::io::Write::write_all(&mut stdin, &input));
            let packed = child.wait_with_output().unwrap().stdout;
            writer.join().unwrap().unwrap();
            let mut out = Vec::new();
            gnome_nifti_preview::formats::bzip2::decompress(packed.as_slice(), data.len() + 10, &mut out).unwrap_or_else(|e| panic!("input {i} at {level}: {e}"));
            assert!(out == *data, "input {i} at {level}: {} bytes of {}", out.len(), data.len());
            // Damaged copies: an error or other bytes, never a panic.
            if i == 2 {
                for k in 0..400 {
                    let mut m = packed.clone();
                    let at = 4 + (k * 7919) % (m.len() - 4);
                    m[at] ^= 1 << (k % 8);
                    let mut out = Vec::new();
                    let _ = gnome_nifti_preview::formats::bzip2::decompress(m.as_slice(), data.len(), &mut out);
                }
            }
        }
    }
}

fn mhd(data: &str) -> String {
    format!("ObjectType = Image\nNDims = 3\nDimSize = 4 4 4\nElementType = MET_UCHAR\nElementDataFile = {data}\n")
}
fn mhd_zlib(data: &str) -> String {
    format!("ObjectType = Image\nNDims = 3\nDimSize = 4 4 4\nElementType = MET_UCHAR\nCompressedData = True\nElementDataFile = {data}\n")
}
fn nhdr(data: &str) -> String {
    format!("NRRD0004\ntype: uchar\ndimension: 3\nsizes: 4 4 4\nencoding: raw\ndata file: {data}\n")
}
fn nhdr_text(data: &str) -> String {
    format!("NRRD0004\ntype: uchar\ndimension: 3\nsizes: 4 4 4\nencoding: ascii\ndata file: {data}\n")
}
fn mih(data: &str) -> String {
    format!("mrtrix image\ndim: 4,4,4\nvox: 1,1,1\nlayout: +0,+1,+2\ndatatype: UInt8\nfile: {data} 0\nEND\n")
}

/// The data file a header names stays in the header's folder: a symbolic
/// link to a file elsewhere, or through a folder linked to another place, is
/// refused like an absolute path or `..`; a link that stays in the folder is
/// followed.
#[test]
fn links_do_not_take_data_files_out_of_the_folder() {
    use std::os::unix::fs::symlink;
    let top = tmpdir("fmt-links");
    let dir = top.join("image");
    std::fs::create_dir_all(dir.join("real")).unwrap();
    std::fs::create_dir_all(top.join("elsewhere")).unwrap();
    let bytes: Vec<u8> = (0..64u8).collect();
    let mut text = String::new();
    for b in &bytes {
        text.push_str(&b.to_string());
        text.push('\n');
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut z, &bytes).unwrap();
    let zlib = z.finish().unwrap();
    for (name, content) in [("data.raw", &bytes), ("data.txt", &text.as_bytes().to_vec()), ("data.z", &zlib)] {
        std::fs::write(top.join("elsewhere").join(name), content).unwrap();
        std::fs::write(dir.join("real").join(name), content).unwrap();
        symlink(top.join("elsewhere").join(name), dir.join(format!("out-{name}"))).unwrap();
        symlink(format!("real/{name}"), dir.join(format!("in-{name}"))).unwrap();
    }
    symlink(top.join("elsewhere"), dir.join("away")).unwrap();
    type Header = fn(&str) -> String;
    let headers: [(&str, Header, &str); 5] =
        [("mhd", mhd, "raw"), ("mhd", mhd_zlib, "z"), ("nhdr", nhdr, "raw"), ("nhdr", nhdr_text, "txt"), ("mih", mih, "raw")];
    for (n, (ext, header, data)) in headers.into_iter().enumerate() {
        for (target, expected) in [
            (format!("real/data.{data}"), "ok"),
            (format!("in-data.{data}"), "ok"),
            (format!("out-data.{data}"), "unsupported_format"),
            (format!("away/data.{data}"), "unsupported_format"),
        ] {
            let p = dir.join(format!("h{n}-{}.{ext}", target.replace('/', "_")));
            std::fs::write(&p, header(&target)).unwrap();
            assert_eq!(outcome(&p), expected, "{ext} {data} -> {target}");
        }
    }
}
