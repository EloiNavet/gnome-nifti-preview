//! An image as the viewer shows it: the volume, the display mode and
//! colormap asked for, the three view plans, the cursor (a point of the
//! grid the views are cut from) and the slices already drawn. Shared by
//! the quick look and the interactive viewer; everything it computes comes
//! from the library.

use std::path::{Path, PathBuf};

use serde_json::Value;

use gnome_nifti_preview::error::{PreviewError, Result};
use gnome_nifti_preview::formats::dicom;
use gnome_nifti_preview::orientation::{self, Grid, ViewPlan};
use gnome_nifti_preview::panel::{self, Display, Section};
use gnome_nifti_preview::render;
use gnome_nifti_preview::source::ReadOptions;
use gnome_nifti_preview::view::{self, Readout, SliceCache};
use gnome_nifti_preview::volume::Volume;

use crate::args::Options;
use crate::picture::{self, Picture};

/// Slices kept drawn (RGB8 at the size of the slice).
pub const CACHE_BYTES: usize = 64 << 20;

pub struct Viewer {
    pub vol: Volume,
    pub path: PathBuf,
    /// The mode asked for (the effective one is "stored" without orientation).
    pub mode: &'static str,
    /// The colormap asked for ("auto" by default).
    pub colormap: &'static str,
    pub plans: [ViewPlan; 3],
    pub cursor: [usize; 3],
    pub cache: SliceCache,
    scratch: Vec<f32>,
}

/// Open `path` (its header; the data are read later) with the display
/// options of `o`.
pub fn open_volume(path: &Path, o: &Options) -> Result<Volume> {
    let options = ReadOptions { dicom: dicom::Options { series: !o.no_series } };
    // A folder: its DICOMDIR, or its longest DICOM series.
    let mut vol = if path.is_dir() { view::open_folder(path, o.max_bytes)? } else { Volume::open_with(path, o.max_bytes, options)? };
    apply_options(&mut vol, o)?;
    Ok(vol)
}

/// The display options of `o` (window, percentiles, palette, label colours).
pub fn apply_options(vol: &mut Volume, o: &Options) -> Result<()> {
    if let Some((low, high)) = o.percentiles {
        vol.set_percentiles(low, high)?;
    }
    let table = match &o.lut {
        Some(p) => Some(gnome_nifti_preview::lut::read_file(p)?),
        None => None,
    };
    if o.labels != "auto" || table.is_some() {
        vol.set_label_colors(o.labels, table);
    }
    vol.palette = o.palette;
    vol.set_window(o.window)
}

/// `open_volume`, then a viewer of it.
pub fn open(path: &Path, o: &Options) -> Result<Viewer> {
    Viewer::new(open_volume(path, o)?, path, o.mode, o.colormap)
}

impl Viewer {
    pub fn new(vol: Volume, path: &Path, mode: &'static str, colormap: &'static str) -> Result<Viewer> {
        let effective = orientation::effective_mode(mode, vol.orientation_known, &vol.affine);
        let plans = vol.plans(effective)?;
        let cursor = view::start_cursor(&vol, &plans[0].grid);
        Ok(Viewer { vol, path: path.to_path_buf(), mode, colormap, plans, cursor, cache: SliceCache::new(CACHE_BYTES), scratch: Vec::new() })
    }

    /// The mode the views are in ("stored" when the orientation is unknown).
    pub fn effective_mode(&self) -> &'static str {
        orientation::effective_mode(self.mode, self.vol.orientation_known, &self.vol.affine)
    }

    pub fn grid(&self) -> &Grid {
        &self.plans[0].grid
    }

    /// The slice of view `v` the cursor is in.
    pub fn index(&self, v: usize) -> usize {
        self.cursor[self.plans[v].slice_axis]
    }

    /// Show another display mode; the cursor stays at the same place.
    pub fn set_mode(&mut self, mode: &'static str) -> Result<()> {
        let before = self.grid().clone();
        self.mode = mode;
        self.plans = self.vol.plans(self.effective_mode())?;
        self.cursor = view::carry_cursor(&before, self.grid(), self.cursor.map(|x| x as i64));
        Ok(())
    }

    /// Read the data and their statistics (the quick look; the interactive
    /// viewer reads on a helper thread).
    pub fn load(&mut self) -> Result<()> {
        self.vol.ensure_data()?;
        self.vol.stats()?;
        Ok(())
    }

    /// Show volume `k` of a 4D image, waiting for it to be decompressed.
    pub fn show_volume(&mut self, k: usize) -> Result<()> {
        if k >= self.vol.n_volumes() {
            return Err(PreviewError::new("bad_request", format!("--volume {k}: the image has {} volume(s)", self.vol.n_volumes())));
        }
        if self.vol.n_volumes() > 1 {
            while !self.vol.set_volume(k as i64)?.0 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        Ok(())
    }

    /// Where the cursor starts: --at, else the labels of a sparse
    /// segmentation, else the image a DICOM series was opened on or the
    /// centre; then --slice of the view chosen.
    pub fn place_cursor(&mut self, o: &Options) -> Result<()> {
        if let Some(ijk) = o.at {
            let shape = self.vol.shape3;
            if (0..3).any(|a| ijk[a] < 0 || ijk[a] as usize >= shape[a]) {
                return Err(PreviewError::new("bad_request", format!("--at {},{},{} is outside the image ({} × {} × {})", ijk[0], ijk[1], ijk[2], shape[0], shape[1], shape[2])));
            }
            let voxel = ijk.map(|x| x as usize);
            self.cursor = if self.grid().resampled {
                self.grid().from_world(orientation::voxel_to_world(&self.vol.affine, voxel.map(|x| x as f64)))
            } else {
                voxel
            };
        } else if o.colormap != "gray" && o.colormap != "rgb" {
            let grid = self.grid().clone();
            if let Some(c) = view::label_cursor(&mut self.vol, &grid)? {
                self.cursor = c;
            }
        }
        if let (Some(n), Some(name)) = (o.slice, o.view) {
            let plan = self.plans.iter().find(|p| p.view == name).expect("three views");
            if n >= plan.n_slices {
                return Err(PreviewError::new("bad_request", format!("--slice {n}: the {name} view has {} slices (0 to {})", plan.n_slices, plan.n_slices - 1)));
            }
            self.cursor[plan.slice_axis] = n;
        }
        Ok(())
    }

    /// The views to show: the one asked for, the one a single-slice image
    /// has, or all three.
    pub fn views(&self, asked: Option<&str>) -> Vec<usize> {
        let name = asked.or_else(|| view::single_view(&self.vol, &self.plans));
        match name {
            Some(n) => self.plans.iter().position(|p| p.view == n).into_iter().collect(),
            None => vec![0, 1, 2],
        }
    }

    pub fn effective_colormap(&mut self) -> Result<&'static str> {
        render::effective_colormap(&mut self.vol, self.colormap)
    }

    /// The picture of view `v` at `w × h` pixels, with the crosshair or not.
    pub fn picture(&mut self, v: usize, w: usize, h: usize, crosshair: bool) -> Result<Picture> {
        let plan = self.plans[v].clone();
        let index = self.index(v);
        let mode = self.effective_mode();
        let (rgb, cmap) = picture::slice(&mut self.vol, mode, &plan, index, self.colormap, &mut self.cache, &mut self.scratch)?;
        let mut pic = picture::scaled(&rgb, &plan, cmap, w, h)?;
        if crosshair {
            picture::crosshair(&mut pic, &plan, plan.screen_point(self.cursor));
        }
        Ok(pic)
    }

    /// What lies under the cursor.
    pub fn readout(&mut self) -> Result<Readout> {
        let grid = self.grid().clone();
        view::readout(&mut self.vol, &grid, self.cursor.map(|x| x as i64))
    }

    /// The colormap in effect and the window shown (low, high, where it
    /// comes from).
    pub fn shown(&mut self) -> Result<(&'static str, (f64, f64, &'static str))> {
        let cmap = self.effective_colormap()?;
        let w = self.vol.display_window()?;
        Ok((cmap, (w.low, w.high, self.vol.window_source())))
    }

    /// The sections of the panel (Geometry, Data, File, Affine...).
    pub fn sections(&mut self, show_patient: bool) -> Result<Vec<Section>> {
        let header = self.vol.header_info();
        let (cmap, window) = self.shown()?;
        let resampled = self.grid().resampled;
        let stats = self.vol.stats()?;
        let d = Display { effective_colormap: cmap, window: Some(window), resampled, show_patient, pending: false };
        Ok(panel::sections(&header, Some(stats), &d))
    }

    /// What the GNOME viewer shows above its panel: the header's warnings
    /// (orientation, obliquity, what assembling a DICOM image found...) and
    /// the notes of the statistics (a truncated file, NaN voxels...).
    pub fn warnings(&mut self) -> Result<Vec<String>> {
        let header = self.vol.header_info();
        let mut out: Vec<String> =
            header["warnings"].as_array().into_iter().flatten().filter_map(|w| w.as_str().map(str::to_string)).collect();
        out.extend(self.vol.stats()?.notes.iter().cloned());
        Ok(out)
    }

    /// The file name, cleaned for the terminal.
    pub fn name(&self) -> String {
        let n = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.path.display().to_string());
        crate::text::clean(&n)
    }

    /// One line: format, dimensions, voxel size, orientation, datatype and
    /// window (or labels), from the panel's rows.
    pub fn summary(&mut self) -> Result<String> {
        let sections = self.sections(false)?;
        let row = |key: &str| sections.iter().flat_map(|s| s.rows.iter()).find(|(k, _)| k == key).map(|(_, v)| v.clone());
        let mut parts = Vec::new();
        for key in ["Format", "Dimensions", "Volumes", "Voxel size", "Orientation", "Datatype"] {
            if let Some(v) = row(key) {
                parts.push(if key == "Volumes" { format!("{v} volumes") } else { v });
            }
        }
        if let Some(l) = row("Labels") {
            parts.push(if l == "binary mask" { l } else { format!("{l} labels") });
        } else if let Some(w) = row("Window") {
            parts.push(format!("window {w}"));
        }
        if self.vol.n_volumes() > 1 && self.vol.browsable() {
            parts.push(format!("volume {}", self.vol.current_volume()));
        }
        Ok(parts.join(" · "))
    }

    /// The header as the protocol sends it.
    pub fn header(&self) -> Value {
        self.vol.header_info()
    }
}

/// An error with what the backend could tell of the file (`about`).
pub fn about_of(e: &PreviewError) -> Vec<Section> {
    e.about.as_deref().map(panel::about_sections).unwrap_or_default()
}
