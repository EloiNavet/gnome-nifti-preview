//! JPEG 2000 and High-Throughput JPEG 2000, through OpenJPEG 2.5 (C, BSD-2,
//! compiled from the sources vendored by the `openjpeg-sys` crate). The
//! codestream is read from memory; the image size is checked after its
//! header, before anything is decoded. Without the `openjpeg` feature these
//! transfer syntaxes are refused.

use crate::error::Result;

pub struct Decoded {
    pub width: usize,
    pub height: usize,
    pub components: usize,
    pub precision: u32,
    pub signed: bool,
    /// Interleaved samples, `components` per pixel.
    pub samples: Vec<i32>,
}

#[cfg(not(feature = "openjpeg"))]
pub fn decode(_data: &[u8]) -> Result<Decoded> {
    crate::error::bail!("unsupported_format", "JPEG 2000 is not supported by this build of the backend (built without OpenJPEG)")
}

#[cfg(feature = "openjpeg")]
pub fn decode(data: &[u8]) -> Result<Decoded> {
    use crate::error::{bail, PreviewError};
    use openjpeg_sys as opj;
    use std::ffi::c_void;

    struct Input<'a> {
        data: &'a [u8],
        pos: usize,
    }

    unsafe extern "C" fn read(buf: *mut c_void, n: usize, user: *mut c_void) -> usize {
        // SAFETY: `user` is the Input given to opj_stream_set_user_data,
        // alive for the whole decoding; `buf` holds at least `n` bytes.
        let input = unsafe { &mut *(user as *mut Input) };
        let left = input.data.len() - input.pos;
        if left == 0 {
            return usize::MAX;
        }
        let k = n.min(left);
        unsafe { std::ptr::copy_nonoverlapping(input.data.as_ptr().add(input.pos), buf as *mut u8, k) };
        input.pos += k;
        k
    }
    unsafe extern "C" fn skip(n: i64, user: *mut c_void) -> i64 {
        // SAFETY: as in `read`.
        let input = unsafe { &mut *(user as *mut Input) };
        let left = (input.data.len() - input.pos) as i64;
        if n < 0 || n > left {
            return -1;
        }
        input.pos += n as usize;
        n
    }
    unsafe extern "C" fn seek(n: i64, user: *mut c_void) -> i32 {
        // SAFETY: as in `read`.
        let input = unsafe { &mut *(user as *mut Input) };
        if n < 0 || n as usize > input.data.len() {
            return 0;
        }
        input.pos = n as usize;
        1
    }

    /// Largest image decoded (samples).
    const MAX_SAMPLES: u64 = 1 << 28;
    let fail = |what: &str| PreviewError::new("corrupt_data", format!("JPEG 2000 data could not be decoded: {what}."));
    let jp2 = data.len() >= 12 && data[4..8] == *b"jP  ";
    if !(jp2 || (data.len() >= 4 && data[0] == 0xFF && data[1] == 0x4F)) {
        return Err(fail("not a JPEG 2000 codestream"));
    }
    let format = if jp2 { opj::CODEC_FORMAT::OPJ_CODEC_JP2 } else { opj::CODEC_FORMAT::OPJ_CODEC_J2K };
    let mut input = Input { data, pos: 0 };

    // Everything OpenJPEG allocates is released by these guards.
    struct Codec(*mut opj::opj_codec_t);
    impl Drop for Codec {
        fn drop(&mut self) {
            // SAFETY: created by opj_create_decompress, destroyed once.
            unsafe { opj::opj_destroy_codec(self.0) }
        }
    }
    struct Stream(*mut opj::opj_stream_t);
    impl Drop for Stream {
        fn drop(&mut self) {
            // SAFETY: created by opj_stream_create, destroyed once.
            unsafe { opj::opj_stream_destroy(self.0) }
        }
    }
    struct Image(*mut opj::opj_image_t);
    impl Drop for Image {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: allocated by opj_read_header, destroyed once.
                unsafe { opj::opj_image_destroy(self.0) }
            }
        }
    }

    // SAFETY: plain calls into OpenJPEG on objects created here and freed by
    // the guards above; the stream reads `input`, which outlives them.
    unsafe {
        let codec = opj::opj_create_decompress(format);
        if codec.is_null() {
            return Err(fail("no decoder"));
        }
        let codec = Codec(codec);
        let mut params: opj::opj_dparameters_t = std::mem::zeroed();
        opj::opj_set_default_decoder_parameters(&mut params);
        if opj::opj_setup_decoder(codec.0, &mut params) == 0 {
            return Err(fail("decoder set-up"));
        }
        let stream = opj::opj_stream_create(1 << 16, 1);
        if stream.is_null() {
            return Err(fail("no stream"));
        }
        let stream = Stream(stream);
        opj::opj_stream_set_user_data(stream.0, &mut input as *mut Input as *mut c_void, None);
        opj::opj_stream_set_user_data_length(stream.0, data.len() as u64);
        opj::opj_stream_set_read_function(stream.0, Some(read));
        opj::opj_stream_set_skip_function(stream.0, Some(skip));
        opj::opj_stream_set_seek_function(stream.0, Some(seek));
        let mut image = Image(std::ptr::null_mut());
        if opj::opj_read_header(stream.0, codec.0, &mut image.0) == 0 || image.0.is_null() {
            return Err(fail("bad header"));
        }
        let img = &*image.0;
        let nc = img.numcomps as usize;
        let (w, h) = (img.x1.saturating_sub(img.x0) as u64, img.y1.saturating_sub(img.y0) as u64);
        if nc == 0 || nc > 4 || w == 0 || h == 0 {
            return Err(fail("bad image size"));
        }
        if w * h * nc as u64 > MAX_SAMPLES {
            bail!("too_large", "JPEG 2000 image too large ({w} × {h})");
        }
        if opj::opj_decode(codec.0, stream.0, image.0) == 0 || opj::opj_end_decompress(codec.0, stream.0) == 0 {
            return Err(fail("bad data"));
        }
        // The colour boxes of a JP2 file (palette, channel definitions) may
        // replace the components while decoding: count them again.
        let img = &*image.0;
        let nc = img.numcomps as usize;
        if nc == 0 || nc > 4 || img.comps.is_null() {
            return Err(fail("bad decoded components"));
        }
        let comps = std::slice::from_raw_parts(img.comps, nc);
        let (width, height) = (comps[0].w as usize, comps[0].h as usize);
        if width == 0 || height == 0 || comps.iter().any(|c| c.data.is_null() || c.w == 0 || c.h == 0) {
            return Err(fail("no decoded data"));
        }
        if comps.iter().any(|c| c.w as u64 * c.h as u64 > MAX_SAMPLES) || (width * height * nc) as u64 > MAX_SAMPLES {
            bail!("too_large", "JPEG 2000 image too large ({width} × {height})");
        }
        let mut samples = Vec::with_capacity(width * height * nc);
        let planes: Vec<&[i32]> = comps.iter().map(|c| std::slice::from_raw_parts(c.data, c.w as usize * c.h as usize)).collect();
        for y in 0..height {
            for x in 0..width {
                for (k, c) in comps.iter().enumerate() {
                    // Subsampled components: nearest sample.
                    let cx = (x * c.w as usize / width).min(c.w as usize - 1);
                    let cy = (y * c.h as usize / height).min(c.h as usize - 1);
                    samples.push(planes[k][cy * c.w as usize + cx]);
                }
            }
        }
        Ok(Decoded { width, height, components: nc, precision: comps[0].prec, signed: comps[0].sgnd != 0, samples })
    }
}

#[cfg(all(test, feature = "openjpeg"))]
mod tests {
    #[test]
    fn lossy_ramp() {
        // Written by OpenJPEG from (x + y) × 30 on 64 × 64, 16 bits, ratio 40.
        let d = super::decode(include_bytes!("testdata/ramp16.j2k")).unwrap();
        assert_eq!((d.width, d.height, d.components, d.signed), (64, 64, 1, false));
        let worst = d.samples.iter().enumerate().map(|(i, &v)| (v - ((i % 64 + i / 64) * 30) as i32).abs()).max().unwrap();
        assert!(worst < 200, "{worst}");
    }

    /// The codestream in a JP2 file whose palette makes 3 channels of its
    /// one component: the components are those after decoding.
    #[test]
    fn jp2_palette_changes_the_components() {
        let code = include_bytes!("testdata/ramp16.j2k");
        let bx = |kind: &[u8], body: &[u8]| [&((8 + body.len()) as u32).to_be_bytes()[..], kind, body].concat();
        let ihdr = bx(b"ihdr", &[&64u32.to_be_bytes()[..], &64u32.to_be_bytes(), &1u16.to_be_bytes(), &[15, 7, 0, 0]].concat());
        let colr = bx(b"colr", &[&[1u8, 0, 0][..], &16u32.to_be_bytes()].concat());
        let entries: Vec<u8> = (0..4u8).flat_map(|e| [e * 60, 255 - e * 60, e * 20]).collect();
        let pclr = bx(b"pclr", &[&4u16.to_be_bytes()[..], &[3, 7, 7, 7], &entries].concat());
        let cmap = bx(b"cmap", &[0u8, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 2]);
        let jp2h = bx(b"jp2h", &[ihdr, colr, pclr, cmap].concat());
        let file = [
            &[0u8, 0, 0, 12, b'j', b'P', b' ', b' ', 13, 10, 0x87, 10][..],
            &bx(b"ftyp", b"jp2 \0\0\0\0jp2 "),
            &jp2h,
            &bx(b"jp2c", code),
        ]
        .concat();
        let d = super::decode(&file).unwrap();
        assert_eq!((d.width, d.height, d.components), (64, 64, 3));
        assert_eq!(d.samples.len(), 64 * 64 * 3);
    }

    #[test]
    fn corrupt_streams_fail() {
        let good = include_bytes!("testdata/ramp16.j2k");
        for cut in (0..good.len()).step_by(11) {
            let _ = super::decode(&good[..cut]);
        }
        let mut flipped = good.to_vec();
        for i in (20..flipped.len()).step_by(3) {
            flipped[i] ^= 0x33;
        }
        let _ = super::decode(&flipped);
    }
}
