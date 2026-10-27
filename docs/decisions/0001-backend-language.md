# ADR 0001 — Backend language: Rust native binary (Python + nibabel kept as test oracle)

Status: accepted · Date: 2026-10-19

## Context

The backend parses a NIfTI file, decompresses it, computes an intensity window
and renders orthogonal slices for the GJS Sushi adapters, which stay in GJS.
The primary use case is *Space → three slices immediately*. Criteria, in order:
cold start, open-to-first-slice latency, navigation smoothness, memory,
correctness of orientation/affine/intensities, NIfTI-1/2 support, robustness to
corrupt files, ease of installation on Ubuntu.

A first backend was written in Python on top of nibabel (121 passing tests).
Before going further we measured it against a Rust prototype.

## Measurements

Machine: cloud container, 4 vCPU, page cache warm (files read once before),
median of 5 cold *processes* per cell. Python 3.13 / nibabel 5.4.2 / numpy 2.5;
Rust 1.97, release + LTO. Synthetic data only (`nifti_preview_ref.synthetic`).
"Ready" = from process start to three windowed RGB slices ready to send.

Startup alone: Rust binary 1.5 ms; Python bare interpreter 14 ms;
interpreter + `import numpy, nibabel` 203 ms.

### Realistic 1 mm T1 (256³ int16, MRI-like noise, 18.5 MB .nii.gz)

| stage (ms) | Python + nibabel | + indexed_gzip | Rust miniz_oxide | Rust zlib-rs | **Rust libdeflate** |
|---|---|---|---|---|---|
| imports | 156 | 159 | – | – | – |
| header | 46 | 51 | 0.2 | 0.1 | 0.2 |
| decompress | 234 | 233 | 217 | 178 | **110** |
| window (+ label detection in Python) | 138 | 78 | 35 | 36 | 35 |
| 3 centre slices | 3.7 | 3.2 | 2.5 | 2.6 | 2.5 |
| **ready (+ interpreter start)** | **~580** | ~552 | 255 | 216 | **151** |
| per slice, steady state | 0.93 | 0.95 | 0.61 | 0.63 | 0.67 |
| peak RSS (MB) | 144 | 144 | 56 | 56 | 56 |

### Other files (ready ms / peak RSS MB)

| file | Python + nibabel | Rust libdeflate |
|---|---|---|
| 256³ int16, smooth phantom, 5.6 MB .nii.gz | 386 / 157 | 74 / 52 |
| same, NIfTI-2 .nii.gz | 394 / 157 | 81 / 52 |
| 256³ int16 .nii (uncompressed, mmap) | 305 / 142 | 27 / 51 |
| 182×218×182 float32, 10.5 MB .nii.gz | 391 / 119 | 111 / 46 |
| 4D 96×96×60×200 int16, 87 MB .nii.gz (volume 0 only) | 287 / 67 | 15 / 12 |

### Other libraries

* **nifti-rs 0.18** (pure Rust): reads NIfTI-1.1 only. It rejects NIfTI-2
  ("Invalid NIfTI-1 file") and stores dims as `u16`. `NiftiHeader::affine()`
  **panics** on a qform file with a negative pixdim, where nibabel takes the
  absolute value and warns. It detects gzip from the extension, reads the whole
  volume into a `Vec` without mmap and inflates with miniz_oxide. Full read of
  the realistic T1: 313 ms.
* **nifti_clib 3.0.1** (C, Ubuntu `libnifti2-dev`): load of the realistic T1
  takes 230–260 ms (zlib through znzlib, the same speed as Python). A
  **truncated .nii.gz is reported as loaded successfully**: the missing bytes
  are filled with zeros and only a warning goes to stderr. `nifti_image_load`
  reads every 4D volume (1.3 s for the BOLD file). Using it would mean FFI,
  `unsafe` code and a C dependency. It would only give us header decoding,
  which is the easy part. Its NIfTI-2 API (`nifti2_io`) was not validated in
  this prototype.
* **indexed_gzip** makes no difference for a 3D preview. The three orthogonal
  centre slices touch the whole volume, so the whole stream must be inflated
  anyway. nibabel already stops early on 4D files. indexed_gzip would only help
  later, when navigating 4D timepoints back and forth.
* **python-isal** (ISA-L inflate) could not be plugged into nibabel's opener
  cleanly (the monkey-patch had no measurable effect). No conclusion.

## Decision

Use a **Rust native binary** (`nifti-preview`) as the backend, with our own small
NIfTI-1/NIfTI-2 reader:

* **Header**: a fixed 348- or 540-byte struct. Both byte orders, strict
  validation, checked arithmetic and no panics on untrusted input. Semantics
  follow nibabel: sform if `sform_code > 0`, else qform with qfac, else
  unknown; `scl_slope` of 0 or NaN means identity; abs(pixdim) with a warning.
* **Decompression**: libdeflate for whole-file inflate of 3D volumes. The output
  size is computed from the header, not trusted from the gzip trailer.
  flate2 (zlib-rs) is used for streaming: the header, volume 0 of 4D files, and
  a fallback for > 4 GiB or multi-member streams. Uncompressed `.nii` files are
  memory-mapped.
* **Orientation**: a port of `nibabel.orientations.io_orientation` (polar
  decomposition, then greedy axis assignment). View plans, edge labels,
  windowing, label detection and palette are ported from the current Python
  backend.
* **Protocol**: unchanged JSON lines, except that slice pixels travel as a
  binary payload after the JSON header line (`"bytes": N`) instead of base64.
  GJS wraps it with `GdkPixbuf.Pixbuf.new_from_bytes` without copying.

The existing **Python + nibabel implementation becomes the test oracle** and is
not shipped to users. CI generates a corpus of synthetic files with nibabel:
every storage orientation, random and sheared affines, all datatypes, both byte
orders, scaling, qform-only, NIfTI-1/2, 2D/4D/5D, NaN/Inf and labels. The Rust
binary must reproduce the oracle's metadata, affine (1e-6), view plans and
rendered slices (±1 grey level). Robustness tests mutate and truncate valid
files: the binary must return an error, never panic or hang.

## Consequences

* Ready time on a realistic T1 `.nii.gz` drops from about 580 ms to about 150 ms.
  The remaining 110 ms is inflate, which is sequential in standard gzip.
  Uncompressed and 4D files become near instant. Memory use is 2.5–5× lower.
* Scrolling smoothness does not depend on the backend language: about 1 ms per
  slice either way, well under a 16 ms frame. IPC and GTK drawing dominate.
* Installation needs no virtualenv. Users either build with
  `sudo apt install cargo build-essential` (Ubuntu 26.04 ships rustc 1.93) or
  use a prebuilt x86_64/aarch64 binary from GitHub Releases once the repository
  is public. Ubuntu 24.04's packaged rustc (1.75) is too old: use rustup or a
  prebuilt binary there.
* We own the NIfTI parsing code, about 500–700 lines. Differential tests against
  nibabel keep it honest.
* Not measured: cold-disk-cache start (first preview after boot). It would
  penalise Python much more than a 600 KB binary.

## Outcome

Implemented as described. The differential suite (108 synthetic files after the functional counter-audit, every mode,
colormap and view) passes against nibabel; 7 200 mutated files never crash the
backend. Measured in the final implementation: 151 ms from process start to three
slices for the 18.5 MB T1, 36 ms for `.nii`, 22 ms for the 4D file
([BENCHMARKS.md](../BENCHMARKS.md)). The differential tests also caught a nibabel
quirk: with `vox_offset = 0` nibabel reads voxel data from byte 0 of the file; the
Rust backend reads it after the header.
