# Architecture

```
Nautilus ──Space──▶ GNOME Sushi (gjs) ──loads──▶ viewer plugin (GJS)
   (D-Bus ShowFile)                                 │  legacy/niftiPreview.js   Sushi 46–50, GTK 3
                                                    │  v1/niftiPreview.js       Sushi 51+,  GTK 4
                                                    │  common/niftiPreview/     shared client + view state
                                                    ▼  Gio.Subprocess, stdin/stdout, JSON lines + raw pixels
                                       nifti-preview serve   (Rust, one process per preview)

a terminal ◀──escape sequences──▶ nitty   (Rust, backend/terminal/: the same library, linked in)
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
* `image.rs` — the format-neutral `ImageHeader` that readers produce (shape,
  datatype, scaling, RAS affine, data layout, format-specific fields), and the
  detection of the format from the content (never the file name: GNOME's
  thumbnail sandbox renames every file).
* `formats/` — `mgh.rs` (FreeSurfer MGH/MGZ: big-endian header, vox2ras with
  `c_ras` at voxel `dims / 2`, scan parameters after the data), `mif.rs` (MRtrix:
  text header, `layout`, `transform`, `Bit`, data files of a `.mih`), `nrrd.rs`
  (attached and detached data, raw, gzip, bzip2, ascii and hex, spaces converted
  to RAS, axes without direction as volumes), `bzip2.rs` (bzip2 decompression,
  written for this project), `metaimage.rs` (ITK MetaImage: `Key = Value`
  header, ITK geometry, zlib data decoded whole, channels). Spatial axes are put in their storage order by changing the affine
  (no copy); volumes interleaved with voxels, bits and detached files are read
  whole (bounded by the image limit) and each volume is copied out in order.
* `formats/dicom/` — `parse.rs` (data sets: meta group, transfer syntaxes,
  implicit and explicit VR, both byte orders, deflate, sequences, encapsulated
  fragments; stops at the pixel data), `series.rs` (where each frame is:
  classic files, enhanced functional groups, mosaics, RT dose grids; stacking
  into slices and volumes; LPS to RAS), `catalog.rs` (the series of a folder,
  DICOMDIR records), `pixels.rs` (native frames, colour, palettes, rescale),
  `rle.rs`, `jpeg.rs`, `jpegls.rs`, `j2k.rs` (OpenJPEG), `about.rs` (what a
  file without an image holds: a report's text, structures, a plan's beams, a
  waveform's channels, sent with the refusal for the viewer's panel), and
  `mod.rs`: the header and a decoding `Plan` (which file, frame and tile makes
  each slice) that the volume store runs once, frames decoded in parallel.
* `storage.rs` — local disk or network file system (`statfs` magic, then the
  mount type in `/proc/self/mountinfo` for FUSE); `progress.rs` — the progress
  of a long load (bytes read or frames decoded), reported by the readers of the
  thread doing it, and its cancellation. The server runs such a load on a
  helper thread that owns the volume until it is done.
* `store.rs` — the other volumes of a 4D image: positional reads (or one shared
  mapping) for uncompressed files, and for gzip a background thread that inflates
  the stream once and hands over each volume as it arrives (all kept when they
  fit in the image limit, otherwise decoded towards the volume asked).
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
* `view.rs` — what a viewer needs besides the slices, as typed functions:
  where the cursor starts (the image a DICOM series was opened on, the centre
  of a sparse segmentation's labels), the value under a grid point with the
  name, voxel count and volume of its label, the list of labels, a cursor
  carried to another display mode's grid, the DICOM series next to an image,
  the LRU slice cache, and a volume read on a helper thread. `server.rs` and
  nitty both build on it.
* `panel.rs` — the text of the side panel, of the readout and of the volume
  line, ported from `viewState.js` with JavaScript's number formatting, for
  viewers written in Rust (`tests/gjs` compares the two).
* `server.rs` — the protocol (below) over `view.rs`, `catch_unwind` per
  request, `PR_SET_PDEATHSIG` so that it dies with Sushi.
* `sandbox.rs` — applied by `serve`, `thumbnail` and nitty: inherited descriptors closed, memory cap
  (`RLIMIT_DATA`), no new privileges, no sockets (seccomp), Landlock read-only
  (see [SECURITY.md](SECURITY.md)).
* `main.rs` — `serve`, plus `info`, `render` and `bench` for debugging.

`backend/` is also a Cargo workspace whose default member is this crate:
`cargo build` there builds `nifti-preview` only, and the terminal viewer
(`backend/terminal/`) shares the lock file and the release profile.

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

## Terminal viewer (nitty)

`backend/terminal/` builds `nitty`, which links the library: every format,
orientation, window, palette and label colour comes from it; nitty only puts
its RGB8 slices on a terminal and handles the interaction. It depends on
`libc` and `serde_json`, both already in the lock file (memmap2, the library):
no TUI crate, no terminfo.

* `viewer.rs` — an opened image as both modes show it: the volume, the display
  mode and colormap, the three view plans, the cursor (a grid point), the
  cache of drawn slices.
* `picture.rs` — `render_slice` (cached) → `thumbnail::resample` to the pixels
  the terminal has for the view (nearest voxel for labels) → the crosshair
  drawn at that size.
* `layout.rs` — boxes of a title row, a row of orientation letters above and
  below the image and a column on each side; the views share one physical
  scale and are put in a row, a column or two rows, whichever shows them
  largest (the quick look: always a row).
* `blocks.rs`, `graphics/` — the encoders: half blocks `▄` (upper pixel as
  background, lower as foreground; a space when both are equal), characters by
  brightness, Kitty's graphics protocol (the PNG of `png.rs`, transmitted in the
  escape codes, `t=d`, chunks of 4096 bytes, sized in cells), iTerm2's inline
  images, Sixel (an exact palette when the colours fit the registers, median
  cut otherwise; run-length encoded), and tmux's passthrough for Kitty and
  iTerm2.
* `color.rs`, `screen.rs`, `text.rs` — colours in 24-bit, 256 (as tmux reduces
  them) or 16; a grid of cells written as lines (the quick look) or as the
  difference from the previous frame (the viewer: only changed cells, minimal
  SGR); every character written passes one filter that drops control
  characters, so that a file name or a header field cannot send escape
  sequences.
* `tty.rs`, `input.rs` — the terminal over libc: `/dev/tty` opened once, raw
  mode, the alternate screen, mouse reports (SGR, or X10 from a terminal that
  ignores mode 1006), signals through a pipe, the terminal restored on return,
  on a panic of the viewer's own thread (a panic the library catches on one of
  its threads is held and printed afterwards), on SIGTERM/SIGHUP/SIGINT/SIGQUIT
  and around Ctrl-Z; bytes parsed into keys, mouse events and query answers,
  sequences cut between reads kept until complete.
* `caps.rs` — the queries sent once: Kitty's (a 1 × 1 image, `a=q`), XTVERSION,
  the cell size (`CSI 16 t`) and the text area (`CSI 14 t`), the Sixel colour
  registers, then DA1, which every terminal answers: its reply ends the wait
  after one round trip, over SSH too (at most 1 s, `NITTY_PROBE_MS`; answers
  later than that are dropped from the input, not left to the shell). Kitty
  when it answers OK; iTerm2 when its XTVERSION says so, or when `TERM_PROGRAM`
  or `LC_TERMINAL` name it outside tmux and screen (SSH forwards
  `LC_TERMINAL`); Sixel when DA1 lists 4; half blocks otherwise. In tmux the
  queries reach tmux, which never passes Kitty or iTerm2 images on unasked:
  only its own Sixel, else half blocks. The Linux console and GNU screen get
  DA1 only. Colours: 24-bit when `COLORTERM` or XTVERSION says so; otherwise
  16 for `TERM` `linux`, `vt*` and `ansi`, 256 for Apple's Terminal and GNU
  screen, 24-bit for the rest.
* `glance.rs` — the quick look (`-p`, or any output that is not a terminal):
  a summary line, the views in a row, `--info`, `--legend`; with an image
  protocol, the rows are printed first and each picture drawn into them, so
  that the output scrolls with the shell.
* `app.rs` — the viewer: the image read on a helper thread (`view::Loading`,
  progress shown), keys read together handled before drawing, pictures
  computed again only when what they show changes, images sent again only then
  (each view's encoded sequence kept; with an image protocol the crosshair is
  marked in the margins, so moving it within a slice resends no picture),
  frames inside synchronized updates (`CSI ?2026`), 4D volumes decompressed in
  the background, a panel (metadata, labels counted once per volume, the time
  course of a voxel).
* `main.rs` — options, the mode, and the library's sandbox: applied once the
  terminal is opened and asked, so that its descriptor keeps its ioctls
  (Landlock decides at open time).

## Data flow of one preview

1. Sushi constructs the viewer; the window appears immediately with a spinner.
2. The client spawns `nifti-preview serve` and writes, without waiting:
   `open`, `load`, then one `slice` per view once `open` has answered.
3. `open` parses the header only (≈0.1 ms) → view geometry and edge labels.
4. `load` reads volume 0 (read, mmap, or inflate; all the data for spectroscopy) and computes the window and
   the label detection (on a 1 M-voxel strided sample, three threads). The
   metadata panel is filled once, now, complete; the slices come right after.
   For a 4D image the client then asks for the time course of the cursor voxel,
   and `set_volume` when the slider moves (asked again while a volume is being
   decompressed, see [PROTOCOL.md](PROTOCOL.md)).
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
~/.local/share/thumbnailers/gnome-nifti-preview.thumbnailer   (thumbnails, when enabled)
/usr/local/bin/gnome-nifti-preview-thumbnailer                 (the backend; only with --thumbnails or a yes)
```

The terminal viewer is installed on its own, by `install.sh --terminal` or
`get.sh --terminal`: one file, `$XDG_BIN_HOME/nitty` or `~/.local/bin/nitty`
(`/usr/local/bin/nitty` as root), static on the release archive. Neither
installation reads or removes the other's files.

**Thumbnails.** GNOME Files (gnome-desktop's thumbnail factory) runs the
program of a `.thumbnailer` entry for each file in a bubblewrap sandbox that
mounts `/usr` read-only, a private `/tmp` with the file (named
`gnome-desktop-file-to-thumbnail.<extension>`, so the format is recognised from
the content) and the output, and nothing of the home folder. Hence the copy of
the backend in `/usr/local/bin`, and the options written into the entry's
`Exec` line (`thumbnailer.js`, from the settings): `nifti-preview thumbnail`
draws the centre slice of one view of volume 0 (or the spectrum of a
single-voxel NIfTI-MRS file) at its physical aspect ratio, scaled to the
requested size (average when shrinking, bilinear when enlarging, nearest voxel
for segmentations), and writes a PNG.

Both viewers are always installed, so an Ubuntu upgrade from Sushi 50 to 51 keeps
working. `install.sh` writes every file atomically, removes files that an older
version installed and this one does not ship, and records whether it created the
user MIME database so that `uninstall.sh` restores the exact previous state.

## Decisions

* [ADR 0001 — Rust backend, nibabel as test oracle](decisions/0001-backend-language.md)
* [ADR 0002 — The terminal viewer](decisions/0002-terminal-viewer.md)
