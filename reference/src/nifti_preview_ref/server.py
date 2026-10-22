"""Line-oriented JSON protocol spoken with the Sushi plugin over stdin/stdout.

One backend process serves one preview. It exits when stdin reaches EOF (the
preview closed or Sushi died) or on an explicit ``close`` request. See
docs/PROTOCOL.md for the message reference.
"""

from __future__ import annotations

import base64
import json
import math
import os
import sys
import traceback
from collections import OrderedDict

from . import PROTOCOL_VERSION, __version__
from .errors import PreviewError
from .orientation import MODES, VIEWS, voxel_to_world
from .render import COLORMAPS, colormap_choices, effective_colormap, render_slice
from .volume import wire_value, NiftiVolume

MAX_REQUEST_BYTES = 1 << 20
DEFAULT_CACHE_BYTES = 64 * 1024**2


def _clean(obj):
    """Make an object strict-JSON serialisable (NaN/Inf become null)."""
    if isinstance(obj, float):
        return obj if math.isfinite(obj) else None
    if isinstance(obj, dict):
        return {k: _clean(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_clean(v) for v in obj]
    return obj


def encode(obj) -> bytes:
    return json.dumps(_clean(obj), separators=(",", ":"), allow_nan=False, ensure_ascii=False).encode("utf-8") + b"\n"


class SliceCache:
    """LRU cache of encoded slices, bounded by total payload size."""

    def __init__(self, max_bytes: int = DEFAULT_CACHE_BYTES):
        self.max_bytes = max_bytes
        self.size = 0
        self._items: OrderedDict = OrderedDict()

    def get(self, key):
        value = self._items.get(key)
        if value is not None:
            self._items.move_to_end(key)
        return value

    def put(self, key, value: dict):
        cost = len(value["data"])
        if cost > self.max_bytes:
            return
        old = self._items.pop(key, None)
        if old is not None:
            self.size -= len(old["data"])
        self._items[key] = value
        self.size += cost
        while self.size > self.max_bytes:
            _, evicted = self._items.popitem(last=False)
            self.size -= len(evicted["data"])

    def clear(self):
        self._items.clear()
        self.size = 0

    def __len__(self):
        return len(self._items)


class Session:
    def __init__(self, cache_bytes: int = DEFAULT_CACHE_BYTES):
        self.volume: NiftiVolume | None = None
        self.mode = "neurological"
        self.colormap = "auto"
        self.cache = SliceCache(cache_bytes)
        self._spectrum = None  # (voxel, reply) of the last spectrum sent
        self.closed = False

    # ---------------------------------------------------------------- helpers

    def _require_volume(self) -> NiftiVolume:
        if self.volume is None:
            raise PreviewError("bad_request", "No file is open")
        return self.volume

    def _effective_mode(self) -> str:
        volume = self._require_volume()
        return self.mode if volume.orientation_known else "stored"

    def _plans(self):
        return self._require_volume().plans(self._effective_mode())

    def _grid(self):
        """The grid the views are currently cut from."""
        return self._plans()["coronal"].grid

    def _display(self) -> dict:
        plans = self._plans()
        return {
            "mode": self._effective_mode(),
            "requested_mode": self.mode,
            "grid": plans["coronal"].grid.to_dict(),
            "plans": {view: plan.to_dict() for view, plan in plans.items()},
        }

    # --------------------------------------------------------------- requests

    def handle(self, request: dict) -> dict:
        if not isinstance(request, dict):
            raise PreviewError("bad_request", "Request must be a JSON object")
        op = request.get("op")
        handler = getattr(self, f"op_{op}", None) if isinstance(op, str) else None
        if handler is None:
            raise PreviewError("bad_request", f"Unknown op {op!r}")
        return handler(request)

    def op_ping(self, request):
        return {"pong": True}

    def op_open(self, request):
        path = request.get("path")
        if not isinstance(path, str) or not path:
            raise PreviewError("bad_request", "open requires a 'path' string")
        mode = request.get("mode", self.mode)
        if mode not in MODES:
            raise PreviewError("bad_request", f"Unknown mode {mode!r}")
        self.volume = NiftiVolume(path)
        self.mode = mode
        self.cache.clear()
        self._spectrum = None
        return {"header": self.volume.header_info(), "cursor": self._grid().center(), **self._display()}

    def op_load(self, request):
        volume = self._require_volume()
        stats = volume.stats()
        return {
            "stats": {
                "window": stats.window.to_dict(),
                "labels": stats.labels.to_dict(),
                "non_finite": stats.non_finite,
                "non_finite_exact": stats.non_finite_exact,
                "min": wire_value(stats.minimum, stats.labels.is_labels),
                "max": wire_value(stats.maximum, stats.labels.is_labels),
                "notes": stats.notes,
            },
            "colormap": self.colormap,
            "effective_colormap": effective_colormap(volume, self.colormap),
            "colormaps": colormap_choices(volume),
        }

    def op_set_display(self, request):
        volume = self._require_volume()
        mode = request.get("mode", self.mode)
        colormap = request.get("colormap", self.colormap)
        if mode not in MODES:
            raise PreviewError("bad_request", f"Unknown mode {mode!r}")
        if colormap not in COLORMAPS:
            raise PreviewError("bad_request", f"Unknown colormap {colormap!r}")
        # The cursor of the current grid, carried over to the new one.
        cursor = request.get("cursor")
        if cursor is not None and not _is_ijk(cursor):
            raise PreviewError("bad_request", "set_display 'cursor' must be three integers")
        before = self._grid()
        self.mode, self.colormap = mode, colormap
        after = self._grid()
        result = self._display()
        if cursor is not None:
            c = [min(max(x, 0), n - 1) for x, n in zip(cursor, before.shape)]
            # Grids of the same kind are the same grid (one image).
            result["cursor"] = c if before.resampled == after.resampled else after.from_world(before.world_of(c))
        result["colormap"] = colormap
        if volume._stats is not None:
            result["effective_colormap"] = effective_colormap(volume, colormap)
        return result

    def op_slice(self, request):
        volume = self._require_volume()
        view = request.get("view")
        if view not in VIEWS:
            raise PreviewError("bad_request", f"Unknown view {view!r}")
        plan = self._plans()[view]
        index = request.get("index")
        if not isinstance(index, int) or isinstance(index, bool):
            raise PreviewError("bad_request", "slice requires an integer 'index'")
        index = min(max(index, 0), plan.n_slices - 1)
        colormap = request.get("colormap", self.colormap)
        if colormap not in COLORMAPS:
            raise PreviewError("bad_request", f"Unknown colormap {colormap!r}")
        cmap = effective_colormap(volume, colormap)
        key = (self._effective_mode(), cmap, view, index)
        cached = self.cache.get(key)
        if cached is None:
            rendered = render_slice(volume, plan, index, cmap)
            cached = {
                "view": view,
                "index": index,
                "width": rendered.width,
                "height": rendered.height,
                "colormap": rendered.colormap,
                "format": "rgb8",
                "data": base64.b64encode(rendered.rgb).decode("ascii"),
            }
            self.cache.put(key, cached)
        return dict(cached)

    def op_value(self, request):
        """Value under grid point ``ijk``: that of the nearest voxel (``voxel``, null outside the image)."""
        volume = self._require_volume()
        ijk = request.get("ijk")
        if not _is_ijk(ijk):
            raise PreviewError("bad_request", "value requires 'ijk' as three integers")
        grid = self._grid()
        if not all(0 <= c < n for c, n in zip(ijk, grid.shape)):
            raise PreviewError("bad_request", f"Voxel {ijk} outside image {list(grid.shape)}")
        voxel = grid.nearest(grid.to_voxel([float(c) for c in ijk])) if grid.resampled else list(ijk)
        value = volume.value_at(voxel) if voxel is not None else None
        # In mm, whatever the spatial unit of the header (as the volumes).
        mm = volume.mm_per_unit()
        reply = {"ijk": ijk, "world": [None if x is None else x * mm for x in voxel_to_world(grid.affine, ijk)], "voxel": voxel, "value": wire_value(value, value is not None and volume.stats().labels.is_labels)}
        if voxel is not None and volume.is_rgb:
            reply["rgb"] = volume.rgb_at(voxel)
        # In a label map, the size of the structure under the cursor.
        if value is not None and value != 0 and volume.stats().labels.is_labels:
            count = volume.count_equal(value)
            reply["label_voxels"] = count
            mm3 = count * volume.voxel_volume_mm3()
            if math.isfinite(mm3):
                reply["label_volume_mm3"] = mm3
        return reply

    def op_spectrum(self, request):
        """Spectrum of the voxel nearest to grid point ``ijk`` (the centre voxel without it)."""
        volume = self._require_volume()
        if volume.mrs is None:
            raise PreviewError("bad_request", "Not a spectroscopy (NIfTI-MRS) file")
        ijk = request.get("ijk")
        if ijk is None:
            voxel = volume.center()
        else:
            if not _is_ijk(ijk):
                raise PreviewError("bad_request", "spectrum 'ijk' must be three integers")
            grid = self._grid()
            if not all(0 <= c < n for c, n in zip(ijk, grid.shape)):
                raise PreviewError("bad_request", f"Voxel {ijk} outside image {list(grid.shape)}")
            voxel = grid.nearest(grid.to_voxel([float(c) for c in ijk])) if grid.resampled else list(ijk)
        if voxel is None:
            return {"voxel": None}
        if self._spectrum is not None and self._spectrum[0] == list(voxel):
            return dict(self._spectrum[1])
        reply = volume.spectrum(voxel)
        self._spectrum = (list(voxel), reply)
        return dict(reply)

    def op_close(self, request):
        self.closed = True
        self.volume = None
        self.cache.clear()
        return {"closed": True}


def _is_ijk(v) -> bool:
    return isinstance(v, list) and len(v) == 3 and all(isinstance(c, int) and not isinstance(c, bool) for c in v)


def respond(session: Session, line: bytes) -> dict:
    req_id = None
    try:
        if len(line) > MAX_REQUEST_BYTES:
            raise PreviewError("bad_request", "Request too large")
        try:
            request = json.loads(line)
        except ValueError:
            raise PreviewError("bad_request", "Malformed JSON request") from None
        if isinstance(request, dict):
            req_id = request.get("id")
        result = session.handle(request)
        return {"id": req_id, "ok": True, **result}
    except PreviewError as exc:
        return {"id": req_id, "ok": False, "error": exc.to_dict()}
    except Exception as exc:  # pragma: no cover - defensive
        traceback.print_exc(file=sys.stderr)
        return {"id": req_id, "ok": False, "error": {"code": "internal", "message": f"{type(exc).__name__}: {exc}"}}


def _die_with_parent():
    """Ask the kernel to terminate us if the parent (Sushi) dies (Linux only)."""
    try:
        import ctypes
        import signal

        libc = ctypes.CDLL("libc.so.6", use_errno=True)
        PR_SET_PDEATHSIG = 1
        libc.prctl(PR_SET_PDEATHSIG, signal.SIGTERM)
    except Exception:
        pass


def serve(stdin=None, stdout=None) -> int:
    stdin = stdin if stdin is not None else sys.stdin.buffer
    stdout = stdout if stdout is not None else sys.stdout.buffer
    # Nothing but protocol messages may reach stdout.
    sys.stdout = sys.stderr
    session = Session()
    stdout.write(encode({"event": "hello", "protocol": PROTOCOL_VERSION, "version": __version__, "pid": os.getpid()}))
    stdout.flush()
    for line in stdin:
        if not line.strip():
            continue
        stdout.write(encode(respond(session, line)))
        stdout.flush()
        if session.closed:
            break
    return 0


def main() -> int:
    _die_with_parent()
    try:
        return serve()
    except (BrokenPipeError, KeyboardInterrupt):
        return 0


if __name__ == "__main__":
    sys.exit(main())
