"""DICOM: the Rust backend must decode the pixels pydicom decodes, for the
test files pydicom ships (every transfer syntax: native in both byte
orders, deflated, RLE, JPEG baseline, extended and lossless, JPEG-LS,
JPEG 2000, colour, palettes). pydicom is the oracle here, as nibabel is for
NIfTI; the compressed files need its decoder plugins (pylibjpeg-libjpeg,
pylibjpeg-openjpeg, pyjpegls), else they are skipped, or fail with
NIFTI_PREVIEW_REQUIRE_CODECS=1 (the reference CI job)."""

from __future__ import annotations

import os
import warnings

import numpy as np
import pytest

from test_differential import RustBackend

pydicom = pytest.importorskip("pydicom")
from pydicom.data import get_testdata_file  # noqa: E402
from pydicom.pixels import apply_color_lut, apply_modality_lut  # noqa: E402

# (file, largest difference allowed): lossy JPEG differs by the rounding of
# the inverse DCT and of the colour conversion (libjpeg's integer arithmetic).
FILES = [
    ("MR_small.dcm", 0), ("MR_small_implicit.dcm", 0), ("MR_small_bigendian.dcm", 0), ("MR_small_expb.dcm", 0),
    ("MR_small_RLE.dcm", 0), ("MR_small_jpeg_ls_lossless.dcm", 0), ("MR_small_jp2klossless.dcm", 0),
    ("MR_small_padded.dcm", 0), ("CT_small.dcm", 0), ("emri_small.dcm", 0), ("emri_small_RLE.dcm", 0),
    ("emri_small_big_endian.dcm", 0), ("emri_small_jpeg_ls_lossless.dcm", 0), ("emri_small_jpeg_2k_lossless.dcm", 0),
    ("JPEG-LL.dcm", 0), ("JPEG2000.dcm", 0), ("JPGExtended.dcm", 1), ("693_J2KI.dcm", 0),
    ("J2K_pixelrep_mismatch.dcm", 0), ("JPEGLSNearLossless_08.dcm", 0), ("JPEGLSNearLossless_16.dcm", 0),
    ("image_dfl.dcm", 0), ("liver_1frame.dcm", 0), ("rtdose.dcm", 0), ("rtdose_rle.dcm", 0), ("rtdose_expb.dcm", 0),
    ("SC_rgb.dcm", 0), ("SC_rgb_rle.dcm", 0), ("SC_rgb_2frame.dcm", 0), ("SC_rgb_small_odd.dcm", 0),
    ("SC_rgb_small_odd_big_endian.dcm", 0), ("SC_rgb_gdcm_KY.dcm", 0), ("SC_rgb_jls_lossy_line.dcm", 0),
    ("SC_rgb_jls_lossy_sample.dcm", 0), ("SC_ybr_full_422_uncompressed.dcm", 0), ("SC_rgb_jpeg_gdcm.dcm", 0),
    ("SC_rgb_jpeg_dcmtk.dcm", 1), ("SC_rgb_dcmtk_+eb+cy+n1.dcm", 3), ("SC_rgb_jpeg_lossy_gdcm.dcm", 3),
    ("SC_jpeg_no_color_transform.dcm", 1), ("GDCMJ2K_TextGBR.dcm", 0), ("examples_jpeg2k.dcm", 0),
    ("examples_palette.dcm", 0), ("examples_rgb_color.dcm", 0), ("examples_ybr_color.dcm", 3),
    ("examples_overlay.dcm", 0),
]


@pytest.fixture(scope="module")
def rust():
    backend = RustBackend()
    yield backend
    backend.close()


def reference_pixels(path):
    """pydicom's frames: (frames, rows, cols[, 3]), colour as 8-bit RGB."""
    ds = pydicom.dcmread(path)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        try:
            a = ds.pixel_array
        except Exception as e:  # a decoder plugin missing
            if os.environ.get("NIFTI_PREVIEW_REQUIRE_CODECS") == "1":
                pytest.fail(f"pydicom cannot decode {path} (NIFTI_PREVIEW_REQUIRE_CODECS=1): {e}")
            pytest.skip(f"pydicom cannot decode {path}: {e}")
    if ds.get("PhotometricInterpretation") == "PALETTE COLOR":
        a = apply_color_lut(a, ds)
        if a.dtype != np.uint8:
            a = (a >> 8).astype(np.uint8)
    elif a.ndim >= 2 and ds.get("SamplesPerPixel", 1) == 1:
        # The backend shows modality values: LUT or rescale (dose grid scaling for RT dose).
        a = apply_modality_lut(a, ds)
        if "RescaleSlope" not in ds and "DoseGridScaling" in ds:
            a = a * float(ds.DoseGridScaling)
    frames = int(ds.get("NumberOfFrames", 1) or 1)
    colour = a.ndim == (4 if frames > 1 else 3)
    if colour and a.dtype != np.uint8:
        a = (a.astype(np.uint64) >> (int(ds.BitsStored) - 8)).astype(np.uint8)
    return ds, a.reshape((frames,) + a.shape[-3:] if colour else (frames,) + a.shape[-2:]), colour


@pytest.mark.parametrize("name,tolerance", FILES)
def test_pixels_match_pydicom(rust, name, tolerance):
    try:
        path = get_testdata_file(name, download=False)
    except TypeError:  # older pydicom without the download switch
        path = get_testdata_file(name)
    if path is None:
        pytest.skip(f"{name} is not bundled with this pydicom")
    ds, ref, colour = reference_pixels(path)
    r = rust.request(op="open", path=path, mode="stored", dicom_series=False)
    assert r["ok"], r
    h = r["header"]
    assert h["format"] == "DICOM"
    rows, cols = ref.shape[1], ref.shape[2]
    assert h["shape"][:2] == [cols, rows]
    assert rust.request(op="load", dicom_window="auto")["ok"]
    # Frames in time are volumes; frames placed in space are slices (sorted
    # along the normal): compare the first frame, wherever it is.
    rng = np.random.default_rng(3)
    n = min(rows * cols, 600)
    points = [(int(rng.integers(0, cols)), int(rng.integers(0, rows))) for _ in range(n)]
    slices = h["shape"][2]
    candidates = range(slices) if slices > 1 else [0]
    best = None
    for k in candidates:
        worst = 0.0
        for i, j in points:
            v = rust.request(op="value", ijk=[i, j, k])
            assert v["ok"], v
            got = np.array(v["rgb"], float) if colour else float(v["value"])
            want = ref[0, j, i].astype(float)
            # float32 display precision for scaled values
            worst = max(worst, float(np.max(np.abs(got - want))) - 1e-6 * abs(float(np.max(want))))
            if worst > tolerance:
                break
        best = worst if best is None else min(best, worst)
        if best <= tolerance:
            break
    assert best <= tolerance, (name, best)
