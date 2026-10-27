"""Loading NIfTI files with nibabel.

The header is parsed when the volume is opened (cheap, even for .nii.gz).
Voxel data is loaded on demand by ``ensure_data()``:

* 3D ``.nii``: memory-mapped through nibabel, nothing is read up front;
* 3D ``.nii.gz``: decompressed once into memory (stored dtype, no float copy);
* 4D and higher: only the first volume is read. Volumes are stored last in
  NIfTI, so for ``.nii.gz`` the gzip stream is only decompressed up to the end
  of volume 0 (see docs/BENCHMARKS.md for measurements).
"""

from __future__ import annotations

import os
import stat
from dataclasses import dataclass, field

import nibabel as nib
import numpy as np

from . import labels as labels_mod
from . import mrs as mrs_mod
from . import orientation
from .errors import PreviewError
from .windowing import Window, compute_window, strided_sample

DEFAULT_MAX_BYTES = 3 * 1024**3
MAX_BYTES_RANGE = (16 * 1024**2, 64 * 1024**3)
# Largest accepted vox_offset (header + extensions).
MAX_VOX_OFFSET = 256 * 1024**2
# Largest slice drawn: 16 megapixels (4096 x 4096), no side above 16384.
MAX_SLICE_PIXELS = orientation.MAX_SLICE_PIXELS
MAX_SLICE_SIDE = orientation.MAX_SLICE_SIDE


def bytes_from_env(name: str, default: int, lo: int, hi: int) -> int:
    value = os.environ.get(name)
    if value is None:
        return default
    try:
        n = int(value.strip())
    except ValueError:
        return default
    if n < 0:
        return default
    return min(max(n, lo), hi)


def _looks_like_nifti(path) -> bool:
    """The first four bytes (after gunzip) hold sizeof_hdr, 348 or 540."""
    import gzip
    import struct

    try:
        with open(path, "rb") as f:
            head = f.read(2)
        opener = gzip.open if head == b"\x1f\x8b" else open
        with opener(path, "rb") as f:
            first = f.read(4)
    except Exception:
        return False
    if len(first) < 4:
        return False
    return any(struct.unpack(order + "i", first)[0] in (348, 540) for order in "<>")


def _max_bytes() -> int:
    return bytes_from_env("NIFTI_PREVIEW_MAX_BYTES", DEFAULT_MAX_BYTES, *MAX_BYTES_RANGE)


def check_slice_size(shape3) -> None:
    b, a = sorted(int(n) for n in shape3)[1:]
    if a > MAX_SLICE_SIDE or a * b > MAX_SLICE_PIXELS:
        raise PreviewError("too_large", f"Image too large to preview (slices of {a} × {b} voxels, limit 4096 × 4096).")


_BIDI = {0x200E, 0x200F, 0x061C, *range(0x202A, 0x202F), *range(0x2066, 0x206A)}


def clean_text(s: str) -> str:
    """Header text as displayed: control characters and bidi overrides removed."""
    out = []
    for c in s:
        o = ord(c)
        if c in "\t\n\r":
            out.append(" ")
        elif o < 0x20 or 0x7F <= o <= 0x9F or o in _BIDI:
            continue
        else:
            out.append(c)
    return "".join(out).strip()


def _label(header, field_name: str) -> str:
    try:
        return str(header.get_value_label(field_name))
    except Exception:
        return "unknown"


def _finite_or_none(x):
    if isinstance(x, (int, np.integer)):
        return int(x)
    try:
        x = float(x)
    except (TypeError, ValueError):
        return None
    return x if np.isfinite(x) else None


def wire_value(value, categorical=False):
    """Preserve integral values beyond JavaScript's exact number range."""
    if (isinstance(value, int) or categorical) and value is not None and np.isfinite(value) and abs(value) > 2**53 - 1 and int(value) == value:
        return str(int(value))
    return value


@dataclass
class VolumeStats:
    window: Window
    labels: labels_mod.LabelInfo
    non_finite: int
    non_finite_exact: bool
    minimum: int | float | None
    maximum: int | float | None
    notes: list[str] = field(default_factory=list)


class NiftiVolume:
    def __init__(self, path, max_bytes: int | None = None):
        self.path = os.fspath(path)
        self.max_bytes = _max_bytes() if max_bytes is None else max_bytes
        try:
            st = os.stat(self.path)
        except FileNotFoundError:
            raise PreviewError("not_found", f"File not found: {self.path}") from None
        except OSError as exc:
            raise PreviewError("not_found", f"Cannot access file: {exc.strerror or exc}") from None
        if not stat.S_ISREG(st.st_mode):
            raise PreviewError("not_a_file", "Not a regular file")
        self.file_size = st.st_size
        self.compressed = self.path.lower().endswith(".gz")

        # Like the backend, recognise the format from the content, whatever the name.
        if not _looks_like_nifti(self.path):
            raise PreviewError("invalid_image", "Not a recognised image file (NIfTI, MGH/MGZ, MRtrix, NRRD or DICOM)")
        try:
            img = nib.load(self.path)
        except Exception as exc:
            raise PreviewError("invalid_nifti", f"Not a readable NIfTI file ({type(exc).__name__}: {exc})") from None
        if not isinstance(img, nib.Nifti1Image):
            raise PreviewError("unsupported_format", f"Unsupported image type: {type(img).__name__}")
        self.img = img
        self.header = img.header
        self.format = "NIfTI-2" if isinstance(img, nib.Nifti2Image) else "NIfTI-1"

        shape = tuple(int(n) for n in img.shape)
        if len(shape) < 2 or any(n <= 0 for n in shape):
            raise PreviewError("invalid_nifti", f"Invalid image dimensions {shape}")
        self.shape = shape
        self.shape3 = (shape + (1, 1))[:3]
        self.n_volumes = int(np.prod(shape[3:])) if len(shape) > 3 else 1
        self.stored_dtype = np.dtype(self.header.get_data_dtype())

        # Non-finite sizes would make every view degenerate (as in the backend).
        pixdim = np.asarray(self.header["pixdim"], dtype=float)
        if not np.all(np.isfinite(pixdim[1:])):
            fixed = pixdim.copy()
            spatial, rest = fixed[1:4], fixed[4:]
            spatial[~np.isfinite(spatial)] = 1.0
            rest[~np.isfinite(rest)] = 0.0
            self.header = self.header.copy()
            self.header["pixdim"] = fixed
            self._affine_override = self.header.get_best_affine()
        else:
            self._affine_override = None

        self.qform_code = int(self.header["qform_code"])
        self.sform_code = int(self.header["sform_code"])
        self.affine = np.asarray(img.affine if self._affine_override is None else self._affine_override, dtype=float)
        self.affine_source = "sform" if self.sform_code > 0 else ("qform" if self.qform_code > 0 else "none")
        self.orientation_known = self.affine_source != "none" and orientation.orientation_is_usable(self.affine)

        # nibabel moves scl_slope/scl_inter from the header into the array proxy on load.
        slope = getattr(img.dataobj, "slope", None)
        inter = getattr(img.dataobj, "inter", None)
        self.slope = 1.0 if slope is None or not np.isfinite(slope) or slope == 0 else float(slope)
        self.inter = 0.0 if inter is None or not np.isfinite(inter) else float(inter)
        self.identity_scaling = self.slope == 1.0 and self.inter == 0.0

        try:
            self.intent_code = int(self.header["intent_code"])
        except Exception:
            self.intent_code = 0

        self.current = 0  # the volume shown (flat index over dims 4+, Fortran order)
        self._data = None  # 3D array of stored values (or scaled floats for >3D)
        self._data_scaled = False
        self._rgb = None  # RGB images: (x, y, z, 3) uint8, `_data` holds the luminance
        self._converted = False  # RGB luminance or complex magnitude
        self._stats: VolumeStats | None = None
        self._stats_by_volume: dict[int, VolumeStats] = {}
        self._load_notes: list[str] = []
        names = self.stored_dtype.names
        self.is_rgb = bool(names) and [n.upper() for n in names[:3]] == ["R", "G", "B"]
        self.mrs = mrs_mod.detect(self.header, self.shape, self.stored_dtype, clean_text)
        self._mrs_data = None

    # ------------------------------------------------------------------ header

    def header_info(self) -> dict:
        h = self.header
        zooms = [float(z) for z in h.get_zooms()]
        try:
            space_units, time_units = h.get_xyzt_units()
        except Exception:
            space_units, time_units = "unknown", "unknown"
        try:
            intent_name = h.get_intent()[0]
        except Exception:
            intent_name = "unknown"
        try:
            descrip = clean_text(h["descrip"].tobytes().split(b"\0", 1)[0].decode("utf-8", "replace"))
        except Exception:
            descrip = ""
        obliq = orientation.obliquity_degrees(self.affine)
        warnings = []
        single_voxel = self.mrs is not None and tuple(self.shape3) == (1, 1, 1)
        if single_voxel:
            pass  # a single-voxel spectrum: no views, so nothing to say about them
        elif not self.orientation_known:
            warnings.append("No valid qform/sform: anatomical orientation is unknown, showing stored voxel axes.")
        elif np.isfinite(obliq) and obliq > orientation.RESAMPLE_ABOVE_DEGREES:
            if self.plans("neurological")["coronal"].grid.resampled:
                warnings.append(
                    f"Oblique acquisition ({obliq:.1f}°): views are resampled onto the anatomical axes; "
                    "the stored mode shows the voxels as acquired."
                )
            else:
                warnings.append(
                    f"Oblique acquisition ({obliq:.1f}°): views follow the nearest anatomical axes, data are not resampled."
                )
        if self.mrs is not None and self.mrs.first_index_only():
            listed = ", ".join(f"{t} ({n})" for t, n in self.mrs.first_index_only())
            warnings.append(f"The spectrum uses index 0 of {listed}: these dimensions hold different signals and are not averaged.")
        return {
            "path": self.path,
            "name": os.path.basename(self.path),
            "file_size": self.file_size,
            "format": self.format,
            "compressed": self.compressed,
            "shape": list(self.shape),
            "n_volumes": self.n_volumes,
            "zooms": zooms,
            "space_units": space_units,
            "time_units": time_units,
            # In spectroscopy, pixdim[4] is the dwell time.
            "repetition_time": zooms[3] if len(zooms) > 3 and self.mrs is None else None,
            "dtype": str(self.stored_dtype),
            "datatype": _label(h, "datatype"),
            "slope": self.slope,
            "inter": self.inter,
            "qform_code": self.qform_code,
            "qform": _label(h, "qform_code"),
            "sform_code": self.sform_code,
            "sform": _label(h, "sform_code"),
            "affine_source": self.affine_source,
            "affine": [[_finite_or_none(x) for x in row] for row in self.affine],
            "orientation": orientation.axis_codes(self.affine) if self.orientation_known else "unknown",
            "orientation_known": self.orientation_known,
            "obliquity_deg": _finite_or_none(obliq),
            "intent": intent_name,
            "intent_code": self.intent_code,
            "descrip": descrip,
            "warnings": warnings,
            "mrs": self.mrs.to_dict(self.shape3) if self.mrs else None,
        }

    def plans(self, mode: str) -> dict[str, orientation.ViewPlan]:
        return orientation.build_plans(self.affine, self.shape3, mode, self.orientation_known)

    def center(self) -> list[int]:
        return [n // 2 for n in self.shape3]

    # -------------------------------------------------------------------- data

    def browsable(self) -> bool:
        return self.mrs is None and self.n_volumes > 1

    def set_volume(self, k: int) -> int:
        """Show volume ``k`` (clamped); returns the volume shown."""
        if self.mrs is not None:
            raise PreviewError("bad_request", "Spectroscopy files have no volumes to browse")
        k = min(max(int(k), 0), self.n_volumes - 1)
        if k != self.current:
            if self._stats is not None:
                self._stats_by_volume[self.current] = self._stats
            self.current = k
            self._data = None
            self._rgb = None
            self._load_notes = []
            self._stats = self._stats_by_volume.get(k)
        self.ensure_data()
        return self.current

    def _volume_slicer(self, k: int):
        rest = self.shape[3:]
        return (slice(None),) * 3 + tuple(int(i) for i in np.unravel_index(k, rest, order="F"))

    def timecourse(self, voxel) -> list:
        """Physical values (float32) of ``voxel`` in every volume, None for NaN/Inf."""
        if self.mrs is not None:
            raise PreviewError("bad_request", "Spectroscopy files have no time course")
        if not all(0 <= c < n for c, n in zip(voxel, self.shape3)):
            raise PreviewError("bad_request", f"Voxel {list(voxel)} outside image {list(self.shape3)}")
        proxy = self.img.dataobj
        values = []
        for k in range(self.n_volumes):
            point = tuple(int(c) for c in voxel) + tuple(self._volume_slicer(k)[3:])
            # nibabel applies the scaling; RGB becomes luminance, complex its magnitude.
            v, _, _ = _to_displayable(np.asarray(np.asanyarray(proxy[point])).reshape(1))
            v = float(np.float32(v[0]))
            values.append(v if np.isfinite(v) else None)
        return values

    def estimated_bytes(self) -> int:
        return int(np.prod(self.shape3)) * self.stored_dtype.itemsize

    def ensure_data(self):
        if self._data is not None:
            return self._data
        check_slice_size(self.shape3)
        if self.estimated_bytes() > self.max_bytes:
            raise PreviewError(
                "too_large",
                f"Image too large to preview ({self.estimated_bytes() / 1024**2:.0f} MB of voxel data, "
                f"limit {self.max_bytes / 1024**2:.0f} MB: see max-image-mb in ~/.config/gnome-nifti-preview/settings.ini).",
            )
        offset = int(getattr(self.img.dataobj, "offset", 0))  # the loaded header copy resets vox_offset
        if offset > MAX_VOX_OFFSET:
            raise PreviewError(
                "corrupt_data",
                f"Image data could not be read; the header puts the voxel data {offset / 1024**2:.0f} MB into the file.",
            )
        proxy = self.img.dataobj
        try:
            if len(self.shape) <= 3:
                data = np.asanyarray(proxy.get_unscaled())
                scaled = False
            else:
                data = np.asanyarray(proxy[self._volume_slicer(self.current)])
                scaled = not self.identity_scaling
        except PreviewError:
            raise
        except MemoryError:
            raise PreviewError("too_large", "Not enough memory to load this volume.") from None
        except Exception as exc:
            raise PreviewError(
                "corrupt_data", f"Image data could not be read; the file may be truncated or corrupted ({exc})."
            ) from None
        data = data.reshape(self.shape3, order="A") if data.ndim != 3 else data
        if self.is_rgb:
            self._rgb = np.stack([data[n] for n in data.dtype.names[:3]], axis=-1).astype(np.uint8)
        data, note, converted = _to_displayable(data)
        if note:
            self._load_notes.append(note)
        if converted:
            self._converted = True
            scaled = True
        self._data = data
        self._data_scaled = scaled
        return data

    def scale(self, values) -> np.ndarray:
        """Stored values -> physical values (float32)."""
        v = np.asarray(values)
        if self._data_scaled or self.identity_scaling:
            return v.astype(np.float32, copy=False)
        return (v.astype(np.float64) * self.slope + self.inter).astype(np.float32)

    def exact(self, values) -> np.ndarray:
        """Unscaled scalar values retain their native precision, including all
        int64/uint64 identifiers; converted/scaled values use float32."""
        v = np.asarray(values)
        if v.dtype.kind in "iuf" and self.identity_scaling and not self._data_scaled:
            return v
        return self.scale(v).astype(np.float64)

    def stats(self) -> VolumeStats:
        if self._stats is not None:
            return self._stats
        data = self.ensure_data()
        sample = strided_sample(data)
        scaled_sample = self.scale(sample)
        window = compute_window(scaled_sample)

        if data.dtype.kind == "f":
            if data.size <= 64 * 1024**2:
                non_finite = int(data.size - np.count_nonzero(np.isfinite(data)))
                exact = True
            else:
                non_finite = int(round((scaled_sample.size - np.count_nonzero(np.isfinite(scaled_sample)))
                                       * data.size / max(1, scaled_sample.size)))
                exact = False
        else:
            non_finite, exact = 0, True

        # Converted datatypes (RGB luminance, complex magnitude) are never labels.
        identity = self.identity_scaling and not self._converted
        label_info = labels_mod.detect_labels(data, sample, identity, self.intent_code)

        range_sample = sample if identity else scaled_sample
        finite = range_sample[np.isfinite(range_sample)]
        minimum = _finite_or_none(finite.min()) if finite.size else None
        maximum = _finite_or_none(finite.max()) if finite.size else None

        single_voxel = self.mrs is not None and data.size == 1
        if self.mrs is None:
            notes = list(self._load_notes)
        else:
            notes = [] if single_voxel else ["Maps show the magnitude of the first time point."]
        if single_voxel:
            pass  # a single voxel is "constant"
        elif window.status == "no_finite":
            notes.append("Volume contains no finite values.")
        elif window.status == "constant" and minimum != maximum:
            if not label_info.is_labels:
                notes.append("Values differ below grayscale display precision.")
        elif window.status == "constant":
            constant = str(wire_value(minimum)) if isinstance(wire_value(minimum), str) else f"{window.low:g}"
            notes.append(f"Constant volume (value {constant}).")
        if non_finite:
            notes.append(f"{'' if exact else '≈'}{non_finite} NaN/Inf voxels (shown black).")
        self._stats = VolumeStats(window, label_info, non_finite, exact, minimum, maximum, notes)
        return self._stats

    def voxel_volume_mm3(self) -> float:
        """Volume of one voxel in mm³ (spatial units taken as mm when unknown)."""
        try:
            space_units = self.header.get_xyzt_units()[0]
        except Exception:
            space_units = "unknown"
        return abs(float(np.linalg.det(self.affine[:3, :3]))) * self.mm_per_unit() ** 3

    def mm_per_unit(self) -> float:
        """Millimetres per spatial unit of the header (mm when it gives none)."""
        try:
            space_units = self.header.get_xyzt_units()[0]
        except Exception:
            space_units = "unknown"
        return {"meter": 1e3, "micron": 1e-3}.get(space_units, 1.0)

    def count_equal(self, value: float) -> int:
        """Number of voxels of volume 0 whose displayed value equals ``value``."""
        data = self.ensure_data()
        if data.dtype.kind in "iuf" and self.identity_scaling and not self._data_scaled:
            return int(np.count_nonzero(data == value))  # exact, as value_at
        return int(np.count_nonzero(self.scale(data) == np.float32(value)))

    def slice_values(self, plan: orientation.ViewPlan, index: int, nearest: bool = False) -> np.ndarray:
        """Scaled values (float32) of a view slice, rows = screen y. On a
        resampled grid they are interpolated trilinearly, or taken from the
        nearest voxel when ``nearest`` (labels); NaN outside the image."""
        data = self.ensure_data()
        if plan.grid is not None and plan.grid.resampled:
            return orientation.resample_2d(lambda i, j, k: self.scale(data[i, j, k]), plan, index, nearest)
        return self.scale(orientation.extract_2d(data, plan, index))

    def slice_labels(self, plan: orientation.ViewPlan, index: int) -> np.ndarray:
        """Exact values of a label slice: native integers (objects when resampled)
        or float64; nearest voxel on a
        resampled grid, NaN outside the image."""
        data = self.ensure_data()
        if plan.grid is not None and plan.grid.resampled:
            dtype = object if data.dtype.kind in "iu" and self.identity_scaling and not self._data_scaled else np.float64
            return orientation.resample_2d(lambda i, j, k: self.exact(data[i, j, k]), plan, index, True, dtype)
        return self.exact(orientation.extract_2d(data, plan, index))

    def slice_rgb(self, plan: orientation.ViewPlan, index: int) -> np.ndarray:
        """(height, width, 3) uint8 slice of an RGB image."""
        self.ensure_data()
        if plan.grid is not None and plan.grid.resampled:
            return orientation.resample_rgb(self._rgb, plan, index)
        return np.stack([orientation.extract_2d(self._rgb[..., c], plan, index) for c in range(3)], axis=-1)

    def rgb_at(self, ijk) -> list[int] | None:
        self.ensure_data()
        if self._rgb is None:
            return None
        return [int(x) for x in self._rgb[tuple(int(c) for c in ijk)]]

    def spectrum(self, voxel) -> dict:
        """Spectrum of a voxel of a spectroscopy file."""
        if self.mrs is None:
            raise PreviewError("bad_request", "Not a spectroscopy (NIfTI-MRS) file")
        mrs_mod.too_long(self.mrs)
        if not all(0 <= c < n for c, n in zip(voxel, self.shape3)):
            raise PreviewError("bad_request", f"Voxel {list(voxel)} outside image {list(self.shape3)}")
        self.ensure_data()
        if self._mrs_data is None:
            total = int(np.prod(self.shape)) * self.stored_dtype.itemsize
            if total > self.max_bytes:
                raise PreviewError(
                    "too_large",
                    f"Image too large to preview ({total / 1024**2:.0f} MB of spectroscopy data, "
                    f"limit {self.max_bytes / 1024**2:.0f} MB: see max-image-mb in ~/.config/gnome-nifti-preview/settings.ini).",
                )
            try:
                self._mrs_data = np.asanyarray(self.img.dataobj.get_unscaled())
            except MemoryError:
                raise PreviewError("too_large", "Not enough memory to load this volume.") from None
            except Exception as exc:
                raise PreviewError(
                    "corrupt_data", f"Image data could not be read; the file may be truncated or corrupted ({exc})."
                ) from None
        return mrs_mod.spectrum(self._mrs_data, voxel, self.mrs)

    def value_at(self, ijk) -> int | float | None:
        data = self.ensure_data()
        i, j, k = (int(x) for x in ijk)
        if not all(0 <= c < n for c, n in zip((i, j, k), self.shape3)):
            raise PreviewError("bad_request", f"Voxel {ijk} outside image {self.shape3}")
        return _finite_or_none(self.exact(data[i, j, k]).item())


def _to_displayable(data: np.ndarray):
    """Convert datatypes without a natural scalar display: (values, note, converted).

    RGB becomes its luminance (shown in grayscale; the colours are kept apart).
    """
    dt = data.dtype
    if dt.names:
        names = [n.upper() for n in dt.names]
        if names[:3] == ["R", "G", "B"]:
            r, g, b = (data[n].astype(np.float32) for n in dt.names[:3])
            return 0.299 * r + 0.587 * g + 0.114 * b, None, True
        raise PreviewError("unsupported_datatype", f"Unsupported structured datatype {dt}")
    if dt.kind == "c":
        return np.abs(data).astype(np.float32), "Complex data shown as magnitude.", True
    if dt.kind == "b":
        return data.astype(np.uint8), None, False
    if dt.kind == "f" and dt.itemsize < 4:
        return data.astype(np.float32), None, False
    if dt.kind not in "iuf":
        raise PreviewError("unsupported_datatype", f"Unsupported datatype {dt}")
    return data, None, False
