# Architecture

```
Nautilus ──Space──▶ GNOME Sushi (gjs) ──loads──▶ viewer plugin (GJS)
   (D-Bus ShowFile)                                 │  legacy/niftiPreview.js   Sushi 46–50, GTK 3
                                                    │  v1/niftiPreview.js       Sushi 51+,  GTK 4
                                                    │  common/niftiPreview/     shared client + view state
                                                    ▼  Gio.Subprocess, stdin/stdout, JSON lines + raw pixels
                                       nifti-preview serve   (Rust, one process per preview)
```

## Why a Sushi plugin

Space in Nautilus always calls the D-Bus service `org.gnome.NautilusPreviewer`,
which is GNOME Sushi. A standalone application could only be reached through
"Open With"; replacing the service would mean reimplementing every other preview.
Sushi loads user viewers from `~/.local/share/sushi/viewers/` (versions 46–50)
and `~/.local/share/sushi/plugins-1/` (51+), so the plugin needs no patching.

Two facts about Sushi shaped the design:

* Sushi 46–50 import every viewer when they start, **without error handling**: a
  viewer that throws at import time breaks every preview. Our legacy viewer does
  nothing at import time except define its class inside a `try`, and it degrades
  to "no MIME types" on failure. `tests/gjs/sushi_mimehandler.js` loads the real
  Sushi registry and checks that built-in types are unaffected.
* Sushi is a D-Bus service that stays alive between previews (it exits after 12 s
  of inactivity). Every preview owns a backend process, closed when the preview
  is unloaded; Sushi 51 does not call `cleanup()` for a renderer that failed
  before being shown, so the plugin releases it itself.

## Components

**Backend (`backend/`, Rust, binary `nifti-preview`)**
* `header.rs` — NIfTI-1 (348 bytes) and NIfTI-2 (540 bytes) headers, both byte
  orders, nibabel-compatible repairs (invalid xform codes, qfac, zero/negative
  pixdims, unset `vox_offset`), qform quaternion → affine, sform/qform precedence,
  header extensions (the first MiB).
* `source.rs` — the file is opened once (non-blocking, checked with `fstat`)
  and every read uses that descriptor; gzip is detected by magic bytes.
  Uncompressed files up to 256 MB are read, larger ones memory-mapped.
  Single-volume `.nii.gz` are inflated in one call with libdeflate (the output
  size comes from the header, checked against the gzip trailer); 4D+ files and
  multi-member (bgzip) streams are inflated as a stream that stops at the end of
  volume 0. Every buffer is bounded by the image limit and allocated fallibly.
* `orientation.rs` — port of `nibabel.io_orientation` (polar decomposition then
  greedy axis assignment), the display grid (the voxel grid, or a world-aligned
  grid for oblique images, with trilinear and nearest sampling), view plans
  (sliced axis, screen axes, flips, edge labels, pixel size), obliquity.
* `window.rs`, `labels.rs`, `render.rs` — percentile window, label-map detection,
  categorical palette, slice → RGB8 (grayscale, label colours, or the colours of
  an RGB image).
* `mrs.rs` — NIfTI-MRS spectroscopy: detection, metadata from the JSON
  extension, combination of transients, FFT (radix 2, Bluestein for other
  lengths), chemical-shift axis and zero-order phase.
* `server.rs` — the protocol (below), an LRU slice cache (64 MiB), `catch_unwind`
  per request, `PR_SET_PDEATHSIG` so that it dies with Sushi.
* `sandbox.rs` — applied by `serve`: inherited descriptors closed, memory cap
  (`RLIMIT_DATA`), no new privileges, no sockets (seccomp), Landlock read-only
  (see [SECURITY.md](SECURITY.md)).
* `main.rs` — `serve`, plus `info`, `render` and `bench` for debugging.

**Viewers (`sushi/`, GJS)**
* `common/niftiPreview/backendClient.js` — spawns the backend, pipelines requests,
  matches responses by id, reads `bytes`-sized binary payloads (refused before
  any allocation when above the slice limits), maps errors, enforces deadlines,
  closes politely then kills after 1.5 s. After a protocol error it fails every
  request and stops the backend. Legacy module (GLib/Gio only) so that both viewers share it; the
  GTK 4 ES module reaches it through the global `imports` object.
* `common/niftiPreview/viewState.js` — cursor on the display grid, scroll/click → slice
  requests with "latest wins" coalescing (one request in flight per view),
  metadata formatting, spectrum requests.
* `common/niftiPreview/spectrumPlot.js` — the spectrum of a spectroscopy file,
  drawn with Cairo and Pango for both toolkits.
* `common/niftiPreview/noViewer.js` — a small GTK dialog that says which NIfTI
  viewer to install (see MIME below).
* `common/niftiPreview/settings.js` — optional `~/.config/gnome-nifti-preview/settings.ini`
  ([SETTINGS.md](SETTINGS.md)); display options go to the viewers, performance
  options to the backend's environment (`NIFTI_PREVIEW_MAX_BYTES`,
  `NIFTI_PREVIEW_CACHE_BYTES`).
* `legacy/niftiPreview.js` — GTK 3: `Gtk.DrawingArea` + Cairo (NEAREST filter for
  labels and voxels above 6 screen pixels), overlay labels, Sushi OSD toolbar, keyboard shortcuts on the window.
* `v1/niftiPreview.js` — GTK 4: `Gtk.Widget.vfunc_snapshot` with
  `Gdk.MemoryTexture` and `append_scaled_texture` (NEAREST/LINEAR), event
  controllers, `markReady/markFailed/cleanup`.

**MIME (`mime/gnome-nifti-preview.xml`)** — `image/x-nifti` (`*.nii`, alias
`image/x.nifti` as used by libmagic) and `image/x-compressed-nifti` (`*.nii.gz`,
sub-class of `application/gzip`, like `application/x-compressed-tar`). The longest
glob wins, so other `.gz` files keep their type. The sub-class is needed: from a
per-user definition, `*.gz` and `*.nii.gz` both match and GLib breaks the tie
with the content, a gzip stream. It also makes NIfTI files inherit the
applications of gzip archives (Files would extract them), so when no NIfTI
viewer is the default, `install.sh` hides those for `image/x-compressed-nifti`
(`[Removed Associations]` in `mimeapps.list`; `uninstall.sh` restores them).
That is not enough on GNOME: `gnome-mimeapps.list` makes Files the *default*
application of `application/gzip`, and a default inherited this way is not
removed by `[Removed Associations]`. So when it finds no viewer, the installer
also writes a hidden launcher, `gnome-nifti-preview-no-viewer.desktop`
("a NIfTI viewer…", running `noViewer.js`), and makes it the default: the
preview's button reads "Open With a NIfTI viewer…" and explains what to install.
A viewer found by a later install replaces it.

**Reference (`reference/`, Python + nibabel)** — the first implementation of the
backend, kept as a test oracle: CI checks that the Rust backend reproduces it
(metadata, affine, plans, statistics, pixels). It also generates every test
fixture. It is not installed for users.

## Data flow of one preview

1. Sushi constructs the viewer; the window appears immediately with a spinner.
2. The client spawns `nifti-preview serve` and writes, without waiting:
   `open`, `load`, then one `slice` per view once `open` has answered.
3. `open` parses the header only (≈0.1 ms) → view geometry and edge labels.
4. `load` reads volume 0 (read, mmap, or inflate; all the data for spectroscopy) and computes the window and
   the label detection (on a 1 M-voxel strided sample, three threads). The
   metadata panel is filled once, now, complete; the slices come right after.
5. Each `slice` is rendered at native resolution (≈1 ms) and sent as raw RGB;
   the viewer wraps it in a pixbuf/texture without copying it again and scales
   it with the physical voxel aspect ratio.
6. Scrolling and clicking move the cursor; only the latest wanted slice of each
   view is requested. The value under the cursor is fetched the same way.
7. Closing the preview sends `close`, closes stdin, and kills the process if it
   is still alive 1.5 s later.

## Installation layout

```
~/.local/share/gnome-nifti-preview/bin/nifti-preview
~/.local/share/gnome-nifti-preview/gjs/niftiPreview/{backendClient,viewState,settings,spectrumPlot,noViewer}.js
~/.local/share/applications/gnome-nifti-preview-no-viewer.desktop   (only without a viewer)
~/.local/share/gnome-nifti-preview/{manifest.txt,state}
~/.local/share/sushi/viewers/niftiPreview.js        (Sushi 46–50)
~/.local/share/sushi/plugins-1/niftiPreview.js      (Sushi 51+)
~/.local/share/mime/packages/gnome-nifti-preview.xml
```

Both viewers are always installed, so an Ubuntu upgrade from Sushi 50 to 51 keeps
working. `install.sh` writes every file atomically, removes files that an older
version installed and this one does not ship, and records whether it created the
user MIME database so that `uninstall.sh` restores the exact previous state.

## Decisions

* [ADR 0001 — Rust backend, nibabel as test oracle](decisions/0001-backend-language.md)
