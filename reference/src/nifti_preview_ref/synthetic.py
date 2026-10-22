"""Synthetic NIfTI generators for tests, benchmarks and screenshots.

Everything here is generated procedurally; no real subject data is used.
Arrays are first built in RAS+ voxel order (axis 0 -> Right, 1 -> Anterior,
2 -> Superior) and can then be stored in any other orientation with
``reorient()``, which keeps the world-space content identical.
"""

from __future__ import annotations

import gzip
import json
import os
import struct

import nibabel as nib
import numpy as np
from nibabel.orientations import apply_orientation, axcodes2ornt, inv_ornt_aff, ornt_transform

MARKER_VALUES = {"R": 300.0, "A": 500.0, "S": 700.0}


def ras_affine(shape, spacing=(1.0, 1.0, 1.0)) -> np.ndarray:
    spacing = np.asarray(spacing, dtype=float)
    affine = np.diag(list(spacing) + [1.0])
    affine[:3, 3] = -(np.asarray(shape[:3]) - 1) / 2.0 * spacing
    return affine


def marker_phantom(shape=(40, 48, 36), spacing=(1.0, 1.0, 1.0)):
    """Asymmetric phantom: a body (100) and three cubic markers near the
    Right (300), Anterior (500) and Superior (700) ends. Returns (data, affine)."""
    nx, ny, nz = shape
    data = np.zeros(shape, dtype=np.float32)
    data[nx // 8: nx - nx // 8, ny // 8: ny - ny // 8, nz // 8: nz - nz // 8] = 100.0
    s = max(2, min(shape) // 8)
    cx, cy, cz = nx // 2, ny // 2, nz // 2
    data[nx - nx // 8 - s: nx - nx // 8, cy - s // 2: cy + s // 2 + 1, cz - s // 2: cz + s // 2 + 1] = MARKER_VALUES["R"]
    data[cx - s // 2: cx + s // 2 + 1, ny - ny // 8 - s: ny - ny // 8, cz - s // 2: cz + s // 2 + 1] = MARKER_VALUES["A"]
    data[cx - s // 2: cx + s // 2 + 1, cy - s // 2: cy + s // 2 + 1, nz - nz // 8 - s: nz - nz // 8] = MARKER_VALUES["S"]
    return data, ras_affine(shape, spacing)


def reorient(data, affine, codes: str):
    """Store RAS-ordered ``data`` in the voxel orientation given by ``codes``."""
    transform = ornt_transform(axcodes2ornt("RAS"), axcodes2ornt(tuple(codes)))
    new_data = apply_orientation(data, transform)
    new_affine = affine @ inv_ornt_aff(transform, data.shape)
    return np.ascontiguousarray(new_data), new_affine


def rotate_affine(affine, degrees: float, axis: int = 2) -> np.ndarray:
    t = np.radians(degrees)
    c, s = np.cos(t), np.sin(t)
    r = np.eye(4)
    i, j = [a for a in range(3) if a != axis]
    r[i, i], r[i, j], r[j, i], r[j, j] = c, -s, s, c
    return r @ affine


def head_phantom(shape=(160, 192, 160), spacing=(1.0, 1.0, 1.0), seed: int = 0):
    """A brain-like synthetic head (scalp, skull, grey/white matter, ventricles)
    with a small bright marker in the right-anterior-superior quadrant."""
    rng = np.random.default_rng(seed)
    nx, ny, nz = shape
    x, y, z = np.meshgrid(
        (np.arange(nx) - nx / 2) / (nx / 2),
        (np.arange(ny) - ny / 2) / (ny / 2),
        (np.arange(nz) - nz / 2) / (nz / 2),
        indexing="ij",
    )
    r_head = np.sqrt((x / 0.92) ** 2 + (y / 0.95) ** 2 + ((z + 0.05) / 0.9) ** 2)
    r_brain = np.sqrt((x / 0.8) ** 2 + (y / 0.84) ** 2 + ((z + 0.02) / 0.76) ** 2)
    folds = 0.035 * np.sin(14 * np.arctan2(y, x)) * np.cos(11 * np.arctan2(z, np.hypot(x, y)))
    data = np.zeros(shape, dtype=np.float32)
    data[r_head < 1.0] = 55.0  # scalp
    data[(r_head < 0.93) & (r_brain >= 1.0)] = 12.0  # skull
    brain = r_brain < 1.0
    data[brain] = 72.0  # grey matter
    data[r_brain + folds < 0.82] = 108.0  # white matter
    for side in (-1, 1):
        vent = np.sqrt(((x - 0.12 * side) / 0.07) ** 2 + ((y + 0.05) / 0.32) ** 2 + ((z - 0.08) / 0.14) ** 2) < 1.0
        data[vent] = 22.0
    data[np.sqrt((x - 0.42) ** 2 + (y - 0.38) ** 2 + (z - 0.38) ** 2) < 0.06] = 160.0  # R-A-S marker
    data += rng.normal(0, 2.5, size=shape).astype(np.float32) * (data > 0)
    return data, ras_affine(shape, spacing)


def brain_phantom(shape=(176, 208, 176), spacing=(1.0, 1.0, 1.0), seed: int = 0):
    """Brain-like synthetic T1 image and matching segmentation (no subject data).

    Returns (t1, labels, affine). Labels: 1 CSF, 2 cortex, 3 white matter,
    4 deep grey nuclei, 5 ventricles, 6 cerebellum, 7 brainstem.
    """
    rng = np.random.default_rng(seed)
    nx, ny, nz = shape
    x, y, z = np.meshgrid(
        (np.arange(nx) - nx / 2) / (nx / 2),
        (np.arange(ny) - ny / 2) / (ny / 2),
        (np.arange(nz) - nz / 2) / (nz / 2),
        indexing="ij",
    )
    theta = np.arctan2(y, x)
    phi = np.arctan2(z, np.hypot(x, y))
    gyri = (0.030 * np.sin(17 * theta + 3 * np.sin(5 * phi)) * np.cos(13 * phi)
            + 0.018 * np.sin(29 * theta - 7 * phi) + 0.012 * np.cos(23 * phi + 4 * theta))
    head = np.sqrt((x / 0.93) ** 2 + (y / 0.96) ** 2 + ((z + 0.04) / 0.92) ** 2)
    skull_in = np.sqrt((x / 0.85) ** 2 + (y / 0.88) ** 2 + ((z + 0.02) / 0.83) ** 2)
    cerebrum = np.sqrt((x / 0.80) ** 2 + (y / 0.84) ** 2 + ((z - 0.06) / 0.70) ** 2) + gyri
    # corpus callosum: a white-matter arch across the midline; the interhemispheric
    # fissure only cuts the hemispheres above it.
    arch = np.sqrt(np.clip(1 - (y / 0.46) ** 2, 0, None))
    callosum = (np.abs(z - (0.10 + 0.20 * arch)) < 0.035) & (np.abs(y) < 0.46) & (np.abs(x) < 0.30)
    fissure = (np.abs(x) < 0.014) & (cerebrum > 0.84) & (z > -0.1)
    midline = np.abs(x) < 0.03
    cbl = np.sqrt((x / 0.42) ** 2 + ((y + 0.55) / 0.26) ** 2 + ((z + 0.52) / 0.22) ** 2)
    taper = np.clip(z + 0.95, 0, None)
    stem = np.sqrt((x / (0.07 + 0.05 * taper)) ** 2 + ((y + 0.16) / (0.08 + 0.04 * taper)) ** 2) < 1.0
    stem &= (z < -0.05) & (z > -0.97)

    labels = np.zeros(shape, dtype=np.int16)
    labels[skull_in < 1.0] = 1  # CSF everywhere inside the skull first
    brain = (cerebrum < 1.0) & ~fissure
    labels[brain] = 2
    labels[brain & (cerebrum < 0.90) & ~midline] = 3
    labels[callosum & (cerebrum < 0.9)] = 3   # medial surface is cortex except the callosum
    for side in (-1, 1):
        caudate = np.sqrt(((x - 0.16 * side) / 0.07) ** 2 + ((y - 0.12) / 0.16) ** 2 + ((z - 0.10) / 0.10) ** 2) < 1
        putamen = np.sqrt(((x - 0.30 * side) / 0.07) ** 2 + ((y - 0.02) / 0.18) ** 2 + ((z - 0.0) / 0.12) ** 2) < 1
        thalamus = np.sqrt(((x - 0.09 * side) / 0.09) ** 2 + ((y + 0.12) / 0.13) ** 2 + ((z - 0.02) / 0.10) ** 2) < 1
        labels[caudate | putamen | thalamus] = 4
        ventricle = np.sqrt(((x - 0.10 * side) / 0.055) ** 2 + ((y + 0.02) / 0.34) ** 2
                            + ((z - 0.20 - 0.08 * np.cos(2.5 * y)) / 0.09) ** 2) < 1
        labels[ventricle] = 5
    third = (np.abs(x) < 0.012) & (y > -0.22) & (y < 0.06) & (z > -0.12) & (z < 0.08)
    labels[third] = 5
    folia = 0.04 * np.sin(40 * z + 6 * y)
    labels[(cbl + folia < 1.0)] = 6
    labels[(cbl + folia < 1.0) & (cbl < 0.75) & (np.sin(40 * z + 6 * y) > 0.3)] = 3
    labels[stem & (labels != 5)] = 7

    t1 = np.zeros(shape, dtype=np.float32)
    t1[head < 1.0] = 95.0                       # scalp (fat is bright on T1)
    t1[(head < 0.96) & (skull_in >= 1.0)] = 12.0  # skull
    t1[(head < 0.94) & (head > 0.925) & (skull_in >= 1.0)] = 55.0  # marrow
    t1[labels == 1] = 18.0
    t1[labels == 2] = 68.0
    t1[labels == 3] = 108.0
    t1[labels == 4] = 78.0
    t1[labels == 5] = 14.0
    t1[labels == 6] = 72.0
    t1[labels == 7] = 98.0
    # gentle bias field and noise, like a real acquisition
    t1 *= (1.0 + 0.06 * x - 0.04 * z).astype(np.float32)
    t1 += rng.normal(0, 2.2, size=shape).astype(np.float32) * (t1 > 0)
    labels[labels == 1] = 1
    return t1, labels, ras_affine(shape, spacing)


def label_phantom(shape=(64, 72, 56)):
    """Integer parcellation: background 0 and labels 1..6 by region."""
    nx, ny, nz = shape
    x, y, z = np.meshgrid(np.linspace(-1, 1, nx), np.linspace(-1, 1, ny), np.linspace(-1, 1, nz), indexing="ij")
    inside = x**2 + y**2 + z**2 < 0.8
    labels = np.zeros(shape, dtype=np.int16)
    labels[inside & (x >= 0) & (y >= 0)] = 1
    labels[inside & (x < 0) & (y >= 0)] = 2
    labels[inside & (x >= 0) & (y < 0)] = 3
    labels[inside & (x < 0) & (y < 0)] = 4
    labels[inside & (z > 0.4)] = 5
    labels[x**2 + y**2 + z**2 < 0.05] = 6
    return labels, ras_affine(shape)


def save(path, data, affine, version: int = 1, dtype=None, slope=None, inter=None,
         qform_code: int = 1, sform_code: int = 1, intent: str | None = None, zooms=None, units: str = "mm"):
    data = np.asarray(data)
    if dtype is not None:
        data = data.astype(dtype)
    cls = nib.Nifti2Image if version == 2 else nib.Nifti1Image
    explicit = {"dtype": data.dtype} if data.dtype in (np.int64, np.uint64) else {}
    img = cls(data, affine if (qform_code or sform_code) else None, **explicit)
    img.set_qform(affine if qform_code else None, code=int(qform_code))
    img.set_sform(affine if sform_code else None, code=int(sform_code))
    if intent is not None:
        img.header.set_intent(intent)
    if zooms is not None:
        img.header.set_zooms(zooms)
    img.header.set_xyzt_units(units, "sec")
    nib.save(img, os.fspath(path))
    if slope is not None or inter is not None:
        # nibabel recomputes scl_slope/scl_inter on save; patch the two fields in
        # place so that the raw values are kept and the requested scaling applies.
        set_scaling(os.fspath(path), 1.0 if slope is None else slope, 0.0 if inter is None else inter)
    return path


# Brain metabolites (ppm, amplitude): NAA, creatine, choline, myo-inositol, lactate.
METABOLITES = ((2.01, 1.0), (3.03, 0.7), (3.21, 0.55), (3.56, 0.3), (1.33, 0.15))


def mrs_fid(n: int, sf_mhz: float, dwell: float, peaks=METABOLITES, centre: float = 4.65,
            phase: float = 0.0, gain: float = 1.0, t2: float = 0.08) -> np.ndarray:
    """Complex FID of peaks at the given shifts (``ppm = centre - f / SF``)."""
    t = np.arange(n) * dwell
    fid = np.zeros(n, dtype=np.complex128)
    for ppm, amp in peaks:
        fid += amp * np.exp(2j * np.pi * (centre - ppm) * sf_mhz * t)
    return gain * np.exp(1j * phase) * np.exp(-t / t2) * fid


def save_mrs(path, data, meta: dict | None, dwell: float, affine=None, intent_name: str = "mrs_v0_3",
             version: int = 2, extension: bytes | None = None):
    """NIfTI-MRS file: complex data, dwell time in pixdim[4], JSON extension (code 44)."""
    data = np.asarray(data)
    cls = nib.Nifti2Image if version == 2 else nib.Nifti1Image
    img = cls(data, affine)
    if affine is None:
        img.set_qform(None, code=0)
        img.set_sform(None, code=0)
    img.header.set_intent(0, name=intent_name)
    zooms = list(img.header.get_zooms())
    zooms[3] = dwell
    img.header.set_zooms(zooms)
    img.header.set_xyzt_units("mm", "sec")
    content = extension if extension is not None else (json.dumps(meta).encode() if meta is not None else None)
    if content is not None:
        img.header.extensions.append(nib.nifti1.Nifti1Extension(44, content))
    nib.save(img, os.fspath(path))
    return path


def svs_phantom(n: int = 1024, coils: int = 4, averages: int = 2, sf_mhz: float = 123.2, dwell: float = 1 / 2000):
    """Single-voxel 1H spectrum: coils with different phases and gains, averages."""
    rng = np.random.default_rng(3)
    phases = rng.uniform(-np.pi, np.pi, coils)
    gains = rng.uniform(0.3, 1.0, coils)
    data = np.zeros((1, 1, 1, n, coils, averages), dtype=np.complex64)
    for c in range(coils):
        for a in range(averages):
            noise = 0.01 * (rng.normal(size=n) + 1j * rng.normal(size=n))
            data[0, 0, 0, :, c, a] = mrs_fid(n, sf_mhz, dwell, phase=phases[c], gain=gains[c]) + noise
    return data


def mrsi_phantom(shape=(6, 5, 3), n: int = 256, sf_mhz: float = 49.86, dwell: float = 1 / 4000):
    """31P spectroscopic imaging: phosphocreatine at 0 ppm, inorganic phosphate
    moving with the voxel, amplitude growing towards the centre."""
    data = np.zeros(tuple(shape) + (n,), dtype=np.complex64)
    for x, y, z in np.ndindex(*shape):
        r = np.hypot(x - shape[0] / 2, y - shape[1] / 2)
        pi = 4.8 + 0.2 * x - 0.1 * y
        data[x, y, z] = mrs_fid(n, sf_mhz, dwell, ((0.0, 1.0), (pi, 0.4), (-2.5, 0.3)), centre=0.0,
                                gain=1.0 / (1.0 + 0.3 * r) * (1 + 0.1 * z), t2=0.03)
    return data


def set_scaling(path: str, slope: float, inter: float) -> None:
    """Overwrite scl_slope/scl_inter of a little-endian single-file NIfTI."""
    opener = gzip.open if path.lower().endswith(".gz") else open
    with opener(path, "rb") as fh:
        blob = bytearray(fh.read())
    if struct.unpack("<i", blob[:4])[0] == 540:
        blob[176:192] = struct.pack("<dd", slope, inter)
    else:
        blob[112:120] = struct.pack("<ff", slope, inter)
    with opener(path, "wb") as fh:
        fh.write(bytes(blob))


def make_fixtures(directory, large: bool = True) -> dict[str, str]:
    """Write a set of example files (used by integration tests and screenshots)."""
    os.makedirs(directory, exist_ok=True)
    out = {}

    def p(name):
        out[name] = os.path.join(directory, name)
        return out[name]

    shape = (160, 192, 160) if large else (64, 76, 64)
    head, aff = head_phantom(shape)
    save(p("sub-001_T1w.nii.gz"), head, aff, dtype=np.int16)
    save(p("sub 001 with spaces.nii"), head, aff, dtype=np.int16)
    lps, lps_aff = reorient(head, aff, "LPS")
    save(p("sub-001_T1w_LPS.nii.gz"), lps, lps_aff, dtype=np.int16)
    labels, laff = label_phantom()
    save(p("sub-001_dseg.nii.gz"), labels, laff, dtype=np.int16)
    if large:
        brain, blabels, baff = brain_phantom()
        save(p("sub-01_T1w.nii.gz"), brain, baff, dtype=np.int16)
        save(p("sub-01_dseg.nii.gz"), blabels, baff, dtype=np.int16)
    small, saff = head_phantom((64, 76, 64))
    vol4d = np.stack([small * (1 + 0.01 * t) for t in range(12)], axis=-1)
    save(p("sub-001_bold.nii.gz"), vol4d, saff, dtype=np.int16, zooms=(1.0, 1.0, 1.0, 2.0))
    # Spectroscopy (NIfTI-MRS): a single-voxel 1H spectrum and 31P imaging.
    meta = {"SpectrometerFrequency": [123.2], "ResonantNucleus": ["1H"], "EchoTime": 0.03, "RepetitionTime": 2.0,
            "dim_5": "DIM_COIL", "dim_6": "DIM_DYN"}
    save_mrs(p("sub-001_svs.nii.gz"), svs_phantom(), meta, 1 / 2000, affine=np.eye(4))
    save_mrs(p("sub-001_mrsi.nii.gz"), mrsi_phantom((8, 8, 4)),
             {"SpectrometerFrequency": [49.86], "ResonantNucleus": ["31P"]}, 1 / 4000, affine=np.diag([20.0, 20.0, 25.0, 1.0]))
    # Tilted acquisition (shown resampled onto the anatomical axes) and colour.
    save(p("sub-001_T1w_oblique.nii.gz"), small, rotate_affine(saff, 15.0, axis=0), dtype=np.int16)
    rgb = np.zeros(small.shape, dtype=[("R", "u1"), ("G", "u1"), ("B", "u1")])
    scaled = (255 * small / max(float(small.max()), 1.0)).astype(np.uint8)
    rgb["R"], rgb["G"], rgb["B"] = scaled, scaled // 2, np.linspace(0, 255, small.shape[2]).astype(np.uint8)[None, None, :]
    nib.save(nib.Nifti1Image(rgb, saff), p("sub-001_rgb.nii.gz"))
    with open(p("not-a-nifti.nii.gz"), "wb") as fh:
        fh.write(b"this is not a gzip stream at all\n" * 8)
    with open(p("sub-001_T1w.nii.gz"), "rb") as src, open(p("truncated.nii.gz"), "wb") as dst:
        dst.write(src.read(4096))
    return out
