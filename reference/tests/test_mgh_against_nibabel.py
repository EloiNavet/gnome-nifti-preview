"""FreeSurfer MGH/MGZ: nibabel writes the files, the Rust backend must read the
same affine (vox2ras, with c_ras at voxel dims / 2), shape, datatype and
values, in every frame. nibabel is the oracle here, as for NIfTI."""

from __future__ import annotations

import json
import os
import subprocess
import time

import nibabel as nib
import numpy as np
import pytest

from test_differential import RustBackend


def _rotation(rng):
    q, _ = np.linalg.qr(rng.normal(size=(3, 3)))
    return q * np.sign(np.linalg.det(q))


@pytest.fixture(scope="module")
def rust():
    backend = RustBackend()
    yield backend
    backend.close()


def _cases(tmp_path):
    rng = np.random.default_rng(7)
    cases = []
    for i, (dtype, shape, ext) in enumerate([
        (np.float32, (9, 7, 5), ".mgh"),
        (np.int16, (8, 6, 4, 3), ".mgz"),
        (np.uint8, (10, 9, 8), ".mgz"),
        (np.int32, (6, 6, 6, 2), ".mgh"),
    ]):
        data = (rng.normal(size=shape) * 100).astype(dtype) if dtype != np.uint8 else rng.integers(0, 255, shape).astype(np.uint8)
        affine = np.eye(4)
        affine[:3, :3] = _rotation(rng) * rng.uniform(0.5, 2.0, 3)
        affine[:3, 3] = rng.uniform(-50, 50, 3)
        path = tmp_path / f"case{i}{ext}"
        nib.save(nib.MGHImage(data, affine), path)
        cases.append((str(path), data))
    return cases


def test_mgh_matches_nibabel(rust, tmp_path):
    for path, data in _cases(tmp_path):
        img = nib.load(path)
        r = rust.request(op="open", path=path, mode="stored")
        assert r["ok"], r
        h = r["header"]
        assert h["format"] == ("MGZ" if path.endswith(".mgz") else "MGH")
        assert h["shape"] == list(img.shape)
        assert h["dtype"] == str(img.get_data_dtype())
        np.testing.assert_allclose(np.array(h["affine"]), img.affine, atol=1e-4)
        assert rust.request(op="load")["ok"]
        expected = np.asanyarray(img.dataobj)
        frames = img.shape[3] if img.ndim == 4 else 1
        rng = np.random.default_rng(1)
        points = [[int(rng.integers(0, n)) for n in img.shape[:3]] for _ in range(6)]
        for t in range(frames):
            for _ in range(200):
                v = rust.request(op="set_volume", volume=t) if frames > 1 else {"ready": True}
                if v["ready"]:
                    break
                time.sleep(0.01)
            for p in points:
                got = rust.request(op="value", ijk=p)["value"]
                want = expected[tuple(p)] if img.ndim == 3 else expected[tuple(p) + (t,)]
                assert got == pytest.approx(float(want), rel=1e-6, abs=1e-6), (path, t, p)
