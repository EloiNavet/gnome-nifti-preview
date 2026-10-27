"""NIfTI-MRS (MR spectroscopy): detection, metadata from the JSON header
extension (code 44), and the spectrum of a voxel.

Same algorithm as the Rust backend (backend/src/mrs.rs): the transients
(dimensions 5 to 7) are phased by their first point and weighted by its
amplitude, the first point counts half, then FFT and ``fftshift``. The
chemical-shift axis is ``ppm = shift - f / SF``, using a finite SpecFreqChemShift when given,
otherwise 4.65 ppm for 1H and 2H and 0 for other nuclei.
"""

from __future__ import annotations

import json
import math
from dataclasses import dataclass, field

import numpy as np

from .errors import PreviewError

EXTENSION_CODE = 44
MAX_POINTS = 1 << 16
_DEFAULT_TAGS = ("DIM_COIL", "DIM_DYN", "DIM_INDIRECT_0")
# Repeats of one signal, averaged; the other dimensions (edit, ISIS, phase
# cycle, indirect, user) encode different signals: only index 0 is used.
AVERAGED_TAGS = ("DIM_COIL", "DIM_DYN", "DIM_MEAS")
GYROMAGNETIC_MHZ_PER_T = {
    "1H": 42.577478, "2H": 6.536, "3HE": 32.434, "7LI": 16.546, "13C": 10.7084, "15N": 4.316,
    "17O": 5.772, "19F": 40.078, "23NA": 11.262, "31P": 17.235, "129XE": 11.777,
}


@dataclass
class Mrs:
    format: str
    nucleus: str | None
    spectrometer_mhz: float | None
    dwell_s: float | None
    points: int
    dims: list = field(default_factory=list)  # [(tag, size)] for dimensions 5 to 7
    reference_ppm: float | None = None
    echo_time_s: float | None = None
    repetition_time_s: float | None = None

    @property
    def transients(self) -> int:
        return int(np.prod([n for _, n in self.dims])) if self.dims else 1

    def first_index_only(self) -> list:
        return [(t, n) for t, n in self.dims if n > 1 and t not in AVERAGED_TAGS]

    def combined_transients(self) -> list[int]:
        """Flat indices (dimensions 5 to 7, Fortran order) of the transients averaged."""
        out, stride = [0], 1
        for tag, size in self.dims:
            if tag in AVERAGED_TAGS:
                out = [j + i * stride for j in out for i in range(size)]
            stride *= size
        return sorted(out)

    def to_dict(self, shape3) -> dict:
        gamma = GYROMAGNETIC_MHZ_PER_T.get(self.nucleus.upper()) if self.nucleus else None
        field_t = self.spectrometer_mhz / gamma if gamma and self.spectrometer_mhz else None
        return {
            "format": self.format,
            "nucleus": self.nucleus,
            "spectrometer_mhz": self.spectrometer_mhz,
            "field_t": field_t,
            "points": self.points,
            "dwell_s": self.dwell_s,
            "bandwidth_hz": 1.0 / self.dwell_s if self.dwell_s else None,
            "dims": [{"tag": t, "size": n} for t, n in self.dims],
            "transients": self.transients,
            "combined": math.prod(n for tag, n in self.dims if tag in AVERAGED_TAGS),
            "first_index_only": [{"tag": t, "size": n} for t, n in self.first_index_only()],
            "single_voxel": tuple(shape3) == (1, 1, 1),
            "echo_time_s": self.echo_time_s,
            "repetition_time_s": self.repetition_time_s,
        }


def _finite(x):
    return x if x is not None and math.isfinite(x) else None


def _nonnegative(x):
    return x if x is not None and math.isfinite(x) and x >= 0 else None


def _positive(x):
    return x if x is not None and math.isfinite(x) and x > 0 else None


def _first_number(v):
    if isinstance(v, list):
        v = v[0] if v else None
    if isinstance(v, (int, float)) and not isinstance(v, bool):
        return float(v)
    return None


def _first_string(v, clean_text):
    if isinstance(v, list):
        v = v[0] if v else None
    if not isinstance(v, str):
        return None
    s = clean_text(v)
    return s if s and len(s) <= 32 else None


def _extension_json(header) -> dict:
    for ext in header.extensions:
        if ext.get_code() != EXTENSION_CODE:
            continue
        content = bytes(ext.get_content()).rstrip(b"\0 \t\n\x0c\r")
        try:
            value = json.loads(content.decode("utf-8"))
        except Exception:
            return {}
        return value if isinstance(value, dict) else {}
    return {}


def detect(header, shape, stored_dtype, clean_text) -> Mrs | None:
    """Spectroscopy data: a complex image with a spectral dimension, marked by
    ``intent_name`` "mrs_v…" or by a NIfTI-MRS extension."""
    if stored_dtype.kind != "c" or len(shape) < 4:
        return None
    try:
        raw = header["intent_name"].tobytes().split(b"\0", 1)[0]
        intent_name = clean_text(raw.decode("utf-8", "replace").strip())
    except Exception:
        intent_name = ""
    meta = _extension_json(header)
    if not intent_name.startswith("mrs_v") and "SpectrometerFrequency" not in meta:
        return None
    try:
        time_units = header.get_xyzt_units()[1]
    except Exception:
        time_units = "unknown"
    per_second = {"msec": 1e-3, "usec": 1e-6}.get(time_units, 1.0)
    dims = []
    for i in range(4, len(shape)):
        tag = _first_string(meta.get(f"dim_{i + 1}"), clean_text) or _DEFAULT_TAGS[i - 4]
        dims.append((tag, int(shape[i])))
    return Mrs(
        format=intent_name if intent_name.startswith("mrs_v") else "",
        nucleus=_first_string(meta.get("ResonantNucleus"), clean_text),
        spectrometer_mhz=_positive(_first_number(meta.get("SpectrometerFrequency"))),
        dwell_s=_positive(float(header["pixdim"][4]) * per_second),
        reference_ppm=_finite(_first_number(meta.get("SpecFreqChemShift"))),
        points=int(shape[3]),
        dims=dims,
        echo_time_s=_nonnegative(_first_number(meta.get("EchoTime"))),
        repetition_time_s=_positive(_first_number(meta.get("RepetitionTime"))),
    )


def spectrum(data, voxel, mrs: Mrs) -> dict:
    """Spectrum of ``voxel``; ``data`` holds all the (complex) data."""
    n = data.shape[3]
    fid = np.array(data[voxel[0], voxel[1], voxel[2]], dtype=np.complex128).reshape(n, -1, order="F")
    fid[~(np.isfinite(fid.real) & np.isfinite(fid.imag))] = 0
    fid = fid[:, mrs.combined_transients()]
    k = fid.shape[1]
    f0 = fid[0].copy()
    weight = float(np.abs(f0).sum())
    if weight > 0 and math.isfinite(weight):
        comb = (fid * np.conj(f0)).sum(axis=1) / weight
    else:
        comb = fid.sum(axis=1) / k
    comb[0] *= 0.5
    spec = np.fft.fftshift(np.fft.fft(comb))

    half = n // 2
    centre = mrs.reference_ppm if mrs.reference_ppm is not None else (4.65 if mrs.nucleus in ("1H", "2H") else 0.0)
    if mrs.dwell_s and mrs.spectrometer_mhz:
        hz = lambda i: (i - half) / (n * mrs.dwell_s)  # noqa: E731
        unit, first, last = "ppm", centre - hz(0) / mrs.spectrometer_mhz, centre - hz(n - 1) / mrs.spectrometer_mhz
    elif mrs.dwell_s:
        hz = lambda i: (i - half) / (n * mrs.dwell_s)  # noqa: E731
        unit, first, last = "hz", -hz(0), -hz(n - 1)
    else:
        unit, first, last = "points", 0.0, float(max(n, 1) - 1)
    proton = unit == "ppm" and mrs.nucleus == "1H"
    default_range = [0.2, 4.2] if proton else None

    axis = first + (last - first) * np.arange(n) / (n - 1) if n > 1 else np.full(n, first)
    sel = (axis >= default_range[0]) & (axis <= default_range[1]) if default_range else np.ones(n, bool)
    total = spec[sel].sum()
    if abs(total) > 0:
        spec = spec * np.exp(-1j * math.atan2(total.imag, total.real))
    return {
        "voxel": [int(c) for c in voxel],
        "unit": unit,
        "first": float(first),
        "last": float(last),
        "points": n,
        "real": [float(x) for x in spec.real.astype(np.float32)],
        "magnitude": [float(x) for x in np.abs(spec).astype(np.float32)],
        "default_range": default_range,
        "default_part": "real" if proton else "magnitude",
        "transients": k,
    }


def too_long(mrs: Mrs):
    if mrs.points > MAX_POINTS:
        raise PreviewError("too_large", f"Spectrum too long to preview ({mrs.points} points, limit {MAX_POINTS}).")
