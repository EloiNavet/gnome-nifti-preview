"""Differential tests: the Rust backend must reproduce the nibabel reference.

Both implementations speak the same protocol. Every file of a synthetic
corpus is opened by the in-process reference ``Session`` and by the Rust
binary (``nifti-preview serve``); metadata, view plans, statistics, rendered
slices and voxel values are compared.

The Rust binary is taken from $NIFTI_PREVIEW_BIN, or backend/target/release.
"""

import base64
import json
import math
import os
import subprocess

import nibabel as nib
import numpy as np
import pytest

from nifti_preview_ref import synthetic
from nifti_preview_ref.server import Session, respond

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
BIN = os.environ.get("NIFTI_PREVIEW_BIN", os.path.join(ROOT, "backend", "target", "release", "nifti-preview"))

pytestmark = pytest.mark.skipif(not os.path.exists(BIN), reason=f"Rust backend not built ({BIN})")


class RustBackend:
    def __init__(self):
        self.proc = subprocess.Popen([BIN, "serve"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        self.hello = json.loads(self.proc.stdout.readline())
        self.next_id = 0

    def request(self, **req):
        self.next_id += 1
        req["id"] = self.next_id
        self.proc.stdin.write(json.dumps(req).encode() + b"\n")
        self.proc.stdin.flush()
        header = json.loads(self.proc.stdout.readline())
        assert header["id"] == self.next_id
        if header.get("ok") and "bytes" in header:
            header["pixels"] = self.proc.stdout.read(header["bytes"])
        return header

    def close(self):
        self.proc.stdin.close()
        assert self.proc.wait(timeout=10) == 0


class Reference:
    def __init__(self):
        self.session = Session()
        self.next_id = 0

    def request(self, **req):
        self.next_id += 1
        req["id"] = self.next_id
        out = respond(self.session, json.dumps(req).encode())
        out = json.loads(json.dumps(out))  # same JSON round trip as the wire
        if out.get("ok") and "data" in out:
            out["pixels"] = base64.b64decode(out.pop("data"))
        return out


@pytest.fixture(scope="module")
def rust():
    backend = RustBackend()
    yield backend
    backend.close()


# ------------------------------------------------------------------ corpus

def _corpus(d):
    """name -> path for a wide range of synthetic NIfTI variants."""
    files = {}

    def save(name, data, affine, **kw):
        files[name] = str(synthetic.save(os.path.join(d, name), data, affine, **kw))

    marker, maff = synthetic.marker_phantom()
    for codes in ["RAS", "LPS", "LAS", "RPI", "LIA", "ASR", "PIL", "SLP"]:
        dd, aa = synthetic.reorient(marker, maff, codes)
        save(f"marker_{codes}.nii.gz", dd, aa, dtype=np.int16)
    save("marker_nifti2.nii", marker, maff, dtype=np.int16, version=2)
    save("marker_nifti2.nii.gz", marker, maff, dtype=np.float32, version=2)
    save("marker_qform_only.nii.gz", marker, maff, dtype=np.int16, sform_code=0)
    save("marker_no_orientation.nii.gz", marker, maff, dtype=np.int16, sform_code=0, qform_code=0)
    save("marker_oblique20.nii.gz", marker, synthetic.rotate_affine(maff, 20.0), dtype=np.int16)
    save("marker_oblique30_x.nii.gz", marker, synthetic.rotate_affine(maff, 30.0, axis=0), dtype=np.int16)
    save("marker_oblique20_qform.nii.gz", marker, synthetic.rotate_affine(maff, 20.0), dtype=np.int16, sform_code=0)
    save("marker_aniso.nii.gz", marker, synthetic.ras_affine(marker.shape, (0.7, 1.3, 3.0)), dtype=np.int16)
    save("marker_scaled.nii.gz", marker, maff, dtype=np.int16, slope=0.5, inter=10.0)

    rng = np.random.default_rng(42)
    for i in range(12):
        # random rotation (well away from 45° ties), zooms and a little shear
        q, _ = np.linalg.qr(rng.normal(size=(3, 3)))
        perm = np.eye(3)[rng.permutation(3)] * rng.choice([-1, 1], size=3)
        rot = perm @ (np.eye(3) + 0.25 * (q - np.eye(3)))
        zooms = rng.uniform(0.5, 3.0, size=3)
        aff = np.eye(4)
        aff[:3, :3] = rot @ np.diag(zooms) + (0.05 * rng.normal(size=(3, 3)) if i % 3 == 0 else 0)
        aff[:3, 3] = rng.uniform(-100, 100, size=3)
        save(f"random_affine_{i}.nii.gz", marker, aff, dtype=np.int16, qform_code=(i % 2), sform_code=1 - (i % 2))

    base = np.clip(marker, 0, 120)
    for dt in [np.uint8, np.int8, np.int16, np.uint16, np.int32, np.uint32, np.int64, np.uint64, np.float32, np.float64]:
        save(f"dtype_{np.dtype(dt).name}.nii.gz", base, maff, dtype=dt)
        save(f"dtype_{np.dtype(dt).name}.nii", base, maff, dtype=dt)
    save("dtype_complex64.nii.gz", (base + 1j * base / 2).astype(np.complex64), maff)
    save("dtype_complex128.nii", (base - 2j).astype(np.complex128), maff)
    for name, fields in [("rgb24", "RGB"), ("rgba32", "RGBA")]:
        dt = np.dtype([(c, "u1") for c in fields])
        arr = np.zeros(base.shape, dtype=dt)
        arr["R"], arr["G"], arr["B"] = base.astype(np.uint8), (base // 2).astype(np.uint8), 7
        img = nib.Nifti1Image(arr, maff)
        files[f"dtype_{name}.nii.gz"] = os.path.join(d, f"dtype_{name}.nii.gz")
        nib.save(img, files[f"dtype_{name}.nii.gz"])
    for dt in [">i2", ">f4", ">f8", ">u2"]:
        path = os.path.join(d, f"bigendian_{dt[1:]}.nii")
        nib.save(nib.Nifti1Image(marker.astype(dt), maff), path)
        files[os.path.basename(path)] = path

    f = marker.copy()
    f[0, :, :] = np.nan
    f[5, 5, 5] = np.inf
    f[6, 6, 6] = -np.inf
    save("float_nan_inf.nii.gz", f, maff, dtype=np.float32)
    save("constant.nii.gz", np.full((12, 13, 14), 7.0, np.float32), np.eye(4))
    save("zeros.nii.gz", np.zeros((12, 13, 14), np.int16), np.eye(4))
    save("all_nan.nii", np.full((8, 9, 10), np.nan, np.float32), np.eye(4))
    tiny = np.zeros((20, 20, 20), np.float32)
    tiny[5:15, 5:15, 5:15] = rng.uniform(1e-9, 3e-9, (10, 10, 10))
    save("tiny_units.nii.gz", tiny, np.eye(4))

    labels, laff = synthetic.label_phantom()
    save("labels_int16.nii.gz", labels, laff, dtype=np.int16)
    save("labels_float32.nii.gz", labels, laff, dtype=np.float32)
    save("labels_uint8.nii", labels, laff, dtype=np.uint8)
    save("mask_binary.nii.gz", (labels > 0).astype(np.uint8), laff)
    lesion = np.zeros((40, 40, 40), np.uint8)
    lesion[3:6, 3:6, 3:6] = 1
    save("mask_sparse.nii.gz", lesion, np.eye(4))
    save("labels_intent.nii.gz", rng.integers(1, 50, (30, 30, 30)).astype(np.int16), np.eye(4), intent="label")
    save("labels_scaled_is_intensity.nii.gz", labels, laff, dtype=np.int16, slope=2.0)
    save("noisy_uint8.nii.gz", rng.integers(1, 200, (30, 30, 30)).astype(np.uint8), np.eye(4))
    i, j, k = np.meshgrid(*(np.arange(40) // 4,) * 3, indexing="ij")
    save("labels_300.nii.gz", ((i * 100 + j * 10 + k) % 300 + 1).astype(np.int16), np.eye(4))
    save("labels_big_values.nii.gz", (labels.astype(np.int32) * 1000 + 2000) * (labels > 0), laff)
    # Label volumes: anisotropic voxels, and spatial units other than mm.
    save("labels_aniso_unknown_units.nii.gz", labels, np.diag([0.8, 1.2, 2.5, 1]), dtype=np.uint8, units="unknown")
    save("labels_meter.nii.gz", labels, np.diag([0.001, 0.001, 0.002, 1]), dtype=np.int16, units="meter")
    save("labels_micron.nii.gz", labels, np.diag([50.0, 50.0, 80.0, 1]), dtype=np.uint16, units="micron")
    # Identifiers above 2**24 (float32 would merge them), also resampled.
    big_ids = np.where(labels > 0, labels.astype(np.uint32) % 3 + 2**24, 0).astype(np.uint32)
    save("labels_above_2pow24.nii.gz", big_ids, laff, dtype=np.uint32)
    save("labels_above_2pow24_oblique.nii.gz", big_ids, synthetic.rotate_affine(laff, 25.0), dtype=np.uint32)

    # Native 64-bit identifiers, float64 labels, and extreme finite contrast.
    # Unit tests supply independent integer/count expectations for these cases.
    for dtype, a, b in [(np.int64, 2**53, 2**53 + 1),
                        (np.uint64, 2**64 - 2, 2**64 - 1),
                        (np.float64, 2**24, 2**24 + 1)]:
        data = np.zeros((6, 6, 6), dtype=dtype)
        data[1:3, 1:5, 1:5], data[3:5, 1:5, 1:5] = a, b
        save(f"exact_{np.dtype(dtype).name}_4d.nii.gz", np.stack([data, np.zeros_like(data)], axis=3), np.eye(4), dtype=dtype)
        for suffix, affine in [("straight", np.eye(4)), ("oblique", synthetic.rotate_affine(np.eye(4), 25))]:
            save(f"exact_{np.dtype(dtype).name}_{suffix}.nii.gz", data, affine, dtype=dtype)
    extremes = np.full((4, 4, 4), -3e38, np.float32)
    extremes[2:] = 3e38
    save("extreme_finite_range.nii.gz", extremes, np.eye(4), dtype=np.float32)

    # Hand-written headers for values nibabel would not save.
    def raw(name, data, edit, sform=True):
        hdr = nib.Nifti1Header()
        hdr.set_data_dtype(data.dtype)
        hdr.set_data_shape(data.shape)
        hdr.set_qform(maff, 1)
        if sform:
            hdr.set_sform(maff, 1)
        hdr["vox_offset"] = 352
        edit(hdr)
        blob = hdr.binaryblock + b"\0" * 4 + np.asfortranarray(data).tobytes(order="F")
        path = os.path.join(d, name)
        with open(path, "wb") as fh:
            fh.write(__import__("gzip").compress(blob) if name.endswith(".gz") else blob)
        files[name] = path

    m16 = marker.astype(np.int16)

    def nan_pixdim(h):
        h["pixdim"][1] = np.nan
        h["pixdim"][3] = np.inf

    def nan_translation(h):
        h["srow_y"][3] = np.nan

    def descrip(h):
        h["descrip"] = b"T1\x1b[31m\nfake row\xe2\x80\xae evil\x7f end"

    raw("pixdim_nan_qform.nii.gz", m16, nan_pixdim, sform=False)
    raw("pixdim_nan_sform.nii", m16, nan_pixdim)
    raw("affine_nan_translation.nii.gz", m16, nan_translation)
    raw("descrip_controls.nii.gz", m16, descrip)

    small4d = np.stack([marker + 1000 * t for t in range(4)], axis=-1)
    save("bold_4d.nii.gz", small4d, maff, dtype=np.int16, zooms=(1, 1, 1, 2.5))
    save("bold_4d.nii", small4d, maff, dtype=np.int16, zooms=(1, 1, 1, 2.5))
    save("bold_4d_scaled.nii.gz", small4d, maff, dtype=np.int16, slope=0.25, inter=-3.0)
    save("disp_5d.nii.gz", np.ones(marker.shape + (1, 3), np.float32) * np.arange(3), maff)
    save("flat_2d.nii.gz", np.arange(30 * 20, dtype=np.float32).reshape(30, 20), np.eye(4))
    # Spectroscopy (NIfTI-MRS)
    meta = {"SpectrometerFrequency": [123.2], "ResonantNucleus": ["1H"], "dim_5": "DIM_COIL", "dim_6": "DIM_DYN"}
    for name, data, kw in [
        ("mrs_svs.nii.gz", synthetic.svs_phantom(512, 3, 2), dict(meta=meta, affine=np.eye(4))),
        ("mrs_svs_c128_no_orientation.nii", synthetic.svs_phantom(300, 2, 1).astype(np.complex128), dict(meta=meta)),
        ("mrs_svs_nifti1.nii.gz", synthetic.svs_phantom(256, 1, 1), dict(meta=meta, affine=np.eye(4), version=1)),
        ("mrs_mrsi.nii", synthetic.mrsi_phantom((4, 3, 2), 128), dict(meta={"SpectrometerFrequency": [49.86], "ResonantNucleus": ["31P"]},
                                                                    affine=np.diag([20.0, 20.0, 25.0, 1.0]))),
        ("mrs_mrsi_oblique.nii.gz", synthetic.mrsi_phantom((5, 4, 3), 64), dict(meta={"SpectrometerFrequency": [49.86], "ResonantNucleus": ["31P"]},
                                                                             affine=synthetic.rotate_affine(np.diag([10.0, 10.0, 12.0, 1.0]), 20.0))),
        ("mrs_extension_only.nii.gz", synthetic.svs_phantom(128, 2, 1), dict(meta=meta, affine=np.eye(4), intent_name="")),
        ("mrs_bad_json.nii.gz", synthetic.svs_phantom(128, 1, 1), dict(meta=None, affine=np.eye(4), extension=b"{not json")),
        ("mrs_no_frequency.nii.gz", synthetic.svs_phantom(128, 1, 1), dict(meta={}, affine=np.eye(4))),
        # Coils averaged, edit conditions not: index 0 only.
        ("mrs_edit.nii.gz", synthetic.svs_phantom(256, 3, 2),
         dict(meta={**meta, "dim_6": "DIM_EDIT"}, affine=np.eye(4))),
    ]:
        files[name] = str(synthetic.save_mrs(os.path.join(d, name), data, dwell=1 / 2000, **kw))
    with open(files["mrs_svs_c128_no_orientation.nii"], "rb") as src:
        whole = src.read()
    files["mrs_truncated.nii"] = os.path.join(d, "mrs_truncated.nii")
    with open(files["mrs_truncated.nii"], "wb") as dst:
        dst.write(whole[: len(whole) * 2 // 3])
    head, haff = synthetic.head_phantom((48, 56, 44))
    save("head.nii.gz", head, haff, dtype=np.int16)
    save("sub 001 é spaces.nii.gz", head, haff, dtype=np.int16)
    return files


@pytest.fixture(scope="module")
def corpus(tmp_path_factory):
    return _corpus(str(tmp_path_factory.mktemp("corpus")))


# -------------------------------------------------------------- comparison

def _close(a, b, path=""):
    if isinstance(a, dict) and isinstance(b, dict):
        assert set(a) <= set(b), f"{path}: keys {set(a) - set(b)} missing in Rust"
        for k in a:
            _close(a[k], b[k], f"{path}.{k}")
    elif isinstance(a, list) and isinstance(b, list):
        assert len(a) == len(b), f"{path}: length {len(a)} != {len(b)}"
        for i, (x, y) in enumerate(zip(a, b)):
            _close(x, y, f"{path}[{i}]")
    elif isinstance(a, bool) or isinstance(b, bool) or isinstance(a, str) or a is None or b is None:
        if path.endswith("obliquity_deg") and (a is None or b is None):
            assert (a or 0.0) < 1e-5 and (b or 0.0) < 1e-5, f"{path}: {a!r} != {b!r}"
            return
        assert a == b, f"{path}: {a!r} != {b!r}"
    elif isinstance(a, (int, float)):
        assert isinstance(b, (int, float)), f"{path}: {a!r} != {b!r}"
        assert math.isclose(a, b, rel_tol=1e-6, abs_tol=1e-9), f"{path}: {a!r} != {b!r}"
    else:
        assert a == b, f"{path}: {a!r} != {b!r}"


def _compare_pixels(ref, out, tag, exact=False):
    assert (out["width"], out["height"]) == (ref["width"], ref["height"]), tag
    assert out["colormap"] == ref["colormap"], tag
    a = np.frombuffer(ref["pixels"], np.uint8).astype(int)
    b = np.frombuffer(out["pixels"], np.uint8).astype(int)
    assert a.shape == b.shape, tag
    diff = np.abs(a - b)
    if exact or ref["colormap"] in ("labels", "rgb"):
        assert diff.max(initial=0) == 0, f"{tag}: {np.count_nonzero(diff)} pixels differ"
    else:
        # float32 scaling paths may differ by one grey level in rare voxels
        assert diff.max(initial=0) <= 1, f"{tag}: max diff {diff.max()}"
        assert np.count_nonzero(diff) <= max(3, a.size // 1000), f"{tag}: {np.count_nonzero(diff)} pixels differ"


def _compare_spectra(ref, out, tag):
    assert ref["ok"] == out["ok"], (tag, ref, out)
    if not ref["ok"]:
        assert ref["error"]["code"] == out["error"]["code"], (tag, ref, out)
        return
    _close({k: v for k, v in ref.items() if k not in ("id", "real", "magnitude")}, out, tag)
    for part in ("real", "magnitude"):
        if ref.get(part) is None:
            continue
        a, b = np.asarray(ref[part], float), np.asarray(out[part], float)
        assert a.shape == b.shape, (tag, part)
        scale = max(float(np.abs(a).max(initial=0)), 1e-30)
        assert float(np.abs(a - b).max(initial=0)) <= 1e-4 * scale, f"{tag} {part}: max diff {np.abs(a - b).max()} (scale {scale})"


def _check_file(rust, path):
    ref = Reference()
    # the Rust session is shared across files: reset mode and colormap explicitly
    r_open = ref.request(op="open", path=path, mode="neurological")
    o_open = rust.request(op="open", path=path, mode="neurological")
    if o_open["ok"]:
        rust.request(op="set_display", colormap="auto")
    assert o_open["ok"] == r_open["ok"], (r_open, o_open)
    if not r_open["ok"]:
        assert o_open["error"]["code"] == r_open["error"]["code"], (r_open, o_open)
        return
    _close({k: v for k, v in r_open.items() if k != "id"}, o_open, "open")

    r_load = ref.request(op="load")
    o_load = rust.request(op="load")
    assert o_load["ok"] == r_load["ok"], (r_load, o_load)
    if not r_load["ok"]:
        assert o_load["error"]["code"] == r_load["error"]["code"]
        return
    _close({k: v for k, v in r_load.items() if k != "id"}, o_load, "load")

    if r_open["header"]["mrs"]:
        for ijk in [None, r_open["cursor"], [0, 0, 0], [n - 1 for n in r_open["grid"]["shape"]]]:
            req = {} if ijk is None else {"ijk": ijk}
            _compare_spectra(ref.request(op="spectrum", **req), rust.request(op="spectrum", **req), f"spectrum {ijk}")

    cursor = r_open["cursor"]
    for mode in ["neurological", "radiological", "stored", "neurological"]:
        for colormap in ["auto", "gray", "rgb"] + (["labels"] if r_load["stats"]["labels"]["is_labels"] else []):
            # the cursor is carried over from grid to grid
            r_d = ref.request(op="set_display", mode=mode, colormap=colormap, cursor=cursor)
            o_d = rust.request(op="set_display", mode=mode, colormap=colormap, cursor=cursor)
            _close({k: v for k, v in r_d.items() if k != "id"}, o_d, f"display {mode}")
            cursor = r_d["cursor"]
            for view, plan in r_d["plans"].items():
                for index in sorted({0, plan["n_slices"] // 3, plan["n_slices"] // 2, plan["n_slices"] - 1}):
                    r_s = ref.request(op="slice", view=view, index=index)
                    o_s = rust.request(op="slice", view=view, index=index)
                    assert o_s["ok"], o_s
                    _compare_pixels(r_s, o_s, f"{os.path.basename(path)} {mode}/{colormap} {view}[{index}]")
        for ijk in [cursor, [0, 0, 0], [n - 1 for n in r_d["grid"]["shape"]], [n // 3 for n in r_d["grid"]["shape"]]]:
            r_v = ref.request(op="value", ijk=ijk)
            o_v = rust.request(op="value", ijk=ijk)
            _close({k: v for k, v in r_v.items() if k != "id"}, o_v, f"value {mode} {ijk}")


def test_corpus_matches_reference(rust, corpus):
    failures = []
    for name, path in sorted(corpus.items()):
        try:
            _check_file(rust, path)
        except AssertionError as exc:
            failures.append(f"{name}: {exc}")
    assert not failures, "\n".join(failures)


def test_invalid_files_match(rust, tmp_path, corpus):
    cases = {
        "empty.nii.gz": b"",
        "text.nii.gz": b"not a nifti\n" * 40,
        "gzip_garbage.nii.gz": __import__("gzip").compress(b"garbage" * 100),
        "gzip_zeros.nii.gz": __import__("gzip").compress(b"\0" * 400),
        "garbage.nii": b"\x01" * 600,
    }
    blob = open(corpus["head.nii.gz"], "rb").read()
    cases["truncated.nii.gz"] = blob[: len(blob) // 2]
    raw = open(corpus["dtype_float32.nii"], "rb").read()
    cases["truncated.nii"] = raw[: len(raw) // 2]
    for name, content in cases.items():
        p = tmp_path / name
        p.write_bytes(content)
        ref, out = Reference(), rust
        r = ref.request(op="open", path=str(p))
        o = out.request(op="open", path=str(p))
        if r["ok"]:
            r = ref.request(op="load")
            assert o["ok"], (name, o)
            o = out.request(op="load")
        assert not r["ok"] and not o["ok"], (name, r, o)
        assert o["error"]["code"] == r["error"]["code"], (name, r["error"], o["error"])


def _hostile_cases(tmp_path):
    def header(shape, offset=None, version=1):
        hdr = (nib.Nifti1Header if version == 1 else nib.Nifti2Header)()
        hdr.set_data_dtype(np.uint8)
        hdr.set_data_shape(shape)
        if offset is not None:
            hdr["vox_offset"] = offset
        return hdr.binaryblock + b"\0" * 4

    gz = __import__("gzip").compress
    cases = {
        "offset_1e12.nii.gz": gz(header((4, 4, 4), 1e12) + b"\1" * 64),
        "offset_1e18.nii": header((4, 4, 4), 1e18) + b"\1" * 64,
        "offset_nifti2.nii.gz": gz(header((4, 4, 4), 9e18, version=2) + b"\1" * 64),
        "flat_32767.nii.gz": gz(header((32767, 32767, 1)) + b"\0" * 64),
        "flat_2d.nii.gz": gz(header((20000, 2)) + b"\0" * 64),
        "slab_4097.nii.gz": gz(header((2, 4097, 4097), version=2) + b"\0" * 64),
    }
    paths = {}
    for name, content in cases.items():
        p = tmp_path / name
        p.write_bytes(content)
        paths[name] = p
    sparse = tmp_path / "sparse_8gib.nii"
    sparse.write_bytes(header((2048, 2048, 2048)))
    os.truncate(sparse, 352 + (8 << 30))
    paths[sparse.name] = sparse
    return paths


def test_hostile_files_match(rust, tmp_path):
    for name, path in _hostile_cases(tmp_path).items():
        ref = Reference()
        r = ref.request(op="open", path=str(path))
        o = rust.request(op="open", path=str(path))
        assert r["ok"] and o["ok"], (name, r, o)
        r = ref.request(op="load")
        o = rust.request(op="load")
        assert not r["ok"] and not o["ok"], (name, r, o)
        assert o["error"]["code"] == r["error"]["code"], (name, r["error"], o["error"])
        assert o["error"]["message"] == r["error"]["message"], (name, r["error"], o["error"])


def test_protocol_errors_match(corpus):
    rust = RustBackend()  # fresh session: "no file open" must be reported
    ref = Reference()
    for req in [
        {"op": "slice", "view": "axial", "index": 0},
        {"op": "explode"},
        {"op": "open"},
        {"op": "open", "path": corpus["head.nii.gz"]},
        {"op": "slice", "view": "oblique", "index": 0},
        {"op": "slice", "view": "axial", "index": "3"},
        {"op": "slice", "view": "axial", "index": True},
        {"op": "value", "ijk": [1, 2]},
        {"op": "value", "ijk": [999, 0, 0]},
        {"op": "set_display", "mode": "upside-down"},
        {"op": "set_display", "colormap": "rainbow"},
    ]:
        r = ref.request(**req)
        o = rust.request(**req)
        assert r["ok"] == o["ok"], (req, r, o)
        if not r["ok"]:
            assert r["error"]["code"] == o["error"]["code"], (req, r, o)
    rust.close()


def test_failed_load_then_slice_is_an_error_not_a_crash(tmp_path, corpus):
    blob = open(corpus["head.nii.gz"], "rb").read()
    bad = tmp_path / "half.nii.gz"
    bad.write_bytes(blob[: len(blob) // 2])
    rust = RustBackend()
    assert rust.request(op="open", path=str(bad))["ok"]
    for colormap in ["gray", "labels", "auto"]:
        out = rust.request(op="slice", view="axial", index=3, colormap=colormap)
        assert not out["ok"] and out["error"]["code"] == "corrupt_data", out
    assert rust.request(op="open", path=corpus["head.nii.gz"])["ok"]
    assert rust.request(op="slice", view="axial", index=3)["ok"]
    rust.close()
