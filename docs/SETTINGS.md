# Settings

Nothing needs configuring. To change the defaults, create
`~/.config/gnome-nifti-preview/settings.ini` and keep only the lines you need;
the next preview uses them (no restart).

```ini
[display]
# Orientation when a file opens: neurological, radiological or stored
orientation = neurological
# Colour segmentations (false: grayscale, RGB images keep their colours; L still switches)
label-colors = true
# Cursor lines in the three views: on-click (once you click), always or never
crosshair = on-click
# Value under the mouse pointer, e.g. "412.5" or "label 51 · 21.6 cm³"
hover = true
# Add the world coordinates (mm) to that readout
hover-coordinates = false
# Volume of the label under the pointer
label-volume = true

[panel]
# The Affine section (bottom right of the panel)
affine = true

[performance]
# Largest image the preview loads (voxel data of the first volume, or all the
# data of a spectroscopy file, MB, 16 to 65536). Larger images show their
# metadata only. Lower it on small machines.
max-image-mb = 3072
# Memory kept for slices already drawn, so scrolling back is instant (MB)
slice-cache-mb = 64
```

A missing key keeps its default. An invalid value also keeps the default and
is reported in the previewer log (`journalctl --user -b | grep "GNOME NIfTI Preview"`);
a broken file never stops a preview. `uninstall.sh` leaves this file alone.

How much memory a preview uses: the voxel data of the first volume (an
uncompressed `.nii` above 256 MB is memory-mapped instead, so only the pages
read count), plus up to `slice-cache-mb` of rendered slices. A 1 mm brain T1
needs about 30 MB. The backend cannot use much more than `max-image-mb` plus
`slice-cache-mb` plus about 1.3 GB, whatever the file, and its process ends as
soon as the preview closes. Slices of more than 16 megapixels (4096 × 4096)
or wider than 16384 pixels are not drawn (metadata only).
