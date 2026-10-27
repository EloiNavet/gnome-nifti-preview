//! JSON-lines protocol spoken with the Sushi plugin over stdin/stdout.
//!
//! Every request is one JSON object per line and gets exactly one response
//! line, in order, so the client may pipeline requests. A `slice` response is
//! followed immediately by `bytes` raw RGB8 bytes. One process serves one
//! preview and exits on EOF or after `close`. See docs/PROTOCOL.md.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::error::{bail, PreviewError, Result};
use crate::orientation::{self, Grid, MODES, VIEWS};
use crate::render::{self, COLORMAPS};
use crate::volume::{self, Volume, WindowPolicy};
use crate::window::PALETTES;

pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_REQUEST_BYTES: usize = 1 << 20;
pub const DEFAULT_CACHE_BYTES: usize = 64 << 20;
/// Bookkeeping counted per cached slice on top of its pixels.
const CACHE_ENTRY_OVERHEAD: usize = 256;

/// Mode, colormap, view, slice index and volume.
type Key = (&'static str, &'static str, &'static str, usize, usize);

pub struct SliceCache {
    max_bytes: usize,
    size: usize,
    items: HashMap<Key, (Arc<Vec<u8>>, usize, usize)>,
    order: VecDeque<Key>,
}

impl SliceCache {
    pub fn new(max_bytes: usize) -> Self {
        SliceCache { max_bytes, size: 0, items: HashMap::new(), order: VecDeque::new() }
    }

    fn touch(&mut self, key: &Key) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(pos).unwrap();
            self.order.push_back(k);
        }
    }

    pub fn get(&mut self, key: &Key) -> Option<(Arc<Vec<u8>>, usize, usize)> {
        let hit = self.items.get(key).cloned();
        if hit.is_some() {
            self.touch(key);
        }
        hit
    }

    pub fn put(&mut self, key: Key, value: (Arc<Vec<u8>>, usize, usize)) {
        let cost = value.0.len() + CACHE_ENTRY_OVERHEAD;
        if cost > self.max_bytes {
            return;
        }
        if let Some(old) = self.items.remove(&key) {
            self.size -= old.0.len() + CACHE_ENTRY_OVERHEAD;
            if let Some(pos) = self.order.iter().position(|k| *k == key) {
                self.order.remove(pos);
            }
        }
        self.items.insert(key, value);
        self.order.push_back(key);
        self.size += cost;
        while self.size > self.max_bytes {
            let Some(old) = self.order.pop_front() else { break };
            if let Some(v) = self.items.remove(&old) {
                self.size -= v.0.len() + CACHE_ENTRY_OVERHEAD;
            }
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.size = 0;
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

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

/// A volume being read on a helper thread, and how far it got.
struct Loading {
    done: std::sync::mpsc::Receiver<(Box<Volume>, Result<()>)>,
    progress: Arc<crate::progress::Progress>,
    started: std::time::Instant,
}

impl Drop for Loading {
    /// A load given up (another file, close): its reads stop.
    fn drop(&mut self) {
        self.progress.cancel();
    }
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
        let cursor = match v.header.dicom.as_ref().and_then(|d| d.start) {
            Some((voxel, _)) => {
                let w = orientation::voxel_to_world(&v.affine, voxel.map(|x| x as f64));
                if grid.resampled || v.orientation_known { grid.from_world(w) } else { voxel }
            }
            None => grid.center(),
        };
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
        let v = self.volume()?;
        let Some(info) = v.header.dicom.as_mut() else { bail!("bad_request", "Not a DICOM image") };
        if info.catalog.is_none() {
            let Some(folder) = info.folder.clone() else { bail!("not_found", "No folder to look for series in") };
            let (catalog, _) = crate::formats::dicom::catalog::scan_folder(&folder, None, None)?;
            info.catalog = Some(Arc::new(catalog));
        }
        let catalog = info.catalog.clone().unwrap();
        let current = info.series_uid.clone();
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
        let v = self.volume()?;
        let Some(info) = v.header.dicom.as_ref() else { bail!("bad_request", "Not a DICOM image") };
        let Some(catalog) = info.catalog.clone() else { bail!("bad_request", "List the series first (series_list)") };
        let Some(entry) = catalog.find(uid) else { bail!("not_found", "No series {uid:?} here") };
        let folder = info.folder.clone();
        let header = crate::formats::dicom::open_entry(entry, catalog.clone(), folder)?;
        let first = entry.files.first().cloned().unwrap_or_default();
        let file = crate::source::open_data_file(&first)?;
        let file_size = file.metadata().map_err(crate::source::io_error)?.len();
        let storage = crate::storage::of(&file, &first);
        let opened = crate::source::Opened { header, extensions: Vec::new(), compressed: false, file_size, file, storage };
        let volume = Volume::from_opened(&first, opened, volume::max_bytes_from_env())?;
        self.opened(volume, mode)
    }

    /// A sparse segmentation (a hippocampus, lesions): when a plane through
    /// the centre of the grid holds no label, the grid point at the centre
    /// of the labels, for the client to start there.
    fn label_cursor(&mut self) -> Result<Option<Value>> {
        let grid = self.grid()?;
        let v = self.volume()?;
        let Some((centre, planes)) = v.label_extent()? else { return Ok(None) };
        let at = grid.to_voxel(grid.center());
        let empty = (0..3).any(|a| {
            let i = (at[a].round().max(0.0) as usize).min(planes[a].len() - 1);
            planes[a][i] == 0
        });
        if !empty {
            return Ok(None);
        }
        let world = orientation::voxel_to_world(&v.affine, centre);
        Ok(Some(json!(grid.from_world(world))))
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
            match job.done.try_recv() {
                Ok((volume, result)) => {
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
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    let seconds = job.started.elapsed().as_secs_f64();
                    let rate = job.progress.bytes().filter(|_| seconds > 0.2).map(|b| b as f64 / seconds);
                    return Ok(Reply::Json(json!({
                        "ready": false, "progress": job.progress.fraction(), "bytes_per_second": rate,
                    })));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
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
        let progress = Arc::new(crate::progress::Progress::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = progress.clone();
        // The thread takes the volume from here; a thread that never ran leaves it.
        let slot = Arc::new(std::sync::Mutex::new(Some(volume)));
        let theirs = slot.clone();
        let spawned = std::thread::Builder::new().name("load".into()).spawn(move || {
            let Some(mut volume) = theirs.lock().unwrap_or_else(|p| p.into_inner()).take() else { return };
            crate::progress::set(Some(shared));
            let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
                volume.ensure_data()?;
                volume.stats()?;
                Ok(())
            }))
            .unwrap_or_else(|_| Err(PreviewError::new("internal", "Internal error while reading the image.")));
            crate::progress::set(None);
            let _ = tx.send((volume, result));
        });
        match spawned {
            Ok(_) => {
                self.loading = Some(Loading { done: rx, progress, started: std::time::Instant::now() });
                Ok(json!({"ready": false, "progress": 0.0, "bytes_per_second": null}))
            }
            Err(_) => Err(slot.lock().unwrap_or_else(|p| p.into_inner()).take().expect("the thread never started")),
        }
    }

    /// Time of volume `k` (k × TR) for a 4D series with a repetition time.
    fn volume_time(&mut self, k: usize) -> Result<Value> {
        let h = &self.volume()?.header;
        Ok(match h.repetition_time {
            Some(tr) if h.ndim() == 4 && tr.is_finite() && tr > 0.0 => json!(k as f64 * tr),
            _ => Value::Null,
        })
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
        let grid = self.grid()?;
        if (0..3).any(|a| ijk[a] < 0 || ijk[a] as usize >= grid.shape[a]) {
            bail!("bad_request", "Voxel {:?} outside image {:?}", ijk, grid.shape);
        }
        let g = ijk.map(|x| x as usize);
        let voxel = if grid.resampled { grid.nearest(grid.to_voxel(g)) } else { Some(g) };
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
            let c = c.map(|x| x.max(0) as usize);
            let c = [0, 1, 2].map(|a| c[a].min(before.shape[a] - 1));
            // Grids of the same kind are the same grid (one image).
            let moved = if before.resampled == after.resampled {
                c
            } else {
                after.from_world(orientation::voxel_to_world(&before.affine, c.map(|x| x as f64)))
            };
            m.insert("cursor".into(), json!(moved));
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
                let grid = self.grid()?;
                if (0..3).any(|a| ijk[a] < 0 || ijk[a] as usize >= grid.shape[a]) {
                    bail!("bad_request", "Voxel {:?} outside image {:?}", ijk, grid.shape);
                }
                let g = ijk.map(|x| x as usize);
                if grid.resampled { grid.nearest(grid.to_voxel(g)) } else { Some(g) }
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
        if (0..3).any(|a| ijk[a] < 0 || ijk[a] as usize >= grid.shape[a]) {
            bail!("bad_request", "Voxel {:?} outside image {:?}", ijk, grid.shape);
        }
        let g = ijk.map(|x| x as usize);
        let voxel = if grid.resampled { grid.nearest(grid.to_voxel(g)) } else { Some(g) };
        let v = self.volume()?;
        let value = match voxel {
            Some(p) => v.exact_value_at(p.map(|x| x as i64))?,
            None => None,
        };
        // In mm, whatever the spatial unit of the header (as the volumes).
        let mm = v.mm_per_unit();
        let world = orientation::voxel_to_world(&grid.affine, g.map(|x| x as f64)).map(|x| x * mm);
        let categorical = value.is_some() && v.stats()?.labels.is_labels;
        let json_value = value.map(|x| if categorical { x.to_label_json() } else { x.to_json() });
        let mut reply = json!({"ijk": ijk, "world": world, "voxel": voxel, "value": json_value});
        if let Some(p) = voxel {
            if let Some(rgb) = v.rgb_at(p)? {
                reply["rgb"] = json!(rgb);
            }
        }
        // In a label map, the name and size of the structure under the cursor.
        if let Some(x) = value.filter(|x| !x.is_zero()) {
            if categorical {
                if let Some(e) = v.label_table().and_then(|t| t.get(&x.label_id())) {
                    reply["label_name"] = json!(e.name);
                }
                let n = v.count_equal(x)?;
                reply["label_voxels"] = json!(n);
                let mm3 = n as f64 * v.voxel_volume_mm3();
                if mm3.is_finite() {
                    reply["label_volume_mm3"] = json!(mm3);
                }
            }
        }
        Ok(Reply::Json(reply))
    }
}

impl Session {
    /// The labels of the volume shown: value, name (from the colour table),
    /// colour, number of voxels and volume in mm³, in increasing order.
    fn op_labels(&mut self) -> Result<Reply> {
        let v = self.volume()?;
        let s = v.stats()?;
        if !s.labels.is_labels {
            return Ok(Reply::Json(json!({"labels": [], "complete": true, "scheme": ""})));
        }
        let (binary, scheme) = (s.labels.binary, s.labels.scheme);
        let (counts, complete) = v.label_counts()?;
        let mm3 = v.voxel_volume_mm3();
        let table = v.label_table();
        let list: Vec<Value> = counts
            .iter()
            .map(|(value, n)| {
                let id = value.label_id();
                let entry = table.and_then(|t| t.get(&id));
                let rgb = if binary { [255, 255, 255] } else { entry.map_or_else(|| crate::labels::label_color(id), |e| e.rgb) };
                let volume = *n as f64 * mm3;
                json!({"label": value.to_label_json(), "name": entry.map(|e| e.name.clone()), "rgb": rgb, "voxels": n,
                       "volume_mm3": if volume.is_finite() { json!(volume) } else { Value::Null }})
            })
            .collect();
        Ok(Reply::Json(json!({"labels": list, "complete": complete, "scheme": scheme})))
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
