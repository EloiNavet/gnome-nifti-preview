import numpy as np
import pytest

from nifti_preview_ref import labels, synthetic
from nifti_preview_ref.render import render_slice
from nifti_preview_ref.volume import NiftiVolume
from nifti_preview_ref.windowing import strided_sample


def _detect(path):
    return NiftiVolume(path).stats().labels


def test_parcellation_is_detected(labels_path):
    info = _detect(labels_path)
    assert info.is_labels and not info.binary
    assert info.n_labels == 6


def test_intensity_image_is_not_labels(t1_path):
    assert not _detect(t1_path).is_labels


def test_binary_mask(write):
    data, affine = synthetic.label_phantom()
    path = write("mask.nii.gz", (data > 0).astype(np.uint8), affine)
    info = _detect(path)
    assert info.is_labels and info.binary


def test_float_labels_with_integral_values(write):
    data, affine = synthetic.label_phantom()
    path = write("float_labels.nii.gz", data.astype(np.float32), affine)
    assert _detect(path).is_labels


def test_scaled_integers_are_intensity(write):
    data, affine = synthetic.label_phantom()
    path = write("scaled.nii.gz", data, affine, dtype=np.int16, slope=2.0, inter=0.0)
    assert not _detect(path).is_labels


def test_negative_values_are_intensity(write):
    data, affine = synthetic.label_phantom()
    path = write("neg.nii.gz", data.astype(np.int16) - 3, affine)
    assert not _detect(path).is_labels


def test_uint8_intensity_with_few_values_is_not_labels(write):
    rng = np.random.default_rng(0)
    data = rng.integers(1, 200, size=(40, 40, 40)).astype(np.uint8)
    path = write("noisy_uint8.nii.gz", data, synthetic.ras_affine(data.shape))
    assert not _detect(path).is_labels


def test_intent_label_forces_labels(write):
    rng = np.random.default_rng(0)
    data = rng.integers(1, 50, size=(30, 30, 30)).astype(np.int16)
    path = write("intent.nii.gz", data, synthetic.ras_affine(data.shape), intent="label")
    assert _detect(path).is_labels


def test_sparse_lesion_mask_is_labels(write):
    data = np.zeros((40, 40, 40), np.uint8)
    data[3:6, 3:6, 3:6] = 1  # far from the centre slices
    path = write("lesion.nii.gz", data, synthetic.ras_affine(data.shape))
    info = _detect(path)
    assert info.is_labels and info.binary


def test_colorize_never_blends_labels():
    data, _ = synthetic.label_phantom()
    s = data[:, :, data.shape[2] // 2]
    rgb = labels.colorize(s, binary=False)
    allowed = {(0, 0, 0)} | {labels.label_color(v) for v in np.unique(s) if v}
    seen = {tuple(px) for px in rgb.reshape(-1, 3)}
    assert seen <= allowed
    # every voxel gets exactly its own label's colour
    for v in np.unique(s):
        expected = (0, 0, 0) if v == 0 else labels.label_color(v)
        assert (rgb[s == v] == expected).all()


def test_label_colors_deterministic_and_distinct():
    assert labels.label_color(17) == labels.label_color(17)
    first = [labels.label_color(i) for i in range(1, 33)]
    assert len(set(first)) == 32
    assert (0, 0, 0) not in first


def test_binary_mask_renders_white(write):
    data, affine = synthetic.label_phantom()
    path = write("mask2.nii.gz", (data > 0).astype(np.uint8), affine)
    vol = NiftiVolume(path)
    plan = vol.plans("neurological")["axial"]
    rgb = render_slice(vol, plan, plan.n_slices // 2).to_array()
    assert {tuple(p) for p in rgb.reshape(-1, 3)} <= {(0, 0, 0), (255, 255, 255)}


def test_nan_in_label_slice_is_black():
    s = np.array([[1.0, np.nan], [0.0, 2.0]])
    rgb = labels.colorize(s, binary=False)
    assert tuple(rgb[0, 1]) == (0, 0, 0)
    assert tuple(rgb[0, 0]) == labels.label_color(1)


@pytest.mark.parametrize("count", [255, 300])
def test_too_many_labels(write, count):
    # 10x10x10 grid of 4-voxel cubes, cube k gets label (k % count) + 1
    i, j, k = np.meshgrid(*(np.arange(40) // 4,) * 3, indexing="ij")
    data = ((i * 100 + j * 10 + k) % count + 1).astype(np.int16)
    path = write(f"many{count}.nii.gz", data, synthetic.ras_affine(data.shape))
    vol = NiftiVolume(path)
    info = labels.detect_labels(vol.ensure_data(), strided_sample(vol.ensure_data()), True)
    assert info.is_labels == (count <= labels.MAX_LABELS)
