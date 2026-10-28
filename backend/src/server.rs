//! JSON-lines protocol spoken with the Sushi plugin over stdin/stdout.
//!
//! Every request is one JSON object per line and gets exactly one response
//! line, in order, so the client may pipeline requests. A `slice` response is
//! followed immediately by `bytes` raw RGB8 bytes. One process serves one
//! preview and exits on EOF or after `close`. See docs/PROTOCOL.md.

use std::io::{BufRead, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::error::{bail, PreviewError, Result};
use crate::orientation::{self, Grid, MODES, VIEWS};
use crate::render::{self, COLORMAPS};
use crate::view::{self, LoadState, Loading};
use crate::volume::{self, Volume, WindowPolicy};
use crate::window::PALETTES;

pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_REQUEST_BYTES: usize = 1 << 20;
pub const DEFAULT_CACHE_BYTES: usize = 64 << 20;
pub use crate::view::SliceCache;

pub enum Reply {
    Json(Value),
    Binary(Value, Arc<Vec<u8>>),
}

pub struct Session {
    pub volume: Option<Volume>,
    mode: &'static str,
    colormap: &'static str,
    pub cache: SliceCache,
    scratch: Vec<f32>,
    /// The last spectrum sent, by voxel.
    spectrum: Option<([usize; 3], Value)>,
    /// A load running in the background (an image on a network file
    /// system): the volume is with it until it is done.
    loading: Option<Loading>,
    pub closed: bool,
}

fn opt_bool(req: &Value, key: &str) -> Result<bool> {
    match req.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => bail!("bad_request", "'{key}' must be a boolean"),
    }
}

fn static_choice(value: &str, choices: &[&'static str], what: &str) -> Result<&'static str> {
    match choices.iter().find(|c| **c == value) {
        Some(c) => Ok(c),
        None => bail!("bad_request", "Unknown {what} {value:?}"),
    }
}

fn opt_str<'a>(req: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match req.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => bail!("bad_request", "'{key}' must be a string"),
    }
}

/// Two finite numbers.
fn pair_of(v: &Value, message: &str) -> Result<(f64, f64)> {
    match v.as_array().map(|a| a.as_slice()) {
        Some([a, b]) => match (a.as_f64(), b.as_f64()) {
            (Some(a), Some(b)) if a.is_finite() && b.is_finite() => Ok((a, b)),
            _ => bail!("bad_request", "{message}"),
        },
        _ => bail!("bad_request", "{message}"),
    }
}

fn as_int(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64().or(Some(i64::MAX)),
        _ => None,
    }
}

/// Three integers (grid coordinates); `message` otherwise.
fn ijk_of(v: Option<&Value>, message: &str) -> Result<[i64; 3]> {
    if let Some(Value::Array(a)) = v {
        if a.len() == 3 {
            if let (Some(i), Some(j), Some(k)) = (as_int(a.first()), as_int(a.get(1)), as_int(a.get(2))) {
                return Ok([i, j, k]);
            }
        }
    }
    bail!("bad_request", "{message}")
}

/// Slice cache size from NIFTI_PREVIEW_CACHE_BYTES (0 to 4 GiB).
pub fn cache_bytes_from_env() -> usize {
    let bytes = crate::volume::bytes_from_env("NIFTI_PREVIEW_CACHE_BYTES", DEFAULT_CACHE_BYTES as u64, (0, 4 << 30));
    usize::try_from(bytes).unwrap_or(DEFAULT_CACHE_BYTES)
}

impl Default for Session {
    /// Slice cache size from NIFTI_PREVIEW_CACHE_BYTES (the viewers set it from
    /// the user settings), 64 MiB otherwise.
    fn default() -> Self {
        Self::new(cache_bytes_from_env())
    }
}

impl Session {
    pub fn new(cache_bytes: usize) -> Self {
        Session {
            volume: None,
            mode: "neurological",
            colormap: "auto",
            cache: SliceCache::new(cache_bytes),
            scratch: Vec::new(),
            spectrum: None,
            loading: None,
            closed: false,
        }
    }

    fn volume(&mut self) -> Result<&mut Volume> {
        match self.volume.as_mut() {
            Some(v) => Ok(v),
            None if self.loading.is_some() => bail!("bad_request", "The image is still being read: ask `load` again"),
            None => bail!("bad_request", "No file is open"),
        }
    }

    fn effective_mode(&mut self) -> Result<&'static str> {
        let mode = self.mode;
        let v = self.volume()?;
        Ok(orientation::effective_mode(mode, v.orientation_known, &v.affine))
    }

    /// The grid the views are currently cut from.
    fn grid(&mut self) -> Result<Grid> {
        let mode = self.effective_mode()?;
        Ok(self.volume()?.plans(mode)?[0].grid.clone())
    }

    fn display(&mut self) -> Result<Map<String, Value>> {
        let mode = self.effective_mode()?;
        let requested = self.mode;
        let plans = self.volume()?.plans(mode)?;
        let mut m = Map::new();
        m.insert("mode".into(), json!(mode));
        m.insert("requested_mode".into(), json!(requested));
        m.insert("grid".into(), plans[0].grid.to_json());
        let mut p = Map::new();
        for plan in plans.iter() {
            p.insert(plan.view.into(), plan.to_json());
        }
        m.insert("plans".into(), Value::Object(p));
        Ok(m)
    }

    pub fn handle(&mut self, req: &Value) -> Result<Reply> {
        if !req.is_object() {
            bail!("bad_request", "Request must be a JSON object");
        }
        let op = match req.get("op") {
            Some(Value::String(s)) => s.as_str(),
            other => bail!("bad_request", "Unknown op {}", other.map(|v| v.to_string()).unwrap_or("None".into())),
        };
        match op {
            "ping" => Ok(Reply::Json(json!({"pong": true}))),
            "open" => self.op_open(req),
            "load" => self.op_load(req),
            "set_display" => self.op_set_display(req),
            "slice" => self.op_slice(req),
            "set_volume" => self.op_set_volume(req),
            "timecourse" => self.op_timecourse(req),
            "value" => self.op_value(req),
            "spectrum" => self.op_spectrum(req),
            "labels" => self.op_labels(),
            "series_list" => self.op_series_list(),
            "open_series" => self.op_open_series(req),
            "close" => {
                self.closed = true;
                self.loading = None;
                self.volume = None;
                self.cache.clear();
                Ok(Reply::Json(json!({"closed": true})))
            }
            other => bail!("bad_request", "Unknown op {other:?}"),
        }
    }

    fn op_open(&mut self, req: &Value) -> Result<Reply> {
        let path = match opt_str(req, "path")? {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => bail!("bad_request", "open requires a 'path' string"),
        };
        let mode = match opt_str(req, "mode")? {
            Some(m) => static_choice(m, &MODES, "mode")?,
            None => self.mode,
        };
        // DICOM: rebuild the series from the folder (default), or show the file alone.
        let series = match req.get("dicom_series") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => *b,
            Some(_) => bail!("bad_request", "'dicom_series' must be a boolean"),
        };
        self.loading = None;
        let options = crate::source::ReadOptions { dicom: crate::formats::dicom::Options { series } };
        let volume = Volume::open_with(Path::new(&path), volume::max_bytes_from_env(), options)?;
        self.opened(volume, mode)
    }

    /// The reply to `open` (and `open_series`) once the volume is there.
    fn opened(&mut self, volume: Volume, mode: &'static str) -> Result<Reply> {
        self.loading = None;
        self.volume = Some(volume);
        self.mode = mode;
        self.cache.clear();
        self.spectrum = None;
        let header = self.volume.as_ref().unwrap().header_info();
        let grid = self.grid()?;
        // A DICOM series opens on the file chosen.
        let v = self.volume.as_ref().unwrap();
        let cursor = view::start_cursor(v, &grid);
        let mut m = Map::new();
        m.insert("header".into(), header);
        m.insert("cursor".into(), json!(cursor));
        m.insert("volume".into(), json!(v.current_volume()));
        m.extend(self.display()?);
        Ok(Reply::Json(Value::Object(m)))
    }

    /// The DICOM series next to the image (its folder, or its DICOMDIR),
    /// grouped by study in the client.
    fn op_series_list(&mut self) -> Result<Reply> {
        let (catalog, current) = view::series_catalog(self.volume()?)?;
        let series: Vec<Value> = catalog
            .related(current.as_deref())
            .into_iter()
            .map(|s| {
                json!({
                    "uid": s.uid, "number": s.number, "description": s.description, "modality": s.modality,
                    "study_uid": s.study_uid, "study_description": s.study_description, "study_date": s.study_date,
                    "patient": s.patient, "images": s.images, "current": current.as_deref() == Some(s.uid.as_str()),
                })
            })
            .collect();
        Ok(Reply::Json(json!({
            "series": series,
            "origin": if catalog.dicomdir { "dicomdir" } else { "folder" },
            "incomplete": catalog.incomplete,
        })))
    }

    /// Open another series of the catalog `series_list` gave.
    fn op_open_series(&mut self, req: &Value) -> Result<Reply> {
        let Some(uid) = opt_str(req, "uid")? else { bail!("bad_request", "open_series requires a 'uid' string") };
        let mode = match opt_str(req, "mode")? {
            Some(m) => static_choice(m, &MODES, "mode")?,
            None => self.mode,
        };
        let volume = view::open_series(self.volume()?, uid)?;
        self.opened(volume, mode)
    }

    /// A sparse segmentation (a hippocampus, lesions): when a plane through
    /// the centre of the grid holds no label, the grid point at the centre
    /// of the labels, for the client to start there.
    fn label_cursor(&mut self) -> Result<Option<Value>> {
        let grid = self.grid()?;
        Ok(view::label_cursor(self.volume()?, &grid)?.map(|c| json!(c)))
    }

    /// Statistics of the volume shown, its colormaps.
    fn stats_reply(&mut self) -> Result<Map<String, Value>> {
        let colormap = self.colormap;
        let v = self.volume()?;
        let s = v.stats()?;
        let stats = json!({
            "window": {"low": s.window.low, "high": s.window.high, "status": s.window.status.as_str()},
            "labels": {"is_labels": s.labels.is_labels, "binary": s.labels.binary, "n_labels": s.labels.n_labels, "reason": s.labels.reason,
                       "scheme": s.labels.scheme},
            "non_finite": s.non_finite,
            "non_finite_exact": s.non_finite_exact,
            "min": s.min,
            "max": s.max,
            "notes": s.notes,
        });
        let effective = render::effective_colormap(v, colormap)?;
        let choices = render::colormap_choices(v)?;
        let shown = v.display_window()?;
        let source = v.window_source();
        let palette = v.palette;
        let mut m = Map::new();
        m.insert("display_window".into(), json!({"low": shown.low, "high": shown.high, "source": source}));
        m.insert("palette".into(), json!(palette));
        m.insert("stats".into(), stats);
        m.insert("colormap".into(), json!(colormap));
        m.insert("effective_colormap".into(), json!(effective));
        m.insert("colormaps".into(), json!(choices));
        Ok(m)
    }

    /// Statistics of volume 0. Optional display settings: `window_percentiles`
    /// ([low, high], 0 to 100), `window_volume` ("per-volume" or "first") and
    /// `palette`.
    fn op_load(&mut self, req: &Value) -> Result<Reply> {
        // A background load: how far it got, or its result.
        if let Some(job) = &self.loading {
            match job.poll() {
                LoadState::Done(volume, result) => {
                    self.loading = None;
                    self.volume = Some(*volume);
                    result?;
                    self.cache.clear();
                    let mut m = self.stats_reply()?;
                    m.insert("volume".into(), json!(self.volume()?.current_volume()));
                    m.insert("ready".into(), json!(true));
                    if let Some(c) = self.label_cursor()? {
                        m.insert("label_cursor".into(), c);
                    }
                    return Ok(Reply::Json(Value::Object(m)));
                }
                LoadState::Running => {
                    return Ok(Reply::Json(json!({
                        "ready": false, "progress": job.fraction(), "bytes_per_second": job.rate(),
                    })));
                }
                LoadState::Lost => {
                    self.loading = None;
                    bail!("internal", "Internal error while reading the image.");
                }
            }
        }
        let percentiles = match req.get("window_percentiles") {
            None | Some(Value::Null) => None,
            Some(v) => Some(pair_of(v, "window_percentiles must be [low, high]")?),
        };
        let policy = match opt_str(req, "window_volume")? {
            None => None,
            Some("per-volume") => Some(WindowPolicy::PerVolume),
            Some("first") => Some(WindowPolicy::First),
            Some(other) => bail!("bad_request", "Unknown window_volume {other:?}"),
        };
        let palette = match opt_str(req, "palette")? {
            Some(p) => Some(static_choice(p, &PALETTES, "palette")?),
            None => None,
        };
        // Label colours: a scheme, and a user table (FreeSurferColorLUT format).
        let label_colors = match opt_str(req, "label_colors")? {
            Some(c) => Some(static_choice(c, &["auto", "freesurfer", "random"], "label_colors")?),
            None => None,
        };
        let table = match opt_str(req, "label_lut")? {
            Some(p) if !p.is_empty() => Some(crate::lut::read_file(Path::new(p))?),
            _ => None,
        };
        // DICOM: the window the file gives ("file", default) or the automatic one ("auto").
        let file_window = match opt_str(req, "dicom_window")? {
            None => None,
            Some("file") => Some(true),
            Some("auto") => Some(false),
            Some(other) => bail!("bad_request", "Unknown dicom_window {other:?}"),
        };
        let v = self.volume()?;
        if let Some(on) = file_window {
            v.set_use_file_window(on);
        }
        if label_colors.is_some() || table.is_some() {
            v.set_label_colors(label_colors.unwrap_or("auto"), table);
        }
        if let Some((low, high)) = percentiles {
            v.set_percentiles(low, high)?;
        }
        if let Some(p) = policy {
            v.set_window_policy(p);
        }
        if let Some(p) = palette {
            v.palette = p;
        }
        self.cache.clear();
        // On a network file system the data may take long to read: with
        // `defer_network` nothing is read yet (the client asks first, or
        // waits a moment); with `async` they are read on a helper thread and
        // `load` answers `ready: false` with the progress until done.
        let v = self.volume()?;
        if v.storage.network && !v.data_loaded() {
            if opt_bool(req, "defer_network")? {
                return Ok(Reply::Json(json!({
                    "ready": false, "deferred": true, "bytes_to_read": v.bytes_to_read(), "progress": 0.0,
                })));
            }
            if opt_bool(req, "async")? {
                let volume = Box::new(self.volume.take().unwrap());
                match self.start_loading(volume) {
                    Ok(reply) => return Ok(Reply::Json(reply)),
                    // No thread could be started: read here.
                    Err(volume) => self.volume = Some(*volume),
                }
            }
        }
        let mut m = self.stats_reply()?;
        m.insert("volume".into(), json!(self.volume()?.current_volume()));
        if let Some(c) = self.label_cursor()? {
            m.insert("label_cursor".into(), c);
        }
        Ok(Reply::Json(Value::Object(m)))
    }

    /// Read the data of `volume` (and its statistics) on a helper thread;
    /// the volume back when no thread can be started.
    fn start_loading(&mut self, volume: Box<Volume>) -> std::result::Result<Value, Box<Volume>> {
        self.loading = Some(Loading::start(volume)?);
        Ok(json!({"ready": false, "progress": 0.0, "bytes_per_second": null}))
    }

    /// Time of volume `k` (k × TR) for a 4D series with a repetition time.
    fn volume_time(&mut self, k: usize) -> Result<Value> {
        Ok(json!(view::volume_time(&self.volume()?.header, k)))
    }

    /// Show another volume of a 4D image. A volume still being decompressed
    /// answers `ready: false` with the `progress` made; the client asks again.
    fn op_set_volume(&mut self, req: &Value) -> Result<Reply> {
        let Some(k) = as_int(req.get("volume")) else { bail!("bad_request", "set_volume requires an integer 'volume'") };
        let (ready, progress) = self.volume()?.set_volume(k)?;
        let v = self.volume()?;
        let wanted = k.clamp(0, v.last_volume()) as usize;
        let shown = v.current_volume();
        let n_volumes = v.n_volumes();
        let mut m = if ready { self.stats_reply()? } else { Map::new() };
        m.insert("volume".into(), json!(if ready { shown } else { wanted }));
        m.insert("shown".into(), json!(shown));
        m.insert("n_volumes".into(), json!(n_volumes));
        m.insert("ready".into(), json!(ready));
        m.insert("progress".into(), json!(progress));
        m.insert("time".into(), self.volume_time(wanted)?);
        Ok(Reply::Json(Value::Object(m)))
    }

    /// Values of the voxel nearest to grid point `ijk` in every volume.
    fn op_timecourse(&mut self, req: &Value) -> Result<Reply> {
        let ijk = ijk_of(req.get("ijk"), "timecourse requires 'ijk' as three integers")?;
        let voxel = view::voxel_under(&self.grid()?, ijk)?;
        let v = self.volume()?;
        let (tr, units) = (v.header.repetition_time, v.header.time_units());
        let (values, complete) = match voxel {
            Some(p) => v.timecourse(p)?,
            None => (Vec::new(), true),
        };
        let progress = v.decoded_fraction();
        Ok(Reply::Json(json!({
            "ijk": ijk, "voxel": voxel, "values": values, "complete": complete, "progress": progress,
            "volume": v.current_volume(), "repetition_time": tr, "time_units": units,
        })))
    }

    fn op_set_display(&mut self, req: &Value) -> Result<Reply> {
        self.volume()?;
        let mode = match opt_str(req, "mode")? {
            Some(m) => static_choice(m, &MODES, "mode")?,
            None => self.mode,
        };
        let colormap = match opt_str(req, "colormap")? {
            Some(c) => static_choice(c, &COLORMAPS, "colormap")?,
            None => self.colormap,
        };
        // Palette and window of the grey levels: what is cached was drawn
        // with the previous ones.
        let palette = match opt_str(req, "palette")? {
            Some(p) => Some(static_choice(p, &PALETTES, "palette")?),
            None => None,
        };
        let window = match req.get("window") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(w) => Some(Some(pair_of(w, "window must be [low, high] or null")?)),
        };
        if palette.is_some() || window.is_some() {
            let v = self.volume()?;
            if let Some(w) = window {
                v.set_window(w)?;
            }
            if let Some(p) = palette {
                v.palette = p;
            }
            self.cache.clear();
        }
        // The cursor of the current grid, carried over to the new one.
        let cursor = match req.get("cursor") {
            None | Some(Value::Null) => None,
            Some(c) => Some(ijk_of(Some(c), "set_display 'cursor' must be three integers")?),
        };
        let before = self.grid()?;
        self.mode = mode;
        self.colormap = colormap;
        let after = self.grid()?;
        let mut m = self.display()?;
        if let Some(c) = cursor {
            m.insert("cursor".into(), json!(view::carry_cursor(&before, &after, c)));
        }
        m.insert("colormap".into(), json!(colormap));
        let v = self.volume()?;
        m.insert("palette".into(), json!(v.palette));
        if v.stats_if_loaded().is_some() {
            m.insert("effective_colormap".into(), json!(render::effective_colormap(v, colormap)?));
            let shown = v.display_window()?;
            let source = v.window_source();
            m.insert("display_window".into(), json!({"low": shown.low, "high": shown.high, "source": source}));
        }
        Ok(Reply::Json(Value::Object(m)))
    }

    fn op_slice(&mut self, req: &Value) -> Result<Reply> {
        self.volume()?;
        let view = match opt_str(req, "view")? {
            Some(v) => static_choice(v, &VIEWS, "view")?,
            None => bail!("bad_request", "Unknown view None"),
        };
        let index = match as_int(req.get("index")) {
            Some(i) => i,
            None => bail!("bad_request", "slice requires an integer 'index'"),
        };
        let colormap = match opt_str(req, "colormap")? {
            Some(c) => static_choice(c, &COLORMAPS, "colormap")?,
            None => self.colormap,
        };
        let mode = self.effective_mode()?;
        let plans = self.volume()?.plans(mode)?;
        let plan = plans.iter().find(|p| p.view == view).unwrap().clone();
        let index = index.clamp(0, plan.n_slices as i64 - 1) as usize;
        let cmap = render::effective_colormap(self.volume()?, colormap)?;
        let key = (mode, cmap, view, index, self.volume()?.current_volume());
        let (rgb, w, h) = match self.cache.get(&key) {
            Some(hit) => hit,
            None => {
                let mut scratch = std::mem::take(&mut self.scratch);
                let result = render::render_slice(self.volume()?, &plan, index, cmap, &mut scratch);
                self.scratch = scratch;
                let (rgb, _) = result?;
                let entry = (Arc::new(rgb), plan.width, plan.height);
                self.cache.put(key, entry.clone());
                entry
            }
        };
        let header = json!({
            "view": view, "index": index, "width": w, "height": h,
            "colormap": cmap, "format": "rgb8", "bytes": rgb.len(),
        });
        Ok(Reply::Binary(header, rgb))
    }

    /// Spectrum of the voxel nearest to grid point `ijk` (the centre voxel
    /// without `ijk`); `voxel` is null outside the image.
    fn op_spectrum(&mut self, req: &Value) -> Result<Reply> {
        if self.volume()?.mrs.is_none() {
            bail!("bad_request", "Not a spectroscopy (NIfTI-MRS) file");
        }
        let voxel = match req.get("ijk") {
            None | Some(Value::Null) => Some(self.volume()?.center()),
            Some(v) => {
                let ijk = ijk_of(Some(v), "spectrum 'ijk' must be three integers")?;
                view::voxel_under(&self.grid()?, ijk)?
            }
        };
        let Some(voxel) = voxel else { return Ok(Reply::Json(json!({"voxel": null}))) };
        if let Some((cached, reply)) = &self.spectrum {
            if *cached == voxel {
                return Ok(Reply::Json(reply.clone()));
            }
        }
        let reply = self.volume()?.spectrum(voxel)?.to_json(voxel);
        self.spectrum = Some((voxel, reply.clone()));
        Ok(Reply::Json(reply))
    }

    /// Value under grid point `ijk`: that of the nearest voxel (`voxel`, null
    /// outside the image).
    fn op_value(&mut self, req: &Value) -> Result<Reply> {
        let ijk = ijk_of(req.get("ijk"), "value requires 'ijk' as three integers")?;
        let grid = self.grid()?;
        let r = view::readout(self.volume()?, &grid, ijk)?;
        let json_value = r.value.map(|x| if r.categorical { x.to_label_json() } else { x.to_json() });
        let mut reply = json!({"ijk": r.ijk, "world": r.world_mm, "voxel": r.voxel, "value": json_value});
        if let Some(rgb) = r.rgb {
            reply["rgb"] = json!(rgb);
        }
        // In a label map, the name and size of the structure under the cursor.
        if let Some(label) = r.label {
            if let Some(name) = label.name {
                reply["label_name"] = json!(name);
            }
            reply["label_voxels"] = json!(label.voxels);
            if let Some(mm3) = label.volume_mm3 {
                reply["label_volume_mm3"] = json!(mm3);
            }
        }
        Ok(Reply::Json(reply))
    }
}

impl Session {
    /// The labels of the volume shown: value, name (from the colour table),
    /// colour, number of voxels and volume in mm³, in increasing order.
    fn op_labels(&mut self) -> Result<Reply> {
        let list = view::label_rows(self.volume()?)?;
        let rows: Vec<Value> = list
            .rows
            .iter()
            .map(|r| {
                json!({"label": r.value.to_label_json(), "name": r.name, "rgb": r.rgb, "voxels": r.voxels,
                       "volume_mm3": r.volume_mm3})
            })
            .collect();
        Ok(Reply::Json(json!({"labels": rows, "complete": list.complete, "scheme": list.scheme})))
    }
}

/// Handle one request line; never panics.
pub fn respond(session: &mut Session, line: &[u8]) -> (Value, Option<Arc<Vec<u8>>>) {
    let mut id = Value::Null;
    let result: Result<Reply> = (|| {
        if line.len() > MAX_REQUEST_BYTES {
            bail!("bad_request", "Request too large");
        }
        let req: Value = serde_json::from_slice(line).map_err(|_| PreviewError::new("bad_request", "Malformed JSON request"))?;
        if let Some(v) = req.get("id") {
            id = v.clone();
        }
        match catch_unwind(AssertUnwindSafe(|| session.handle(&req))) {
            Ok(r) => r,
            Err(p) => {
                // The details go to stderr (the previewer log), not to the UI.
                let msg = p
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| p.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic".into());
                eprintln!("nifti-preview: internal error: {msg}");
                Err(PreviewError::new("internal", "Internal error in the preview backend."))
            }
        }
    })();
    let merge = |extra: Value| {
        let mut m = Map::new();
        m.insert("id".into(), id.clone());
        m.insert("ok".into(), json!(true));
        if let Value::Object(o) = extra {
            m.extend(o);
        }
        Value::Object(m)
    };
    match result {
        Ok(Reply::Json(v)) => (merge(v), None),
        Ok(Reply::Binary(v, payload)) => (merge(v), Some(payload)),
        Err(e) => {
            let mut error = json!({"code": e.code, "message": e.message});
            if let Some(about) = e.about {
                error["about"] = *about;
            }
            (json!({"id": id, "ok": false, "error": error}), None)
        }
    }
}

fn write_message<W: Write>(out: &mut W, header: &Value, payload: Option<&[u8]>) -> std::io::Result<()> {
    serde_json::to_writer(&mut *out, header)?;
    out.write_all(b"\n")?;
    if let Some(p) = payload {
        out.write_all(p)?;
    }
    out.flush()
}

pub fn serve<R: BufRead, W: Write>(mut input: R, mut output: W) -> std::io::Result<()> {
    let mut hello = json!({
        "event": "hello", "protocol": PROTOCOL_VERSION, "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(), "backend": "rust",
    });
    if let Some(r) = crate::sandbox::current() {
        hello["sandbox"] = json!({"data_limit": r.data_limit, "no_new_privs": r.no_new_privs, "seccomp": r.seccomp, "landlock": r.landlock});
    }
    write_message(&mut output, &hello, None)?;
    let mut session = Session::default();
    let mut line = Vec::with_capacity(256);
    loop {
        line.clear();
        let n = input.by_ref().take(MAX_REQUEST_BYTES as u64 + 1).read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(()); // EOF: the preview closed or Sushi died
        }
        if line.len() > MAX_REQUEST_BYTES && !line.ends_with(b"\n") {
            discard_line(&mut input)?;
        }
        let trimmed = line.trim_ascii();
        if trimmed.is_empty() {
            continue;
        }
        let (header, payload) = respond(&mut session, trimmed);
        write_message(&mut output, &header, payload.as_deref().map(|v| v.as_slice()))?;
        if session.closed {
            return Ok(());
        }
    }
}

/// Skip the rest of an oversized line without storing it.
fn discard_line<R: BufRead>(input: &mut R) -> std::io::Result<()> {
    loop {
        let buf = input.fill_buf()?;
        if buf.is_empty() {
            return Ok(());
        }
        match buf.iter().position(|&b| b == b'\n') {
            Some(i) => {
                input.consume(i + 1);
                return Ok(());
            }
            None => {
                let n = buf.len();
                input.consume(n);
            }
        }
    }
}

/// Ask the kernel to terminate this helper if its parent (Sushi) dies.
pub fn die_with_parent() {
    #[cfg(target_os = "linux")]
    {
        extern "C" {
            fn prctl(option: i32, ...) -> i32;
        }
        const PR_SET_PDEATHSIG: i32 = 1;
        const SIGTERM: i32 = 15;
        // SAFETY: plain syscall wrapper with integer arguments.
        unsafe {
            prctl(PR_SET_PDEATHSIG, SIGTERM as std::ffi::c_ulong);
        }
    }
}
