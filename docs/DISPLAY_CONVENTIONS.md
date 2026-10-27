# Display conventions

## Orientation

The affine is nibabel's "best affine": the sform if `sform_code > 0`, otherwise
the qform if `qform_code > 0`. World coordinates are NIfTI's RAS+ (x towards the
patient's Right, y Anterior, z Superior).

For the neurological and radiological modes, each stored voxel axis is matched
to its nearest world axis (`nibabel.io_orientation`) and the slice is a
permutation/flip of the stored grid (oblique images are resampled, see below):

| View | Screen right | Screen up | Scroll up goes towards |
|---|---|---|---|
| Axial, neurological | patient right (R) | anterior (A) | superior |
| Coronal, neurological | patient right (R) | superior (S) | anterior |
| Axial / coronal, radiological | patient left (L) | as above | as above |
| Sagittal (both modes) | posterior (P), i.e. anterior on the left | superior (S) | right |

The **stored** mode shows the raw voxel axes: first axis to the right, last
displayed axis upwards (axial ≈ axes 0×1, coronal ≈ 0×2, sagittal ≈ 1×2).

Whatever the mode, the letters at the edges of each view are computed from the
affine for the axis actually displayed, so a flipped or permuted file is never
shown with misleading labels. The test suite checks this property with markers
near the right, anterior and superior ends of a phantom stored in eight
different orientations (`reference/tests/test_orientation.py`).

* **Oblique images** (an axis more than 1° from its nearest world axis, e.g. a
  tilted CT gantry or an fMRI planned along the AC–PC line) are resampled in the
  neurological and radiological modes, so that they appear upright:
  * the views are cut from a grid aligned with the world axes that covers the
    centres of the image's corner voxels, as fine as the smallest voxel size;
  * values are interpolated trilinearly (label colours: nearest voxel, so that
    labels are never mixed); points outside the image are black;
  * one scroll step moves by the thickness of the acquired slices
    (`slice_step` grid slices);
  * the value under the pointer is that of the nearest voxel;
  * the **stored** mode shows the voxels as acquired, without resampling;
  * a warning gives the angle, and the panel shows "Obliquity …°, resampled".

  Images with a single slice keep their own grid.
* **No qform/sform**: orientation is unknown, the stored mode is forced and the
  edge labels are `?`.
* Pixels keep their physical aspect ratio (voxel sizes from the affine).

## Intensities

The window is the 0.5th–99.5th percentile of the finite values of a
deterministic strided sample of the volume shown (at most 1 M voxels), computed after
`scl_slope`/`scl_inter`. When clearly non-zero voxels (|v| > 1e-6) are at least
5 % of the sample, only they are used, so that a large zero background does not
pull the lower bound to zero; then exactly non-zero values; then all values.
Grey levels are `rint(255 · clip((v − low) / (high − low), 0, 1))`, intensities
are smoothed (bilinear) when magnified (see Magnification below).

* NaN and ±Inf are black and counted in the metadata panel.
* A constant volume is mid-grey (black if the constant is 0).
* RGB images are shown in colour (interpolated per channel when resampled);
  **L** shows their luminance, `0.299 R + 0.587 G + 0.114 B`, in grayscale.
  Complex data are shown as magnitude.

## Segmentations

A volume is a label map when it has identity scaling, integral non-negative
values, at most 256 distinct non-zero labels (centre slices and sample) and
piecewise-constant content (at most 30 % of horizontally adjacent foreground
voxel pairs differ in the three centre slices). `intent_code = NIFTI_INTENT_LABEL`
forces label mode.

A segmentation that looks like a FreeSurfer parcellation (aseg, aparc+aseg,
aparc.a2009s+aseg, wmparc, SynthSeg...) is shown with FreeSurfer's colours and
names (FreeSurferColorLUT, see [THIRD-PARTY-NOTICES](../THIRD-PARTY-NOTICES.md)):
at least 3 labels, at least half of them in FreeSurfer's table, and one of the
structures a naive 1, 2, 3... labelling never reaches (41–54, 251–255, 1000 and
above), MIQ's rule; a tissue map 1/2/3 keeps the generic palette. Labels the table
does not have, and every other segmentation, get a deterministic colour
(golden-ratio hue up to 2^24, an integer bit mixer above that to preserve large
identifiers, alternating saturation/brightness). A table of your own
(FreeSurferColorLUT format, `label-lut` in the [settings](SETTINGS.md)) gives the
colours and names of your atlas. 0 is black; a binary mask is white. Label images
are always magnified with nearest-neighbour filtering: colours are never blended.
The **L** key switches between label colours and grayscale.

Pointing at a label shows its number, its name when a table has it, and its
volume: the number of voxels of the volume shown with that label times the voxel
volume (|det| of the affine), in cm³. The metadata panel lists every label of the
volume with its colour, name and volume (collapsed under "Labels"). Spatial
units missing from the header (`xyzt_units = 0`, common in segmentation outputs)
are taken as millimetres; metres and microns are converted.

Unscaled integer labels retain all 64 bits, and unscaled float64 labels retain
their stored precision, including on oblique nearest-voxel grids. Identifiers
outside JavaScript's exact integer range are transported as decimal strings
and shown in full. Grayscale rendering still uses float32; two distinct large
labels can therefore share a gray level when **L** switches off their colours.
For scaled scalar data, the physical value is computed with float64 slope and
intercept before conversion to float32 for display. This preserves finite
physical values when large or small stored values are compensated by scaling.

## Other formats

Every format is shown as NIfTI would show the same voxels in the same place:
the affine maps voxels to RAS world coordinates in mm.

* **FreeSurfer MGH/MGZ**: the affine is FreeSurfer's `vox2ras` (as nibabel:
  direction cosines scaled by the voxel sizes, the centre `c_ras` at voxel
  `dims / 2`). Without `goodRASFlag` the orientation is unknown. Frames are
  volumes; TR, TE, TI and the flip angle (after the data) are read for `.mgh`.
* **MRtrix**: the scanner transform, scaled by `vox`. The voxel grid is the
  stored one: with `layout: -0,-1,+2`, the first two axes count from their end
  and the affine is changed accordingly, so the image is the same. `comments`
  (often patient details) are not shown; the diffusion scheme is summarised.
* **NRRD**: `space directions` and `space origin` converted from the `space`
  (LPS for 3D Slicer) to RAS; `scanner-xyz` or no directions: orientation
  unknown. Axes without a direction (`none`: diffusion gradients, sequence
  frames, segmentation layers) are volumes, wherever they are in the file; a
  first axis of kind `RGB-color` makes an RGB image.

* **MetaImage**: ITK's geometry, LPS converted to RAS: row *i* of
  `TransformMatrix` is the direction of axis *i*, times `ElementSpacing`,
  from `Offset`.
* **DICOM**: patient coordinates (LPS) converted to RAS. Columns go along the
  row direction of `ImageOrientationPatient` (times the column spacing), rows
  along the column direction; slices are sorted along the normal and the third
  axis of the affine goes from the first slice's `ImagePositionPatient` to the
  last one's, so a gantry tilt gives a sheared affine (the views are then
  resampled) and irregular spacing is shown with the mean spacing, with a
  warning. Files at the same position make the volumes, in acquisition order
  (or by the dimension indices of an enhanced file). A mosaic's position is that
  of its corner: its tiles are placed as nibabel and dcm2niix place them. Images
  without a position (radiographs, ultrasound, secondary captures) have no
  known orientation and are shown as stored, rows from the top. The window the
  file gives (`WindowCenter`/`WindowWidth`) is used by default, in rescaled
  values: `LINEAR` with its half-step shifts for integers, exactly centre ±
  width/2 for `LINEAR_EXACT` and for float pixel data (a parametric map's
  centre 0.5 and width 1 is the range 0 to 1, not a threshold); MONOCHROME1 is
  drawn inverted. Values are the modality values
  (rescale or modality LUT, RT dose grid scaling); a rescale that changes from
  slice to slice makes the image float32. Colour images are converted to 8-bit
  RGB (YCbCr full range, palettes taking the high byte of 16-bit entries).

A sparse segmentation (labels that miss a plane through the centre) opens at
the centre of its labels, and its thumbnail cuts through them. A thumbnail
whose view would be a strip (an axial cut through a few coronal slices) shows
the acquired plane instead. DICOM images are never taken for label maps
(except segmentations, SEG), whatever their number of distinct values.

## 4D and higher

The preview opens on the first volume, and only it is read at first (for
`.nii.gz`, the gzip stream is decompressed only up to the end of volume 0). A
slider above the metadata panel scrubs through the volumes (also Shift + wheel
on a view, comma / period, Home / End). Volumes are numbered from 0 over the
dimensions after the third, in Fortran order (`t` for a 4D image); the time
shown is the volume number times the repetition time (`pixdim[4]`) for a 4D
series. Moving away from volume 0 in a `.nii.gz` starts a background
decompression of the whole stream; the slider says how far it got, and the
volume appears as soon as its bytes are there.

The window, the label detection and the range are those of the volume shown
(a b=0 and a b=1000 volume of a diffusion series need different windows). Below
the slider, the time course of the voxel under the pointer (else of the cursor)
shows its value in every volume, at display precision; clicking or dragging on
it shows that volume. When all the volumes do not fit in the image limit
(`max-image-mb`), the backend decodes up to the volume asked and the time
course stays incomplete.

## Spectroscopy (NIfTI-MRS)

A complex image with `intent_name` "mrs_v…" or a NIfTI-MRS header extension
(code 44, JSON) is a spectroscopy file: dimension 4 holds the time-domain
signal (FID), dimensions 5 to 7 the coils, averages, dynamics…, and `pixdim[4]`
is the dwell time.

* **Single voxel**: the spectrum takes the place of the three views.
* **Spectroscopic imaging**: the views show the magnitude of the first time
  point of each voxel, and the spectrum of the cursor voxel sits above the
  metadata (click or scroll to change voxel).
* Coils, dynamics and measurements (`DIM_COIL`, `DIM_DYN`, `DIM_MEAS`) are
  averaged: each transient is phased by its first point and weighted by its
  amplitude (the usual combination of uncombined coils), the first point counts
  half, then the Fourier transform. The other dimensions hold different
  signals (edit conditions, ISIS steps, phase cycles, indirect or user
  dimensions: an edited spectrum is a difference, not an average): the
  spectrum takes their index 0, and the panel says so.
* The axis is the chemical shift, `ppm = shift − f / SF`, with SF the
  spectrometer frequency of the JSON. A finite `SpecFreqChemShift` in that JSON
  supplies the shift, including zero. When absent or invalid, the default is
  4.65 ppm for 1H and 2H (water) and 0 otherwise. High shifts are on the left. Without SF, the axis is
  in Hz. Without a dwell time, it is in points. On the NIfTI-MRS example data,
  this puts NAA at 2.0 ppm, creatine at 3.0 and choline at 3.2.
* 1H spectra show the real part between 0.2 and 4.2 ppm (as FSL-MRS), phased
  so that this range integrates to a positive real. Other nuclei show the
  magnitude over the whole width (no first-order phasing). **L** switches real
  part and magnitude, **Z** the range and the whole width.
* Spectra are computed for at most 65 536 points; all the data must fit in
  `max-image-mb`.
* Standard uppercase nucleus names (for example `23NA`, `3HE`, `129XE`) are
  recognised for the field-strength estimate. A known `EchoTime` of zero is
  retained and displayed as TE 0 ms; an absent or invalid value is omitted.

## Magnification

Intensities are smoothed when magnified, except when a voxel is larger than 6
screen pixels (spectroscopic imaging, fMRI): voxels then stay sharp. Label
maps are never smoothed.
