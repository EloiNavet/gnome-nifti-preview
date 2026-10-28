# Backend protocol (version 2)

`nifti-preview serve` reads requests on stdin and writes responses on stdout.
Every request is one JSON object on one line and receives exactly one response
line, in order, so requests can be pipelined. A response that contains
`"bytes": N` is immediately followed by `N` raw bytes. stderr is free-form logging.
The process exits on EOF or after `close`.

On start the backend writes:

```json
{"event":"hello","protocol":2,"version":"0.2.0","pid":1234,"backend":"rust",
 "sandbox":{"data_limit":4630511616,"no_new_privs":true,"seccomp":true,"landlock":7}}
```

`sandbox` tells what `serve` enforced (`seccomp`: sockets denied; `landlock`:
the Landlock ABI version, null when unavailable). Other lines carrying an `event` key may be added later:
clients ignore them. The client checks that every response carries the id of
the oldest pending request.

Responses echo the request `id` and carry `"ok": true` or
`"ok": false, "error": {"code": "...", "message": "..."}`.
Error codes: `not_found`, `not_a_file`, `invalid_image` (content not recognised), `invalid_nifti`, `unsupported_format`,
`unsupported_datatype`, `corrupt_data`, `too_large`, `bad_request`, `internal`.
An `open` refused for a file that is recognised but has no slices to show (a
DICOM report, radiotherapy plan or waveform, a CIFTI-2 file, a surface overlay)
adds `"about"` to the error: sections for the viewer's panel, each
`{"title", "rows": [[key, value]...]}` or `{"title", "text", "prose": true}`
(the text of a report, on lines). Names of people are never in it.

## Requests

| op | parameters | result |
|---|---|---|
| `open` | `path`, optional `mode`, `dicom_series` (default true: a DICOM file opens with its series, read from its folder; false: the file alone) | `header`, `cursor` (centre of the grid; for a DICOM series, the slice of the file opened), `volume` (the volume shown: 0, or that of the DICOM file opened), `mode`, `requested_mode`, `grid`, `plans` |
| `load` | optional `window_percentiles` (`[low, high]`, 0 ≤ low < high ≤ 100, default `[0.5, 99.5]`), `window_volume` (`per-volume` or `first`), `palette`, `label_colors` (`auto`, `freesurfer`, `random`), `label_lut` (path of a FreeSurferColorLUT-format file: its colours and names), `dicom_window` (`file`, default: the window a DICOM file gives; `auto`: the percentiles) | `stats`, `colormap`, `effective_colormap`, `colormaps` (the ones worth offering, default first: `["labels","gray"]`, `["rgb","gray"]` or `["gray"]`), `volume` (the volume shown), `display_window` (`low`, `high`, `source`: `auto`, `first`, `file` or `manual`), `palette` |
| `load` (network) | on a network file system, before the data are read: `defer_network` (true: read nothing yet), then `async` (true: read on a helper thread) | `defer_network`: `ready: false`, `deferred: true`, `bytes_to_read`; `async`: `ready: false`, `progress` (0 to 1), `bytes_per_second` (null when counting decoded frames), asked again until the reply of `load` with `ready: true`. Meanwhile only `load`, `open`, `open_series`, `ping` and `close` answer; `open` gives the load up |
| `series_list` | – | DICOM images: `series` (the series of the same study in the folder, or every series of the DICOMDIR opened: `uid`, `number`, `description`, `modality`, `study_uid`, `study_description`, `study_date`, `patient`, `images`, `current`), `origin` (`folder` or `dicomdir`), `incomplete` (the folder held more files than were read) |
| `open_series` | `uid` (from `series_list`), optional `mode` | another series, opened like `open` (in its middle) |
| `set_volume` | `volume` (clamped) | 4D images, not spectroscopy: `volume` (the one asked, or shown when ready), `shown`, `n_volumes`, `ready`, `progress` (0 to 1), `time` (`volume` × repetition time for a 4D series, else null); when `ready`, also what `load` returns for that volume. `ready: false`: the volume is still being decompressed (gzip), the one shown does not change; ask again |
| `timecourse` | `ijk` (grid point) | `ijk`, `voxel` (nearest voxel, null outside the image), `values` (one per volume: the physical value at display precision, null for NaN/Inf or a volume not decompressed yet), `complete`, `progress`, `volume` (shown), `repetition_time`, `time_units`; `too_large` above 65 536 volumes |
| `set_display` | optional `mode`, `colormap`, `cursor` (grid point before the change), `palette`, `window` (`[low, high]` in physical values, or null for the automatic window) | `mode`, `requested_mode`, `grid`, `plans`, `colormap`, `palette`, `effective_colormap` and `display_window` (if loaded); with `cursor`, the same anatomical point on the new grid (clamped) |
| `slice` | `view`, `index` (clamped), optional `colormap` | `view`, `index`, `width`, `height`, `colormap`, `format: "rgb8"`, `bytes` + pixels |
| `value` | `ijk` (grid point) | `ijk`, `world` (mm, whatever the spatial unit of the header), `voxel` (nearest voxel, null outside the image), `value` (of that voxel, exact for integer data; null for NaN/Inf or outside); `rgb` for RGB images; in a label map and outside label 0, also `label_name` (from the colour table, when it has the label), `label_voxels` and `label_volume_mm3` (voxels of the volume shown with that label) |
| `spectrum` | optional `ijk` (grid point; the centre voxel without it) | spectroscopy files only: `voxel` (null outside the image), `unit` (`ppm`, `hz` or `points`), `first`, `last` (axis of the first and last points, linear between), `points`, `real`, `magnitude` (arrays), `default_range` (`[0.2, 4.2]` for 1H, else null), `default_part` (`real` for 1H, else `magnitude`), `transients` (averaged) |
| `labels` | – | segmentations: `labels` (in increasing order: `label`, `name` (from the colour table, or null), `rgb`, `voxels`, `volume_mm3`) of the volume shown, `complete` (false beyond 4 096 different values), `scheme` |
| `ping` | – | `pong` |
| `close` | – | `closed`, then exit |

`mode` ∈ `neurological` (default), `radiological`, `stored`. When the file has no
valid qform/sform, `mode` is always `stored` (`requested_mode` keeps the request).
`colormap` ∈ `auto` (default: `rgb` for RGB images, `labels` for label maps,
`gray` otherwise), `gray`, `labels`, `rgb` (`gray` for an image that is not RGB).
`view` ∈ `coronal`, `sagittal`, `axial`. `palette` ∈ `gray` (default), `hot`,
`inferno`, `viridis`: the colours grey levels are shown with (not for labels
or RGB images). The grey levels use `display_window`: the window set with
`set_display` (`manual`), else the window of a DICOM file (`file`, unless
`dicom_window: auto`), else with `window_volume: first` that of volume 0
(`first`), else that of the volume shown (`auto`, the `stats.window`). A DICOM
image in MONOCHROME1 is drawn inverted (low values white).

`load` also gives `label_cursor` for a segmentation whose labels miss a plane
through the centre of the grid (a hippocampus, a few lesions): the grid point
at the centre of its labels, where the client starts (unless the cursor moved).

`slice`, `value` and the label counts of `value` use the volume shown (volume 0
until `set_volume`). Volumes are indexed over the dimensions after the third, in
Fortran order (`t` for a 4D image). A gzipped 4D file is decompressed once, in a
background thread, the first time another volume or a time course is asked:
volume 0 stays as fast as before. When all the volumes fit in the image limit
they are all kept (scrubbing and time courses are then instant); otherwise the
backend decodes up to the volume asked and keeps the most recent ones within half
the limit, and a time course stays incomplete. Uncompressed files are read volume
by volume (or mapped when large). Statistics (window, labels, range) are those of
the volume shown.

Pixels are RGB8, rows from the top of the screen to the bottom, `width * 3` bytes
per row. A slice is at most 16 megapixels with no side above 16384, so `bytes`
is at most 48 MiB; `load` answers `too_large` for images with larger slices,
voxel data above the image limit (`NIFTI_PREVIEW_MAX_BYTES`, 16 MiB to 64 GiB,
3 GiB by default) or `corrupt_data` for a `vox_offset` above 256 MiB. Clients
refuse a slice header whose `bytes` is not `width * height * 3` within these
limits. They are at the resolution of the grid: the client scales them by
`pixel_w`/`pixel_h` (in the header's spatial unit) to keep the physical aspect ratio, with nearest-neighbour
filtering when `colormap` is `labels`.

## Grid

Views, slice indices, `cursor` and `value` refer to the display grid:

```json
{"shape":[64,89,81],"resampled":true,
 "affine":[[1,0,0,-31.5],[0,1,0,-44.38],[0,0,1,-40.13],[0,0,0,1]]}
```

It is the voxel grid (`resampled: false`, `affine` = the image's), except for
oblique images in the neurological and radiological modes: then a grid aligned
with the world axes covering the image, with an isotropic spacing equal to the
smallest voxel size (made coarser if a slice would exceed the slice limits).
`affine` maps a grid point to coordinates in `header.space_units` (assumed mm
when unknown). The `value.world` readout is separately converted to mm. See
[DISPLAY_CONVENTIONS.md](DISPLAY_CONVENTIONS.md).

## View plans

```json
{"view":"axial","slice_axis":2,"h_axis":0,"h_flip":false,"v_axis":1,"v_flip":true,
 "width":176,"height":208,"n_slices":176,"pixel_w":1.0,"pixel_h":1.0,
 "labels":{"left":"L","right":"R","top":"A","bottom":"P"},
 "slice_up":1,"slice_up_label":"S","slice_step":1,"title":"Axial"}
```

The image pixel (col, row) of a view at slice `index` is the grid point with
`ijk[slice_axis] = index`, `ijk[h_axis] = h_flip ? width-1-col : col` and
`ijk[v_axis] = v_flip ? height-1-row : row`. Scrolling "up" moves the slice
index by `slice_up × slice_step`, towards `slice_up_label` (`slice_step` > 1 on
a resampled grid finer than the acquired slices).

## Header

`header` contains `name`, `path`, `file_size`, `format` (`NIfTI-1`, `NIfTI-2`,
`MGH`, `MGZ`, `MRtrix`, `NRRD`, `MetaImage` or `DICOM`),
`compressed`, `shape`, `n_volumes`, `zooms`, `space_units`, `time_units`,
`repetition_time`, `dtype` (numpy notation), `datatype`, `slope`, `inter`,
`qform_code`/`qform`, `sform_code`/`sform`, `affine_source`
(`sform`/`qform`/`none`), `affine` (4×4), `orientation` (e.g. `LPS`, or
`unknown`), `orientation_known`, `obliquity_deg`, `intent`, `intent_code`,
`descrip`, `warnings` (shown to the user) and `header_fixes` (repairs applied to
an out-of-spec header).

`storage` tells where the file is: `network` (a network file system: NFS,
SMB/CIFS, sshfs, GVfs…), `fs` (its type, as the kernel names it) and
`bytes_to_read` (about how much loading reads: volume 0, its compressed share in
a gzip stream, the files of a DICOM series). Over the network the backend never
maps the file and reads it in 4 MiB blocks; other volumes of a 4D image are read
in the background, like those of a gzip stream.

`format_fields` are the format's own details, in display order (`[key,
value]`: MGH timing, MRtrix diffusion scheme, NRRD space, DICOM modality,
scanner, series, timing, transfer syntax...). `dicom` is null, except for DICOM
images: `inverted` (MONOCHROME1), `window` (`[low, high]` the file gives, or
null), `patient` (`[key, value]`: name, ID, birth date... to show only on
request), `series_uid`, `files` (files the image is made of), `series_count`
(series of the folder or DICOMDIR, null until listed) and `dicomdir`.

`mrs` is null, except for spectroscopy (NIfTI-MRS) files: `format` (the
`intent_name`, e.g. `mrs_v0_3`, or empty), `nucleus`, `spectrometer_mhz`,
`field_t`, `points`, `dwell_s`, `bandwidth_hz`, `dims` (`[{tag, size}]` for
dimensions 5 to 7, e.g. `DIM_COIL`), `transients` (all of them), `combined`
(those the spectrum averages), `first_index_only` (`[{tag, size}]`, the
dimensions the spectrum takes at index 0), `single_voxel`,
`echo_time_s` and `repetition_time_s` (from the JSON extension, null when
absent). For these files `load` reads all the data, not only volume 0, and the
views show the magnitude of the first time point.

## Stats

`stats` contains `window` (`low`, `high`, `status` ∈ `ok`/`constant`/`no_finite`),
`labels` (`is_labels`, `binary`, `n_labels`, `reason`, `scheme`: `binary`, `freesurfer`, `custom` or `random`), `non_finite`,
`non_finite_exact`, `min`, `max` and `notes` (shown to the user).

Scalar `value` readouts and `stats.min`/`stats.max` are JSON numbers, or null
for non-finite values. Unscaled integers and integral labels outside JavaScript's exact integer
range, −(2^53−1) to 2^53−1, are decimal strings instead. Clients must display
these strings without converting them to `Number`. Unscaled integer and
float64 values retain their stored precision; grayscale interpolation and
windowing still use float32. Label counts and colours use the exact values.
