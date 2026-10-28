//! What a viewer needs besides the slices themselves, shared by the Sushi
//! protocol (`server`) and the terminal viewer: where the cursor starts, what
//! lies under it, the labels of a segmentation, the DICOM series next to an
//! image, the time of a volume, a cache of drawn slices and reading an image
//! on a helper thread.

use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

use crate::error::{bail, PreviewError, Result};
use crate::formats::dicom::catalog::{Catalog, SeriesEntry};
use crate::image::ImageHeader;
use crate::orientation::{self, Grid, ViewPlan};
use crate::progress::Progress;
use crate::volume::{self, Volume};
use crate::voxel::VoxelValue;

/// Bookkeeping counted per cached slice on top of its pixels.
const CACHE_ENTRY_OVERHEAD: usize = 256;

/// Mode, colormap, view, slice index and volume.
pub type SliceKey = (&'static str, &'static str, &'static str, usize, usize);

/// RGB8 slices already drawn (pixels, width, height), least recently used
/// dropped first, within a byte budget.
pub struct SliceCache {
    max_bytes: usize,
    size: usize,
    items: HashMap<SliceKey, (Arc<Vec<u8>>, usize, usize)>,
    order: VecDeque<SliceKey>,
}

impl SliceCache {
    pub fn new(max_bytes: usize) -> Self {
        SliceCache { max_bytes, size: 0, items: HashMap::new(), order: VecDeque::new() }
    }

    fn touch(&mut self, key: &SliceKey) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(pos).unwrap();
            self.order.push_back(k);
        }
    }

    pub fn get(&mut self, key: &SliceKey) -> Option<(Arc<Vec<u8>>, usize, usize)> {
        let hit = self.items.get(key).cloned();
        if hit.is_some() {
            self.touch(key);
        }
        hit
    }

    pub fn put(&mut self, key: SliceKey, value: (Arc<Vec<u8>>, usize, usize)) {
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

/// Where the cursor starts on `grid`: the image a DICOM series was opened
/// on, else the centre.
pub fn start_cursor(vol: &Volume, grid: &Grid) -> [usize; 3] {
    match vol.header.dicom.as_ref().and_then(|d| d.start) {
        Some((voxel, _)) => {
            let w = orientation::voxel_to_world(&vol.affine, voxel.map(|x| x as f64));
            if grid.resampled || vol.orientation_known {
                grid.from_world(w)
            } else {
                voxel
            }
        }
        None => grid.center(),
    }
}

/// A sparse segmentation (a hippocampus, lesions): when a plane through the
/// centre of the grid holds no label, the grid point at the centre of the
/// labels, to start there. None otherwise, and for an image that is not a
/// label map.
pub fn label_cursor(vol: &mut Volume, grid: &Grid) -> Result<Option<[usize; 3]>> {
    let Some((centre, planes)) = vol.label_extent()? else { return Ok(None) };
    let at = grid.to_voxel(grid.center());
    let empty = (0..3).any(|a| {
        let i = (at[a].round().max(0.0) as usize).min(planes[a].len() - 1);
        planes[a][i] == 0
    });
    if !empty {
        return Ok(None);
    }
    let world = orientation::voxel_to_world(&vol.affine, centre);
    Ok(Some(grid.from_world(world)))
}

/// A cursor of grid `before` on grid `after` (after a change of display
/// mode), clamped to `before` first: the same point when both are the same
/// kind of grid (one image), else the point nearest to the same position in
/// the world.
pub fn carry_cursor(before: &Grid, after: &Grid, cursor: [i64; 3]) -> [usize; 3] {
    let c = cursor.map(|x| x.max(0) as usize);
    let c = [0, 1, 2].map(|a| c[a].min(before.shape[a] - 1));
    if before.resampled == after.resampled {
        c
    } else {
        after.from_world(orientation::voxel_to_world(&before.affine, c.map(|x| x as f64)))
    }
}

/// Grid point `ijk` checked against `grid`.
fn grid_point(grid: &Grid, ijk: [i64; 3]) -> Result<[usize; 3]> {
    if (0..3).any(|a| ijk[a] < 0 || ijk[a] as usize >= grid.shape[a]) {
        bail!("bad_request", "Voxel {:?} outside image {:?}", ijk, grid.shape);
    }
    Ok(ijk.map(|x| x as usize))
}

/// The voxel shown at grid point `ijk` (None outside the image on a resampled
/// grid); an error when `ijk` is outside the grid.
pub fn voxel_under(grid: &Grid, ijk: [i64; 3]) -> Result<Option<[usize; 3]>> {
    Ok(grid.voxel_at(grid_point(grid, ijk)?))
}

/// The structure under the cursor in a label map.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelHere {
    /// From the colour table (FreeSurfer's or the user's).
    pub name: Option<String>,
    pub voxels: u64,
    /// None when the voxel size makes it meaningless (not finite).
    pub volume_mm3: Option<f64>,
}

/// What lies under grid point `ijk`.
#[derive(Debug, Clone, PartialEq)]
pub struct Readout {
    pub ijk: [i64; 3],
    /// World coordinates of the grid point, in mm whatever the unit of the header.
    pub world_mm: [f64; 3],
    /// The voxel shown there, None outside the image (resampled grid).
    pub voxel: Option<[usize; 3]>,
    /// Its exact value; None for NaN/Inf and outside the image.
    pub value: Option<VoxelValue>,
    /// The value is a label of a segmentation.
    pub categorical: bool,
    /// The colour of an RGB image.
    pub rgb: Option<[u8; 3]>,
    /// The structure of a label map (not for the background).
    pub label: Option<LabelHere>,
}

/// The value under grid point `ijk` of `grid`: that of the nearest voxel.
pub fn readout(vol: &mut Volume, grid: &Grid, ijk: [i64; 3]) -> Result<Readout> {
    let g = grid_point(grid, ijk)?;
    let voxel = grid.voxel_at(g);
    let value = match voxel {
        Some(p) => vol.exact_value_at(p.map(|x| x as i64))?,
        None => None,
    };
    let mm = vol.mm_per_unit();
    let world_mm = orientation::voxel_to_world(&grid.affine, g.map(|x| x as f64)).map(|x| x * mm);
    let categorical = value.is_some() && vol.stats()?.labels.is_labels;
    let rgb = match voxel {
        Some(p) => vol.rgb_at(p)?,
        None => None,
    };
    let mut label = None;
    if let Some(x) = value.filter(|x| !x.is_zero()) {
        if categorical {
            let name = vol.label_table().and_then(|t| t.get(&x.label_id())).map(|e| e.name.clone());
            let voxels = vol.count_equal(x)?;
            let mm3 = voxels as f64 * vol.voxel_volume_mm3();
            label = Some(LabelHere { name, voxels, volume_mm3: mm3.is_finite().then_some(mm3) });
        }
    }
    Ok(Readout { ijk, world_mm, voxel, value, categorical, rgb, label })
}

/// One label of a segmentation.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelRow {
    pub value: VoxelValue,
    pub name: Option<String>,
    pub rgb: [u8; 3],
    pub voxels: u64,
    pub volume_mm3: Option<f64>,
}

/// The labels of the volume shown.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelList {
    pub rows: Vec<LabelRow>,
    /// Every label was counted (at most `volume::MAX_LISTED_LABELS` are).
    pub complete: bool,
    /// How they are coloured (`labels::LabelInfo::scheme`), empty when the
    /// image is not a label map.
    pub scheme: &'static str,
}

/// The labels of the volume shown: value, name (from the colour table),
/// colour, number of voxels and volume in mm³, in increasing order; none
/// for an image that is not a label map.
pub fn label_rows(vol: &mut Volume) -> Result<LabelList> {
    let s = vol.stats()?;
    if !s.labels.is_labels {
        return Ok(LabelList { rows: Vec::new(), complete: true, scheme: "" });
    }
    let (binary, scheme) = (s.labels.binary, s.labels.scheme);
    let (counts, complete) = vol.label_counts()?;
    let mm3 = vol.voxel_volume_mm3();
    let table = vol.label_table();
    let rows = counts
        .iter()
        .map(|&(value, voxels)| {
            let id = value.label_id();
            let entry = table.and_then(|t| t.get(&id));
            let rgb = if binary { [255, 255, 255] } else { entry.map_or_else(|| crate::labels::label_color(id), |e| e.rgb) };
            let volume = voxels as f64 * mm3;
            LabelRow { value, name: entry.map(|e| e.name.clone()), rgb, voxels, volume_mm3: volume.is_finite().then_some(volume) }
        })
        .collect();
    Ok(LabelList { rows, complete, scheme })
}

/// Time of volume `k` (k × TR) for a 4D series with a repetition time.
pub fn volume_time(h: &ImageHeader, k: usize) -> Option<f64> {
    match h.repetition_time {
        Some(tr) if h.ndim() == 4 && tr.is_finite() && tr > 0.0 => Some(k as f64 * tr),
        _ => None,
    }
}

/// The view to show alone, for an image of a single slice (a radiograph, an
/// ultrasound clip, one DICOM image): the one that holds the slice whole.
/// None for an image with depth, and for spectroscopy.
pub fn single_view(vol: &Volume, plans: &[ViewPlan; 3]) -> Option<&'static str> {
    if vol.mrs.is_some() || vol.header.shape.get(2).copied().unwrap_or(1) != 1 {
        return None;
    }
    ["axial", "coronal", "sagittal"].into_iter().find(|view| {
        plans.iter().any(|p| p.view == *view && p.n_slices == 1 && p.width > 1 && p.height > 1)
    })
}

/// The DICOM series next to the image (its folder, or its DICOMDIR), read
/// once and kept with the image, and the series shown.
pub fn series_catalog(vol: &mut Volume) -> Result<(Arc<Catalog>, Option<String>)> {
    let Some(info) = vol.header.dicom.as_mut() else { bail!("bad_request", "Not a DICOM image") };
    if info.catalog.is_none() {
        let Some(folder) = info.folder.clone() else { bail!("not_found", "No folder to look for series in") };
        let (catalog, _) = crate::formats::dicom::catalog::scan_folder(&folder, None, None)?;
        info.catalog = Some(Arc::new(catalog));
    }
    Ok((info.catalog.clone().unwrap(), info.series_uid.clone()))
}

/// Another series of the catalog `series_catalog` read.
pub fn open_series(vol: &Volume, uid: &str) -> Result<Volume> {
    open_series_with(vol, uid, volume::max_bytes_from_env())
}

/// `open_series` with the image limit given.
pub fn open_series_with(vol: &Volume, uid: &str, max_bytes: u64) -> Result<Volume> {
    let Some(info) = vol.header.dicom.as_ref() else { bail!("bad_request", "Not a DICOM image") };
    let Some(catalog) = info.catalog.clone() else { bail!("bad_request", "List the series first (series_list)") };
    let Some(entry) = catalog.find(uid) else { bail!("not_found", "No series {uid:?} here") };
    open_entry(&catalog, entry, info.folder.clone(), max_bytes)
}

/// Series `entry` of `catalog`, opened on its first file.
fn open_entry(catalog: &Arc<Catalog>, entry: &SeriesEntry, folder: Option<PathBuf>, max_bytes: u64) -> Result<Volume> {
    let header = crate::formats::dicom::open_entry(entry, catalog.clone(), folder)?;
    let first = entry.files.first().cloned().unwrap_or_default();
    let file = crate::source::open_data_file(&first)?;
    let file_size = file.metadata().map_err(crate::source::io_error)?.len();
    let storage = crate::storage::of(&file, &first);
    let opened = crate::source::Opened { header, extensions: Vec::new(), compressed: false, file_size, file, storage };
    Volume::from_opened(Path::new(&first), opened, max_bytes)
}

/// The image of a folder: its DICOMDIR when it has one, else the image
/// series of the DICOM files it holds with the most images (the others are
/// then listed by `series_catalog`).
pub fn open_folder(dir: &Path, max_bytes: u64) -> Result<Volume> {
    if let Some(d) = ["DICOMDIR", "dicomdir"].into_iter().map(|n| dir.join(n)).find(|p| p.is_file()) {
        return Volume::open_with(&d, max_bytes, crate::source::ReadOptions::default());
    }
    let (catalog, _) = crate::formats::dicom::catalog::scan_folder(dir, None, None)?;
    let catalog = Arc::new(catalog);
    let Some(entry) = catalog.series.iter().max_by_key(|s| s.images) else {
        bail!("not_found", "No DICOM image in the folder {}", dir.display());
    };
    open_entry(&catalog, entry, Some(dir.to_path_buf()), max_bytes)
}

/// A volume being read on a helper thread (its data and statistics), and how
/// far it got. Dropping it stops the reads.
pub struct Loading {
    done: Receiver<(Box<Volume>, Result<()>)>,
    progress: Arc<Progress>,
    started: std::time::Instant,
}

/// Where a `Loading` is.
pub enum LoadState {
    Running,
    /// The volume, and whether its data could be read.
    Done(Box<Volume>, Result<()>),
    /// The helper thread died without an answer.
    Lost,
}

impl Drop for Loading {
    /// A load given up (another file, close): its reads stop.
    fn drop(&mut self) {
        self.progress.cancel();
    }
}

impl Loading {
    /// Read the data of `volume` (and its statistics) on a helper thread;
    /// the volume back when no thread can be started.
    pub fn start(volume: Box<Volume>) -> std::result::Result<Loading, Box<Volume>> {
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
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
            Ok(_) => Ok(Loading { done: rx, progress, started: std::time::Instant::now() }),
            Err(_) => Err(slot.lock().unwrap_or_else(|p| p.into_inner()).take().expect("the thread never started")),
        }
    }

    /// Fraction of the data read (0 while unknown).
    pub fn fraction(&self) -> f64 {
        self.progress.fraction()
    }

    /// The reads were told to stop (the load was dropped).
    pub fn cancelled(&self) -> bool {
        self.progress.is_cancelled()
    }

    pub fn poll(&self) -> LoadState {
        match self.done.try_recv() {
            Ok((volume, result)) => LoadState::Done(volume, result),
            Err(TryRecvError::Empty) => LoadState::Running,
            Err(TryRecvError::Disconnected) => LoadState::Lost,
        }
    }

    /// Bytes read per second, once the load has run long enough to tell.
    pub fn rate(&self) -> Option<f64> {
        let seconds = self.started.elapsed().as_secs_f64();
        self.progress.bytes().filter(|_| seconds > 0.2).map(|b| b as f64 / seconds)
    }
}
