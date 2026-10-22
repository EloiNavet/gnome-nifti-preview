<p align="center">
  <img src="docs/images/hero.png" alt="GNOME NIfTI Preview: press Space on a brain MRI in GNOME Files to see three views, its metadata and its labels">
</p>

<p align="center">
  <b>Press Space on a brain MRI in GNOME Files.</b><br>
  Three views, the metadata and the labels of a segmentation, in a fraction of a second.
</p>

## Install

Open a terminal (<kbd>Ctrl</kbd> <kbd>Alt</kbd> <kbd>T</kbd>) and paste:

```sh
wget -qO- https://raw.githubusercontent.com/EloiNavet/gnome-nifti-preview/main/get.sh | bash
```

That's all: nothing to compile, everything stays in your home folder. If GNOME's
previewer (GNOME Sushi) is missing, the installer offers to add it.

## Use

Select a `.nii` or `.nii.gz` file in Files and press <kbd>Space</kbd>.

| | |
|---|---|
| <kbd>Scroll</kbd> | Move through the slices |
| Hover | The value under the pointer; on a segmentation, the label and its volume |
| <kbd>Click</kbd> | Navigate: the other views follow |
| <kbd>←</kbd> <kbd>→</kbd> | Previous / next file |
| <kbd>O</kbd> | Neurological, radiological or stored orientation |
| <kbd>L</kbd> | Colours (segmentations, RGB images) or grayscale |
| <kbd>Space</kbd> | Close |

<p align="center">
  <img src="docs/images/demo.webp" alt="Pressing Space in Files, hovering to read values, scrolling, clicking, then switching to the lobe, mask and structure maps with the arrow keys" width="720">
</p>

## What you get

<p align="center">
  <img src="docs/images/t1.png" alt="T1 image: three views, the value under the pointer and the metadata panel" width="49%">
  <img src="docs/images/mask.png" alt="Brain mask: pointing at it shows label 1 and its volume, 1870 cm³" width="49%">
</p>

- **Instant.** The slices of a 1 mm T1 appear in about 0.2 s, even compressed.
- **The right way up.** Every view is labelled R/L, A/P, S/I from the file's own orientation; tilted acquisitions are resampled upright.
- **Segmentations.** Detected automatically and coloured; point at a structure to see its label and its volume in cm³.
- **The essentials at a glance.** Dimensions, voxel size, orientation, datatype, value range; the rest on demand.
- **fMRI and diffusion too.** 4D series show their first volume.
- **Spectroscopy.** NIfTI-MRS files show their spectrum in ppm, all coils and averages combined; on spectroscopic imaging, click a voxel to see its spectrum.
- **One click to your viewer.** With ITK-SNAP installed (or FSLeyes, MRIcroGL, MRIcron), the preview's *Open With ITK-SNAP* opens the file in it. Without one, the button says what to install, rather than letting Files extract the `.nii.gz`.
- **Private.** Nothing leaves your computer: the preview uploads nothing and saves no copy of your images (GNOME keeps its usual caches).

Works with GNOME Files on Ubuntu 24.04, 26.04 and 26.10 (GNOME Sushi 46 to 51) and other
GNOME distributions, for NIfTI-1 and NIfTI-2 `.nii` / `.nii.gz` files.
Prefer other defaults (radiological view, more header details, less memory…)? See [settings](docs/SETTINGS.md).

To remove it: `~/.local/share/gnome-nifti-preview/uninstall.sh`

## More

[Troubleshooting](docs/TROUBLESHOOTING.md) ·
[Build from source](docs/DEVELOPMENT.md) ·
[How it works](docs/ARCHITECTURE.md) ·
[Display conventions](docs/DISPLAY_CONVENTIONS.md) ·
[Compatibility](docs/COMPATIBILITY.md) ·
[Benchmarks](docs/BENCHMARKS.md) ·
[Security](docs/SECURITY.md)

On macOS or Windows? Have a look at [MIQ](https://github.com/marcoduering/MIQ) and
[MIQ-Win](https://github.com/marcoduering/MIQ-Win) by Marco Düring, which bring the same
Space-bar preview of medical images to Finder and File Explorer and inspired this project.

GPL-2.0-or-later; the screenshots and videos (`docs/images`, `site/assets`) are CC BY 4.0.
A convenience tool for research, **not a medical device**.
