import json
import struct

from nifti_preview_ref.cli import main
from nifti_preview_ref.pngwriter import encode_rgb


def _png_size(path):
    blob = open(path, "rb").read()
    assert blob[:8] == b"\x89PNG\r\n\x1a\n"
    return struct.unpack(">II", blob[16:24])


def test_png_writer():
    png = encode_rgb(2, 1, bytes([255, 0, 0, 0, 255, 0]))
    assert png.startswith(b"\x89PNG") and png.endswith(b"IEND\xaeB`\x82")


def test_info(capsys, t1_path):
    assert main(["info", t1_path, "--data"]) == 0
    out = capsys.readouterr().out
    assert "orientation  RAS" in out and "window" in out
    assert main(["info", t1_path, "--json"]) == 0
    info = json.loads(capsys.readouterr().out)
    assert info["plans"]["axial"]["labels"]["right"] == "R"


def test_info_error(tmp_path, capsys):
    bad = tmp_path / "bad.nii.gz"
    bad.write_bytes(b"x")
    assert main(["info", str(bad)]) == 2
    assert "invalid_image" in capsys.readouterr().err


def test_render_all_views(tmp_path, t1_path):
    out = tmp_path / "out.png"
    assert main(["render", t1_path, "-o", str(out)]) == 0
    for view in ("axial", "coronal", "sagittal"):
        assert _png_size(tmp_path / f"out-{view}.png")[0] > 0
    assert main(["render", t1_path, "-o", str(out), "--view", "axial", "--index", "3"]) == 0
    assert _png_size(out) == (48, 56)


def test_doctor(capsys):
    assert main(["doctor"]) == 0
    assert "nibabel" in json.loads(capsys.readouterr().out)


def test_make_fixtures(tmp_path, capsys):
    assert main(["make-fixtures", str(tmp_path), "--small"]) == 0
    files = capsys.readouterr().out.split()
    assert any(f.endswith("sub 001 with spaces.nii") for f in files) or "spaces.nii" in " ".join(files)
