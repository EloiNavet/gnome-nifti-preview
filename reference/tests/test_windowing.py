import numpy as np

from nifti_preview_ref.windowing import Window, apply_window, compute_window, strided_sample


def test_percentiles_over_foreground():
    rng = np.random.default_rng(0)
    values = np.concatenate([np.zeros(80_000), rng.uniform(100, 200, 20_000)])
    w = compute_window(values)
    assert w.status == "ok"
    assert 100 <= w.low < 102 and 198 < w.high <= 200


def test_small_foreground_falls_back_to_all_values():
    values = np.concatenate([np.zeros(100_000), np.full(10, 500.0)])
    w = compute_window(values)
    # foreground is too small to be trusted; the window spans the full range
    assert w.status == "ok" and w.low == 0.0 and w.high == 500.0


def test_tiny_units_use_non_zero_values():
    rng = np.random.default_rng(1)
    values = np.concatenate([np.zeros(50_000), rng.uniform(1e-9, 3e-9, 50_000)])
    w = compute_window(values)
    assert w.status == "ok"
    assert 0.9e-9 < w.low < 1.1e-9


def test_nan_and_inf_are_ignored_and_black():
    values = np.array([np.nan, np.inf, -np.inf] + list(np.linspace(0, 100, 1000)))
    w = compute_window(values)
    assert np.isfinite(w.low) and np.isfinite(w.high)
    out = apply_window(values, w)
    assert out.dtype == np.uint8
    assert list(out[:3]) == [0, 0, 0]
    assert out[-1] == 255


def test_outlier_does_not_dominate():
    values = np.concatenate([np.linspace(0, 100, 100_000), [1e30]])
    w = compute_window(values)
    assert w.high < 200


def test_all_nan_volume():
    w = compute_window(np.full(1000, np.nan))
    assert w.status == "no_finite"
    assert not apply_window(np.full((4, 4), np.nan), w).any()


def test_constant_and_empty_volumes():
    w = compute_window(np.full(1000, 7.0))
    assert w == Window(7.0, 7.0, "constant")
    out = apply_window(np.full((3, 3), 7.0), w)
    assert (out == 128).all()
    w0 = compute_window(np.zeros(1000))
    assert w0.status == "constant"
    assert not apply_window(np.zeros((3, 3)), w0).any()


def test_apply_window_clips_and_scales():
    w = Window(10.0, 20.0, "ok")
    out = apply_window(np.array([0, 10, 15, 20, 30], np.float32), w)
    assert list(out) == [0, 0, 128, 255, 255]


def test_strided_sample_is_bounded_and_deterministic():
    data = np.arange(10_000_000, dtype=np.int16).reshape(100, 100, 1000, order="F")
    a = strided_sample(data, max_samples=1000)
    b = strided_sample(data, max_samples=1000)
    assert a.size <= 1000
    np.testing.assert_array_equal(a, b)


def test_extreme_finite_and_subnormal_ranges_keep_contrast():
    for low, high in [(-3e38, 3e38), (0, np.nextafter(np.float32(0), np.float32(1)))]:
        values = np.array([low, high, np.nan, np.inf], np.float32)
        window = Window(float(values[0]), float(values[1]), "ok")
        assert apply_window(values, window).tolist() == [0, 255, 0, 0]
