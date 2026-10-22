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
use crate::volume::Volume;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 1 << 20;
pub const DEFAULT_CACHE_BYTES: usize = 64 << 20;
/// Bookkeeping counted per cached slice on top of its pixels.
const CACHE_ENTRY_OVERHEAD: usize = 256;

type Key = (&'static str, &'static str, &'static str, usize);

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
    pub closed: bool,
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
            closed: false,
        }
    }

    fn volume(&mut self) -> Result<&mut Volume> {
        match self.volume.as_mut() {
            Some(v) => Ok(v),
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
            "load" => self.op_load(),
            "set_display" => self.op_set_display(req),
            "slice" => self.op_slice(req),
            "value" => self.op_value(req),
            "spectrum" => self.op_spectrum(req),
            "close" => {
                self.closed = true;
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
        let volume = Volume::open(Path::new(&path))?;
        self.volume = Some(volume);
        self.mode = mode;
        self.cache.clear();
        self.spectrum = None;
        let header = self.volume.as_ref().unwrap().header_info();
        let mut m = Map::new();
        m.insert("header".into(), header);
        m.insert("cursor".into(), json!(self.grid()?.center()));
        m.extend(self.display()?);
        Ok(Reply::Json(Value::Object(m)))
    }

    fn op_load(&mut self) -> Result<Reply> {
        let colormap = self.colormap;
        let v = self.volume()?;
        let s = v.stats()?;
        let stats = json!({
            "window": {"low": s.window.low, "high": s.window.high, "status": s.window.status.as_str()},
            "labels": {"is_labels": s.labels.is_labels, "binary": s.labels.binary, "n_labels": s.labels.n_labels, "reason": s.labels.reason},
            "non_finite": s.non_finite,
            "non_finite_exact": s.non_finite_exact,
            "min": s.min,
            "max": s.max,
            "notes": s.notes,
        });
        let effective = render::effective_colormap(v, colormap)?;
        let choices = render::colormap_choices(v)?;
        Ok(Reply::Json(json!({"stats": stats, "colormap": colormap, "effective_colormap": effective, "colormaps": choices})))
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
        if v.stats_if_loaded().is_some() {
            m.insert("effective_colormap".into(), json!(render::effective_colormap(v, colormap)?));
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
        let key = (mode, cmap, view, index);
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
        // In a label map, the size of the structure under the cursor.
        if let Some(x) = value.filter(|x| !x.is_zero()) {
            if categorical {
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
        Err(e) => (json!({"id": id, "ok": false, "error": {"code": e.code, "message": e.message}}), None),
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
