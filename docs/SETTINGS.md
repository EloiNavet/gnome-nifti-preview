# Settings

Nothing needs configuring. To change the defaults, open **NIfTI Preview** from
the applications menu (or the gear button of a preview): every change is saved
at once and the next preview uses it, no restart.

The window writes `~/.config/gnome-nifti-preview/settings.ini`, which you can
also edit by hand: keep only the lines you need, comments are kept.

```ini
[display]
# Orientation when a file opens: neurological, radiological or stored
orientation = neurological
# Orientation letters (R/L, A/P, S/I) at the edges of the views
axis-labels = true
# Colours of intensities: gray, hot, inferno or viridis
# (segmentations and RGB images keep their own colours)
palette = gray
# The window: values below the low percentile are black, above the high one
# white (0 to 100, low < high). A right-drag on a view changes it for that
# preview (right: wider, up: brighter); a double right-click goes back.
window-low = 0.5
window-high = 99.5
# auto: smooth intensities, sharp segmentations and voxels larger than 6
# screen pixels; nearest: always sharp; linear: smooth (segmentations stay sharp)
interpolation = auto
# Colours of segmentations: auto (FreeSurfer's colours and names when it looks
# like a FreeSurfer parcellation), freesurfer, random (a generic palette) or
# off (grayscale; RGB images keep their colours; L still switches)
label-colors = auto
# Colours and names of your own atlas: a file in FreeSurferColorLUT format
# ("label name red green blue alpha" per line); empty: none
label-lut =
# Cursor lines in the three views: on-click (once you click), always or never
crosshair = on-click
crosshair-color = #61adff
# Value under the mouse pointer, e.g. "412.5" or "label 51 · 21.6 cm³"
hover = true
# Add the world coordinates (mm) to that readout
hover-coordinates = false
# Volume of the label under the pointer
label-volume = true

[4d]
# For 4D images, the time course of the voxel under the pointer below the slider
timecourse = true
# per-volume: each volume its own window (diffusion: b=0 and b=1000 differ);
# first: the window of the first volume for all (no flicker in fMRI)
window = per-volume
# Volume shown when a file opens: first, middle or last
start-volume = first

[dicom]
# A DICOM file opens with the other images of its series, read from its
# folder, on the slice of the file chosen; false: the file alone
series = true
# The window the file gives (file), or the automatic one (auto, percentiles)
window = file
# Patient name, ID, birth date... in the panel (hidden by default, for
# screenshots and screen sharing)
show-patient = false

[panel]
# Rows of the metadata panel, in this order (rows not listed are hidden):
# dimensions, volumes, voxel-size, orientation, obliquity, transform,
# datatype, scaling, labels, range, window, intent, description, format, size,
# details (the format's own: MGH timing, MRtrix diffusion scheme, NRRD space…),
# label-list (every label of a segmentation, its name and volume)
rows = dimensions, volumes, voxel-size, orientation, obliquity, transform, datatype, scaling, labels, range, window, intent, description, format, size, details, label-list
# The Affine section (bottom right of the panel)
affine = true

[thumbnails]
# Thumbnails in Files (they also need the program install.sh copies to
# /usr/local/bin). GNOME makes them in a sandbox that cannot read this file:
# the Preferences window (or install.sh) writes these options into
# ~/.local/share/thumbnailers/gnome-nifti-preview.thumbnailer. After a change,
# "Make thumbnails again" in the window refreshes the existing ones.
enabled = true
# The centre slice of: axial, coronal or sagittal
view = axial
# neurological, radiological or stored
orientation = neurological
# Files whose compressed data to read for one thumbnail exceed this keep their icon (MB)
max-file-mb = 250
# Also make thumbnails of files on network file systems (slow: images are read whole)
network = false

[network]
# Files on servers (sftp://, smb://, NFS, sshfs) are read in the background,
# with their progress; above this, the preview asks first (MB, 0: never ask)
confirm-above-mb = 100

[performance]
# Largest image the preview loads (voxel data of one volume, or all the
# data of a spectroscopy file, MB, 16 to 65536). Larger images show their
# metadata only. Lower it on small machines.
max-image-mb = 3072
# Memory kept for slices already drawn, so scrolling back is instant (MB)
slice-cache-mb = 64
```

A missing key keeps its default. An invalid value also keeps the default and
is reported in the previewer log (`journalctl --user -b | grep "GNOME NIfTI Preview"`);
a broken file never stops a preview. `uninstall.sh` leaves this file alone.
The thumbnails use the palette, the window percentiles and `label-colors` too
(and, for a DICOM file, the window it gives: a thumbnail shows that file
alone, never its series).

How much memory a preview uses: the voxel data of the volume shown (an
uncompressed `.nii` above 256 MB is memory-mapped instead, so only the pages
read count), plus up to `slice-cache-mb` of rendered slices. A 1 mm brain T1
needs about 30 MB. A gzipped 4D image keeps all its volumes in memory once you
move away from the first one, if they fit in `max-image-mb` (otherwise it reads
up to the volume asked, keeping the latest ones within half of it). The backend
cannot use much more than `max-image-mb` plus `slice-cache-mb` plus about
1.3 GB, whatever the file, and its process ends as soon as the preview closes.
Slices of more than 16 megapixels (4096 × 4096) or wider than 16384 pixels are
not drawn (metadata only).
