//! Geometry of DICOM images: where each frame is (classic files, enhanced
//! multi-frame functional groups, Siemens mosaics, RT dose grids), how the
//! frames stack into slices and volumes, and the voxel-to-world affine.

use super::parse::DataSet;
use super::pixels::PixelDesc;
use super::tags;
use crate::orientation::Mat4;

/// Positions closer than this along the slice normal are the same slice (mm).
const SAME_POSITION: f64 = 0.01;

/// One frame (or one mosaic tile) and what places it.
#[derive(Debug, Clone)]
pub struct FrameGeo {
    pub source: usize,
    pub frame: usize,
    /// Tile of a Siemens mosaic.
    pub tile: Option<usize>,
    pub position: Option<[f64; 3]>,
    pub orientation: Option<[f64; 6]>,
    /// Row spacing, column spacing (DICOM order).
    pub spacing: Option<[f64; 2]>,
    /// Slice thickness, or spacing between slices.
    pub thickness: Option<f64>,
    /// The volume the frame belongs to, when the file says it (dimension
    /// indices of enhanced images, segment numbers).
    pub key: Option<Vec<i64>>,
    /// What each value of `key` is (TemporalPositionIndex for a time point,
    /// an echo time pointer for an echo...).
    pub key_dims: Option<std::sync::Arc<[tags::Tag]>>,
    /// Echo number and echo time of a classic image (0 when absent).
    pub echo: [f64; 2],
    /// Order among the frames at the same position, when no key is given.
    pub order: [f64; 6],
    pub rescale: (f64, f64),
    pub window: Option<(f64, f64)>,
}

/// The rescale of a data set (slope, intercept), (1, 0) when absent.
pub fn rescale_of(set: &DataSet) -> Option<(f64, f64)> {
    let slope = set.number(tags::RESCALE_SLOPE);
    let inter = set.number(tags::RESCALE_INTERCEPT);
    if slope.is_none() && inter.is_none() {
        return None;
    }
    let slope = slope.filter(|s| *s != 0.0).unwrap_or(1.0);
    Some((slope, inter.unwrap_or(0.0)))
}

/// The DICOM window (centre, width) as (low, high), PS3.3 C.11.2.1.2.
/// LINEAR shifts by half a step for integer values; LINEAR_EXACT, and the
/// continuous values of float pixel data, do not (a parametric map's
/// centre 0.5 and width 1 is the range 0 to 1, not a threshold at 0).
pub fn window_of(set: &DataSet, float: bool) -> Option<(f64, f64)> {
    let c = set.number(tags::WINDOW_CENTER)?;
    let w = set.number(tags::WINDOW_WIDTH)?;
    let exact = float || set.text(tags::VOI_LUT_FUNCTION).as_deref() == Some("LINEAR_EXACT");
    if exact {
        return (w > 0.0).then(|| (c - w / 2.0, c + w / 2.0));
    }
    (w >= 1.0).then(|| (c - 0.5 - (w - 1.0) / 2.0, c - 0.5 + (w - 1.0) / 2.0))
}

/// A VOI LUT given in place of a window (PS3.3 C.11.2.1.1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VoiLut {
    /// A straight line from the lowest output to the highest: this window,
    /// exactly (the first value mapped to black, the last to white).
    Window(f64, f64),
    /// Another curve, which is not applied.
    Curve,
}

/// The first VOI LUT of a data set, if it has one.
pub fn voi_lut_of(set: &DataSet) -> Option<VoiLut> {
    let item = set.item(tags::VOI_LUT_SEQUENCE)?;
    let Some(lut) = super::pixels::Lut::read(item, tags::LUT_DESCRIPTOR, tags::LUT_DATA) else { return Some(VoiLut::Curve) };
    let n = lut.values.len();
    let top = ((1u32 << lut.bits.min(16)) - 1) as f64;
    // Within one output step of the line from 0 to the top (the entries are
    // rounded to integers).
    let straight = n > 1
        && lut.values.iter().enumerate().all(|(i, &v)| (v as f64 - top * i as f64 / (n - 1) as f64).abs() <= 1.0)
        && lut.values[0] == 0
        && lut.values[n - 1] as f64 == top;
    Some(if straight { VoiLut::Window(lut.first as f64, (lut.first + n as i64 - 1) as f64) } else { VoiLut::Curve })
}

/// Whether the window of `frame` has the SIGMOID function (the window shown
/// is linear).
pub fn sigmoid(set: &DataSet, frame: usize) -> bool {
    let is_sigmoid = |s: &DataSet| s.text(tags::VOI_LUT_FUNCTION).as_deref() == Some("SIGMOID");
    let group = |s: &DataSet| s.item(tags::FRAME_VOI_LUT_SEQUENCE).is_some_and(is_sigmoid);
    is_sigmoid(set)
        || set.items(tags::PER_FRAME_FUNCTIONAL_GROUPS).get(frame).is_some_and(|i| group(&i.set))
        || set.item(tags::SHARED_FUNCTIONAL_GROUPS).is_some_and(group)
}

fn vec3(v: &[f64]) -> Option<[f64; 3]> {
    (v.len() >= 3 && v[..3].iter().all(|x| x.is_finite())).then(|| [v[0], v[1], v[2]])
}

fn orientation(v: &[f64]) -> Option<[f64; 6]> {
    if v.len() < 6 || !v[..6].iter().all(|x| x.is_finite()) {
        return None;
    }
    let (r, c) = ([v[0], v[1], v[2]], [v[3], v[4], v[5]]);
    let n = cross(r, c);
    (norm(r) > 0.5 && norm(c) > 0.5 && norm(n) > 0.5).then(|| {
        let (r, c) = (unit(r), unit(c));
        [r[0], r[1], r[2], c[0], c[1], c[2]]
    })
}

pub fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn unit(a: [f64; 3]) -> [f64; 3] {
    let n = norm(a);
    [a[0] / n, a[1] / n, a[2] / n]
}

fn spacing(set: &DataSet) -> Option<[f64; 2]> {
    let v = set.numbers(tags::PIXEL_SPACING);
    let v = if v.len() >= 2 { v } else { set.numbers(tags::IMAGER_PIXEL_SPACING) };
    (v.len() >= 2 && v[0] > 0.0 && v[1] > 0.0 && v[0].is_finite() && v[1].is_finite()).then(|| [v[0], v[1]])
}

fn thickness(set: &DataSet) -> Option<f64> {
    set.number(tags::SPACING_BETWEEN_SLICES)
        .filter(|v| *v > 0.0)
        .or_else(|| set.number(tags::SLICE_THICKNESS).filter(|v| *v > 0.0))
}

/// HHMMSS.FFFFFF as seconds.
fn seconds(t: &str) -> Option<f64> {
    let t = t.trim().replace(':', "");
    let (main, frac) = t.split_once('.').unwrap_or((&t, ""));
    if main.len() < 2 || !main.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let num = |s: &str| s.parse::<f64>().ok();
    let h = num(&main[..2])?;
    let m = if main.len() >= 4 { num(&main[2..4])? } else { 0.0 };
    let s = if main.len() >= 6 { num(&main[4..6])? } else { 0.0 };
    let f = if frac.is_empty() { 0.0 } else { format!("0.{frac}").parse::<f64>().unwrap_or(0.0) };
    Some(h * 3600.0 + m * 60.0 + s + f)
}

/// Sort keys of a classic file among others at the same position.
fn order_of(set: &DataSet) -> [f64; 6] {
    let n = |t| set.number(t).unwrap_or(0.0);
    [
        n(tags::ACQUISITION_NUMBER),
        n(tags::TEMPORAL_POSITION_IDENTIFIER),
        n(tags::TRIGGER_TIME),
        n(tags::ECHO_NUMBERS),
        set.string(tags::ACQUISITION_TIME).and_then(|t| seconds(&t)).unwrap_or(0.0),
        n(tags::INSTANCE_NUMBER),
    ]
}

/// A Siemens mosaic: `images` slices of `rows` × `cols` tiled in a square
/// of `per_side` × `per_side`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mosaic {
    pub images: usize,
    pub per_side: usize,
    pub rows: usize,
    pub cols: usize,
    /// Size of the whole mosaic.
    pub full_rows: usize,
    pub full_cols: usize,
}

pub fn mosaic_of(set: &DataSet, desc: &PixelDesc) -> Option<Mosaic> {
    if desc.frames != 1 || !set.strings(tags::IMAGE_TYPE).iter().any(|s| s == "MOSAIC") {
        return None;
    }
    let images = set
        .private(0x0019, "SIEMENS MR HEADER", 0x0A)
        .and_then(|t| set.numbers(t).first().copied())
        .or_else(|| csa_value(set, "NumberOfImagesInMosaic").and_then(|v| v.first().copied()))?;
    let images = images as usize;
    if images < 1 {
        return None;
    }
    let per_side = (images as f64).sqrt().ceil() as usize;
    if desc.rows < per_side || desc.cols < per_side {
        return None;
    }
    Some(Mosaic { images, per_side, rows: desc.rows / per_side, cols: desc.cols / per_side, full_rows: desc.rows, full_cols: desc.cols })
}

/// A numeric value of the Siemens CSA image header (SV10 format).
pub fn csa_value(set: &DataSet, name: &str) -> Option<Vec<f64>> {
    let t = set.private(0x0029, "SIEMENS CSA HEADER", 0x10)?;
    let b = set.bytes(t)?;
    if b.len() < 16 || &b[..4] != b"SV10" {
        return None;
    }
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    let n_tags = u32_at(8)? as usize;
    if n_tags > 1024 {
        return None;
    }
    let mut p = 16usize;
    for _ in 0..n_tags {
        let tag_name = b.get(p..p + 64)?;
        let end = tag_name.iter().position(|&c| c == 0).unwrap_or(64);
        let tag_name = std::str::from_utf8(&tag_name[..end]).ok()?;
        let n_items = u32_at(p + 76)? as usize;
        if n_items > 1024 {
            return None;
        }
        p += 84;
        let mut values = Vec::new();
        for _ in 0..n_items {
            let len = u32_at(p + 4)? as usize;
            p += 16;
            let v = b.get(p..p + len)?;
            if tag_name == name {
                let s = String::from_utf8_lossy(v);
                if let Ok(x) = s.trim_matches(|c: char| c == '\0' || c.is_whitespace()).parse::<f64>() {
                    values.push(x);
                }
            }
            p += len.div_ceil(4) * 4;
        }
        if tag_name == name {
            return Some(values);
        }
    }
    None
}

/// Every frame of a file, placed.
pub fn frames_of(set: &DataSet, desc: &PixelDesc, source: usize) -> Vec<FrameGeo> {
    let file_rescale = rescale_of(set)
        .or_else(|| set.number(tags::DOSE_GRID_SCALING).filter(|s| *s > 0.0).map(|s| (s, 0.0)))
        .unwrap_or((1.0, 0.0));
    let base = FrameGeo {
        source,
        frame: 0,
        tile: None,
        position: vec3(&set.numbers(tags::IMAGE_POSITION)),
        orientation: orientation(&set.numbers(tags::IMAGE_ORIENTATION)),
        spacing: spacing(set),
        thickness: thickness(set),
        key: None,
        key_dims: None,
        echo: [set.number(tags::ECHO_NUMBERS).unwrap_or(0.0), set.number(tags::ECHO_TIME).unwrap_or(0.0)],
        order: order_of(set),
        rescale: file_rescale,
        window: window_of(set, desc.float),
    };
    let per_frame = set.items(tags::PER_FRAME_FUNCTIONAL_GROUPS);
    if !per_frame.is_empty() {
        return enhanced(set, desc, source, base);
    }
    if let Some(m) = mosaic_of(set, desc) {
        return mosaic(set, m, base);
    }
    if desc.frames == 1 {
        return vec![base];
    }
    // RT dose: frames along the normal, at the grid offsets.
    let offsets = set.numbers(tags::GRID_FRAME_OFFSETS);
    if let (Some(p), Some(o)) = (base.position, base.orientation) {
        if offsets.len() >= desc.frames {
            let n = cross([o[0], o[1], o[2]], [o[3], o[4], o[5]]);
            return (0..desc.frames)
                .map(|k| {
                    let d = offsets[k] - offsets[0];
                    FrameGeo { frame: k, position: Some([p[0] + n[0] * d, p[1] + n[1] * d, p[2] + n[2] * d]), ..base.clone() }
                })
                .collect();
        }
    }
    // Other multi-frame images (ultrasound or angiography cines, nuclear
    // medicine): frames in time, without a position each.
    (0..desc.frames)
        .map(|k| FrameGeo { frame: k, position: None, order: [k as f64, 0.0, 0.0, 0.0, 0.0, 0.0], ..base.clone() })
        .collect()
}

/// Frames of an enhanced multi-frame image (shared and per-frame groups).
fn enhanced(set: &DataSet, desc: &PixelDesc, source: usize, base: FrameGeo) -> Vec<FrameGeo> {
    let shared = set.item(tags::SHARED_FUNCTIONAL_GROUPS);
    let per_frame = set.items(tags::PER_FRAME_FUNCTIONAL_GROUPS);
    // Dimensions that place a frame in space are not part of its volume key.
    let pointers: Vec<tags::Tag> = set
        .items(tags::DIMENSION_INDEX_SEQUENCE)
        .iter()
        .map(|i| i.set.attribute_tags(tags::DIMENSION_INDEX_POINTER).first().copied().unwrap_or(0))
        .collect();
    let spatial = |t: tags::Tag| matches!(t, tags::IN_STACK_POSITION | tags::IMAGE_POSITION | tags::SLICE_LOCATION);
    let volume_dims: std::sync::Arc<[tags::Tag]> = pointers.iter().copied().filter(|&p| !spatial(p)).collect();
    let temporal: std::sync::Arc<[tags::Tag]> = std::sync::Arc::new([tags::TEMPORAL_POSITION_INDEX]);
    let segment: std::sync::Arc<[tags::Tag]> = std::sync::Arc::new([tags::REFERENCED_SEGMENT_NUMBER]);
    (0..desc.frames)
        .map(|k| {
            let own = per_frame.get(k).map(|i| &i.set);
            // A functional group: this frame's, else the shared one.
            let group = |seq: tags::Tag| own.and_then(|s| s.item(seq)).or_else(|| shared.and_then(|s| s.item(seq)));
            let measures = group(tags::PIXEL_MEASURES_SEQUENCE);
            let content = own.and_then(|s| s.item(tags::FRAME_CONTENT_SEQUENCE));
            let key = content.and_then(|c| {
                let values = c.numbers(tags::DIMENSION_INDEX_VALUES);
                if !pointers.is_empty() && values.len() == pointers.len() {
                    let key = values.iter().zip(&pointers).filter(|(_, &p)| !spatial(p)).map(|(v, _)| *v as i64).collect();
                    Some((key, volume_dims.clone()))
                } else {
                    c.int(tags::TEMPORAL_POSITION_INDEX).map(|t| (vec![t], temporal.clone()))
                }
            });
            let key = key.or_else(|| {
                group(tags::SEGMENT_IDENTIFICATION_SEQUENCE)
                    .and_then(|s| s.int(tags::REFERENCED_SEGMENT_NUMBER))
                    .map(|s| (vec![s], segment.clone()))
            });
            let (key, key_dims) = key.map_or((None, None), |(k, d)| (Some(k), Some(d)));
            FrameGeo {
                source,
                frame: k,
                tile: None,
                position: group(tags::PLANE_POSITION_SEQUENCE).and_then(|s| vec3(&s.numbers(tags::IMAGE_POSITION))).or(base.position),
                orientation: group(tags::PLANE_ORIENTATION_SEQUENCE)
                    .and_then(|s| orientation(&s.numbers(tags::IMAGE_ORIENTATION)))
                    .or(base.orientation),
                spacing: measures.and_then(spacing).or(base.spacing),
                thickness: measures.and_then(thickness).or(base.thickness),
                key,
                key_dims,
                echo: base.echo,
                order: [k as f64, 0.0, 0.0, 0.0, 0.0, 0.0],
                rescale: group(tags::PIXEL_VALUE_TRANSFORMATION_SEQUENCE).and_then(rescale_of).unwrap_or(base.rescale),
                window: group(tags::FRAME_VOI_LUT_SEQUENCE).and_then(|g| window_of(g, desc.float)).or(base.window),
            }
        })
        .collect()
}

/// The tiles of a Siemens mosaic as slices (nibabel's and dcm2niix's
/// geometry: the position given is the corner of the whole mosaic).
fn mosaic(set: &DataSet, m: Mosaic, base: FrameGeo) -> Vec<FrameGeo> {
    let (Some(p), Some(o), Some(s)) = (base.position, base.orientation, base.spacing) else {
        return (0..m.images).map(|k| FrameGeo { tile: Some(k), position: None, order: [k as f64, 0.0, 0.0, 0.0, 0.0, 0.0], ..base.clone() }).collect();
    };
    let (row, col) = ([o[0], o[1], o[2]], [o[3], o[4], o[5]]);
    // nibabel: (mosaic size − mosaic size / tiles per side) / 2, in pixels.
    let full = [m.rows * m.per_side, m.cols * m.per_side];
    let full = [full[0].max(m.full_rows), full[1].max(m.full_cols)];
    let fix = [(full[0] as f64 - full[0] as f64 / m.per_side as f64) / 2.0, (full[1] as f64 - full[1] as f64 / m.per_side as f64) / 2.0];
    let corner: [f64; 3] = std::array::from_fn(|a| p[a] + col[a] * s[0] * fix[0] + row[a] * s[1] * fix[1]);
    let normal = csa_value(set, "SliceNormalVector")
        .and_then(|v| vec3(&v))
        .filter(|n| norm(*n) > 0.5)
        .map(unit)
        .unwrap_or_else(|| cross(row, col));
    let step = base.thickness.unwrap_or(1.0);
    (0..m.images)
        .map(|k| FrameGeo {
            tile: Some(k),
            position: Some(std::array::from_fn(|a| corner[a] + normal[a] * step * k as f64)),
            ..base.clone()
        })
        .collect()
}

/// How the frames make the image.
#[derive(Debug, Clone)]
pub struct Assembly {
    /// Slices and volumes.
    pub nz: usize,
    pub nt: usize,
    /// For each slot (slice fastest, then volume): the index of its frame.
    pub slots: Vec<Option<usize>>,
    /// Voxel to RAS (mm); `oriented` false when the frames give no orientation.
    pub affine: Mat4,
    pub oriented: bool,
    pub zooms: [f64; 3],
    /// The volumes are time points: k × TR gives their times. False when
    /// they are (also) echoes, segments or another dimension.
    pub timed: bool,
    pub warnings: Vec<String>,
}

/// What a dimension of the volumes counts, in a warning.
fn dimension_name(t: Option<tags::Tag>) -> String {
    match t {
        Some(tags::TEMPORAL_POSITION_INDEX) => "time points".into(),
        Some(tags::EFFECTIVE_ECHO_TIME | tags::ECHO_TIME | tags::ECHO_NUMBERS) => "echoes".into(),
        Some(tags::STACK_ID) => "stacks".into(),
        Some(tags::DIFFUSION_B_VALUE) => "b-values".into(),
        Some(t) => format!("values of ({:04X},{:04X})", t >> 16, t & 0xFFFF),
        None => "values".into(),
    }
}

/// Whether the volumes are time points, and if not, a warning that says
/// what tells them apart. Echoes of one time point are not k × TR apart:
/// those volumes get no times rather than wrong ones.
fn volume_meaning(frames: &[FrameGeo], keyed: bool, nt: usize) -> (bool, Option<String>) {
    if nt <= 1 {
        return (true, None);
    }
    let distinct = |mut v: Vec<u64>| {
        v.sort_unstable();
        v.dedup();
        v.len()
    };
    if keyed {
        let dims = frames[0].key_dims.clone();
        let len = frames[0].key.as_ref().map_or(0, Vec::len);
        let varying: Vec<(Option<tags::Tag>, usize)> = (0..len)
            .map(|c| {
                let values = frames.iter().filter_map(|f| f.key.as_ref()?.get(c).map(|&v| v as u64)).collect();
                (dims.as_ref().and_then(|d| d.get(c)).copied(), distinct(values))
            })
            .filter(|&(_, n)| n > 1)
            .collect();
        if varying.iter().all(|&(t, _)| t == Some(tags::TEMPORAL_POSITION_INDEX)) {
            return (true, None);
        }
        // Segments of a segmentation have no times and need no warning.
        if varying.iter().all(|&(t, _)| t == Some(tags::REFERENCED_SEGMENT_NUMBER)) {
            return (false, None);
        }
        let parts: Vec<String> = varying.iter().map(|&(t, n)| format!("{n} {}", dimension_name(t))).collect();
        // "4 time points × 2 echoes" when every combination is a volume.
        let product = varying.iter().map(|&(_, n)| n).product::<usize>() == nt;
        let what = if product { format!("are {}", parts.join(" × ")) } else { format!("differ in {}", parts.join(" and ")) };
        return (false, Some(format!("The {nt} volumes {what}: they are shown without times, and a time course mixes them.")));
    }
    let echoes = distinct(frames.iter().map(|f| f.echo[0].to_bits() ^ f.echo[1].to_bits().rotate_left(1)).collect());
    if echoes <= 1 {
        return (true, None);
    }
    (false, Some(format!("The {nt} volumes hold {echoes} echoes: they are shown without times, and a time course mixes them.")))
}

pub fn assemble(frames: &[FrameGeo]) -> Assembly {
    let first = &frames[0];
    let [dr, dc] = first.spacing.unwrap_or([1.0, 1.0]);
    let oriented = frames.iter().all(|f| f.position.is_some() && f.orientation.is_some());
    let mut warnings = Vec::new();
    if !oriented {
        // Frames in time (or a single image): one slice, the frames as volumes.
        let dz = first.thickness.unwrap_or(1.0);
        let mut affine = [[0.0; 4]; 4];
        affine[0][0] = dc;
        affine[1][1] = dr;
        affine[2][2] = dz;
        affine[3][3] = 1.0;
        return Assembly {
            nz: 1,
            nt: frames.len(),
            slots: (0..frames.len()).map(Some).collect(),
            affine,
            oriented: false,
            zooms: [dc, dr, dz],
            timed: true,
            warnings,
        };
    }
    let o = first.orientation.unwrap();
    let (row, col) = ([o[0], o[1], o[2]], [o[3], o[4], o[5]]);
    let normal = cross(row, col);
    let along = |f: &FrameGeo| dot(f.position.unwrap(), normal);
    let mut sorted: Vec<usize> = (0..frames.len()).collect();
    sorted.sort_by(|&a, &b| {
        along(&frames[a]).total_cmp(&along(&frames[b])).then_with(|| {
            frames[a].order.iter().zip(&frames[b].order).map(|(x, y)| x.total_cmp(y)).find(|c| c.is_ne()).unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    // Slices: runs of (nearly) equal positions.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for &i in &sorted {
        match groups.last_mut() {
            Some(g) if (along(&frames[i]) - along(&frames[g[0]])).abs() <= SAME_POSITION => g.push(i),
            _ => groups.push(vec![i]),
        }
    }
    let nz = groups.len();
    // Volumes: by the key the file gives, else by rank at each position.
    let keyed = frames.iter().all(|f| f.key.is_some());
    let (nt, slots) = if keyed {
        let mut keys: Vec<&Vec<i64>> = frames.iter().map(|f| f.key.as_ref().unwrap()).collect();
        keys.sort();
        keys.dedup();
        let nt = keys.len();
        let mut slots = vec![None; nz * nt];
        for (z, g) in groups.iter().enumerate() {
            for &i in g {
                let t = keys.binary_search(&frames[i].key.as_ref().unwrap()).unwrap();
                slots[z + nz * t].get_or_insert(i);
            }
        }
        (nt, slots)
    } else {
        let nt = groups.iter().map(Vec::len).max().unwrap_or(1);
        let mut slots = vec![None; nz * nt];
        for (z, g) in groups.iter().enumerate() {
            for (t, &i) in g.iter().enumerate() {
                slots[z + nz * t] = Some(i);
            }
        }
        (nt, slots)
    };
    let (timed, meaning) = volume_meaning(frames, keyed, nt);
    warnings.extend(meaning);
    let empty = slots.iter().filter(|s| s.is_none()).count();
    if empty > 0 {
        warnings.push(format!("{empty} of {} slices are missing from the series (shown black).", slots.len()));
    }
    let p0 = frames[groups[0][0]].position.unwrap();
    let last = frames[groups[nz - 1][0]].position.unwrap();
    let step: [f64; 3] = if nz > 1 {
        std::array::from_fn(|a| (last[a] - p0[a]) / (nz - 1) as f64)
    } else {
        let dz = first.thickness.unwrap_or(1.0);
        [normal[0] * dz, normal[1] * dz, normal[2] * dz]
    };
    if nz > 2 {
        let d: Vec<f64> = groups.iter().map(|g| along(&frames[g[0]])).collect();
        let gaps: Vec<f64> = d.windows(2).map(|w| w[1] - w[0]).collect();
        let mean = (d[nz - 1] - d[0]) / (nz - 1) as f64;
        let (lo, hi) = gaps.iter().fold((f64::MAX, f64::MIN), |(a, b), &g| (a.min(g), b.max(g)));
        if hi - lo > 0.01 * mean.abs() + 0.001 {
            warnings.push(format!("Irregular slice spacing ({lo:.3} to {hi:.3} mm): shown with the mean spacing, {mean:.3} mm."));
        }
    }
    let dz = norm(step);
    if nz > 1 && dz > 0.0 {
        let tilt = (dot(step, normal).abs() / dz).clamp(-1.0, 1.0).acos().to_degrees();
        if tilt > 0.5 {
            warnings.push(format!("Slices are not stacked along their normal (gantry tilt, {tilt:.1}°): the views are resampled."));
        }
    }
    // LPS to RAS: x and y change sign.
    let mut affine = [[0.0; 4]; 4];
    for a in 0..3 {
        let s = if a < 2 { -1.0 } else { 1.0 };
        affine[a][0] = s * row[a] * dc;
        affine[a][1] = s * col[a] * dr;
        affine[a][2] = s * step[a];
        affine[a][3] = s * p0[a];
    }
    affine[3][3] = 1.0;
    Assembly { nz, nt, slots, affine, oriented: true, zooms: [dc, dr, if dz > 0.0 { dz } else { 1.0 }], timed, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(i: usize, z: f64, order: f64) -> FrameGeo {
        FrameGeo {
            source: i,
            frame: 0,
            tile: None,
            position: Some([10.0, 20.0, z]),
            orientation: Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
            spacing: Some([0.5, 0.25]),
            thickness: Some(2.0),
            key: None,
            key_dims: None,
            echo: [0.0; 2],
            order: [order, 0.0, 0.0, 0.0, 0.0, 0.0],
            rescale: (1.0, 0.0),
            window: None,
        }
    }

    #[test]
    fn axial_stack_and_volumes() {
        // Three slices, two volumes, shuffled.
        let frames: Vec<FrameGeo> = vec![frame(0, 4.0, 2.0), frame(1, 0.0, 1.0), frame(2, 2.0, 1.0), frame(3, 0.0, 2.0), frame(4, 4.0, 1.0), frame(5, 2.0, 2.0)];
        let a = assemble(&frames);
        assert_eq!((a.nz, a.nt), (3, 2));
        assert_eq!(a.slots, vec![Some(1), Some(2), Some(4), Some(3), Some(5), Some(0)]);
        // LPS (10, 20, 0) → RAS (-10, -20, 0); columns 0.25 mm, rows 0.5 mm, slices 2 mm.
        assert_eq!(a.affine[0], [-0.25, 0.0, 0.0, -10.0]);
        assert_eq!(a.affine[1], [0.0, -0.5, 0.0, -20.0]);
        assert_eq!(a.affine[2], [0.0, 0.0, 2.0, 0.0]);
        assert!(a.warnings.is_empty());
    }

    #[test]
    fn missing_and_irregular() {
        let frames = vec![frame(0, 0.0, 1.0), frame(1, 2.0, 1.0), frame(2, 5.0, 1.0), frame(3, 0.0, 2.0)];
        let a = assemble(&frames);
        assert_eq!((a.nz, a.nt), (3, 2));
        assert_eq!(a.warnings.len(), 2, "{:?}", a.warnings);
    }

    #[test]
    fn no_geometry_is_time() {
        let mut f = frame(0, 0.0, 0.0);
        f.position = None;
        let a = assemble(&[f.clone(), f]);
        assert_eq!((a.nz, a.nt, a.oriented), (1, 2, false));
    }

    #[test]
    fn acquisition_time() {
        assert_eq!(seconds("101502.5"), Some(36902.5));
        assert_eq!(seconds("10"), Some(36000.0));
        assert_eq!(seconds("x"), None);
    }
}
