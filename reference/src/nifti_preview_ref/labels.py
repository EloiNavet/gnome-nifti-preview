"""Segmentation (label map) detection and categorical colouring.

Detection is deliberately conservative: identity scaling, integral values, a
bounded number of distinct labels and, as the real discriminator,
piecewise-constant content (neighbouring foreground voxels are usually equal in
a label map and usually different in an intensity image).
"""

from __future__ import annotations

import colorsys
from dataclasses import dataclass

import numpy as np

MAX_LABELS = 256
PIECEWISE_MIN_PAIRS = 256
PIECEWISE_MAX_DIFF_RATIO = 0.30
NIFTI_INTENT_LABEL = 1002
_GOLDEN = 0.618033988749895


@dataclass(frozen=True)
class LabelInfo:
    is_labels: bool
    binary: bool
    n_labels: int  # distinct non-zero labels seen in the sample
    reason: str

    def to_dict(self) -> dict:
        return {"is_labels": self.is_labels, "binary": self.binary, "n_labels": self.n_labels, "reason": self.reason}


NOT_LABELS = LabelInfo(False, False, 0, "intensity")


def _integral(values: np.ndarray) -> bool:
    if values.dtype.kind in "iub":
        return True
    return bool(np.all(np.abs(values - np.rint(values)) <= 1e-3))


def _diff_pairs(slice2d: np.ndarray) -> tuple[int, int]:
    a, b = slice2d[:, :-1], slice2d[:, 1:]
    both = (a != 0) & (b != 0) & np.isfinite(a) & np.isfinite(b)
    total = int(np.count_nonzero(both))
    diff = int(np.count_nonzero(both & (a != b)))
    return total, diff


def center_slices(data3) -> list[np.ndarray]:
    nx, ny, nz = data3.shape
    return [
        np.asarray(data3[nx // 2, :, :]),
        np.asarray(data3[:, ny // 2, :]),
        np.asarray(data3[:, :, nz // 2]),
    ]


def detect_labels(data3, sample: np.ndarray, identity_scaling: bool, intent_code: int = 0) -> LabelInfo:
    """Decide whether a 3D volume is a label map.

    ``data3`` holds the stored values (identity scaling) and ``sample`` is a
    deterministic strided sample of it used for whole-volume checks.
    """
    forced = intent_code == NIFTI_INTENT_LABEL
    if not identity_scaling and not forced:
        return NOT_LABELS
    if data3.dtype.kind not in "iuf":
        return NOT_LABELS

    slices = center_slices(data3)
    # Cheap checks on the three centre slices first: most intensity images stop here.
    labels = None
    for values in ([s[np.isfinite(s)].ravel() for s in slices], [np.asarray(sample)]):
        pooled = np.concatenate(values + ([labels] if labels is not None else []))
        pooled = pooled[np.isfinite(pooled)]
        if pooled.size == 0:
            continue
        if not _integral(pooled) or pooled.min() < 0:
            return NOT_LABELS
        labels = np.unique(pooled)
        labels = labels[labels != 0]
        if labels.size > MAX_LABELS:
            return NOT_LABELS
    if labels is None or labels.size == 0:
        return NOT_LABELS

    total = diff = 0
    for s in slices:
        t, d = _diff_pairs(s)
        total += t
        diff += d
    if total >= PIECEWISE_MIN_PAIRS and diff / total > PIECEWISE_MAX_DIFF_RATIO and not forced:
        return NOT_LABELS
    reason = "intent" if forced else ("piecewise-constant" if total >= PIECEWISE_MIN_PAIRS else "sparse-integral")
    return LabelInfo(True, labels.size == 1, int(labels.size), reason)


def label_color(label: int) -> tuple[int, int, int]:
    """Deterministic, well-spread colour for a label value (0 is never coloured)."""
    label = int(label)
    if abs(label) <= 2**24:
        hue = (label * _GOLDEN) % 1.0
    else:
        mask = 2**64 - 1
        x = ((label & mask) ^ ((label >> 64) & mask))
        x = (x + 0x9e3779b97f4a7c15) & mask
        x = ((x ^ (x >> 30)) * 0xbf58476d1ce4e5b9) & mask
        x = ((x ^ (x >> 27)) * 0x94d049bb133111eb) & mask
        hue = ((x ^ (x >> 31)) >> 11) / 2**53
    saturation = (0.55, 0.75, 0.95)[label % 3]
    value = 1.0 if (label // 3) % 2 == 0 else 0.8
    r, g, b = colorsys.hsv_to_rgb(hue, saturation, value)
    return int(round(r * 255)), int(round(g * 255)), int(round(b * 255))


def colorize(slice2d, binary: bool) -> np.ndarray:
    """RGB uint8 image of a label slice. Background and non-finite voxels are black."""
    s = np.asarray(slice2d)
    if s.dtype.kind in "iu":
        values = s
    else:
        # Object arrays preserve native integers on resampled label grids;
        # float labels round to Python integers without an int64 saturation.
        values = np.array([int(round(v)) if np.isfinite(v) else 0 for v in s.flat], dtype=object).reshape(s.shape)
    rgb = np.zeros(s.shape + (3,), dtype=np.uint8)
    if binary:
        rgb[values != 0] = 255
        return rgb
    uniques, inverse = np.unique(values, return_inverse=True)
    lut = np.array([(0, 0, 0) if u == 0 else label_color(u) for u in uniques], dtype=np.uint8)
    return lut[inverse.reshape(values.shape)]
