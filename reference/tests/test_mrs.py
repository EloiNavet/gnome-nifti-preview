"""NIfTI-MRS in the reference: metadata, the chemical-shift axis, combination
of transients, per-voxel spectra."""

import json

import numpy as np
import pytest

from nifti_preview_ref import synthetic
from nifti_preview_ref.mrs import Mrs
from nifti_preview_ref.server import Session, respond
from nifti_preview_ref.volume import NiftiVolume

META = {"SpectrometerFrequency": [123.2], "ResonantNucleus": ["1H"], "EchoTime": 0.03, "dim_5": "DIM_COIL"}


@pytest.mark.parametrize("dims,expected", [
    ([], 1),
    ([("DIM_COIL", 2**48 + 2)], 2**48 + 2),
    ([("DIM_COIL", 2**48 + 2), ("DIM_EDIT", 3), ("DIM_DYN", 2)], 2**49 + 4),
    ([("DIM_EDIT", 2**48 + 2), ("DIM_DYN", 2)], 2),
])
def test_metadata_counts_transients_without_materializing_indices(monkeypatch, dims, expected):
    def forbidden(*_):
        raise AssertionError("Metadata must not enumerate transient indices")

    monkeypatch.setattr(Mrs, "combined_transients", forbidden)
    mrs = Mrs("mrs_v0_3", "1H", 123.2, 0.001, 64, dims=dims)
    assert mrs.to_dict((1, 1, 1))["combined"] == expected


@pytest.mark.parametrize("nucleus", ["1H", "13C"])
@pytest.mark.parametrize("reference", [0.0, 2.0, -1.5, None, "invalid"])
def test_explicit_chemical_reference(tmp_path, nucleus, reference):
    data = np.ones((1, 1, 1, 64), np.complex128)
    meta = {"SpectrometerFrequency": [100.0], "ResonantNucleus": [nucleus], "SpecFreqChemShift": reference}
    vol = NiftiVolume(synthetic.save_mrs(tmp_path / "reference.nii", data, meta, 0.001))
    spectrum = vol.spectrum([0, 0, 0])
    peak = int(np.argmax(spectrum["magnitude"]))
    expected = reference if isinstance(reference, float) else (4.65 if nucleus == "1H" else 0.0)
    assert peak == 32
    axis = np.linspace(spectrum["first"], spectrum["last"], 64)
    assert axis[peak] == pytest.approx(expected, abs=1e-12)


@pytest.mark.parametrize("nucleus,gamma", [("3HE", 32.434), ("7LI", 16.546), ("23NA", 11.262), ("129XE", 11.777), ("23Na", 11.262)])
def test_standard_nuclei_and_zero_echo_time(tmp_path, nucleus, gamma):
    meta = {"SpectrometerFrequency": [3 * gamma], "ResonantNucleus": [nucleus], "EchoTime": 0.0}
    path = synthetic.save_mrs(tmp_path / "nucleus.nii", np.ones((1, 1, 1, 16), np.complex64), meta, 0.001)
    info = NiftiVolume(path).header_info()["mrs"]
    assert info["field_t"] == pytest.approx(3.0, abs=1e-12)
    assert info["nucleus"] == nucleus and info["echo_time_s"] == 0.0


def _peaks(s, part, lo, hi, count):
    v = np.asarray(s[part])
    axis = np.linspace(s["first"], s["last"], s["points"])
    idx = [i for i in range(1, len(v) - 1) if lo <= axis[i] <= hi and v[i] > v[i - 1] and v[i] >= v[i + 1]]
    return sorted(float(axis[i]) for i in sorted(idx, key=lambda i: -v[i])[:count])


def test_single_voxel_metadata_and_peaks(tmp_path):
    path = synthetic.save_mrs(tmp_path / "svs.nii.gz", synthetic.svs_phantom(1024, 4, 2), META, 1 / 2000, affine=np.eye(4))
    vol = NiftiVolume(path)
    info = vol.header_info()
    m = info["mrs"]
    assert (m["nucleus"], m["points"], m["transients"], m["single_voxel"]) == ("1H", 1024, 8, True)
    assert m["dims"] == [{"tag": "DIM_COIL", "size": 4}, {"tag": "DIM_DYN", "size": 2}]
    assert m["field_t"] == pytest.approx(123.2 / 42.577478)
    assert m["bandwidth_hz"] == pytest.approx(2000)
    assert info["repetition_time"] is None and info["warnings"] == []
    s = vol.spectrum([0, 0, 0])
    assert (s["unit"], s["default_part"], s["default_range"]) == ("ppm", "real", [0.2, 4.2])
    assert s["first"] > s["last"]
    found = _peaks(s, "real", 0.2, 4.2, 3)
    assert found == pytest.approx([2.01, 3.03, 3.21], abs=0.02)
    real = np.asarray(s["real"])
    assert real.max() > 3 * -real.min(), "phased upright"
    assert vol.stats().notes == []


def test_maps_and_spectra_of_spectroscopic_imaging(tmp_path):
    data = synthetic.mrsi_phantom((4, 3, 2), 256)
    path = synthetic.save_mrs(tmp_path / "mrsi.nii", data, {"SpectrometerFrequency": [49.86], "ResonantNucleus": ["31P"]},
                              1 / 4000, affine=np.diag([20.0, 20.0, 25.0, 1.0]))
    vol = NiftiVolume(path)
    assert vol.header_info()["mrs"]["single_voxel"] is False
    assert vol.stats().notes == ["Maps show the magnitude of the first time point."]
    for voxel in [(0, 0, 0), (3, 2, 1)]:
        s = vol.spectrum(list(voxel))
        assert (s["unit"], s["default_part"], s["default_range"]) == ("ppm", "magnitude", None)
        pi = 4.8 + 0.2 * voxel[0] - 0.1 * voxel[1]
        assert min(abs(p - pi) for p in _peaks(s, "magnitude", -20, 20, 3)) < 0.5


def test_protocol(tmp_path):
    path = synthetic.save_mrs(tmp_path / "svs.nii", synthetic.svs_phantom(128, 1, 1), META, 1 / 2000)
    s = Session()
    ask = lambda **req: respond(s, json.dumps({"id": 1, **req}).encode())  # noqa: E731
    assert ask(op="open", path=str(path))["header"]["mrs"]["nucleus"] == "1H"
    r = ask(op="spectrum")
    assert r["ok"] and r["points"] == 128 and len(r["real"]) == 128 and "bytes" not in r
    assert ask(op="spectrum", ijk=[1, 0, 0])["error"]["code"] == "bad_request"
    t1 = synthetic.save(tmp_path / "t1.nii", np.ones((4, 4, 4), np.int16), np.eye(4))
    ask(op="open", path=str(t1))
    assert ask(op="spectrum")["error"]["code"] == "bad_request"


def test_encoded_dimensions_use_their_first_index(tmp_path):
    """An edited spectrum is a difference: edit conditions are not averaged."""
    n = 256
    outer = synthetic.mrs_fid(n, 123.2, 1 / 2000)
    target = synthetic.mrs_fid(n, 123.2, 1 / 2000, phase=1.0, gain=0.1)
    data = np.zeros((1, 1, 1, n, 2), np.complex64)
    data[0, 0, 0, :, 0] = outer + target
    data[0, 0, 0, :, 1] = outer - target
    alone = NiftiVolume(synthetic.save_mrs(tmp_path / "a.nii", data[..., :1], META, 1 / 2000)).spectrum([0, 0, 0])
    for tag in ["DIM_EDIT", "DIM_ISIS", "DIM_PHASE_CYCLE", "DIM_INDIRECT_0", "DIM_USER_0"]:
        vol = NiftiVolume(synthetic.save_mrs(tmp_path / f"{tag}.nii", data, {**META, "dim_5": tag}, 1 / 2000))
        s = vol.spectrum([0, 0, 0])
        assert s["transients"] == 1
        np.testing.assert_allclose(s["real"], alone["real"], atol=1e-5 * max(map(abs, alone["real"])))
        info = vol.header_info()
        assert info["mrs"]["combined"] == 1 and info["mrs"]["first_index_only"] == [{"tag": tag, "size": 2}]
        assert any(f"index 0 of {tag} (2)" in w for w in info["warnings"])
    coils = NiftiVolume(synthetic.save_mrs(tmp_path / "coils.nii", data, META, 1 / 2000))
    assert coils.spectrum([0, 0, 0])["transients"] == 2 and coils.header_info()["warnings"] == []
