# Compatibility

Status as of version 0.1.0. "Tested" means a test was run on that
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
message; PNG and text previews still work and start no backend; 20 open/close
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
| 2D, 4D, 5D | Tested (first volume) |
| NIfTI-MRS spectroscopy (single voxel and spectroscopic imaging, complex64/128, up to 65 536 points) | Tested (coils, dynamics and measurements combined; index 0 of encoded dimensions; longer spectra are refused before reading all FIDs; differential tests against numpy) |
| Multi-member gzip (bgzip) | Tested |
| Header/image pairs (`.hdr`/`.img`) | Unsupported (`.hdr` is also Radiance HDR) |
| FreeSurfer `.mgz`, MRtrix `.mif`, NRRD | Not supported yet |
| Remote locations without a local path (non-FUSE gvfs) | Unsupported (clear error) |
| Corrupted, truncated, mislabelled files | Tested (7 200 mutated files, never a crash) |

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
