import base64
import io
import json
import os
import subprocess
import sys
import time

import numpy as np
import pytest

from nifti_preview_ref import PROTOCOL_VERSION
from nifti_preview_ref.server import Session, SliceCache, respond, serve


def _req(session, **kwargs):
    return respond(session, json.dumps(kwargs).encode())


def _run(lines):
    stdin = io.BytesIO(b"".join(json.dumps(x).encode() + b"\n" if not isinstance(x, bytes) else x for x in lines))
    stdout = io.BytesIO()
    old = sys.stdout
    try:
        assert serve(stdin, stdout) == 0
    finally:
        sys.stdout = old
    return [json.loads(line) for line in stdout.getvalue().splitlines()]


def test_full_session(t1_path):
    out = _run([
        {"id": 1, "op": "open", "path": t1_path},
        {"id": 2, "op": "load"},
        {"id": 3, "op": "slice", "view": "axial", "index": 10},
        {"id": 4, "op": "value", "ijk": [5, 6, 7]},
        {"id": 5, "op": "set_display", "mode": "radiological"},
        {"id": 6, "op": "slice", "view": "axial", "index": 10},
        {"id": 7, "op": "close"},
        {"id": 8, "op": "ping"},  # never answered: the server stops after close
    ])
    hello, opened, loaded, sl, value, display, sl_radio, closed = out
    assert hello["event"] == "hello" and hello["protocol"] == PROTOCOL_VERSION
    assert opened["ok"] and opened["id"] == 1
    assert opened["header"]["name"] == "sub-001_T1w.nii.gz"
    assert set(opened["plans"]) == {"axial", "coronal", "sagittal"}
    assert opened["mode"] == "neurological"
    assert loaded["stats"]["window"]["status"] == "ok"
    assert loaded["effective_colormap"] == "gray"
    raw = base64.b64decode(sl["data"])
    assert len(raw) == sl["width"] * sl["height"] * 3
    assert sl["format"] == "rgb8"
    assert value["ok"] and len(value["world"]) == 3
    assert display["mode"] == "radiological"
    assert display["plans"]["axial"]["labels"]["left"] == "R"
    img_neuro = np.frombuffer(raw, np.uint8).reshape(sl["height"], sl["width"], 3)
    img_radio = np.frombuffer(base64.b64decode(sl_radio["data"]), np.uint8).reshape(sl["height"], sl["width"], 3)
    np.testing.assert_array_equal(img_neuro[:, ::-1], img_radio)
    assert closed["closed"] is True


def test_errors_are_reported_not_raised(tmp_path, t1_path):
    bad = tmp_path / "fake.nii.gz"
    bad.write_bytes(b"not a nifti")
    out = _run([
        b"{not json\n",
        {"id": 1, "op": "slice", "view": "axial", "index": 0},
        {"id": 2, "op": "explode"},
        {"id": 3, "op": "open", "path": str(bad)},
        {"id": 4, "op": "open"},
        {"id": 5, "op": "open", "path": t1_path},
        {"id": 6, "op": "slice", "view": "oblique", "index": 0},
        {"id": 7, "op": "slice", "view": "axial", "index": "3"},
        {"id": 8, "op": "value", "ijk": [1, 2]},
        {"id": 9, "op": "set_display", "mode": "upside-down"},
        [1, 2, 3],
    ])
    codes = [m.get("error", {}).get("code") for m in out[1:]]
    assert codes == ["bad_request", "bad_request", "bad_request", "invalid_nifti", "bad_request", None,
                     "bad_request", "bad_request", "bad_request", "bad_request", "bad_request"]
    assert out[2]["id"] == 1


def test_slice_index_is_clamped(t1_path):
    s = Session()
    assert _req(s, op="open", path=t1_path)["ok"]
    r = _req(s, op="slice", view="sagittal", index=10_000)
    assert r["ok"] and r["index"] == s.volume.plans("neurological")["sagittal"].n_slices - 1


def test_cache_hits_and_bounds(t1_path):
    s = Session(cache_bytes=200_000)
    _req(s, op="open", path=t1_path)
    first = _req(s, op="slice", view="axial", index=5)
    assert len(s.cache) == 1
    again = _req(s, op="slice", view="axial", index=5)
    assert first["data"] == again["data"]
    for i in range(40):
        _req(s, op="slice", view="axial", index=i)
    assert s.cache.size <= 200_000


def test_slice_cache_lru():
    c = SliceCache(max_bytes=10)
    c.put("a", {"data": "12345"})
    c.put("b", {"data": "12345"})
    c.get("a")
    c.put("c", {"data": "12345"})
    assert c.get("b") is None and c.get("a") is not None
    c.put("huge", {"data": "x" * 100})
    assert c.get("huge") is None


def test_reopen_in_same_session(t1_path, labels_path):
    s = Session()
    for _ in range(5):
        assert _req(s, op="open", path=t1_path)["ok"]
        assert _req(s, op="load")["effective_colormap"] == "gray"
        assert _req(s, op="open", path=labels_path)["ok"]
        assert _req(s, op="load")["effective_colormap"] == "labels"
        assert _req(s, op="slice", view="coronal", index=3, colormap="gray")["colormap"] == "gray"


@pytest.mark.parametrize("units, affine_scale, mm3", [
    ("mm", (1.0, 1.0, 1.0), 1.0),
    ("unknown", (0.8, 1.2, 2.5), 2.4),        # unknown units are taken as mm
    ("meter", (0.001, 0.001, 0.002), 2.0),
    ("micron", (500.0, 500.0, 400.0), 0.1),
])
def test_label_volume_under_cursor(write, units, affine_scale, mm3):
    data = np.zeros((20, 20, 20), np.uint8)
    data[2:6, 2:7, 2:8] = 3                    # 120 voxels
    data[10:12, 10:12, 10:12] = 7              # 8 voxels
    path = write("seg.nii.gz", data, np.diag(list(affine_scale) + [1.0]), units=units)
    s = Session()
    assert _req(s, op="open", path=path)["ok"]
    assert _req(s, op="load")["stats"]["labels"]["is_labels"]
    r = _req(s, op="value", ijk=[3, 3, 3])
    assert r["value"] == 3 and r["label_voxels"] == 120
    assert r["label_volume_mm3"] == pytest.approx(120 * mm3)
    assert _req(s, op="value", ijk=[11, 11, 11])["label_voxels"] == 8
    background = _req(s, op="value", ijk=[0, 0, 0])
    assert background["value"] == 0 and "label_voxels" not in background


def test_no_label_volume_for_intensity_images(t1_path):
    s = Session()
    _req(s, op="open", path=t1_path)
    _req(s, op="load")
    assert "label_volume_mm3" not in _req(s, op="value", ijk=[24, 28, 22])


def test_unknown_orientation_reports_stored_mode(write, marker):
    data, affine = marker
    path = write("noorient.nii.gz", data, affine, qform_code=0, sform_code=0)
    s = Session()
    r = _req(s, op="open", path=path, mode="neurological")
    assert r["mode"] == "stored" and r["requested_mode"] == "neurological"


def _backend_env():
    env = dict(os.environ)
    src = os.path.join(os.path.dirname(__file__), "..", "src")
    env["PYTHONPATH"] = os.path.abspath(src) + os.pathsep + env.get("PYTHONPATH", "")
    return env


def test_subprocess_exits_on_eof(t1_path):
    proc = subprocess.Popen([sys.executable, "-m", "nifti_preview_ref.server"], stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=_backend_env())
    hello = json.loads(proc.stdout.readline())
    assert hello["event"] == "hello"
    proc.stdin.write(json.dumps({"id": 1, "op": "open", "path": t1_path}).encode() + b"\n")
    proc.stdin.flush()
    assert json.loads(proc.stdout.readline())["ok"]
    t = time.monotonic()
    proc.stdin.close()
    assert proc.wait(timeout=5) == 0
    assert time.monotonic() - t < 2.0


def test_subprocess_close_request(t1_path):
    proc = subprocess.run(
        [sys.executable, "-m", "nifti_preview_ref.server"],
        input=(json.dumps({"id": 1, "op": "open", "path": t1_path}) + "\n" + json.dumps({"id": 2, "op": "close"}) + "\n").encode(),
        capture_output=True, timeout=30, env=_backend_env(),
    )
    lines = [json.loads(x) for x in proc.stdout.splitlines()]
    assert proc.returncode == 0 and lines[-1]["closed"] is True
    # stdout carries nothing but protocol messages
    assert all("ok" in m or "event" in m for m in lines)


@pytest.mark.parametrize("n", [3])
def test_many_sequential_backends(t1_path, n):
    for _ in range(n):
        test_subprocess_exits_on_eof(t1_path)
