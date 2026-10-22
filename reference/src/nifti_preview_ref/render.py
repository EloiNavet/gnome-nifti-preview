"""Slice rendering: 2D extraction + grayscale windowing, label colouring, or
the colours of an RGB image."""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from . import labels as labels_mod
from .orientation import ViewPlan
from .volume import NiftiVolume
from .windowing import apply_window

COLORMAPS = ("auto", "gray", "labels", "rgb")


@dataclass(frozen=True)
class RenderedSlice:
    view: str
    index: int
    width: int
    height: int
    colormap: str  # effective colormap: "gray", "labels" or "rgb"
    rgb: bytes  # height * width * 3 bytes, row-major, top row first

    def to_array(self) -> np.ndarray:
        return np.frombuffer(self.rgb, dtype=np.uint8).reshape(self.height, self.width, 3)


def effective_colormap(volume: NiftiVolume, colormap: str) -> str:
    """"auto" is "rgb" for RGB images, "labels" for label maps, "gray"
    otherwise; "rgb" is "gray" for an image that is not RGB."""
    if colormap not in COLORMAPS:
        raise ValueError(f"unknown colormap {colormap!r}")
    if colormap == "auto":
        if volume.is_rgb:
            return "rgb"
        return "labels" if volume.stats().labels.is_labels else "gray"
    if colormap == "rgb" and not volume.is_rgb:
        return "gray"
    return colormap


def colormap_choices(volume: NiftiVolume) -> list[str]:
    """The colormaps worth offering for this image, its default first."""
    if volume.is_rgb:
        return ["rgb", "gray"]
    return ["labels", "gray"] if volume.stats().labels.is_labels else ["gray"]


def render_slice(volume: NiftiVolume, plan: ViewPlan, index: int, colormap: str = "auto") -> RenderedSlice:
    volume.ensure_data()
    stats = volume.stats()
    cmap = effective_colormap(volume, colormap)
    if cmap == "rgb":
        rgb = volume.slice_rgb(plan, index)
    elif cmap == "labels":
        rgb = labels_mod.colorize(volume.slice_labels(plan, index), stats.labels.binary)
    else:
        gray = apply_window(volume.slice_values(plan, index), stats.window)
        rgb = np.repeat(gray[:, :, None], 3, axis=2)
    rgb = np.ascontiguousarray(rgb, dtype=np.uint8)
    return RenderedSlice(plan.view, index, rgb.shape[1], rgb.shape[0], cmap, rgb.tobytes())
