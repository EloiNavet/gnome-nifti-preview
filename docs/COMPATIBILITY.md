# Compatibility

Status as of version 0.2.0. "Tested" means a test was run on that
configuration; the evidence column says which one. The automated GUI tests run
the real, distribution-packaged GNOME Sushi under Xvfb (X11, no window manager)
and talk to it over D-Bus exactly like Nautilus does; the real Nautilus 50 was
driven the same way in an Ubuntu 26.04 root. Nothing here runs GNOME Shell or a
Wayland compositor.

## Desktop

| Platform | Status | Evidence |
|---|---|---|
| Ubuntu 26.04, GNOME Sushi 50.0 (GTK 3) | **Tested** | `tests/integration/sushi_xvfb.sh` in an Ubuntu 26.04 root (all checks pass); CI job "Ubuntu 26.04, Sushi 50" |
| Ubuntu 24.04, GNOME Sushi 46.0 (GTK 3) | **Tested** | same script on Ubuntu 24.04; CI job "Ubuntu 24.04, Sushi 46"; real Sushi 46 renderer registry (`tests/gjs/sushi_mimehandler.js`) |
| Ubuntu 26.10, GNOME Sushi 51.0 (GTK 4, plugin API 1) | **Tested, experimental** | same script in an Ubuntu 26.10 root (all checks pass); CI job allowed to fail |
| Nautilus 50 pressing Space | **Tested** | on the author's Ubuntu 26.04 desktop (GNOME 50, Wayland), with real data (a build from before the security review); and real Nautilus 50.2 + Sushi 50 in an Ubuntu 26.04 root under Xvfb, as an ordinary user, with this version installed by `get.sh`: Space opens the preview, ← → switch files (held down too: at most one backend runs), the metadata panel appears once complete, Space closes it and the backend exits; installing while Files is open needs no restart. CI exercises the same D-Bus call (`NautilusPreviewer2.ShowFile`) on 24.04, 26.04 and 26.10 |
| Wayland session (GNOME Shell) | **Tested by hand** | the author's Ubuntu 26.04 desktop (26.04 has no X11 session), with a build from before the security review; not automated |
| Other distributions with Sushi 46–51 | **Expected to work, not tested** | nothing Ubuntu-specific except the dependency hints in `install.sh` |
| Flatpak Sushi | **Unsupported** | the sandboxed previewer cannot see `~/.local/share/sushi` |
| Sushi ≤ 45 | **Unsupported** (untested) | — |

What the GUI test checks on each Sushi version: the preview window opens; three
slices are drawn; one backend process runs; scrolling and clicking change the
views; the orientation shortcut works; Space closes the window and the backend
exits; segmentation, 4D, file name with spaces and LPS-stored files open; an
invalid file shows Sushi's error page; a truncated file keeps its metadata with a
message (a DICOM report or a surface overlay shows what it holds instead of an
error page); PNG and text previews still work and start no backend; 20 open/close
cycles leave no backend and no zombie; no JavaScript error is logged.

## Installation

| Path | Status | Evidence |
|---|---|---|
| One command (`get.sh`, prebuilt backend) | **Tested** | `tests/install/test_get.sh` against a local copy of a release (curl, wget only, upgrade, both removal paths, corrupted download, network error, missing Sushi); and the exact README command with `wget` in an Ubuntu 26.04 root without curl |
| Ubuntu 26.04, `apt install cargo build-essential` (Rust 1.93) + `./install.sh` | **Tested** | built from source in an Ubuntu 26.04 root |
| Ubuntu 24.04, `apt install cargo-1.85 build-essential` + `./install.sh` | **Tested** | built from source with only `/usr/bin` in `PATH` |
| `./install.sh --binary PATH`, reinstall, upgrade, uninstall | **Tested** | `tests/install/test_install.sh` (exact rollback of the home directory) |
| Rust 1.85 (minimum supported) and stable | **Tested** | `cargo test` with both; CI matrix |
| Release archive with prebuilt backends | Built on tags | `.github/workflows/release.yml` with `tools/package.sh`: x86_64, plus aarch64 when the repository is public (GitHub's ARM runners serve public repositories only); the one-command install needs a public repository |

## Files

| Input | Status |
|---|---|
| NIfTI-1 and NIfTI-2 single file, `.nii` and `.nii.gz`, little and big endian | Tested (differential tests against nibabel) |
| uint8/16/32/64, int8/16/32/64, float32/64 | Tested |
| complex64/128 (magnitude), RGB24/RGBA32 (in colour, or luminance) | Tested |
| float128, complex256, binary | Unsupported (clear error) |
| `scl_slope`/`scl_inter`, sform only, qform only, neither | Tested |
| Oblique and sheared affines | Tested (resampled onto the world axes, warning; stored mode as acquired) |
| 2D, 4D, 5D | Tested (every volume: slider, time course) |
| NIfTI-MRS spectroscopy (single voxel and spectroscopic imaging, complex64/128, up to 65 536 points) | Tested (coils, dynamics and measurements combined; index 0 of encoded dimensions; longer spectra are refused before reading all FIDs; differential tests against numpy) |
| Multi-member gzip (bgzip) | Tested |
| Header/image pairs (`.hdr`/`.img`) | Unsupported (`.hdr` is also Radiance HDR) |
| FreeSurfer MGH (`.mgh`, `.mgz`, `.mgh.gz`): uint8, int16, uint16, int32, float32, frames | Tested (affine, values and frames against nibabel; equivalence with NIfTI) |
| MRtrix (`.mif`, `.mif.gz`; `.mih` with its data files): every datatype including `Bit` and complex, any `layout` (reversed, permuted, volumes interleaved with voxels), `scaling` | Tested (equivalence with NIfTI for each layout, and for a `.mih` whose data are in two files); no thumbnail for `.mih` (the thumbnail sandbox sees only the header) |
| NRRD (`.nrrd`; `.nhdr` with one data file, a `LIST` or a `%d` pattern, in its folder): raw, gzip, bzip2, ascii and hex encodings, LPS/RAS spaces, axes without direction (DWI gradients, sequences) as volumes, RGB, `byte skip`, `line skip` | Tested (equivalence with NIfTI for each encoding, 3D Slicer and pynrrd sample data; the bzip2 decoder against the `bzip2` tool) |
| MetaImage (`.mha`; `.mhd` with one data file): every scalar element type, zlib compression, channels (RGB, or volumes), `HeaderSize -1`, ITK geometry (`TransformMatrix`, `Offset`, `ElementSpacing`) | Tested (equivalence with NIfTI for each variant); a list or a pattern of data files is refused |
| CIFTI-2 (`.dscalar.nii`, `.dtseries.nii`…) and surface overlays (one row of values: FreeSurfer `.mgh` overlays; FreeSurfer's 27307 × 1 × 6 layout of the fsaverage vertices) | Recognised: the preview shows their header and names the viewer to use (Connectome Workbench, Freeview) instead of grey slices |
| DICOM files without an image: structured reports, radiotherapy structure sets and plans, waveforms (ECG), presentation states, key object selections | Recognised: the preview shows the object, its series and its content (the text of a report without the names of people, the structures, the plan's beams and fractions, the channels of a waveform) instead of an error |
| DICOM files (Part 10, or without the preamble): implicit and explicit VR, little and big endian, deflated; monochrome (MONOCHROME1 shown inverted), RGB, YBR, palette colour (also segmented), 1 to 32 bits, float and double float pixel data, rescale, modality LUT | Tested (pixels against pydicom for its test files); uncompressed pixel data cut short by a truncated file are shown up to the cut, with a note |
| DICOM compression: RLE, JPEG baseline and extended (8 and 12 bits), JPEG lossless (process 14, every predictor), JPEG-LS (lossless and near-lossless, every interleave mode), JPEG 2000 and HTJ2K (OpenJPEG), deflated frames, encapsulated uncompressed | Tested (lossless syntaxes identical to the uncompressed references of the DICOM WG04 set; lossy ones equal to CharLS and OpenJPEG, within 3 levels of libjpeg for baseline JPEG); progressive and arithmetic JPEG, JPEG XL, JPIP and video are refused |
| DICOM series from a folder (slices sorted along the normal, 4D, irregular spacing, gantry tilt, missing slices), Siemens mosaics, enhanced multi-frame (functional groups, dimension indices; Siemens XA one file per volume), RT dose, cines, DICOMDIR | Tested (affines and values identical to dcm2niix for the GE, Siemens, Canon and CT series of the `dcm_qa` repositories; Philips values are the display values, dcm2niix writes the "precise" ones) |
| Files on network file systems (NFS, SMB/CIFS, sshfs, GVfs's FUSE bridge for sftp://, smb://…, other FUSE mounts of remote storage) | Tested with every file taken as remote (`NIFTI_PREVIEW_ASSUME_NETWORK=1`): never mapped, read in 4 MiB blocks in the background with progress, a confirmation above `[network] confirm-above-mb`, DICOM files alone, no thumbnail unless asked; detection of the mount from `statfs` and `/proc/self/mountinfo` unit-tested |
| Remote locations without a local path (GVfs without its FUSE bridge) | Unsupported (clear message: install gvfs-fuse, or mount with sshfs or NFS) |
| Corrupted, truncated, mislabelled files | Tested (7 200 mutated NIfTI files, 1 560 MGH, MRtrix, NRRD and MetaImage ones (text and bzip2 encodings, a header alone included), 600 DICOM ones in the tests and 3 000 more from real samples, never a crash) |

## Terminal viewer (nitty)

What nitty draws with is chosen from the terminal's answers to a few queries
(see [ARCHITECTURE](ARCHITECTURE.md#terminal-viewer-nitty)); `--protocol` and
`--colors` override it.

| Terminal | Drawn with | Status | Evidence |
|---|---|---|---|
| VTE 0.84 (GNOME Terminal 3.58, Ptyxis 50) | half blocks, 24-bit colour | **Tested by hand** | nitty run in a real VTE widget under Xvfb by a script outside the test suite, keys sent, screen read back and its screenshot checked by eye. This VTE answers DA1 without Sixel, and draws nothing when Sixel is forced |
| tmux 3.6 | half blocks (tmux answers the queries itself) | **Tested by hand** | `tests/tmux.rs` run in an Ubuntu 26.04 container; the CI job runs it with Ubuntu 24.04's tmux (pending its first run) |
| A terminal answering like Kitty, like a Sixel terminal, or not at all | Kitty images, Sixel, half blocks | **Tested** (at the protocol level) | `tests/interactive.rs`: a pseudo-terminal plays the terminal |
| Kitty, Ghostty, WezTerm, iTerm2, Konsole, foot, xterm with Sixel | images (Kitty, iTerm2 or Sixel) | **Expected to work, not tested** in those terminals | the sequences follow each protocol's specification and are checked byte for byte; the Sixel output, decoded by hand with libsixel's `sixel2png`, gives back the slices; Kitty and iTerm2 carry the library's PNG |
| GNU screen 4.09 | half blocks, 256 colours | Expected to work, not tested | screen gets DA1 only (it would show the other queries) |
| Linux console | half blocks, 16 colours | Tested (output) | `tests/glance.rs` with `TERM=linux` |
| Dumb terminal, a pipe, a file | characters by brightness; half blocks in 24-bit colour | Tested | `tests/glance.rs` |
| Over SSH | as the terminal at the other end | Not tested over a real link | the queries take one round trip (DA1 last); images are sent in the escape codes (`t=d`), never as files the remote terminal could not read |

| Platform | Status | Evidence |
|---|---|---|
| Linux x86_64, static binary (musl) | **Tested by hand** | built in an Ubuntu 26.04 container, run in Ubuntu 22.04, 24.04, 26.04 and 26.10 containers; the CI job checks it for no dynamic loader or library (pending its first run) |
| Linux aarch64, static binary | **Not built yet** | the release workflow builds it on GitHub's ARM runners when a tag is pushed (public repositories); it has never run |
| Built from source, Rust 1.85 and stable | **Tested** | Clippy and the tests with both, with and without default features (the CI matrix: pending its first run); `install.sh --terminal` from a checkout |
| macOS, BSD | Unsupported | the sandbox is Linux's (seccomp, Landlock), and the terminal layer was only built and run on Linux |

## Known limitations

* Sushi 51.0 keeps every unloaded renderer object alive (reproduced with a
  minimal plugin that does nothing). The GTK 4 viewer drops its widgets and
  textures when unloaded, but Sushi's memory still grows by about 0.6 MiB per
  NIfTI preview while it runs; it is returned when Sushi exits after 12 s idle.
* Under Xvfb without a window manager a Sushi 51 window is sometimes undersized
  (the first of a session, and occasionally a later one), for every file type;
  this is a test-environment artefact.
* `vox_offset = 0` in a single-file NIfTI is read as "data right after the
  header" (offset 352/544); nibabel reads such files from byte 0.
* Sushi 51 binds `o` to its own *Open With…*; while a NIfTI file is shown, `O`
  switches the orientation as on Sushi 46–50, and *Open With…* stays available
  from its button.
* A qform quaternion with |b, c, d| > 1 is refused as an invalid file, as nibabel
  does.
* Spectra are a quick look, not a processing: no frequency alignment, no
  eddy-current correction, no first-order phase, no fitting. Edit conditions,
  ISIS steps, phase cycles, indirect and user dimensions are not decoded: the
  spectrum shows their index 0. Spectroscopic imaging spatially resampled
  (oblique) shows the spectrum of the nearest voxel.
* Accessibility: the views and the spectrum have accessible names and
  descriptions (slice, cursor, value under the pointer), and the keyboard can
  choose the view to scroll (V, Page Up/Down). Orca has not been tested, and
  moving the cursor within a slice needs the pointer.
* nitty over SSH: `COLORTERM` is not forwarded by default, so 24-bit colour is
  assumed unless `TERM` says otherwise (`linux`, `screen`, `vt*`); an older
  terminal (macOS Terminal before 26) wants `--colors 256` or
  `NITTY_COLORS=256`.
* nitty in tmux: Kitty and iTerm2 images only with `--protocol` and tmux's
  `set -g allow-passthrough on`, and they are not redrawn by tmux (switching
  windows loses them until the next frame, Ctrl-L); Sixel needs tmux 3.4 or
  later built with it. Kitty's Unicode placeholders are not used.
* nitty takes the mouse to place the cursor: most terminals select text with
  Shift held, or `--no-mouse` leaves the mouse to the terminal.
* In nitty, moving the cursor changes a slice of another view, which is drawn
  again. On a 200 × 60 terminal (a 1 mm T1, a 512² CT): 25 to 60 kB per key in
  half blocks; with images, that view alone, 250 to 370 kB (Kitty, iTerm2) or
  400 to 790 kB (Sixel). Over a slow link, `--protocol blocks` sends least.
* `nitty -p` in a terminal first asks it what it can show (up to 1 s when it
  does not answer): keys typed meanwhile are lost, since they cannot be given
  back to the shell. `--protocol blocks` (or `ascii`) with `--colors`, or
  `NITTY_PROTOCOL` and `NITTY_COLORS`, skip the question.
