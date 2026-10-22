# Backend protocol (version 1)

`nifti-preview serve` reads requests on stdin and writes responses on stdout.
Every request is one JSON object on one line and receives exactly one response
line, in order, so requests can be pipelined. A response that contains
`"bytes": N` is immediately followed by `N` raw bytes. stderr is free-form logging.
The process exits on EOF or after `close`.

On start the backend writes:

```json
{"event":"hello","protocol":1,"version":"0.1.0","pid":1234,"backend":"rust",
 "sandbox":{"data_limit":4630511616,"no_new_privs":true,"seccomp":true,"landlock":7}}
```

`sandbox` tells what `serve` enforced (`seccomp`: sockets denied; `landlock`:
the Landlock ABI version, null when unavailable). Other lines carrying an `event` key may be added later:
clients ignore them. The client checks that every response carries the id of
the oldest pending request.

Responses echo the request `id` and carry `"ok": true` or
`"ok": false, "error": {"code": "...", "message": "..."}`.
Error codes: `not_found`, `not_a_file`, `invalid_nifti`, `unsupported_format`,
`unsupported_datatype`, `corrupt_data`, `too_large`, `bad_request`, `internal`.

## Requests

| op | parameters | result |
|---|---|---|
| `open` | `path`, optional `mode` | `header`, `cursor` (centre of the grid), `mode`, `requested_mode`, `grid`, `plans` |
| `load` | – | `stats`, `colormap`, `effective_colormap`, `colormaps` (the ones worth offering, default first: `["labels","gray"]`, `["rgb","gray"]` or `["gray"]`) |
| `set_display` | optional `mode`, `colormap`, `cursor` (grid point before the change) | `mode`, `requested_mode`, `grid`, `plans`, `colormap`, `effective_colormap` (if loaded); with `cursor`, the same anatomical point on the new grid (clamped) |
| `slice` | `view`, `index` (clamped), optional `colormap` | `view`, `index`, `width`, `height`, `colormap`, `format: "rgb8"`, `bytes` + pixels |
| `value` | `ijk` (grid point) | `ijk`, `world` (mm, whatever the spatial unit of the header), `voxel` (nearest voxel, null outside the image), `value` (of that voxel, exact for integer data; null for NaN/Inf or outside); `rgb` for RGB images; in a label map and outside label 0, also `label_voxels` and `label_volume_mm3` (voxels of volume 0 with that label) |
| `spectrum` | optional `ijk` (grid point; the centre voxel without it) | spectroscopy files only: `voxel` (null outside the image), `unit` (`ppm`, `hz` or `points`), `first`, `last` (axis of the first and last points, linear between), `points`, `real`, `magnitude` (arrays), `default_range` (`[0.2, 4.2]` for 1H, else null), `default_part` (`real` for 1H, else `magnitude`), `transients` (averaged) |
| `ping` | – | `pong` |
| `close` | – | `closed`, then exit |

`mode` ∈ `neurological` (default), `radiological`, `stored`. When the file has no
valid qform/sform, `mode` is always `stored` (`requested_mode` keeps the request).
`colormap` ∈ `auto` (default: `rgb` for RGB images, `labels` for label maps,
`gray` otherwise), `gray`, `labels`, `rgb` (`gray` for an image that is not RGB).
`view` ∈ `coronal`, `sagittal`, `axial`.

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

`header` contains `name`, `path`, `file_size`, `format` (`NIfTI-1`/`NIfTI-2`),
`compressed`, `shape`, `n_volumes`, `zooms`, `space_units`, `time_units`,
`repetition_time`, `dtype` (numpy notation), `datatype`, `slope`, `inter`,
`qform_code`/`qform`, `sform_code`/`sform`, `affine_source`
(`sform`/`qform`/`none`), `affine` (4×4), `orientation` (e.g. `LPS`, or
`unknown`), `orientation_known`, `obliquity_deg`, `intent`, `intent_code`,
`descrip`, `warnings` (shown to the user) and `header_fixes` (repairs applied to
an out-of-spec header).

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
`labels` (`is_labels`, `binary`, `n_labels`, `reason`), `non_finite`,
`non_finite_exact`, `min`, `max` and `notes` (shown to the user).

Scalar `value` readouts and `stats.min`/`stats.max` are JSON numbers, or null
for non-finite values. Unscaled integers and integral labels outside JavaScript's exact integer
range, −(2^53−1) to 2^53−1, are decimal strings instead. Clients must display
these strings without converting them to `Number`. Unscaled integer and
float64 values retain their stored precision; grayscale interpolation and
windowing still use float32. Label counts and colours use the exact values.
