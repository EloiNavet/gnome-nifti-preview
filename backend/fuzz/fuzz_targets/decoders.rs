//! The pixel decoders on any bytes: JPEG (baseline, extended, lossless),
//! JPEG-LS, RLE and JPEG 2000 (DICOM), bzip2 (NRRD). Must never crash or
//! hang.
//!
//!   cargo +nightly fuzz run decoders CORPUS -- -max_total_time=600
//!
//! Seeds: the streams of src/formats/dicom/testdata/ and src/formats/testdata/.

#![no_main]

use gnome_nifti_preview::formats::dicom::{j2k, jpeg, jpegls, pixels, rle};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = jpeg::decode(data);
    let _ = jpegls::decode(data);
    let _ = j2k::decode(data);
    let _ = gnome_nifti_preview::formats::bzip2::decompress(data, 1 << 20, &mut Vec::new());
    for (bits, spp) in [(8, 1), (16, 1), (8, 3), (1, 1)] {
        let desc = pixels::PixelDesc {
            rows: 9,
            cols: 7,
            frames: 1,
            spp,
            bits_allocated: bits,
            bits_stored: bits,
            high_bit: bits - 1,
            signed: false,
            planar: false,
            photometric: if spp == 3 { pixels::Photometric::Rgb } else { pixels::Photometric::Monochrome2 },
            float: false,
            palette: None,
            modality_lut: None,
        };
        let _ = rle::decode(&desc, data);
    }
});
