"""Display geometry: how voxel axes map onto the screen for each view.

A view is described by a ``ViewPlan``: which grid axis is sliced, which grid
axes run horizontally and vertically on screen, and whether each of them is
reversed, chosen with ``nibabel.io_orientation`` (nearest anatomical axis).
The grid is the stored voxel grid, except for oblique images in the
anatomical modes: they are resampled onto a grid aligned with the world axes
(``Grid.world``). Edge labels are always recomputed from the grid's affine, so
whatever the mode, the letters on screen tell the truth about the data.

The resampling arithmetic is written operation by operation in the same order
as in the Rust backend, so that both give the same pixels.

Conventions (see docs/DISPLAY_CONVENTIONS.md):

* screen x grows to the right, screen y grows downwards;
* neurological: patient right on screen right (axial, coronal);
* radiological: patient right on screen left (axial, coronal);
* axial: anterior at the top; coronal and sagittal: superior at the top;
* sagittal: anterior on the left (both RAS modes);
* stored: raw voxel axes, first axis to the right, last displayed axis upwards.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

import nibabel as nib
import numpy as np

VIEWS = ("coronal", "sagittal", "axial")
MODES = ("neurological", "radiological", "stored")

# In the anatomical modes, images more oblique than this are resampled.
RESAMPLE_ABOVE_DEGREES = 1.0
# Largest slice drawn: 16 megapixels (4096 x 4096), no side above 16384.
MAX_SLICE_PIXELS = 16 * 1024**2
MAX_SLICE_SIDE = 16384
# Resampled slices may always have this many pixels (see Grid.world).
RESAMPLED_SLICE_PIXELS = 1 << 20

# World (RAS+) axis -> (letter at the negative end, letter at the positive end)
_LETTERS = (("L", "R"), ("P", "A"), ("I", "S"))
_OPPOSITE = {"L": "R", "R": "L", "P": "A", "A": "P", "I": "S", "S": "I", "?": "?"}

# RAS modes: view -> (sliced world axis, (h world axis, h sign), (v world axis, v sign)).
# A sign of +1 means that moving right (h) or down (v) on screen moves towards the
# positive end of that world axis.
_RAS_LAYOUT = {
    "axial": (2, (0, +1), (1, -1)),
    "coronal": (1, (0, +1), (2, -1)),
    "sagittal": (0, (1, -1), (2, -1)),
}

# Stored mode: view -> (sliced voxel axis, h voxel axis, v voxel axis).
# Horizontal index grows to the right, vertical index grows upwards.
_STORED_LAYOUT = {
    "axial": (2, 0, 1),
    "coronal": (1, 0, 2),
    "sagittal": (0, 1, 2),
}


def _norm(v) -> float:
    return math.sqrt(v[0] * v[0] + v[1] * v[1] + v[2] * v[2])


def _det3(m) -> float:
    return (m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]))


def _inv3(m):
    """Inverse of a 3x3 matrix; None when (nearly) singular relative to its column sizes."""
    d = _det3(m)
    scale = _norm([m[0][0], m[1][0], m[2][0]]) * _norm([m[0][1], m[1][1], m[2][1]]) * _norm([m[0][2], m[1][2], m[2][2]])
    if not (math.isfinite(d) and abs(d) > 1e-9 * scale):
        return None
    r = [[0.0] * 3 for _ in range(3)]
    for i in range(3):
        for j in range(3):
            i1, i2 = (i + 1) % 3, (i + 2) % 3
            j1, j2 = (j + 1) % 3, (j + 2) % 3
            r[j][i] = (m[i1][j1] * m[i2][j2] - m[i1][j2] * m[i2][j1]) / d
    return r


@dataclass(frozen=True)
class Grid:
    """The grid the views are cut from: the stored voxel grid, or for an
    oblique image in an anatomical mode, a grid aligned with the world (RAS)
    axes that covers the whole image."""

    shape: tuple
    affine: tuple  # 4x4, grid index -> world
    resampled: bool
    steps: tuple = (1, 1, 1)  # grid slices per scroll step along each axis
    voxel_shape: tuple = ()
    inv: tuple = ()  # inverse of the image's 3x3 matrix (resampled grids)
    t: tuple = ()  # the image's translation (resampled grids)

    @staticmethod
    def voxel(affine, shape3) -> "Grid":
        a = tuple(tuple(float(x) for x in row) for row in np.asarray(affine, dtype=float))
        shape3 = tuple(int(n) for n in shape3)
        return Grid(shape3, a, False, (1, 1, 1), shape3)

    @staticmethod
    def world(affine, shape3) -> "Grid | None":
        """A grid aligned with the world axes covering the image (the centres
        of its corner voxels), as fine as its smallest voxel size. Scrolling
        moves by the size of the voxel axis closest to the slice normal. None
        for images that are flat (a dimension of 1) or degenerate. Slices have
        at most 16 times the pixels of the largest stored slice (and at least
        1 Mpixel)."""
        shape3 = tuple(int(n) for n in shape3)
        if any(n < 2 for n in shape3):
            return None
        w2v = _ras_axes(affine)
        if w2v is None:
            return None
        a = [[float(x) for x in row] for row in np.asarray(affine, dtype=float)]
        inv = _inv3([row[:3] for row in a[:3]])
        if inv is None:
            return None
        sizes = [_norm([a[0][c], a[1][c], a[2][c]]) for c in range(3)]
        spacing = min(min(sizes[0], sizes[1]), sizes[2])
        lo, hi = [0.0] * 3, [0.0] * 3
        for r in range(3):
            lo[r] = hi[r] = a[r][3]
            for c in range(3):
                e = a[r][c] * float(shape3[c] - 1)
                lo[r] += min(e, 0.0)
                hi[r] += max(e, 0.0)
        # Coarser if needed to keep slices within the slice limits.
        stored = sorted(shape3)
        budget = min(MAX_SLICE_PIXELS, max(16 * stored[2] * stored[1], RESAMPLED_SLICE_PIXELS))
        while True:
            shape = []
            for r in range(3):
                n = (hi[r] - lo[r]) / spacing + 1e-6
                if not (n >= 0.0 and n < 1e9):
                    return None
                shape.append(math.floor(n) + 1)
            b, big = sorted(shape)[1:]
            if big <= MAX_SLICE_SIDE and big * b <= budget:
                break
            f = max(big / MAX_SLICE_SIDE, math.sqrt(float(big) * float(b) / budget))
            spacing *= f * 1.001
        g = [[0.0] * 4 for _ in range(4)]
        steps = []
        for r in range(3):
            g[r][r] = spacing
            g[r][3] = lo[r]
            steps.append(max(1, math.floor(sizes[w2v[r][0]] / spacing + 0.5)))
        g[3][3] = 1.0
        return Grid(tuple(shape), tuple(tuple(row) for row in g), True, tuple(steps), shape3,
                    tuple(tuple(row) for row in inv), (a[0][3], a[1][3], a[2][3]))

    def center(self) -> list[int]:
        return [n // 2 for n in self.shape]

    def to_voxel(self, g):
        """Continuous voxel coordinates of grid point(s) g (floats or arrays)."""
        if not self.resampled:
            return list(g)
        a, m, t = self.affine, self.inv, self.t
        d = [(a[0][3] + a[0][0] * g[0]) - t[0], (a[1][3] + a[1][1] * g[1]) - t[1], (a[2][3] + a[2][2] * g[2]) - t[2]]
        return [m[c][0] * d[0] + m[c][1] * d[1] + m[c][2] * d[2] for c in range(3)]

    def nearest(self, v) -> list[int] | None:
        """Voxel nearest to voxel coordinates v, None outside the image."""
        out = []
        for x, n in zip(v, self.voxel_shape):
            if not (x >= -0.5 and x <= n - 0.5):
                return None
            out.append(math.floor(min(max(x, 0.0), float(n - 1)) + 0.5))
        return out

    def world_of(self, g) -> list[float]:
        a = self.affine
        return [a[r][0] * g[0] + a[r][1] * g[1] + a[r][2] * g[2] + a[r][3] for r in range(3)]

    def from_world(self, w) -> list[int]:
        """Grid point closest to world point w, clamped to the grid; the centre
        when the grid's matrix cannot be inverted."""
        a = self.affine
        inv = _inv3([row[:3] for row in a[:3]])
        if inv is None:
            return self.center()
        d = [w[r] - a[r][3] for r in range(3)]
        out = []
        for c in range(3):
            y = inv[c][0] * d[0] + inv[c][1] * d[1] + inv[c][2] * d[2] + 0.5
            n = self.shape[c]
            out.append(0 if not y >= 0.0 else (n - 1 if y >= n - 1 else math.floor(y)))
        return out

    def to_dict(self) -> dict:
        # null where not finite (JSON has no NaN), as in the backend
        affine = [[x if math.isfinite(x) else None for x in row] for row in self.affine]
        return {"shape": list(self.shape), "affine": affine, "resampled": self.resampled}


@dataclass(frozen=True)
class ViewPlan:
    view: str
    slice_axis: int
    h_axis: int
    h_flip: bool
    v_axis: int
    v_flip: bool
    width: int
    height: int
    n_slices: int
    pixel_w: float
    pixel_h: float
    left: str
    right: str
    top: str
    bottom: str
    # Voxel index step along slice_axis for "scroll up", and the anatomical
    # direction it moves towards.
    slice_up: int
    slice_up_label: str
    title: str
    slice_step: int = 1  # slices moved by one scroll step
    grid: Grid | None = None  # the grid the view is cut from (axes and sizes refer to it)

    def to_dict(self) -> dict:
        return {
            "view": self.view,
            "slice_axis": self.slice_axis,
            "h_axis": self.h_axis,
            "h_flip": self.h_flip,
            "v_axis": self.v_axis,
            "v_flip": self.v_flip,
            "width": self.width,
            "height": self.height,
            "n_slices": self.n_slices,
            "pixel_w": self.pixel_w,
            "pixel_h": self.pixel_h,
            "labels": {"left": self.left, "right": self.right, "top": self.top, "bottom": self.bottom},
            "slice_up": self.slice_up,
            "slice_up_label": self.slice_up_label,
            "slice_step": self.slice_step,
            "title": self.title,
        }


def direction_letter(vector) -> str:
    """Anatomical letter of the dominant component of a world-space vector."""
    v = np.asarray(vector, dtype=float)
    if v.shape != (3,) or not np.all(np.isfinite(v)) or not np.any(v):
        return "?"
    axis = int(np.argmax(np.abs(v)))
    return _LETTERS[axis][1 if v[axis] > 0 else 0]


def axis_codes(affine) -> str:
    """Three-letter storage orientation code such as ``RAS`` or ``LPS``."""
    return "".join(direction_letter(np.asarray(affine, dtype=float)[:3, i]) for i in range(3))


def obliquity_degrees(affine) -> float:
    """Largest angle (degrees) between a voxel axis and its nearest world axis."""
    try:
        angles = nib.affines.obliquity(np.asarray(affine, dtype=float))
    except Exception:
        return float("nan")
    angles = np.asarray(angles, dtype=float)
    if not np.all(np.isfinite(angles)):
        return float("nan")
    return float(np.degrees(np.max(np.abs(angles))))


def voxel_to_world(affine, ijk) -> list[float | None]:
    """World coordinates; None where not finite (JSON null, as in the backend)."""
    a = np.asarray(affine, dtype=float)
    with np.errstate(invalid="ignore", over="ignore"):
        w = a[:3, :3] @ np.asarray(ijk, dtype=float) + a[:3, 3]
    return [float(x) if np.isfinite(x) else None for x in w]


def _ras_axes(affine):
    """For each world axis, the voxel axis that best matches it and its sign.

    Returns None when the affine is degenerate (a zero or duplicated column).
    """
    try:
        ornt = nib.io_orientation(np.asarray(affine, dtype=float))
    except Exception:
        return None
    if not np.all(np.isfinite(ornt)):
        return None
    world_to_voxel = {}
    for voxel_axis, (world_axis, sign) in enumerate(ornt):
        world_to_voxel[int(world_axis)] = (voxel_axis, int(sign))
    if sorted(world_to_voxel) != [0, 1, 2]:
        return None
    return world_to_voxel


def orientation_is_usable(affine) -> bool:
    affine = np.asarray(affine, dtype=float)
    return bool(np.all(np.isfinite(affine[:3]))) and _ras_axes(affine) is not None


def build_plans(affine, shape3, mode: str, orientation_known: bool = True) -> dict[str, ViewPlan]:
    """Compute the three view plans for a 3D shape and an affine.

    ``orientation_known`` is False when neither qform nor sform is set: the
    layout then falls back to "stored" and every edge label is "?". Oblique
    images are resampled onto world axes in the anatomical modes.
    """
    if mode not in MODES:
        raise ValueError(f"unknown display mode {mode!r}")
    affine = np.asarray(affine, dtype=float)
    shape3 = tuple(int(n) for n in shape3)
    if (orientation_known and mode != "stored" and orientation_is_usable(affine)
            and obliquity_degrees(affine) > RESAMPLE_ABOVE_DEGREES):
        grid = Grid.world(affine, shape3)
        if grid is not None:
            return _plans_on(grid, mode, True)
    return _plans_on(Grid.voxel(affine, shape3), mode, orientation_known)


def _plans_on(grid: Grid, mode: str, orientation_known: bool) -> dict[str, ViewPlan]:
    affine = np.asarray(grid.affine, dtype=float)
    shape3 = grid.shape
    world_to_voxel = _ras_axes(affine) if orientation_known else None
    if world_to_voxel is None:
        mode = "stored"
    columns = [affine[:3, i] for i in range(3)]
    sizes = [float(n) if np.isfinite(n) and n > 0 else 1.0 for n in (np.linalg.norm(c) for c in columns)]

    plans = {}
    for view in VIEWS:
        if mode == "stored":
            slice_axis, h_axis, v_axis = _STORED_LAYOUT[view]
            h_flip, v_flip, slice_up = False, True, 1
        else:
            slice_world, (h_world, h_sign), (v_world, v_sign) = _RAS_LAYOUT[view]
            if mode == "radiological" and view in ("axial", "coronal"):
                h_sign = -h_sign
            slice_axis, slice_sign = world_to_voxel[slice_world]
            h_axis, h_vox_sign = world_to_voxel[h_world]
            v_axis, v_vox_sign = world_to_voxel[v_world]
            h_flip = h_sign * h_vox_sign < 0
            v_flip = v_sign * v_vox_sign < 0
            slice_up = slice_sign

        if world_to_voxel is None:
            left = right = top = bottom = up_label = "?"
        else:
            right = direction_letter(columns[h_axis] * (-1 if h_flip else 1))
            bottom = direction_letter(columns[v_axis] * (-1 if v_flip else 1))
            left, top = _OPPOSITE[right], _OPPOSITE[bottom]
            up_label = direction_letter(columns[slice_axis] * slice_up)

        if mode == "stored":
            nearest = _nearest_view_name(columns[slice_axis]) if world_to_voxel is not None else None
            title = f"Axis {slice_axis}" + (f" (≈ {nearest})" if nearest else "")
        else:
            title = view.capitalize()

        plans[view] = ViewPlan(
            view=view,
            slice_axis=slice_axis,
            h_axis=h_axis,
            h_flip=bool(h_flip),
            v_axis=v_axis,
            v_flip=bool(v_flip),
            width=shape3[h_axis],
            height=shape3[v_axis],
            n_slices=shape3[slice_axis],
            pixel_w=sizes[h_axis],
            pixel_h=sizes[v_axis],
            left=left,
            right=right,
            top=top,
            bottom=bottom,
            slice_up=int(slice_up),
            slice_up_label=up_label,
            title=title,
            slice_step=int(grid.steps[slice_axis]),
            grid=grid,
        )
    return plans


def _nearest_view_name(normal) -> str | None:
    letter = direction_letter(normal)
    return {"R": "sagittal", "L": "sagittal", "A": "coronal", "P": "coronal", "S": "axial", "I": "axial"}.get(letter)


def extract_2d(data3, plan: ViewPlan, index: int) -> np.ndarray:
    """Return the slice of a voxel grid as a 2D array with rows = screen y and
    columns = screen x."""
    if plan.grid is not None and plan.grid.resampled:
        raise ValueError("resampled grid: use resample_2d")
    if not 0 <= index < plan.n_slices:
        raise IndexError(f"slice index {index} out of range 0..{plan.n_slices - 1}")
    key = [slice(None)] * 3
    key[plan.slice_axis] = index
    sl = data3[tuple(key)]
    remaining = [a for a in range(3) if a != plan.slice_axis]
    # sl axes are `remaining` in increasing order; we want (v, h).
    if remaining.index(plan.v_axis) != 0:
        sl = sl.T
    if plan.v_flip:
        sl = sl[::-1, :]
    if plan.h_flip:
        sl = sl[:, ::-1]
    return np.asarray(sl)


def screen_to_voxel(plan: ViewPlan, col: int, row: int, index: int) -> list[int]:
    """Voxel coordinates under image pixel (col, row) of a view at a given slice."""
    ijk = [0, 0, 0]
    ijk[plan.slice_axis] = index
    ijk[plan.h_axis] = plan.width - 1 - col if plan.h_flip else col
    ijk[plan.v_axis] = plan.height - 1 - row if plan.v_flip else row
    return ijk


# ------------------------------------------------------------- resampling

def grid_points(plan: ViewPlan, index: int) -> list[np.ndarray]:
    """Grid coordinates (three float arrays, rows = screen y) of the pixels of a slice."""
    h, w = plan.height, plan.width
    rows, cols = np.arange(h), np.arange(w)
    gv = (h - 1 - rows) if plan.v_flip else rows
    gh = (w - 1 - cols) if plan.h_flip else cols
    g = [None, None, None]
    g[plan.slice_axis] = np.full((h, w), float(index))
    g[plan.v_axis] = np.broadcast_to(gv[:, None].astype(float), (h, w))
    g[plan.h_axis] = np.broadcast_to(gh[None, :].astype(float), (h, w))
    return g


def _lerp(a, b, t):
    with np.errstate(invalid="ignore", over="ignore"):
        return np.where(t == 0, a, a + (b - a) * t)


def _sample_points(plan: ViewPlan, index: int):
    """Positions of the pixels in the voxel grid: (inside mask, clamped coordinates)."""
    grid = plan.grid
    v = grid.to_voxel(grid_points(plan, index))
    inside = np.ones((plan.height, plan.width), dtype=bool)
    xs = []
    for c in range(3):
        n = grid.voxel_shape[c]
        ok = (v[c] >= -0.5) & (v[c] <= n - 0.5)
        inside &= ok
        xs.append(np.clip(np.where(ok, v[c], 0.0), 0.0, float(n - 1)))
    return inside, xs


def _trilinear(xs, shape, at):
    i0, i1, f = [], [], []
    for x, n in zip(xs, shape):
        b = np.floor(x)
        i0.append(b.astype(np.intp))
        i1.append(np.minimum(i0[-1] + 1, n - 1))
        f.append(x - b)

    def plane(z):
        x0 = _lerp(at(i0[0], i0[1], z), at(i1[0], i0[1], z), f[0])
        return _lerp(x0, _lerp(at(i0[0], i1[1], z), at(i1[0], i1[1], z), f[0]), f[1])

    return _lerp(plane(i0[2]), plane(i1[2]), f[2])


def resample_2d(values, plan: ViewPlan, index: int, nearest: bool, dtype=np.float32) -> np.ndarray:
    """Slice of a resampled grid (rows = screen y): trilinear, or the nearest
    voxel; NaN outside the image. ``values(i, j, k)`` returns the (scaled,
    float32) voxel values at integer index arrays; label slices use float64 or
    object arrays (``dtype``) to keep all native integer bits exact."""
    if not 0 <= index < plan.n_slices:
        raise IndexError(f"slice index {index} out of range 0..{plan.n_slices - 1}")
    inside, xs = _sample_points(plan, index)
    if nearest:
        out = np.asarray(values(*[np.floor(x + 0.5).astype(np.intp) for x in xs]), dtype=dtype)
    else:
        def at(i, j, k):
            return np.asarray(values(i, j, k), dtype=np.float32).astype(np.float64)
        out = _trilinear(xs, plan.grid.voxel_shape, at).astype(np.float32)
    out = np.array(out, dtype=dtype if nearest else np.float32)
    out[~inside] = np.nan
    return out


def resample_rgb(rgb, plan: ViewPlan, index: int) -> np.ndarray:
    """RGB8 slice (height, width, 3) of an RGB image on a resampled grid,
    interpolated per channel, black outside the image."""
    if not 0 <= index < plan.n_slices:
        raise IndexError(f"slice index {index} out of range 0..{plan.n_slices - 1}")
    inside, xs = _sample_points(plan, index)
    out = np.zeros((plan.height, plan.width, 3), dtype=np.uint8)
    for ch in range(3):
        c = _trilinear(xs, plan.grid.voxel_shape, lambda i, j, k: rgb[i, j, k, ch].astype(np.float64))
        out[..., ch] = np.clip(np.floor(c + 0.5), 0.0, 255.0).astype(np.uint8)
    out[~inside] = 0
    return out
