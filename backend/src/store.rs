//! Access to every volume of a 4D (or higher) image.
//!
//! Volume 0 keeps the fast path of `source::load_volume0` (only what it needs
//! is read or inflated). The other volumes come from:
//!
//! * an uncompressed file: a positional read of the volume, or a slice of one
//!   read-only mapping of the file when the data are large;
//! * a gzipped file: a background thread that inflates the stream once, from
//!   the start, and hands over each volume as soon as its bytes are there.
//!   When all the data fit in the image limit they are all kept, so that
//!   scrubbing back and forth and the time course of a voxel are instant;
//!   otherwise the thread decodes up to the wanted volume, keeps the most
//!   recent ones within half the limit, and starts again from the beginning
//!   to go back.
//!
//! Nothing here blocks the protocol: a volume that is not decoded yet is
//! reported as pending with the progress made, and the client asks again.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufReader, Read};
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Condvar, Mutex};

use flate2::read::MultiGzDecoder;

use crate::error::{bail, PreviewError, Result};
use crate::image::{DataLayout, ImageHeader};
use crate::source::{self, read_full, zeroed, Storage, Stream, READ_BELOW};

/// What a request for a volume gives.
pub enum Fetch {
    /// The storage and the offset of the volume inside it.
    Ready(Storage, usize),
    /// Not decoded yet: fraction of the work done towards it (0 to 1).
    Pending(f64),
}

/// Where the data of a file are and how big its volumes are.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    /// Offset of the first voxel in the decoded stream.
    pub offset: u64,
    /// Bytes of one 3D volume.
    pub vol: u64,
    pub n_volumes: usize,
}

impl Geometry {
    fn total(&self) -> u64 {
        self.vol.saturating_mul(self.n_volumes as u64)
    }
}

pub struct VolumeStore {
    file: File,
    file_size: u64,
    compressed: bool,
    stream: Stream,
    geometry: Geometry,
    max_bytes: u64,
    map: Option<Arc<memmap2::Mmap>>,
    worker: Option<Worker>,
    /// Data that cannot be read one volume at a time where they are stored
    /// (volumes interleaved with voxels, bits, detached files): all of them,
    /// loaded once, each volume then copied out in canonical order.
    plan: Option<FullPlan>,
    full: Option<Full>,
}

/// What reading all the data needs.
struct FullPlan {
    layout: DataLayout,
    shape: Vec<usize>,
    itemsize: usize,
    elements: u64,
}

/// All the stored elements, and where logical index (0, 0, 0, ...) is.
struct Full {
    data: Storage,
    offset: usize,
    strides: Vec<i64>,
    base: i64,
}

impl VolumeStore {
    pub fn new(file: &File, file_size: u64, compressed: bool, header: &ImageHeader, stream: Stream, max_bytes: u64) -> Result<VolumeStore> {
        let file = file.try_clone().map_err(source::io_error)?;
        let geometry = Geometry {
            offset: stream.offset,
            vol: header.volume_bytes().unwrap_or(u64::MAX),
            n_volumes: header.n_volumes(),
        };
        let plan = (!header.layout.is_direct()).then(|| FullPlan {
            layout: header.layout.clone(),
            shape: header.shape.clone(),
            itemsize: header.dtype.itemsize(),
            elements: header.shape.iter().try_fold(1u64, |a, &d| a.checked_mul(d as u64)).unwrap_or(u64::MAX),
        });
        // Over the network, volumes are read in the background like gzip
        // ones (never mapped, never blocking the requests).
        let compressed = compressed || stream.gzip_at.is_some() || stream.network;
        Ok(VolumeStore { file, file_size, compressed, stream, geometry, max_bytes, map: None, worker: None, plan, full: None })
    }

    /// Volumes are copied out of all the data (see `plan`).
    pub fn reads_all(&self) -> bool {
        self.plan.is_some()
    }

    /// Volume `k` copied out of all the data, in canonical order.
    fn volume_from_full(&mut self, k: usize) -> Result<Fetch> {
        self.ensure_full()?;
        let (plan, full) = (self.plan.as_ref().unwrap(), self.full.as_ref().unwrap());
        // Canonical data held once: the volume is a view of them, no copy.
        if plan.layout.is_canonical() && !plan.layout.bits {
            let start = (k as u64).checked_mul(self.geometry.vol).and_then(|s| s.checked_add(full.offset as u64)).ok_or_else(|| {
                PreviewError::new("corrupt_data", "Image data could not be read; the file is truncated.")
            })?;
            match &full.data {
                Storage::Shared(v) => return Ok(Fetch::Ready(Storage::Shared(v.clone()), usize::try_from(start)?)),
                Storage::SharedMap(m) => return Ok(Fetch::Ready(Storage::SharedMap(m.clone()), usize::try_from(start)?)),
                _ => {}
            }
        }
        let shape3 = [0, 1, 2].map(|a| plan.shape.get(a).copied().unwrap_or(1));
        let stride = |a: usize| full.strides.get(a).copied().unwrap_or(0);
        let mut base = full.base;
        let mut rest = k;
        for a in 3..plan.shape.len() {
            base += (rest % plan.shape[a]) as i64 * stride(a);
            rest /= plan.shape[a];
        }
        let n = plan.itemsize;
        let bytes = &full.data.bytes()[full.offset..];
        let mut out = zeroed(shape3.iter().product::<usize>() * n)?;
        let mut o = 0;
        for z in 0..shape3[2] {
            for y in 0..shape3[1] {
                let row = base + y as i64 * stride(1) + z as i64 * stride(2);
                for x in 0..shape3[0] {
                    let e = (row + x as i64 * stride(0)) as usize * n;
                    out[o..o + n].copy_from_slice(&bytes[e..e + n]);
                    o += n;
                }
            }
        }
        Ok(Fetch::Ready(Storage::Owned(out), 0))
    }

    /// Read (or map) all the stored data once.
    fn ensure_full(&mut self) -> Result<()> {
        if self.full.is_some() {
            return Ok(());
        }
        // The plan is taken out while the data are read (mapping borrows self).
        let plan = self.plan.take().expect("full plan");
        let result = self.read_full_data(&plan);
        self.plan = Some(plan);
        self.full = Some(result?);
        Ok(())
    }

    fn read_full_data(&mut self, plan: &FullPlan) -> Result<Full> {
        let stored = if plan.layout.bits { plan.elements.div_ceil(8) } else { plan.elements.saturating_mul(plan.itemsize as u64) };
        let expanded = if plan.layout.bits { plan.elements } else { stored };
        if expanded > self.max_bytes {
            return Err(source::too_large("voxel data", expanded, self.max_bytes));
        }
        let stored_usize = usize::try_from(stored)?;
        if let Some(decoder) = &plan.layout.decoder {
            let bytes = decoder.0.decode(self.max_bytes)?;
            if bytes.len() < stored_usize {
                bail!("corrupt_data", "Image data could not be decoded ({} of {} bytes).", bytes.len(), stored_usize);
            }
            let (strides, base) = plan.layout.strides(&plan.shape);
            return Ok(Full { data: Storage::Shared(Arc::new(bytes)), offset: 0, strides, base });
        }
        let (data, offset) = if !plan.layout.files.is_empty() {
            (Storage::Shared(Arc::new(read_detached(&plan.layout, stored_usize)?)), 0)
        } else if self.stream.gzip_at.is_none() {
            let end = self.stream.offset.checked_add(stored).filter(|&e| e <= self.file_size).ok_or_else(|| {
                PreviewError::new("corrupt_data", "Image data could not be read; the file is truncated.")
            })?;
            if end <= READ_BELOW || self.stream.network {
                crate::progress::expect(stored, false);
                let mut out = zeroed(stored_usize)?;
                source::read_exact_in_blocks(&self.file, &mut out, self.stream.offset).map_err(source::corrupt)?;
                (Storage::Owned(out), 0)
            } else {
                (Storage::SharedMap(self.mapping()?), usize::try_from(self.stream.offset)?)
            }
        } else {
            crate::progress::expect(self.file_size.saturating_sub(self.stream.gzip_at.unwrap_or(0)), false);
            let mut dec = decoder(&self.file, self.stream)?;
            let mut out = zeroed(stored_usize)?;
            if read_full(&mut dec, &mut out).map_err(source::corrupt)? < stored_usize {
                bail!("corrupt_data", "Image data could not be read; the file is truncated or corrupted.");
            }
            (Storage::Owned(out), 0)
        };
        let (data, offset) = if plan.layout.bits {
            // One bit per voxel, most significant first: one byte each.
            let packed = &data.bytes()[offset..offset + stored_usize];
            let mut out = zeroed(usize::try_from(plan.elements)?)?;
            for (i, v) in out.iter_mut().enumerate() {
                *v = (packed[i / 8] >> (7 - i % 8)) & 1;
            }
            (Storage::Owned(out), 0)
        } else {
            (data, offset)
        };
        let (strides, base) = plan.layout.strides(&plan.shape);
        Ok(Full { data, offset, strides, base })
    }

    /// Whether every volume of a gzipped file can be kept in memory.
    pub fn keeps_all(&self) -> bool {
        self.geometry.total() <= self.max_bytes
    }

    /// Volume `k` (volume 0 of a direct layout goes through `source::load_volume0`).
    pub fn volume(&mut self, k: usize) -> Result<Fetch> {
        let g = self.geometry;
        if k >= g.n_volumes {
            bail!("bad_request", "Volume {k} outside 0..{}", g.n_volumes);
        }
        if g.vol > self.max_bytes {
            return Err(source::too_large("voxel data", g.vol, self.max_bytes));
        }
        if self.plan.is_some() {
            return self.volume_from_full(k);
        }
        if self.compressed {
            let worker = self.worker()?;
            if let Some(e) = worker.error() {
                return Err(e);
            }
            return Ok(worker.fetch(k));
        }
        // (Checked: a header may claim more volumes than any file holds.)
        let end = (k as u64).checked_mul(g.vol).and_then(|s| s.checked_add(g.offset)).and_then(|s| s.checked_add(g.vol));
        let Some(end) = end.filter(|&e| e <= self.file_size) else {
            bail!(
                "corrupt_data",
                "Volume {} of {} could not be read; the file is truncated ({} bytes, {} needed).",
                k + 1,
                g.n_volumes,
                self.file_size,
                end.map_or("more".to_string(), |e| e.to_string())
            );
        };
        let start = end - g.vol;
        if g.offset.saturating_add(g.total()) <= READ_BELOW {
            let mut out = zeroed(usize::try_from(g.vol)?)?;
            self.file.read_exact_at(&mut out, start).map_err(source::corrupt)?;
            return Ok(Fetch::Ready(Storage::Owned(out), 0));
        }
        let map = self.mapping()?;
        Ok(Fetch::Ready(Storage::SharedMap(map), usize::try_from(start)?))
    }

    /// One read-only mapping of the data of a large uncompressed file.
    fn mapping(&mut self) -> Result<Arc<memmap2::Mmap>> {
        if let Some(m) = &self.map {
            return Ok(m.clone());
        }
        let len = self.file_size.min(self.geometry.offset.saturating_add(self.geometry.total()));
        // SAFETY: read-only mapping within the file size checked at open; a
        // file truncated while mapped faults this helper process only.
        let map = unsafe { memmap2::MmapOptions::new().len(usize::try_from(len)?).map(&self.file) }.map_err(source::io_error)?;
        let map = Arc::new(map);
        self.map = Some(map.clone());
        Ok(map)
    }

    fn worker(&mut self) -> Result<&Worker> {
        if self.worker.is_none() {
            let file = self.file.try_clone().map_err(source::io_error)?;
            let budget = if self.keeps_all() { self.geometry.total() } else { self.max_bytes / 2 };
            self.worker = Some(Worker::start(file, self.stream, self.geometry, budget));
        }
        Ok(self.worker.as_ref().unwrap())
    }

    /// The bytes of voxel `index` (`itemsize` bytes at `index * itemsize` in
    /// each volume) across all volumes; None for a volume not available yet.
    /// Returns whether every volume was available.
    pub fn timecourse(&mut self, index: u64, itemsize: usize, out: &mut Vec<Option<[u8; 16]>>) -> Result<bool> {
        let g = self.geometry;
        out.clear();
        let within = index.checked_mul(itemsize as u64).filter(|o| o + itemsize as u64 <= g.vol);
        let Some(within) = within else { bail!("bad_request", "Voxel outside the volume") };
        if self.plan.is_some() {
            self.ensure_full()?;
            let (plan, full) = (self.plan.as_ref().unwrap(), self.full.as_ref().unwrap());
            let shape3 = [0, 1, 2].map(|a| plan.shape.get(a).copied().unwrap_or(1) as u64);
            let voxel = [index % shape3[0], index / shape3[0] % shape3[1], index / (shape3[0] * shape3[1])];
            let stride = |a: usize| full.strides.get(a).copied().unwrap_or(0);
            let at = full.base + (0..3).map(|a| voxel[a] as i64 * stride(a)).sum::<i64>();
            let bytes = &full.data.bytes()[full.offset..];
            for k in 0..g.n_volumes {
                let mut e = at;
                let mut rest = k;
                for a in 3..plan.shape.len() {
                    e += (rest % plan.shape[a]) as i64 * stride(a);
                    rest /= plan.shape[a];
                }
                let o = e as usize * itemsize;
                let mut item = [0u8; 16];
                item[..itemsize].copy_from_slice(&bytes[o..o + itemsize]);
                out.push(Some(item));
            }
            return Ok(true);
        }
        if self.compressed {
            // Starting the worker decodes every volume when they all fit.
            let worker = self.worker()?;
            if let Some(e) = worker.error() {
                return Err(e);
            }
            return Ok(worker.collect(within as usize, itemsize, out));
        }
        let mapped = g.offset.saturating_add(g.total()) > READ_BELOW;
        let map = if mapped { Some(self.mapping()?) } else { None };
        let mut complete = true;
        for k in 0..g.n_volumes {
            let pos = (k as u64).checked_mul(g.vol).and_then(|p| p.checked_add(g.offset + within)).unwrap_or(u64::MAX);
            let mut item = [0u8; 16];
            let ok = match &map {
                Some(m) => {
                    let p = pos as usize;
                    match m.get(p..p.saturating_add(itemsize)) {
                        Some(b) => {
                            item[..itemsize].copy_from_slice(b);
                            true
                        }
                        None => false,
                    }
                }
                None => pos.saturating_add(itemsize as u64) <= self.file_size && self.file.read_exact_at(&mut item[..itemsize], pos).is_ok(),
            };
            complete &= ok;
            out.push(ok.then_some(item));
        }
        Ok(complete)
    }

    /// Fraction of the volumes decoded by the background inflater (1 for an
    /// uncompressed file).
    pub fn progress(&self) -> f64 {
        match &self.worker {
            Some(w) => w.progress(),
            None if self.compressed => 0.0,
            None => 1.0,
        }
    }
}

// ------------------------------------------------------------------ worker

struct State {
    /// The volume the client wants now.
    target: usize,
    volumes: HashMap<usize, Arc<Vec<u8>>>,
    /// Least recently used first (only when not every volume fits).
    order: VecDeque<usize>,
    bytes: u64,
    /// The next volume the decoder will produce.
    position: usize,
    error: Option<PreviewError>,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

struct Worker {
    shared: Arc<Shared>,
    geometry: Geometry,
}


impl Worker {
    fn start(file: File, stream: Stream, geometry: Geometry, budget: u64) -> Worker {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                target: 0,
                volumes: HashMap::new(),
                order: VecDeque::new(),
                bytes: 0,
                position: 0,
                error: None,
                stop: false,
            }),
            wake: Condvar::new(),
        });
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new().name("inflate".into()).spawn(move || {
            let shared = thread_shared;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&shared, file, stream, geometry, budget)));
            let error = match outcome {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some(PreviewError::new("internal", "Internal error while decompressing the image.")),
            };
            if let Some(e) = error {
                let mut st = shared.state.lock().unwrap_or_else(|p| p.into_inner());
                st.error = Some(e);
                shared.wake.notify_all();
            }
        });
        if spawned.is_err() {
            let mut st = shared.state.lock().unwrap_or_else(|p| p.into_inner());
            st.error = Some(PreviewError::new("too_large", "Not enough resources to decompress the other volumes."));
        }
        Worker { shared, geometry }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn fetch(&self, k: usize) -> Fetch {
        let mut st = self.lock();
        if let Some(v) = st.volumes.get(&k).cloned() {
            touch(&mut st.order, k);
            return Fetch::Ready(Storage::Shared(v), 0);
        }
        if st.target != k {
            st.target = k;
            self.shared.wake.notify_all();
        }
        let done = if st.position <= k { st.position } else { 0 };
        Fetch::Pending(done as f64 / (k + 1) as f64)
    }

    fn collect(&self, within: usize, itemsize: usize, out: &mut Vec<Option<[u8; 16]>>) -> bool {
        let st = self.lock();
        let mut complete = true;
        for k in 0..self.geometry.n_volumes {
            let item = st.volumes.get(&k).and_then(|v| v.get(within..within + itemsize)).map(|b| {
                let mut a = [0u8; 16];
                a[..itemsize].copy_from_slice(b);
                a
            });
            complete &= item.is_some();
            out.push(item);
        }
        complete
    }

    fn progress(&self) -> f64 {
        let st = self.lock();
        st.volumes.len() as f64 / self.geometry.n_volumes.max(1) as f64
    }

    /// The error that stopped the decoder, if any.
    fn error(&self) -> Option<PreviewError> {
        self.lock().error.clone()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let mut st = self.lock();
        st.stop = true;
        self.shared.wake.notify_all();
    }
}

fn touch(order: &mut VecDeque<usize>, k: usize) {
    if let Some(p) = order.iter().position(|&x| x == k) {
        order.remove(p);
        order.push_back(k);
    }
}

/// The decoded data stream, positioned at the first voxel: inflated, or
/// the raw data themselves (files on a network file system).
fn decoder(file: &File, stream: Stream) -> Result<Box<dyn Read + Send>> {
    let file = file.try_clone().map_err(source::io_error)?;
    let capacity = if stream.network { source::READ_BLOCK } else { 1 << 20 };
    let Some(pos) = stream.gzip_at else {
        return Ok(Box::new(BufReader::with_capacity(capacity, source::At { file, pos: stream.offset })));
    };
    let offset = stream.offset;
    let mut dec = MultiGzDecoder::new(BufReader::with_capacity(capacity, source::At { file, pos }));
    let skipped = std::io::copy(&mut (&mut dec).take(offset), &mut std::io::sink()).map_err(source::corrupt)?;
    if skipped < offset {
        bail!("corrupt_data", "Image data could not be read; the file is truncated or corrupted.");
    }
    Ok(Box::new(dec))
}

/// The decoding loop of the background thread.
fn run(shared: &Shared, file: File, stream: Stream, g: Geometry, budget: u64) -> Result<()> {
    let keep_all = budget >= g.total();
    let vol = usize::try_from(g.vol)?;
    let mut dec = decoder(&file, stream)?;
    let mut position = 0usize;
    loop {
        // Decide what to do next, or sleep until there is something to do.
        {
            let mut st = shared.state.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if st.stop {
                    return Ok(());
                }
                let target = st.target;
                let have_target = st.volumes.contains_key(&target);
                let work = if keep_all {
                    // Everything is kept: decode to the end once started.
                    position < g.n_volumes
                } else if !have_target && target < position {
                    // Going back: start again from the beginning.
                    drop(st);
                    dec = decoder(&file, stream)?;
                    position = 0;
                    st = shared.state.lock().unwrap_or_else(|p| p.into_inner());
                    st.position = 0;
                    true
                } else {
                    // Up to the target, then a little ahead (cheap prefetch).
                    position < g.n_volumes && position <= target + 2
                };
                if work {
                    break;
                }
                st = shared.wake.wait(st).unwrap_or_else(|p| p.into_inner());
            }
        }
        // Decode one volume outside the lock.
        let mut buf = zeroed(vol)?;
        let got = read_full(&mut dec, &mut buf).map_err(source::corrupt)?;
        if got < vol {
            bail!(
                "corrupt_data",
                "Volume {} of {} could not be read; the file is truncated or corrupted.",
                position + 1,
                g.n_volumes
            );
        }
        let mut st = shared.state.lock().unwrap_or_else(|p| p.into_inner());
        // Decoded again after a restart from volume 0: still counted once.
        if st.volumes.insert(position, Arc::new(buf)).is_none() {
            st.order.push_back(position);
            st.bytes += g.vol;
        }
        let target = st.target;
        while !keep_all && st.bytes > budget {
            let Some(old) = st.order.iter().position(|&k| k != target).and_then(|p| st.order.remove(p)) else { break };
            if st.volumes.remove(&old).is_some() {
                st.bytes -= g.vol;
            }
        }
        position += 1;
        st.position = position;
        shared.wake.notify_all();
    }
}


/// The data of a detached header, file after file, until `stored` bytes.
fn read_detached(layout: &DataLayout, stored: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(stored).map_err(|_| PreviewError::new("too_large", "Not enough memory to load this image."))?;
    let single = layout.files.len() == 1;
    for f in &layout.files {
        if out.len() >= stored {
            break;
        }
        let file = source::open_data_file_in(&f.path, f.root.as_deref())?;
        let size = file.metadata().map_err(source::io_error)?.len();
        // Lines to skip (before any decompression), at most 1 MiB of them.
        let mut pos = 0u64;
        if f.line_skip > 0 {
            let mut head = vec![0u8; (size.min(1 << 20)) as usize];
            let got = read_full(&mut source::At { file: &file, pos: 0 }, &mut head).map_err(source::io_error)?;
            let mut lines = 0;
            for (i, &c) in head[..got].iter().enumerate() {
                if c == b'\n' {
                    lines += 1;
                    if lines == f.line_skip {
                        pos = i as u64 + 1;
                        break;
                    }
                }
            }
            if lines < f.line_skip {
                bail!("corrupt_data", "Data file {}: fewer lines than the header skips", f.path.display());
            }
        }
        let want = stored - out.len();
        let start = out.len();
        if layout.files_gzip && f.byte_skip == -1 {
            // The data end the decompressed stream: keep the last bytes read.
            if !single {
                bail!("unsupported_format", "byte skip -1 with several data files is not supported");
            }
            let mut dec = MultiGzDecoder::new(BufReader::with_capacity(1 << 20, source::At { file: &file, pos }));
            let mut ring = Vec::new();
            let mut chunk = vec![0u8; 1 << 20];
            loop {
                let got = dec.read(&mut chunk).map_err(source::corrupt)?;
                if got == 0 {
                    break;
                }
                ring.extend_from_slice(&chunk[..got]);
                if ring.len() > 2 * want + (1 << 20) {
                    ring.drain(..ring.len() - want);
                }
            }
            if ring.len() < want {
                bail!("corrupt_data", "Data file {} is truncated", f.path.display());
            }
            out.extend_from_slice(&ring[ring.len() - want..]);
        } else if layout.files_gzip {
            let mut dec = MultiGzDecoder::new(BufReader::with_capacity(1 << 20, source::At { file: &file, pos }));
            let skip = f.byte_skip.max(0) as u64;
            let skipped = std::io::copy(&mut (&mut dec).take(skip), &mut std::io::sink()).map_err(source::corrupt)?;
            if skipped < skip {
                bail!("corrupt_data", "Data file {} is truncated", f.path.display());
            }
            out.resize(stored, 0);
            let got = read_full(&mut dec, &mut out[start..]).map_err(source::corrupt)?;
            out.truncate(start + got);
        } else {
            let from = if f.byte_skip == -1 {
                if !single {
                    bail!("unsupported_format", "byte skip -1 with several data files is not supported");
                }
                size.checked_sub(want as u64).filter(|&p| p >= pos).ok_or_else(|| {
                    PreviewError::new("corrupt_data", format!("Data file {} is truncated", f.path.display()))
                })?
            } else {
                pos + f.byte_skip as u64
            };
            let n = (size.saturating_sub(from) as usize).min(want);
            out.resize(start + n, 0);
            file.read_exact_at(&mut out[start..], from).map_err(source::corrupt)?;
        }
    }
    if out.len() < stored {
        bail!("corrupt_data", "Image data could not be read; the data files hold {} of {} bytes.", out.len(), stored);
    }
    Ok(out)
}
