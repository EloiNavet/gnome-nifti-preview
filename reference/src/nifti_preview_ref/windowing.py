"""Percentile intensity windowing for grayscale display.

The window is computed over clearly non-zero voxels when they are a
substantial part of the image, so that a large zero background does not pull
the lower percentile to zero.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

DEFAULT_LOW_PERCENTILE = 0.5
DEFAULT_HIGH_PERCENTILE = 99.5
MAX_SAMPLES = 1_000_000
NON_ZERO_FLOOR = 1e-6


@dataclass(frozen=True)
class Window:
    low: float
    high: float
    status: str  # "ok", "constant" or "no_finite"

    def to_dict(self) -> dict:
        return {"low": self.low, "high": self.high, "status": self.status}


def strided_sample(array, max_samples: int = MAX_SAMPLES) -> np.ndarray:
    """Deterministic, evenly strided 1D sample of an array (works on memmaps)."""
    flat = np.asarray(array).reshape(-1, order="F")  # file order for NIfTI arrays
    step = max(1, -(-flat.size // max_samples))
    return np.asarray(flat[::step])


def compute_window(values, low_pct: float = DEFAULT_LOW_PERCENTILE, high_pct: float = DEFAULT_HIGH_PERCENTILE) -> Window:
    """Window bounds from a 1D sample of (already scaled) intensities."""
    values = np.asarray(values, dtype=np.float64).ravel()
    finite = values[np.isfinite(values)]
    if finite.size == 0:
        return Window(0.0, 1.0, "no_finite")

    minimum_subset = max(64, finite.size // 20)
    foreground = finite[np.abs(finite) > NON_ZERO_FLOOR]
    if foreground.size >= minimum_subset:
        source = foreground
    else:
        non_zero = finite[finite != 0]
        source = non_zero if non_zero.size >= minimum_subset else finite

    low, high = (float(x) for x in np.percentile(source, [low_pct, high_pct]))
    if not low < high:
        low, high = float(finite.min()), float(finite.max())
    if not low < high:
        return Window(low, low, "constant")
    return Window(low, high, "ok")


def apply_window(values, window: Window) -> np.ndarray:
    """Map intensities to uint8. Non-finite voxels are black.

    A constant volume renders its (finite) voxels mid-grey, or black if the
    constant is zero, so that "uniform" and "empty" remain distinguishable.
    """
    v = np.asarray(values, dtype=np.float32)
    finite = np.isfinite(v)
    if window.status == "constant":
        out = np.zeros(v.shape, dtype=np.uint8)
        if window.low != 0:
            out[finite] = 128
        return out
    if window.status == "no_finite":
        return np.zeros(v.shape, dtype=np.uint8)
    with np.errstate(invalid="ignore", over="ignore"):
        span = np.float32(window.high - window.low)
        if np.isfinite(span) and span > 0:
            unit = (v - np.float32(window.low)) / span
        else:
            unit = (v.astype(np.float64) - window.low) / (window.high - window.low)
        unit = np.clip(unit, 0.0, 1.0)
        out = np.rint(unit * 255.0)
    out[~finite] = 0
    return out.astype(np.uint8)
