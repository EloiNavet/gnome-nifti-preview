import itertools

import numpy as np
import pytest

from nifti_preview_ref import orientation, synthetic
from nifti_preview_ref.orientation import build_plans, extract_2d, screen_to_voxel
from nifti_preview_ref.synthetic import MARKER_VALUES
from nifti_preview_ref.volume import NiftiVolume

STORAGE = ["RAS", "LPS", "LAS", "RPI", "LIA", "ASR", "PIL", "SLP"]
MODES = ["neurological", "radiological", "stored"]
# Which markers cross the centre slice of each view.
VISIBLE = {"axial": "RA", "coronal": "RS", "sagittal": "AS"}


def _edge_of(mask: np.ndarray) -> str:
    """Screen edge closest to the centroid of a boolean mask."""
    rows, cols = np.nonzero(mask)
    assert rows.size, "marker not visible"
    h, w = mask.shape
    dy = rows.mean() / (h - 1) - 0.5
    dx = cols.mean() / (w - 1) - 0.5
    if abs(dx) > abs(dy):
        return "right" if dx > 0 else "left"
    return "bottom" if dy > 0 else "top"


@pytest.mark.parametrize("codes,mode", list(itertools.product(STORAGE, MODES)))
def test_markers_appear_at_their_labelled_edge(marker, codes, mode):
    """Core safety property: whatever the storage order and display mode, a
    marker near the patient's right is drawn at the edge labelled R, etc."""
    data_ras, affine_ras = marker
    data, affine = synthetic.reorient(data_ras, affine_ras, codes)
    plans = build_plans(affine, data.shape, mode)
    for view, plan in plans.items():
        img = extract_2d(data, plan, plan.n_slices // 2)
        labels = plan.to_dict()["labels"]
        for letter in VISIBLE[view] if mode != "stored" else "RAS":
            mask = img == MARKER_VALUES[letter]
            if not mask.any():
                continue  # in stored mode a view may be any plane
            edge = _edge_of(mask)
            assert labels[edge] == letter, (codes, mode, view, letter, labels)


@pytest.mark.parametrize("codes", STORAGE)
def test_ras_layout_is_storage_independent(marker, codes):
    data_ras, affine_ras = marker
    ref = build_plans(affine_ras, data_ras.shape, "neurological")
    data, affine = synthetic.reorient(data_ras, affine_ras, codes)
    plans = build_plans(affine, data.shape, "neurological")
    for view in plans:
        idx = ref[view].n_slices // 3
        # The same anatomical slice has the mirrored index when the stored axis is reversed.
        idx_b = plans[view].n_slices - 1 - idx if plans[view].slice_up < 0 else idx
        a = extract_2d(data_ras, ref[view], idx)
        b = extract_2d(data, plans[view], idx_b)
        np.testing.assert_array_equal(a, b)


def test_neurological_and_radiological_labels(marker):
    data, affine = marker
    neuro = build_plans(affine, data.shape, "neurological")
    radio = build_plans(affine, data.shape, "radiological")
    expected = {
        "axial": ("L", "R", "A", "P"),
        "coronal": ("L", "R", "S", "I"),
        "sagittal": ("A", "P", "S", "I"),
    }
    for view, (left, right, top, bottom) in expected.items():
        p = neuro[view]
        assert (p.left, p.right, p.top, p.bottom) == (left, right, top, bottom)
    assert (radio["axial"].left, radio["axial"].right) == ("R", "L")
    assert (radio["coronal"].left, radio["coronal"].right) == ("R", "L")
    assert (radio["sagittal"].left, radio["sagittal"].right) == ("A", "P")


def test_radiological_is_mirror_of_neurological(marker):
    data, affine = marker
    neuro = build_plans(affine, data.shape, "neurological")
    radio = build_plans(affine, data.shape, "radiological")
    for view in ("axial", "coronal"):
        idx = neuro[view].n_slices // 2
        np.testing.assert_array_equal(extract_2d(data, neuro[view], idx)[:, ::-1], extract_2d(data, radio[view], idx))


def test_scroll_up_direction(marker):
    data, affine = marker
    for codes in STORAGE:
        d, a = synthetic.reorient(data, affine, codes)
        plans = build_plans(a, d.shape, "neurological")
        assert plans["axial"].slice_up_label == "S"
        assert plans["coronal"].slice_up_label == "A"
        assert plans["sagittal"].slice_up_label == "R"


def test_anisotropic_pixel_size():
    data = np.zeros((10, 20, 30), np.float32)
    affine = synthetic.ras_affine(data.shape, (0.5, 2.0, 3.0))
    plans = build_plans(affine, data.shape, "neurological")
    assert (plans["axial"].width, plans["axial"].height) == (10, 20)
    assert (plans["axial"].pixel_w, plans["axial"].pixel_h) == (0.5, 2.0)
    assert (plans["coronal"].pixel_w, plans["coronal"].pixel_h) == (0.5, 3.0)
    assert (plans["sagittal"].pixel_w, plans["sagittal"].pixel_h) == (2.0, 3.0)
    assert plans["sagittal"].n_slices == 10


def test_oblique_is_resampled_onto_world_axes(marker):
    data, affine = marker
    oblique = synthetic.rotate_affine(affine, 20.0)
    assert orientation.obliquity_degrees(oblique) == pytest.approx(20.0, abs=0.01)
    assert orientation.obliquity_degrees(affine) == pytest.approx(0.0, abs=1e-6)
    a = build_plans(affine, data.shape, "neurological")
    b = build_plans(oblique, data.shape, "neurological")
    for view in a:
        assert (a[view].left, a[view].right, a[view].top, a[view].bottom) == (
            b[view].left, b[view].right, b[view].top, b[view].bottom)
        assert not a[view].grid.resampled and b[view].grid.resampled
        assert b[view].title == view.capitalize()
    assert not build_plans(oblique, data.shape, "stored")["axial"].grid.resampled
    # a single tilted slice keeps its grid
    assert not build_plans(oblique, (40, 30, 1), "neurological")["axial"].grid.resampled


@pytest.mark.parametrize("seed", range(4))
def test_linear_function_survives_resampling(seed):
    rng = np.random.default_rng(seed)
    q, _ = np.linalg.qr(rng.normal(size=(3, 3)))
    affine = np.eye(4)
    affine[:3, :3] = (np.eye(3) + 0.3 * (q - np.eye(3))) @ np.diag(rng.uniform(0.7, 3.0, size=3))
    affine[:3, 3] = rng.uniform(-50, 50, size=3)
    shape = (14, 11, 9)
    coeffs = np.array([0.5, -0.25, 0.75])
    ijk = np.stack(np.meshgrid(*[np.arange(n) for n in shape], indexing="ij"), axis=-1).astype(float)
    data = (ijk @ affine[:3, :3].T + affine[:3, 3]) @ coeffs + 100.0
    plans = build_plans(affine, shape, "radiological")
    grid = plans["axial"].grid
    assert grid.resampled
    for plan in plans.values():
        index = plan.n_slices // 2
        out = orientation.resample_2d(lambda i, j, k: data[i, j, k].astype(np.float32), plan, index, nearest=False)
        g = orientation.grid_points(plan, index)
        v = np.stack(grid.to_voxel(g))
        n = np.array(shape, dtype=float)[:, None, None]
        within = np.all((v >= -0.5) & (v <= n - 0.5), axis=0)
        assert np.array_equal(np.isfinite(out), within), plan.view
        core = np.all((v >= 0) & (v <= n - 1), axis=0)
        world = [np.asarray(grid.affine)[r, r] * g[r] + np.asarray(grid.affine)[r, 3] for r in range(3)]
        want = world[0] * coeffs[0] + world[1] * coeffs[1] + world[2] * coeffs[2] + 100.0
        np.testing.assert_allclose(out[core], want[core], atol=2e-3)
        # nearest: only stored values
        near = orientation.resample_2d(lambda i, j, k: data[i, j, k].astype(np.float32), plan, index, nearest=True)
        assert np.isin(near[within], data.astype(np.float32)).all()


def test_cursor_round_trip_between_grids(marker):
    data, affine = marker
    oblique = synthetic.rotate_affine(affine, 25.0, axis=0)
    world_grid = build_plans(oblique, data.shape, "neurological")["axial"].grid
    voxel_grid = build_plans(oblique, data.shape, "stored")["axial"].grid
    for g in ([0, 0, 0], world_grid.center(), [n - 1 for n in world_grid.shape]):
        assert world_grid.from_world(world_grid.world_of(g)) == list(g)
    c = voxel_grid.center()
    w = voxel_grid.world_of(c)
    back = world_grid.world_of(world_grid.from_world(w))
    assert np.allclose(back, w, atol=0.5 * np.sqrt(3) * world_grid.affine[0][0] + 1e-9)


def test_unknown_orientation_falls_back_to_stored(marker):
    data, affine = marker
    plans = build_plans(affine, data.shape, "neurological", orientation_known=False)
    for plan in plans.values():
        assert {plan.left, plan.right, plan.top, plan.bottom} == {"?"}
        assert plan.h_flip is False and plan.v_flip is True


def test_degenerate_affine_is_unusable():
    affine = np.diag([1.0, 0.0, 1.0, 1.0])
    assert not orientation.orientation_is_usable(affine)
    plans = build_plans(affine, (4, 5, 6), "neurological")
    assert plans["axial"].title.startswith("Axis 2")


@pytest.mark.parametrize("codes,mode", list(itertools.product(["RAS", "LPS", "PIL"], MODES)))
def test_screen_to_voxel_inverts_extraction(marker, codes, mode):
    data_ras, affine_ras = marker
    rng = np.random.default_rng(0)
    data = rng.normal(size=data_ras.shape).astype(np.float32)
    data, affine = synthetic.reorient(data, affine_ras, codes)
    for plan in build_plans(affine, data.shape, mode).values():
        idx = plan.n_slices // 3
        img = extract_2d(data, plan, idx)
        assert img.shape == (plan.height, plan.width)
        for _ in range(20):
            col, row = int(rng.integers(plan.width)), int(rng.integers(plan.height))
            assert img[row, col] == data[tuple(screen_to_voxel(plan, col, row, idx))]


def test_extract_rejects_out_of_range(marker):
    data, affine = marker
    plan = build_plans(affine, data.shape, "neurological")["axial"]
    with pytest.raises(IndexError):
        extract_2d(data, plan, plan.n_slices)


def test_axis_codes_and_world(marker, write):
    data, affine = marker
    d, a = synthetic.reorient(data, affine, "LPS")
    assert orientation.axis_codes(a) == "LPS"
    path = write("lps.nii.gz", d, a)
    vol = NiftiVolume(path)
    assert vol.header_info()["orientation"] == "LPS"
    world = orientation.voxel_to_world(vol.affine, [0, 0, 0])
    np.testing.assert_allclose(world, a[:3, 3])
