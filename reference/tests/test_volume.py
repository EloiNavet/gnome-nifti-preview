import gzip
import os

import nibabel as nib
import numpy as np
import pytest

from nifti_preview_ref import synthetic
from nifti_preview_ref.errors import PreviewError
from nifti_preview_ref.render import render_slice
from nifti_preview_ref.volume import NiftiVolume


@pytest.mark.parametrize("version", [1, 2])
@pytest.mark.parametrize("ext", [".nii", ".nii.gz"])
def test_nifti1_nifti2_compressed_and_not(write, marker, version, ext):
    data, affine = marker
    path = write(f"img_v{version}{ext}", data, affine, version=version, dtype=np.int16)
    vol = NiftiVolume(path)
    info = vol.header_info()
    assert info["format"] == f"NIfTI-{version}"
    assert info["compressed"] == ext.endswith(".gz")
    assert info["shape"] == list(data.shape)
    assert info["dtype"] == "int16"
    assert info["file_size"] == os.path.getsize(path)
    assert info["orientation"] == "RAS" and info["affine_source"] == "sform"
    np.testing.assert_array_equal(np.asarray(vol.ensure_data()), data.astype(np.int16))


def test_uncompressed_is_memory_mapped(write, marker):
    data, affine = marker
    vol = NiftiVolume(write("mm.nii", data, affine, dtype=np.int16))
    assert isinstance(vol.ensure_data(), np.memmap)


def test_metadata_fields(write):
    data = np.zeros((10, 12, 14), np.float32)
    affine = synthetic.ras_affine(data.shape, (0.8, 0.8, 2.5))
    path = write("meta.nii.gz", data, affine, slope=None)
    info = NiftiVolume(path).header_info()
    assert info["zooms"] == pytest.approx([0.8, 0.8, 2.5])
    assert info["space_units"] == "mm"
    assert info["qform"] == "scanner" and info["sform"] == "scanner"
    assert info["n_volumes"] == 1 and info["repetition_time"] is None
    assert len(info["affine"]) == 4


def test_qform_only_and_no_orientation(write, marker):
    data, affine = marker
    q = NiftiVolume(write("q.nii.gz", data, affine, sform_code=0))
    assert q.affine_source == "qform" and q.orientation_known
    none = NiftiVolume(write("none.nii.gz", data, affine, sform_code=0, qform_code=0))
    assert none.affine_source == "none" and not none.orientation_known
    assert any("unknown" in w for w in none.header_info()["warnings"])
    assert none.plans("neurological")["axial"].left == "?"


def test_4d_shows_first_volume(write):
    base, affine = synthetic.marker_phantom((20, 24, 18))
    vol4 = np.stack([base + 1000 * t for t in range(5)], axis=-1)
    vol = NiftiVolume(write("bold.nii.gz", vol4, affine, dtype=np.int16, zooms=(1, 1, 1, 2.0)))
    assert vol.n_volumes == 5
    info = vol.header_info()
    assert info["repetition_time"] == 2.0
    assert any("volume 1 of 5" in w for w in info["warnings"])
    np.testing.assert_array_equal(np.asarray(vol.ensure_data()), base.astype(np.int16))


def test_5d_and_2d_images(write):
    data5 = np.ones((6, 7, 8, 1, 3), np.float32)
    vol = NiftiVolume(write("disp.nii.gz", data5, np.eye(4)))
    assert vol.n_volumes == 3 and vol.ensure_data().shape == (6, 7, 8)
    data2 = np.arange(30, dtype=np.float32).reshape(5, 6)
    vol2 = NiftiVolume(write("flat.nii.gz", data2, np.eye(4)))
    assert vol2.shape3 == (5, 6, 1)
    assert vol2.ensure_data().shape == (5, 6, 1)
    plan = vol2.plans("neurological")["axial"]
    assert render_slice(vol2, plan, 0).width == 5


def test_scaling_is_applied(write, marker):
    data, affine = marker
    vol = NiftiVolume(write("scaled.nii.gz", data, affine, dtype=np.int16, slope=0.5, inter=10.0))
    assert vol.slope == 0.5 and vol.inter == 10.0
    i, j, k = vol.center()
    raw = float(np.asarray(vol.ensure_data())[i, j, k])
    assert vol.value_at([i, j, k]) == pytest.approx(raw * 0.5 + 10.0)


@pytest.mark.parametrize("version", [1, 2])
@pytest.mark.parametrize("sign", [-1, 1])
def test_scaling_compensates_before_float32_conversion(write, version, sign):
    raw_unit, slope = (1e40, sign * 1e-40) if version == 1 else (1e-300, sign * 1e300)
    data = np.arange(1, 9, dtype=np.float64).reshape((2, 2, 2), order="F") * raw_unit
    vol = NiftiVolume(write("compensated.nii", data, np.eye(4), version=version, slope=slope, inter=10.0))
    # Use the actual header precision for the expectation, not the oracle's scale().
    stored_slope = float(np.float32(slope)) if version == 1 else slope
    expected = (data * stored_slope + 10).astype(np.float32)
    for i, j, k in np.ndindex(data.shape):
        assert vol.value_at([i, j, k]) == float(expected[i, j, k])
    stats = vol.stats()
    assert stats.window.status == "ok" and stats.non_finite == 0
    assert (stats.minimum, stats.maximum) == (float(expected.min()), float(expected.max()))
    plan = vol.plans("stored")["axial"]
    samples = vol.slice_values(plan, 0)
    assert np.isfinite(samples).all()
    assert any(render_slice(vol, plan, 0, "gray").rgb)


@pytest.mark.parametrize("dtype", [np.uint8, np.int8, np.uint16, np.int32, np.int64, np.float64])
def test_datatypes(write, marker, dtype):
    data, affine = marker
    vol = NiftiVolume(write(f"dt_{np.dtype(dtype).name}.nii.gz", np.clip(data, 0, 120), affine, dtype=dtype))
    plan = vol.plans("neurological")["coronal"]
    out = render_slice(vol, plan, plan.n_slices // 2)
    assert out.width == plan.width and len(out.rgb) == plan.width * plan.height * 3


def test_big_endian(tmp_path, marker):
    data, affine = marker
    img = nib.Nifti1Image(data.astype(">i2"), affine)
    path = str(tmp_path / "be.nii")
    nib.save(img, path)
    vol = NiftiVolume(path)
    assert vol.value_at(vol.center()) == float(data[tuple(vol.center())])


def test_complex_and_rgb(write, tmp_path):
    c = (np.ones((5, 6, 7)) * (3 + 4j)).astype(np.complex64)
    vol = NiftiVolume(write("cplx.nii.gz", c, np.eye(4)))
    assert vol.value_at([1, 1, 1]) == pytest.approx(5.0)
    assert any("magnitude" in n for n in vol.stats().notes)

    rgb_dtype = np.dtype([("R", "u1"), ("G", "u1"), ("B", "u1")])
    arr = np.zeros((5, 6, 7), dtype=rgb_dtype)
    arr["R"], arr["G"], arr["B"] = 100, 100, 100
    img = nib.Nifti1Image(arr, np.eye(4))
    path = str(tmp_path / "rgb.nii.gz")
    nib.save(img, path)
    vol = NiftiVolume(path)
    assert vol.value_at([0, 0, 0]) == pytest.approx(100.0, abs=0.01)
    assert not vol.stats().labels.is_labels


def test_nan_inf_volume(write):
    data = np.linspace(0, 1, 1000, dtype=np.float32).reshape(10, 10, 10)
    data[0, 0, :] = np.nan
    data[1, 1, 1] = np.inf
    vol = NiftiVolume(write("nan.nii.gz", data, np.eye(4)))
    st = vol.stats()
    assert st.non_finite == 11
    assert any("NaN" in n for n in st.notes)
    assert vol.value_at([0, 0, 0]) is None
    plan = vol.plans("neurological")["axial"]
    render_slice(vol, plan, 0)


def test_empty_and_constant(write):
    vol = NiftiVolume(write("zeros.nii.gz", np.zeros((8, 8, 8), np.float32), np.eye(4)))
    assert vol.stats().window.status == "constant"
    vol = NiftiVolume(write("nanonly.nii.gz", np.full((8, 8, 8), np.nan, np.float32), np.eye(4)))
    assert vol.stats().window.status == "no_finite"


def test_filename_with_spaces_and_unicode(write, marker):
    data, affine = marker
    path = write("sub 001 é T1w.nii.gz", data, affine)
    assert NiftiVolume(path).header_info()["name"] == "sub 001 é T1w.nii.gz"


def test_not_found_and_directory(tmp_path):
    with pytest.raises(PreviewError) as e:
        NiftiVolume(tmp_path / "missing.nii.gz")
    assert e.value.code == "not_found"
    with pytest.raises(PreviewError) as e:
        NiftiVolume(tmp_path)
    assert e.value.code == "not_a_file"


@pytest.mark.parametrize("content", [b"", b"garbage" * 100, gzip.compress(b"garbage" * 100), gzip.compress(b"\0" * 400)])
def test_invalid_files(tmp_path, content):
    path = tmp_path / "bad.nii.gz"
    path.write_bytes(content)
    with pytest.raises(PreviewError) as e:
        NiftiVolume(path).stats()
    assert e.value.code in ("invalid_nifti", "corrupt_data")


def test_invalid_uncompressed(tmp_path):
    path = tmp_path / "bad.nii"
    path.write_bytes(b"\x01" * 600)
    with pytest.raises(PreviewError) as e:
        NiftiVolume(path)
    assert e.value.code == "invalid_nifti"


@pytest.mark.parametrize("ext", [".nii", ".nii.gz"])
def test_truncated_file(write, marker, ext):
    data, affine = marker
    path = write(f"full{ext}", data, affine, dtype=np.float32)
    blob = open(path, "rb").read()
    trunc = path.replace("full", "trunc")
    with open(trunc, "wb") as fh:
        fh.write(blob[: len(blob) // 2])
    with pytest.raises(PreviewError) as e:
        NiftiVolume(trunc).stats()
    assert e.value.code == "corrupt_data"


def test_too_large(write, marker):
    data, affine = marker
    path = write("big.nii.gz", data, affine, dtype=np.float32)
    vol = NiftiVolume(path, max_bytes=1000)
    vol.header_info()  # metadata is still available
    with pytest.raises(PreviewError) as e:
        vol.ensure_data()
    assert e.value.code == "too_large"


def test_too_large_uncompressed(write, marker):
    data, affine = marker
    vol = NiftiVolume(write("big.nii", data, affine, dtype=np.float32), max_bytes=1000)
    with pytest.raises(PreviewError) as e:
        vol.ensure_data()
    assert e.value.code == "too_large"


@pytest.mark.parametrize("shape", [(5000, 5000, 1), (20000, 2, 2), (2, 4097, 4097)])
def test_giant_slices_are_too_large(tmp_path, shape):
    hdr = nib.Nifti1Header()
    hdr.set_data_dtype(np.uint8)
    hdr.set_data_shape(shape)
    path = tmp_path / "flat.nii.gz"
    path.write_bytes(gzip.compress(hdr.binaryblock + b"\0" * 4 + b"\0" * 64))
    with pytest.raises(PreviewError) as e:
        NiftiVolume(path).ensure_data()
    assert e.value.code == "too_large"


@pytest.mark.parametrize("offset", [1e9, 1e12, 1e18])
def test_huge_vox_offset_is_corrupt(tmp_path, offset):
    hdr = nib.Nifti1Header()
    hdr.set_data_dtype(np.uint8)
    hdr.set_data_shape((4, 4, 4))
    hdr["vox_offset"] = offset
    path = tmp_path / "off.nii.gz"
    path.write_bytes(gzip.compress(hdr.binaryblock + b"\0" * 4 + b"\1" * 64))
    with pytest.raises(PreviewError) as e:
        NiftiVolume(path).ensure_data()
    assert e.value.code == "corrupt_data"


@pytest.mark.parametrize(
    "value, expected",
    [(None, 3 << 30), ("abc", 3 << 30), ("-5", 3 << 30), ("1", 16 << 20), (str(1 << 50), 64 << 30), (" 123456789 ", 123456789)],
)
def test_max_bytes_from_env(monkeypatch, tmp_path, marker, value, expected):
    if value is None:
        monkeypatch.delenv("NIFTI_PREVIEW_MAX_BYTES", raising=False)
    else:
        monkeypatch.setenv("NIFTI_PREVIEW_MAX_BYTES", value)
    data, affine = marker
    path = synthetic.save(tmp_path / "m.nii.gz", data, affine)
    assert NiftiVolume(path).max_bytes == expected


def test_value_out_of_range(t1_path):
    vol = NiftiVolume(t1_path)
    with pytest.raises(PreviewError):
        vol.value_at([999, 0, 0])


def test_large_label_identifiers_and_coordinates_in_mm(tmp_path):
    """2**24 and 2**24 + 1 are one float32: they stay two labels; coordinates
    are in mm whatever the spatial unit."""
    from nifti_preview_ref.server import Session, respond
    import json as _json

    data = np.zeros((4, 4, 4), np.uint32)
    data[:2, :, 1:] = 2**24
    data[2:, :, 1:] = 2**24 + 1
    path = synthetic.save(tmp_path / "big.nii", data, np.diag([0.001, 0.001, 0.001, 1]), dtype=np.uint32, units="meter")
    s = Session()
    ask = lambda **req: respond(s, _json.dumps({"id": 1, **req}).encode())  # noqa: E731
    ask(op="open", path=str(path))
    assert ask(op="load")["stats"]["labels"]["n_labels"] == 2
    a, b = ask(op="value", ijk=[0, 0, 2]), ask(op="value", ijk=[3, 0, 2])
    assert (a["value"], b["value"]) == (2**24, 2**24 + 1)
    assert (a["label_voxels"], b["label_voxels"]) == (24, 24)
    assert a["world"] == pytest.approx([0.0, 0.0, 2.0])


@pytest.mark.parametrize("dtype,a,b", [
    (np.uint32, 2**24, 2**24 + 1),
    (np.int64, 2**53, 2**53 + 1),
    (np.int64, 2**63 - 2, 2**63 - 1),
    (np.uint64, 2**64 - 2, 2**64 - 1),
    (np.float64, 2**24, 2**24 + 1),
])
@pytest.mark.parametrize("ext", [".nii", ".nii.gz"])
@pytest.mark.parametrize("version", [1, 2])
def test_exact_scalar_labels_and_metadata(tmp_path, dtype, a, b, ext, version):
    from nifti_preview_ref.server import Session, respond
    import json

    data = np.empty((2, 2, 2), dtype=dtype)
    data[0] = a
    data[1] = b
    path = synthetic.save(tmp_path / ("exact" + ext), data, np.eye(4), dtype=dtype, version=version)
    s = Session()
    def ask(**req):
        return respond(s, json.dumps({"id": 1, **req}).encode())
    assert ask(op="open", path=str(path), mode="stored")["ok"]
    stats = ask(op="load")["stats"]
    wire = lambda v: str(v) if v > 2**53 - 1 else v
    assert stats["labels"]["n_labels"] == 2
    assert (stats["min"], stats["max"]) == (wire(a), wire(b))
    assert not any("Constant volume" in note for note in stats["notes"])
    for x, v in enumerate([a, b]):
        reply = ask(op="value", ijk=[x, 0, 0])
        assert reply["value"] == wire(v)
        assert reply["label_voxels"] == 4
        assert reply["label_volume_mm3"] == 4.0
    vol = NiftiVolume(path)
    rgb = render_slice(vol, vol.plans("stored")["axial"], 0).rgb
    assert np.unique(np.frombuffer(rgb, np.uint8).reshape(-1, 3), axis=0).shape[0] == 2


@pytest.mark.parametrize("dtype,values", [
    (np.int64, [-2**63, -2**63 + 1, -2**53 - 1, -2**53]),
    (np.float64, [1.0000000001, 1.0000000002]),
])
def test_exact_scalar_counts(tmp_path, dtype, values):
    data = np.asarray(values, dtype=dtype).reshape(-1, 1, 1)
    path = synthetic.save(tmp_path / "exact.nii.gz", data, np.eye(4), dtype=dtype)
    vol = NiftiVolume(path)
    for i, expected in enumerate(values):
        assert vol.value_at([i, 0, 0]) == expected
        assert vol.count_equal(expected) == 1
