# Benchmarks

Machine: cloud container, 4 vCPU (x86_64), files in the page cache (read once
before measuring), synthetic data only. Medians of several cold processes.

These are historical measurements from the initial implementation. They have
not been rerun for the later oblique resampling, spectroscopy and exact
64-bit label paths; they are not latency guarantees for those features.

## Inside GNOME Sushi

Time from the construction of the viewer by Sushi to the moment the metadata,
then the three slices, are on screen (`NIFTI_PREVIEW_DEBUG=1`), GNOME Sushi 46
under Xvfb, Sushi already running:

| File | Metadata | Three slices |
|---|---|---|
| 1 mm T1, 256³ int16 with MRI-like noise, 18.5 MB `.nii.gz` | 15 ms | 167 ms |
| same, uncompressed 33.6 MB `.nii` (read into memory) | 17 ms | 72 ms |
| MNI-sized 182×218×182 float32, 10.5 MB `.nii.gz` | 13 ms | 110 ms |
| 4D 96×96×60×200 int16, 87 MB `.nii.gz` (volume 0 only) | 16 ms | 49 ms |

A real 1 mm MNI-space T1 (181×217×181 float32, 24 MB `.nii.gz`) previewed from the real
Nautilus 50 on Ubuntu 26.04: three slices after 171–250 ms; its segmentations
(uint8, 0.1–0.4 MB) after 55–95 ms. Clicking a label counts its voxels in about 5 ms.

The integration test also measures the 160×192×160 test phantom on each Sushi
version: three slices after 74 ms (Sushi 46), 94 ms (Sushi 50) and 80 ms (Sushi 51).
Scrolling costs about 1 ms of backend time per slice, far below a 16 ms frame.

## Backend stages

`nifti-preview bench FILE` (one process per file, medians of 5):

| File | Header | Decompress | Statistics | 3 slices | Ready | Per slice | Peak RSS |
|---|---|---|---|---|---|---|---|
| T1 256³ int16, 18.5 MB `.nii.gz` | 0.1 ms | 115 ms | 35 ms | 3.5 ms | **151 ms** | 0.84 ms | 53 MB |
| T1 256³ int16, smooth, 5.6 MB `.nii.gz` | 0.1 ms | 46 ms | 30 ms | 2.4 ms | **79 ms** | 0.82 ms | 49 MB |
| same, NIfTI-2 `.nii.gz` | 0.1 ms | 50 ms | 36 ms | 2.4 ms | **89 ms** | 0.97 ms | 49 MB |
| same, uncompressed `.nii` | 0.03 ms | – (read into memory) | 33 ms | 2.7 ms | **36 ms** | 0.81 ms | 49 MB |
| 182×218×182 float32, 10.5 MB `.nii.gz` | 0.1 ms | 74 ms | 26 ms | 1.5 ms | **103 ms** | 0.57 ms | 43 MB |
| 4D 96×96×60×200 int16, 87 MB `.nii.gz` | 0.1 ms | 4.7 ms | 17 ms | 0.3 ms | **22 ms** | 0.06 ms | 13 MB |

Process start-up is 1.5 ms. Decompression (libdeflate, single-threaded,
≈ 300 MB/s of output here) dominates for `.nii.gz`; standard gzip cannot be
decompressed in parallel. For the 4D file only volume 0 (1.1 MB of voxel data) is
inflated; decompressing the whole 221 MB stream takes 1.4 s with zlib.

## Compared with Python + nibabel

The first version of the backend used Python, nibabel and numpy. Same stages,
same files ([ADR 0001](decisions/0001-backend-language.md)):

| File | Python + nibabel ready | Rust ready | Peak RSS Python / Rust |
|---|---|---|---|
| T1 256³, 18.5 MB `.nii.gz` | ≈ 580 ms | 151 ms | 144 / 53 MB |
| T1 256³ `.nii` | ≈ 305 ms | 36 ms | 142 / 49 MB |
| 4D 87 MB `.nii.gz` | ≈ 287 ms | 22 ms | 67 / 13 MB |

Python's start-up alone (interpreter + `import numpy, nibabel`) is about 200 ms;
`indexed_gzip` does not help a 3D preview (233 ms vs 234 ms of decompression),
because three orthogonal slices touch the whole volume.

## Reproduce

```sh
cd backend && cargo build --release
python3 -m venv /tmp/venv && /tmp/venv/bin/pip install -e ../reference
/tmp/venv/bin/nifti-preview-ref make-fixtures /tmp/fixtures      # test phantoms
target/release/nifti-preview bench /tmp/fixtures/*.nii*
```
