//! Helpers shared by the integration tests: NIfTI-1/NIfTI-2 header builder,
//! gzip, per-test temporary directories and a deadline that turns a hang into
//! a failure.

#![allow(dead_code)]

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Header fields, written with the requested version and byte order.
#[derive(Clone)]
pub struct Nifti {
    pub version: u8,
    pub big_endian: bool,
    pub dims: Vec<i64>,
    pub datatype: i16,
    pub bitpix: i16,
    pub pixdim: [f64; 8],
    /// None: the natural offset (352 or 544).
    pub vox_offset: Option<f64>,
    pub slope: f64,
    pub inter: f64,
    pub qform_code: i16,
    pub sform_code: i16,
    pub quatern: [f64; 3],
    pub qoffset: [f64; 3],
    pub srow: [[f64; 4]; 3],
    pub intent: i16,
    pub intent_name: Vec<u8>,
    /// Header extensions (ecode, content), padded to 16 bytes when written.
    pub extensions: Vec<(i32, Vec<u8>)>,
    pub xyzt_units: u8,
    pub descrip: Vec<u8>,
}

pub fn bitpix(datatype: i16) -> i16 {
    match datatype {
        2 | 256 => 8,
        4 | 512 => 16,
        8 | 16 | 768 | 2304 => 32,
        128 => 24,
        32 | 64 | 1024 | 1280 => 64,
        1792 => 128,
        _ => 0,
    }
}

impl Nifti {
    pub fn new(dims: &[i64], datatype: i16) -> Nifti {
        let mut pixdim = [1.0; 8];
        pixdim[0] = 1.0;
        Nifti {
            version: 1,
            big_endian: false,
            dims: dims.to_vec(),
            datatype,
            bitpix: bitpix(datatype),
            pixdim,
            vox_offset: None,
            slope: 1.0,
            inter: 0.0,
            qform_code: 0,
            sform_code: 1,
            quatern: [0.0; 3],
            qoffset: [0.0; 3],
            srow: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
            intent: 0,
            intent_name: Vec::new(),
            extensions: Vec::new(),
            xyzt_units: 2 | 8,
            descrip: b"test".to_vec(),
        }
    }

    pub fn v2(mut self) -> Nifti {
        self.version = 2;
        self
    }

    pub fn be(mut self) -> Nifti {
        self.big_endian = true;
        self
    }

    pub fn natural_offset(&self) -> usize {
        if self.version == 1 {
            352
        } else {
            544
        }
    }

    /// The extension blocks (esize, ecode, content padded to 16 bytes).
    fn extension_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (code, content) in &self.extensions {
            let esize = (8 + content.len()).div_ceil(16) * 16;
            let (s, c) = (esize as i32, *code);
            out.extend_from_slice(&if self.big_endian { s.to_be_bytes() } else { s.to_le_bytes() });
            out.extend_from_slice(&if self.big_endian { c.to_be_bytes() } else { c.to_le_bytes() });
            out.extend_from_slice(content);
            out.resize(out.len() + esize - 8 - content.len(), 0);
        }
        out
    }

    pub fn header(&self) -> Vec<u8> {
        let ext = self.extension_bytes();
        let mut b = vec![0u8; self.natural_offset() + ext.len()];
        let be = self.big_endian;
        macro_rules! put {
            ($off:expr, $v:expr) => {{
                let v = $v;
                let bytes = if be { v.to_be_bytes() } else { v.to_le_bytes() };
                b[$off..$off + bytes.len()].copy_from_slice(&bytes);
            }};
        }
        let mut dim = [1i64; 8];
        dim[0] = self.dims.len() as i64;
        for (i, &d) in self.dims.iter().enumerate() {
            dim[i + 1] = d;
        }
        let off = self.vox_offset.unwrap_or((self.natural_offset() + ext.len()) as f64);
        if !ext.is_empty() {
            let flag = self.natural_offset() - 4;
            b[flag] = 1;
            b[flag + 4..].copy_from_slice(&ext);
        }
        let iname = self.intent_name.len().min(16);
        if self.version == 1 {
            put!(0, 348i32);
            for i in 0..8 {
                put!(40 + 2 * i, dim[i] as i16);
                put!(76 + 4 * i, self.pixdim[i] as f32);
            }
            put!(68, self.intent);
            put!(70, self.datatype);
            put!(72, self.bitpix);
            put!(108, off as f32);
            put!(112, self.slope as f32);
            put!(116, self.inter as f32);
            b[123] = self.xyzt_units;
            put!(252, self.qform_code);
            put!(254, self.sform_code);
            for i in 0..3 {
                put!(256 + 4 * i, self.quatern[i] as f32);
                put!(268 + 4 * i, self.qoffset[i] as f32);
            }
            for r in 0..3 {
                for c in 0..4 {
                    put!(280 + 16 * r + 4 * c, self.srow[r][c] as f32);
                }
            }
            let n = self.descrip.len().min(80);
            b[148..148 + n].copy_from_slice(&self.descrip[..n]);
            b[328..328 + iname].copy_from_slice(&self.intent_name[..iname]);
            b[344..348].copy_from_slice(b"n+1\0");
        } else {
            put!(0, 540i32);
            b[4..12].copy_from_slice(b"n+2\0\r\n\x1a\n");
            put!(12, self.datatype);
            put!(14, self.bitpix);
            for i in 0..8 {
                put!(16 + 8 * i, dim[i]);
                put!(104 + 8 * i, self.pixdim[i]);
            }
            put!(168, off as i64);
            put!(176, self.slope);
            put!(184, self.inter);
            put!(344, self.qform_code as i32);
            put!(348, self.sform_code as i32);
            for i in 0..3 {
                put!(352 + 8 * i, self.quatern[i]);
                put!(376 + 8 * i, self.qoffset[i]);
            }
            for r in 0..3 {
                for c in 0..4 {
                    put!(400 + 32 * r + 8 * c, self.srow[r][c]);
                }
            }
            put!(500, self.xyzt_units as i32);
            put!(504, self.intent as i32);
            let n = self.descrip.len().min(80);
            b[240..240 + n].copy_from_slice(&self.descrip[..n]);
            b[508..508 + iname].copy_from_slice(&self.intent_name[..iname]);
        }
        b
    }

    pub fn file(&self, data: &[u8]) -> Vec<u8> {
        let mut b = self.header();
        b.extend_from_slice(data);
        b
    }
}

pub fn gz(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(bytes).unwrap();
    e.finish().unwrap()
}

/// A fresh directory for one test (tests run in parallel).
pub fn tmpdir(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "np-test-{}-{name}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Run `f` on a helper thread; fail (instead of hanging) after `secs`.
pub fn within<T: Send + 'static>(secs: u64, what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(Ok(v)) => v,
        Ok(Err(p)) => std::panic::resume_unwind(p),
        Err(_) => panic!("{what}: still running after {secs} s"),
    }
}
