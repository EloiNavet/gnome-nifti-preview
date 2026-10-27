# Troubleshooting

**Nothing happens when I press Space.** GNOME Sushi, the previewer behind the Space
key, is not installed: `sudo apt install gnome-sushi` (Fedora: `sudo dnf install sushi`,
Arch: `sudo pacman -S sushi`). The installer offers to do this for you.

**Space shows a generic page for a `.nii.gz`.** Run the install command again. Then
`gio info -a standard::content-type file.nii.gz` must print `image/x-compressed-nifti`.
A file that only *looks* like NIfTI (wrong content) shows Sushi's error page.

**"Image too large to preview".** The metadata is shown but the voxel data is above
the memory limit (3 GB by default): raise `max-image-mb` in the [settings](SETTINGS.md).
If the message mentions slices, a slice has more than 16 megapixels (4096 × 4096)
or is wider than 16384 pixels, which the preview does not draw.

**"do not run this with sudo or as root".** The installer puts everything in your
home folder and never needs root: run the command again without `sudo`.

**"The backend could not be started".** Run the install command again, then check
`~/.local/share/gnome-nifti-preview/bin/nifti-preview --version`.

**"Open With" shows another application.** The button opens NIfTI files with their
default application. The installer makes ITK-SNAP (or FSLeyes, MRIcroGL, MRIcron) the
default if it finds one and you had not chosen anything; install the viewer, then run
the install command again. To pick another one: right-click a `.nii.gz` in Files,
*Open With…*, choose the application and *Always use for this file type*.
The installer leaves `~/.config/mimeapps.list` alone when it is a symbolic link
(dotfiles) or not a valid file, and only uses a viewer that no other user can
modify; it then says so, and you can set the default in Files as above.

**The button says "Open With a NIfTI viewer…".** No NIfTI viewer is installed:
the button explains what to install instead of offering Files or an archive tool
(they would extract the `.nii.gz`). Install ITK-SNAP, FSLeyes, MRIcroGL or
MRIcron, then run the install command again: the button then opens the file in
it.

**The button says "Open With Files".** Run the install command again: it makes
your NIfTI viewer, or "a NIfTI viewer…", the default. It does not change a
default you chose yourself in Files.

**A spectroscopy file shows a flat line or noise.** The preview combines all the
transients by their first point and applies only a zero-order phase; spectra
that need frequency alignment, eddy-current correction or a first-order phase
look worse than in FSL-MRS or spant. **Z** shows the whole width, **L** the
magnitude.

**No thumbnails in Files.** They need the program GNOME's sandbox can run:
`./install.sh --thumbnails` (it asks for your password once, to copy it to
`/usr/local/bin`). Files shows thumbnails of local files only by default
(Settings of Files, *Show thumbnails*), keeps the ones it made, and remembers
failures: to make them again, `rm -rf ~/.cache/thumbnails/*/` then `nautilus -q`.
Files above `[thumbnails] max-file-mb` of compressed data keep their icon.

**A file on a server (sftp://, smb://, NFS, sshfs).** The preview reads it in the
background and says how far it got ("Reading over the network… 34 % · 12 MB/s");
above `[network] confirm-above-mb` (100 MB) it first asks, with a *Load the image*
button. Pressing → before it starts reads nothing. A DICOM file on a server is
shown alone: its series is not rebuilt (that would read every file of the
folder). Files on servers get no thumbnail unless `[thumbnails] network = true`.
"has no local path": the location is reached through GVfs without its FUSE
bridge, so the file has no path a program can open; install the `gvfs-fuse`
package (it is usually there), or mount the server with sshfs or NFS.

**A DICOM folder shows the wrong series, or one file only.** Space on a file shows
the series it belongs to (its `SeriesInstanceUID`), read from the other files of
its folder; the menu above the panel (or **[** **]**) opens the other series of
the same exam (same `StudyInstanceUID`) found in the folder; unrelated files
of the folder are not offered. A DICOMDIR offers all its series. `[dicom] series = false` shows each file alone. Files of the same
series with another size or orientation (localizers) are left out, with a note.

**Flatpak Files / Sushi.** Not supported: the sandboxed previewer cannot load
plugins from your home folder.

**Seeing what the preview sees.**
`~/.local/share/gnome-nifti-preview/bin/nifti-preview info FILE --data` prints the
header, orientation, window and label detection; `… render FILE -o out.png` writes
the three views as PNG files. Sushi's messages: `journalctl --user -f` while
pressing Space, or run `NIFTI_PREVIEW_DEBUG=1 /usr/libexec/org.gnome.NautilusPreviewer`
in a terminal.

## All keyboard shortcuts

| Key | Action |
|---|---|
| Space | Open / close the preview |
| Scroll over a view | Next / previous slice of that view |
| Hover a view | Value under the pointer (label and volume on a segmentation) |
| Click or drag in a view | Move the cursor; the other views follow |
| Page Up / Page Down | Next / previous slice of the view under the pointer |
| O | Neurological → radiological → stored axes |
| L | Label colours (segmentations) or colours (RGB images) ↔ grayscale; real part ↔ magnitude of a spectrum |
| Z | Spectrum: usual range (0.2–4.2 ppm for 1H) ↔ whole width |
| C | Recenter |
| Shift + scroll over a view, comma / period | 4D: previous / next volume |
| Home / End | 4D: first / last volume |
| [ / ] | DICOM: previous / next series of the folder or DICOMDIR |
| Right-drag on a view | Window: right = wider, up = brighter (grey levels only) |
| Double right-click | Automatic window again |
| V | Next view for Page Up / Page Down (keyboard only; the view's title is highlighted) |
| F | Fullscreen |
| ← → ↑ ↓ | Previous / next file in Files |

Moving the pointer over the preview also shows a small toolbar with these actions.
