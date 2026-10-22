//! Mutation and truncation testing: corrupted inputs must produce an error or
//! a valid rendering, never a panic or a hang.

mod common;

use std::path::Path;

use common::{gz, tmpdir, within, Nifti};

use gnome_nifti_preview::render::render_slice;
use gnome_nifti_preview::volume::Volume;

fn nifti1(shape: [i16; 4], datatype: i16, bitpix: i16, data: &[u8], sform: bool) -> Vec<u8> {
    let mut b = vec![0u8; 352];
    b[0..4].copy_from_slice(&348i32.to_le_bytes());
    let ndim: i16 = if shape[3] > 1 { 4 } else { 3 };
    b[40..42].copy_from_slice(&ndim.to_le_bytes());
    for (i, d) in shape.iter().enumerate() {
        b[42 + 2 * i..44 + 2 * i].copy_from_slice(&d.to_le_bytes());
    }
    b[70..72].copy_from_slice(&datatype.to_le_bytes());
    b[72..74].copy_from_slice(&bitpix.to_le_bytes());
    for i in 0..5 {
        b[76 + 4 * i..80 + 4 * i].copy_from_slice(&1f32.to_le_bytes());
    }
    b[108..112].copy_from_slice(&352f32.to_le_bytes());
    b[112..116].copy_from_slice(&1f32.to_le_bytes());
    if sform {
        b[254..256].copy_from_slice(&1i16.to_le_bytes());
        for r in 0..3 {
            b[280 + 16 * r + 4 * r..284 + 16 * r + 4 * r].copy_from_slice(&1f32.to_le_bytes());
        }
    }
    b[123] = 2 | 8;
    b[344..348].copy_from_slice(b"n+1\0");
    b.extend_from_slice(data);
    b
}

fn exercise(path: &Path) {
    let p = path.to_path_buf();
    within(60, &path.display().to_string(), move || exercise_now(&p));
}

fn exercise_now(path: &Path) {
    let Ok(mut vol) = Volume::open(path) else { return };
    let _ = vol.header_info();
    for mode in ["neurological", "radiological", "stored"] {
        let Ok(plans) = vol.plans(mode) else { continue };
        let mut scratch = Vec::new();
        for plan in plans.iter() {
            for idx in [0, plan.n_slices / 2, plan.n_slices.saturating_sub(1)] {
                for cmap in ["auto", "gray", "labels"] {
                    let _ = render_slice(&mut vol, plan, idx, cmap, &mut scratch);
                }
            }
        }
    }
    let _ = vol.value_at([0, 0, 0]);
    let c = vol.center();
    if let Ok(Some(x)) = vol.value_at([c[0] as i64, c[1] as i64, c[2] as i64]) {
        let _ = vol.count_equal(x);
    }
    let _ = vol.voxel_volume_mm3();
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
        (self.next() % n as u64) as usize
    }
}

/// Offsets of the float fields of each header version (pixdim, vox_offset,
/// scl_slope/inter, quatern, qoffset, srow) and their width.
const F32_FIELDS: &[usize] = &[76, 80, 84, 88, 92, 96, 100, 104, 108, 112, 116, 256, 260, 264, 268, 272, 276, 280, 284, 292, 300, 308, 312, 316, 324];
const F64_FIELDS: &[usize] = &[104, 112, 120, 128, 136, 144, 160, 168, 176, 184, 352, 360, 368, 376, 384, 392, 400, 408, 424, 432, 456, 464, 488];
const INT_FIELDS1: &[usize] = &[40, 42, 44, 46, 48, 50, 70, 72, 252, 254];
const INT_FIELDS2: &[usize] = &[12, 16, 24, 32, 40, 48, 56, 344, 348];
const NASTY: [f64; 10] = [1e18, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0, 1e-30, 3e38, 4.5e15, -352.0];

fn bases() -> Vec<(Nifti, Vec<u8>)> {
    let data: Vec<u8> = (0..(12 * 10 * 8 * 16)).map(|i| (i % 251) as u8).collect();
    let mut out = Vec::new();
    let mut add = |n: Nifti| {
        let len = n.dims.iter().product::<i64>() as usize * (n.bitpix as usize / 8);
        out.push((n.clone(), n.file(&data[..len])));
    };
    add(Nifti::new(&[12, 10, 8], 4));
    add(Nifti::new(&[6, 5, 4, 4], 4));
    add(Nifti::new(&[12, 10, 8], 2));
    add(Nifti::new(&[6, 5, 8], 16));
    add(Nifti::new(&[12, 10, 8], 64).v2());
    add(Nifti::new(&[6, 5, 4, 2], 512).be());
    add(Nifti::new(&[6, 5, 4], 16).v2().be());
    add(Nifti::new(&[30, 20], 2));
    add(Nifti::new(&[6, 5, 4, 1, 3], 8));
    for dt in [256, 768, 1024, 1280, 32, 1792, 128, 2304] {
        add(Nifti::new(&[5, 4, 3], dt));
    }
    let mut q = Nifti::new(&[6, 5, 4], 4);
    q.sform_code = 0;
    q.qform_code = 1;
    q.quatern = [0.1, 0.2, 0.3];
    add(q);
    out
}

#[test]
fn mutated_headers_and_data_never_panic() {
    let dir = tmpdir("mutated");
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut n = 0;
    for (b, (spec, base)) in bases().iter().enumerate() {
        let hsize = spec.natural_offset();
        for round in 0..200 {
            let mut m = base.clone();
            let put = |m: &mut Vec<u8>, off: usize, bytes: &[u8]| {
                let mut bytes = bytes.to_vec();
                if spec.big_endian {
                    bytes.reverse();
                }
                m[off..off + bytes.len()].copy_from_slice(&bytes);
            };
            match round % 5 {
                0 => {
                    // flip a few header bytes
                    for _ in 0..1 + rng.below(4) {
                        let i = rng.below(hsize);
                        m[i] = rng.next() as u8;
                    }
                }
                1 => m.truncate(rng.below(m.len() + 1)),
                2 => {
                    // real extreme float values (f32 in NIfTI-1, f64 in NIfTI-2)
                    let v = NASTY[rng.below(NASTY.len())];
                    if spec.version == 1 {
                        put(&mut m, F32_FIELDS[rng.below(F32_FIELDS.len())], &(v as f32).to_le_bytes());
                    } else {
                        put(&mut m, F64_FIELDS[rng.below(F64_FIELDS.len())], &v.to_le_bytes());
                    }
                }
                3 => {
                    // extreme integers in dims, datatype, xform codes
                    let v = [i64::MAX, i64::MIN, -1, 0, 1, 32767, 1 << 31, 1 << 40][rng.below(8)];
                    if spec.version == 1 {
                        put(&mut m, INT_FIELDS1[rng.below(INT_FIELDS1.len())], &(v as i16).to_le_bytes());
                    } else {
                        let off = INT_FIELDS2[rng.below(INT_FIELDS2.len())];
                        match off {
                            12 => put(&mut m, off, &(v as i16).to_le_bytes()),
                            344 | 348 => put(&mut m, off, &(v as i32).to_le_bytes()),
                            _ => put(&mut m, off, &v.to_le_bytes()),
                        }
                    }
                }
                _ => {
                    let i = rng.below(m.len());
                    m[i] ^= 1 << rng.below(8);
                }
            }
            let plain = dir.join(format!("m{b}_{round}.nii"));
            std::fs::write(&plain, &m).unwrap();
            exercise(&plain);
            let mut z = gz(&m);
            if round % 3 == 0 && z.len() > 20 {
                let cut = 10 + rng.below(z.len() - 10);
                z.truncate(cut);
            } else if round % 7 == 0 {
                let i = 10 + rng.below(z.len() - 10);
                z[i] ^= 0x55;
            }
            let zipped = dir.join(format!("m{b}_{round}.nii.gz"));
            std::fs::write(&zipped, &z).unwrap();
            exercise(&zipped);
            n += 2;
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(n, bases().len() * 400);
}

#[test]
fn vox_offset_zero_means_data_after_header() {
    let dir = tmpdir("off0");
    let data: Vec<u8> = (0..(4 * 4 * 4 * 2)).map(|i| (i % 7) as u8).collect();
    let mut b = nifti1([4, 4, 4, 1], 4, 16, &data, true);
    b[108..112].copy_from_slice(&0f32.to_le_bytes());
    let p = dir.join("off0.nii");
    std::fs::write(&p, &b).unwrap();
    let mut v = Volume::open(&p).unwrap();
    assert_eq!(v.header.vox_offset, 352);
    assert_eq!(v.value_at([1, 0, 0]).unwrap(), Some(f64::from(i16::from_le_bytes([data[2], data[3]]))));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn multi_member_gzip_is_read_completely() {
    // bgzip-style: several gzip members concatenated
    let dir = tmpdir("multi");
    let data: Vec<u8> = (0..(16 * 16 * 16 * 2)).map(|i| (i % 13) as u8).collect();
    let file = nifti1([16, 16, 16, 1], 4, 16, &data, true);
    let mut z = Vec::new();
    for chunk in file.chunks(1000) {
        z.extend(gz(chunk));
    }
    z.extend(gz(&[]));
    let p = dir.join("bgzip.nii.gz");
    std::fs::write(&p, &z).unwrap();
    let mut v = Volume::open(&p).unwrap();
    let last = 16 * 16 * 16 - 1;
    let expected = i16::from_le_bytes([data[2 * last], data[2 * last + 1]]) as f64;
    assert_eq!(v.value_at([15, 15, 15]).unwrap(), Some(expected));
    std::fs::remove_dir_all(&dir).ok();
}
