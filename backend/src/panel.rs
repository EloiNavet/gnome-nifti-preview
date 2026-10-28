//! The text of the side panel and of the readout, as the Sushi viewer
//! shows it (`sushi/common/niftiPreview/viewState.js`: `metadataSections`,
//! `spectroscopySections`, `aboutSections`, `readoutText`, `volumeText`,
//! `labelRows`, `seriesLabel`), for viewers written in Rust. The input is
//! what the protocol sends (`Volume::header_info`, the statistics, the
//! window shown), so that both give the same text; `tests/gjs` compares them.
//!
//! Numbers are written as JavaScript writes them (`formatNumber`: four
//! significant digits by `toPrecision`, then the shortest form that reads
//! back, with JavaScript's exponent rules).

use serde_json::Value;

use crate::formats::dicom::catalog::SeriesEntry;
use crate::view::{LabelRow, Readout};
use crate::volume::Stats;

/// A section of the panel: rows of (key, value), or a block of text.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: String,
    pub rows: Vec<(String, String)>,
    /// The affine, or a DICOM report (`prose`: wrapped as running text).
    pub text: Option<String>,
    pub prose: bool,
}

impl Section {
    fn rows(title: &str, rows: Vec<(String, String)>) -> Section {
        Section { title: title.into(), rows, text: None, prose: false }
    }
}

/// What is shown besides the header: the colormap in effect, the window
/// shown (low, high and where it comes from: "manual", "file", "first" or
/// "auto"), whether the grid is resampled.
#[derive(Debug, Clone, Default)]
pub struct Display<'a> {
    pub effective_colormap: &'a str,
    pub window: Option<(f64, f64, &'a str)>,
    pub resampled: bool,
    /// DICOM: who the images are of (off unless asked).
    pub show_patient: bool,
    /// The statistics are still being computed: their rows say "…".
    pub pending: bool,
}

// ------------------------------------------------------------- numbers

/// JavaScript's `String(x)` for a number: the shortest digits that read
/// back, in fixed notation from 1e-6 to below 1e21, else exponential.
pub fn js_number(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if x == 0.0 {
        return "0".into();
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let e = format!("{:e}", x.abs());
    let (mantissa, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().unwrap_or(0) + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{}{}", if n - 1 < 0 { '-' } else { '+' }, (n - 1).abs())
    };
    format!("{sign}{body}")
}

/// Decimal digits of a positive finite number, `precision` after the first,
/// and the power of ten of the first one.
fn digits_of(ax: f64, precision: usize) -> (Vec<u8>, i32) {
    let s = format!("{ax:.precision$e}");
    let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
    (m.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect(), e.parse().unwrap_or(0))
}

/// |x| rounded half up (away from zero, as JavaScript's `toPrecision` and
/// `toFixed` do) to the digit of 10^`place`: n with |x| ≈ n × 10^place. With
/// `place` None, to `significant` digits; returns n and the place used. None
/// when n would not fit (far more digits than any use here).
///
/// 40 digits decide almost always; only a 5 followed by zeros there may be a
/// tie that formatting rounded, and then the exact expansion is used (every
/// double has at most 767 significant digits).
fn round_half_up(ax: f64, place: Option<i32>, significant: i32) -> Option<(u128, i32)> {
    let round = |digits: &[u8], first: i32| -> Option<(u128, i32)> {
        let place = place.unwrap_or(first - significant + 1);
        let keep = first - place + 1;
        if keep > 36 {
            return None;
        }
        if keep < 0 {
            return Some((0, place));
        }
        let mut n: u128 = 0;
        for &d in &digits[..keep as usize] {
            n = n * 10 + d as u128;
        }
        if digits.get(keep as usize).is_some_and(|&d| d >= 5) {
            n += 1;
        }
        Some((n, place))
    };
    let (digits, first) = digits_of(ax, 40);
    let keep = first - place.unwrap_or(first - significant + 1) + 1;
    let tail = digits.get(keep.max(0) as usize..).unwrap_or(&[]);
    let maybe_tie = tail.first() == Some(&5) && tail[1..].iter().all(|&d| d == 0);
    if keep < 0 || keep as usize >= digits.len() || !maybe_tie {
        return round(&digits, first);
    }
    let (exact, first) = digits_of(ax, 1100);
    round(&exact, first)
}

/// `Number(x.toPrecision(digits))`.
fn to_precision(x: f64, digits: usize) -> f64 {
    if x == 0.0 || !x.is_finite() || digits == 0 {
        return x;
    }
    match round_half_up(x.abs(), None, digits as i32) {
        Some((n, place)) => format!("{}{n}e{place}", if x < 0.0 { "-" } else { "" }).parse().unwrap_or(x),
        None => x,
    }
}

/// `x.toFixed(digits)`.
pub fn to_fixed(x: f64, digits: usize) -> String {
    if !x.is_finite() || x.abs() >= 1e21 {
        return js_number(x);
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let n = if x == 0.0 {
        Some(0)
    } else {
        round_half_up(x.abs(), Some(-(digits as i32)), 0).map(|(n, _)| n)
    };
    // Beyond the digits a u128 holds (never asked here): Rust's rounding.
    let Some(n) = n else { return format!("{x:.digits$}") };
    let s = format!("{n:0>width$}", width = digits + 1);
    let (int, frac) = s.split_at(s.len() - digits);
    if digits == 0 {
        format!("{sign}{int}")
    } else {
        format!("{sign}{int}.{frac}")
    }
}

/// `formatNumber(x, digits)` of the viewer.
pub fn format_number(x: f64, digits: usize) -> String {
    if !x.is_finite() || x.fract() == 0.0 {
        return js_number(x);
    }
    js_number(to_precision(x, digits))
}

/// `formatNumber` of a protocol value: a number, an integer too large for a
/// double sent as a decimal string, or null ("–").
pub fn format_value(v: &Value, digits: usize) -> String {
    match v {
        Value::Null => "–".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) if i.unsigned_abs() <= 1 << 53 => i.to_string(),
            (_, Some(u), _) if u <= 1 << 53 => u.to_string(),
            (_, _, Some(f)) => format_number(f, digits),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        other => js_string(other),
    }
}

/// JavaScript's `String(v)` of a JSON value.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map(js_number).unwrap_or_else(|| n.to_string()),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(|x| if x.is_null() { String::new() } else { js_string(x) }).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// Short human-readable byte size (`formatBytes`).
pub fn format_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{} {}", to_fixed(v, if v < 10.0 { 1 } else { 0 }), UNITS[u])
}

fn format_seconds(s: f64) -> String {
    if s < 1.0 {
        format!("{} ms", format_number(s * 1000.0, 3))
    } else {
        format!("{} s", format_number(s, 3))
    }
}

fn format_hertz(hz: f64) -> String {
    if hz >= 1000.0 {
        format!("{} kHz", format_number(hz / 1000.0, 3))
    } else {
        format!("{} Hz", format_number(hz, 3))
    }
}

/// Symbol of a NIfTI time unit ("" when unknown).
pub fn time_unit_symbol(unit: &str) -> &'static str {
    match unit {
        "sec" => "s",
        "msec" => "ms",
        "usec" => "µs",
        "hz" => "Hz",
        "ppm" => "ppm",
        "rads" => "rad/s",
        _ => "",
    }
}

// ------------------------------------------------------------- panel

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

/// JavaScript truthiness of a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0 && !x.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn str_of<'a>(h: &'a Value, key: &str) -> &'a str {
    h[key].as_str().unwrap_or("")
}

fn dims_text(h: &Value) -> String {
    h["shape"].as_array().map(|s| s.iter().take(3).map(js_string).collect::<Vec<_>>().join(" × ")).unwrap_or_default()
}

fn zooms_text(h: &Value) -> String {
    let unit = match str_of(h, "space_units") {
        "unknown" => String::new(),
        u => format!(" {u}"),
    };
    let zooms = h["zooms"].as_array().map(|z| z.iter().take(3).map(|z| format_value(z, 4)).collect::<Vec<_>>().join(" × "));
    format!("{}{unit}", zooms.unwrap_or_default())
}

fn obliquity(h: &Value, d: &Display) -> Option<String> {
    let o = num(&h["obliquity_deg"])?;
    (o > 1.0).then(|| format!("{}°{}", format_number(o, 3), if d.resampled { ", resampled" } else { "" }))
}

/// The 3×4 affine, each column as wide as its widest value (`affineText`).
pub fn affine_text(affine: &Value) -> String {
    let cell = |x: &Value| match x.as_f64() {
        Some(x) if x.is_finite() => {
            if x.abs() < 5e-5 {
                "0".to_string()
            } else {
                js_number(to_fixed(x, 4).parse().unwrap_or(x))
            }
        }
        _ => "–".to_string(),
    };
    let cells: Vec<Vec<String>> = (0..3)
        .map(|r| (0..4).map(|c| cell(&affine[r][c])).collect())
        .collect();
    let widths: Vec<usize> = (0..4).map(|c| cells.iter().map(|row| row[c].chars().count()).max().unwrap_or(0)).collect();
    cells
        .iter()
        .map(|row| row.iter().enumerate().map(|(c, v)| format!("{v:>w$}", w = widths[c])).collect::<Vec<_>>().join("  "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The panel of an image (`metadataSections`): Geometry, Data, File, the
/// patient when asked, Affine; spectroscopy files have their own.
pub fn sections(h: &Value, stats: Option<&Stats>, d: &Display) -> Vec<Section> {
    if !h["mrs"].is_null() {
        return spectroscopy_sections(h, d);
    }
    let mut geometry = vec![("Dimensions".to_string(), dims_text(h))];
    let n_volumes = h["n_volumes"].as_u64().unwrap_or(1);
    if n_volumes > 1 {
        let t = match str_of(h, "time_units") {
            "sec" => "s",
            "msec" => "ms",
            "usec" => "µs",
            "unknown" => "",
            other => other,
        };
        let unit = if t.is_empty() { String::new() } else { format!(" {t}") };
        let every = if str_of(h, "format") == "DICOM" && h["orientation_known"] != Value::Bool(true) { "every" } else { "TR" };
        let tr = if truthy(&h["repetition_time"]) { format!(" · {every} {}{unit}", format_value(&h["repetition_time"], 4)) } else { String::new() };
        geometry.push(("Volumes".into(), format!("{n_volumes}{tr}")));
    }
    geometry.push(("Voxel size".into(), zooms_text(h)));
    let known = h["orientation_known"] == Value::Bool(true);
    geometry.push(("Orientation".into(), if known { str_of(h, "orientation").to_string() } else { "unknown".into() }));
    if let Some(o) = obliquity(h, d) {
        geometry.push(("Obliquity".into(), o));
    }
    let source = str_of(h, "affine_source");
    let transform = match (source, h.get(source).and_then(Value::as_str)) {
        ("none", _) => "none".to_string(),
        (s, Some(x)) if !x.is_empty() => format!("{s} ({x})"),
        (s, _) => s.to_string(),
    };
    geometry.push(("Transform".into(), transform));

    let mut data = vec![("Datatype".to_string(), str_of(h, "datatype").to_string())];
    let (slope, inter) = (num(&h["slope"]).unwrap_or(1.0), num(&h["inter"]).unwrap_or(0.0));
    if slope != 1.0 || inter != 0.0 {
        data.push(("Scaling".into(), format!("× {} + {}", format_value(&h["slope"], 4), format_value(&h["inter"], 4))));
    }
    match stats {
        Some(s) if s.labels.is_labels => {
            data.push(("Labels".into(), if s.labels.binary { "binary mask".into() } else { s.labels.n_labels.to_string() }));
        }
        Some(s) => {
            let v = |x: &Option<Value>| format_value(x.as_ref().unwrap_or(&Value::Null), 4);
            data.push(("Range".into(), format!("{} – {}", v(&s.min), v(&s.max))));
            // The colours of an RGB image are shown as they are, without a window.
            if d.effective_colormap != "rgb" {
                let (low, high, source) = d.window.unwrap_or((s.window.low, s.window.high, ""));
                let how = match source {
                    "manual" => " (set by hand)",
                    "first" => " (first volume)",
                    "file" => " (from the file)",
                    _ => "",
                };
                // (A value JSON cannot carry reaches the viewer as null: "–".)
                let f = |x: f64| if x.is_finite() { format_number(x, 4) } else { "–".to_string() };
                data.push(("Window".into(), format!("{} – {}{how}", f(low), f(high))));
            }
        }
        None if d.pending => {
            data.push(("Range".into(), "…".into()));
            data.push(("Window".into(), "…".into()));
        }
        None => {}
    }
    let intent = str_of(h, "intent");
    if !intent.is_empty() && intent != "none" {
        data.push(("Intent".into(), intent.into()));
    }
    if !str_of(h, "descrip").is_empty() {
        data.push(("Description".into(), str_of(h, "descrip").into()));
    }

    let gzip = if h["compressed"] == Value::Bool(true) { ", gzip" } else { "" };
    let mut file = vec![
        ("Format".to_string(), format!("{}{gzip}", str_of(h, "format"))),
        ("Size".to_string(), format_bytes(h["file_size"].as_u64().unwrap_or(0))),
    ];
    // Details of the format (MGH timing, MRtrix diffusion scheme, NRRD space, DICOM...).
    for pair in h["format_fields"].as_array().into_iter().flatten() {
        file.push((js_string(&pair[0]), js_string(&pair[1])));
    }
    let mut out = vec![Section::rows("Geometry", geometry), Section::rows("Data", data), Section::rows("File", file)];
    let patient = h["dicom"]["patient"].as_array().filter(|p| !p.is_empty());
    if let Some(p) = patient.filter(|_| d.show_patient) {
        out.push(Section::rows("Patient", p.iter().map(|kv| (js_string(&kv[0]), js_string(&kv[1]))).collect()));
    }
    out.push(Section { title: "Affine".into(), rows: Vec::new(), text: Some(affine_text(&h["affine"])), prose: false });
    out
}

/// Words for the NIfTI-MRS dimension tags (others are shown as they are).
fn dim_name(tag: &str) -> &str {
    match tag {
        "DIM_COIL" => "coils",
        "DIM_DYN" => "dynamics",
        "DIM_INDIRECT_0" | "DIM_INDIRECT_1" | "DIM_INDIRECT_2" => "indirect",
        "DIM_PHASE_CYCLE" => "phase cycles",
        "DIM_EDIT" => "edits",
        "DIM_MEAS" => "measurements",
        "DIM_ISIS" => "ISIS steps",
        other => other,
    }
}

/// The panel of a spectroscopy file: Spectroscopy, Geometry, File, Affine.
fn spectroscopy_sections(h: &Value, d: &Display) -> Vec<Section> {
    let m = &h["mrs"];
    let mut frequency = if truthy(&m["spectrometer_mhz"]) { format!("{} MHz", format_value(&m["spectrometer_mhz"], 5)) } else { String::new() };
    if truthy(&m["field_t"]) {
        frequency += &format!(" ({} T)", format_value(&m["field_t"], 2));
    }
    let nucleus = m["nucleus"].as_str().unwrap_or("unknown");
    let mut spectro = vec![("Nucleus".to_string(), if frequency.is_empty() { nucleus.to_string() } else { format!("{nucleus}, {frequency}") })];
    let points = js_string(&m["points"]);
    let bandwidth = num(&m["bandwidth_hz"]).filter(|&b| b != 0.0);
    spectro.push(("Points".into(), match bandwidth {
        Some(b) => format!("{points} over {}", format_hertz(b)),
        None => points,
    }));
    let dims = m["dims"].as_array().cloned().unwrap_or_default();
    if !dims.is_empty() {
        let t = dims.iter().map(|x| format!("{} {}", js_string(&x["size"]), dim_name(x["tag"].as_str().unwrap_or("")))).collect::<Vec<_>>();
        spectro.push(("Transients".into(), t.join(" × ")));
    }
    let mut timing = Vec::new();
    if let Some(te) = num(&m["echo_time_s"]) {
        timing.push(format!("TE {}", format_seconds(te)));
    }
    if let Some(tr) = num(&m["repetition_time_s"]).filter(|&t| t != 0.0) {
        timing.push(format!("TR {}", format_seconds(tr)));
    }
    if !timing.is_empty() {
        spectro.push(("Timing".into(), timing.join(" · ")));
    }
    spectro.push(("Datatype".into(), str_of(h, "datatype").into()));

    let single = m["single_voxel"] == Value::Bool(true);
    let mut geometry = if single { Vec::new() } else { vec![("Voxels".to_string(), dims_text(h))] };
    geometry.push((if single { "Voxel" } else { "Voxel size" }.into(), zooms_text(h)));
    if h["orientation_known"] == Value::Bool(true) {
        geometry.push(("Orientation".into(), str_of(h, "orientation").into()));
    }
    if !single {
        if let Some(o) = obliquity(h, d) {
            geometry.push(("Obliquity".into(), o));
        }
    }
    let source = str_of(h, "affine_source");
    geometry.push(("Transform".into(), if source == "none" { "none".into() } else { format!("{source} ({})", h.get(source).map(js_string).unwrap_or("undefined".into())) }));

    let gzip = if h["compressed"] == Value::Bool(true) { ", gzip" } else { "" };
    let mut file = vec![("Format".to_string(), format!("{}{gzip}", str_of(h, "format")))];
    let standard = m["format"]
        .as_str()
        .and_then(|f| f.strip_prefix("mrs_v"))
        .and_then(|v| v.split_once('_'))
        .filter(|(a, b)| !a.is_empty() && !b.is_empty() && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit()))
        .map(|(a, b)| format!("NIfTI-MRS {a}.{b}"))
        .unwrap_or_else(|| "NIfTI-MRS".into());
    file.push(("Standard".into(), standard));
    file.push(("Size".into(), format_bytes(h["file_size"].as_u64().unwrap_or(0))));
    vec![
        Section::rows("Spectroscopy", spectro),
        Section::rows("Geometry", geometry),
        Section::rows("File", file),
        Section { title: "Affine".into(), rows: Vec::new(), text: Some(affine_text(&h["affine"])), prose: false },
    ]
}

/// What the backend tells of a file it recognises but has no slices of (the
/// `about` of an error), as sections (`aboutSections`).
pub fn about_sections(about: &Value) -> Vec<Section> {
    let Some(list) = about.as_array() else { return Vec::new() };
    list.iter()
        .filter(|s| s["title"].is_string())
        .take(12)
        .map(|s| {
            let title = s["title"].as_str().unwrap_or("").to_string();
            if let Some(text) = s["text"].as_str() {
                return Section { title, rows: Vec::new(), text: Some(text.chars().take(50000).collect()), prose: s["prose"] == Value::Bool(true) };
            }
            let rows = s["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| r.as_array().is_some_and(|a| a.len() >= 2))
                .take(500)
                .map(|r| (js_string(&r[0]), js_string(&r[1])))
                .collect();
            Section::rows(&title, rows)
        })
        .collect()
}

// ------------------------------------------------------------- readout

/// The text under the pointer (`readoutText`): the value, the colour of an
/// RGB image, or the label, its name and its volume; with `world`, the
/// position in mm on a second line. Empty over the background of a label
/// map and outside the image.
pub fn readout_text(r: &Readout, effective_colormap: &str, label_volume: bool, world: bool) -> String {
    if r.voxel.is_none() {
        return String::new();
    }
    let value = r.value.map(|x| if r.categorical { x.to_label_json() } else { x.to_json() });
    let mut text = match (effective_colormap, r.rgb, &value) {
        ("rgb", Some(c), _) => format!("R {} · G {} · B {}", c[0], c[1], c[2]),
        (_, _, None) => "NaN/Inf".to_string(),
        ("labels", _, Some(v)) => {
            if v.as_f64() == Some(0.0) {
                return String::new();
            }
            let safe = v.as_i64().is_some_and(|i| i.unsigned_abs() < 1 << 53) || v.as_u64().is_some_and(|u| u < 1 << 53);
            let mut t = format!("label {}", if safe { js_string(v) } else { format_value(v, 6) });
            if let Some(name) = r.label.as_ref().and_then(|l| l.name.as_ref()).filter(|n| !n.is_empty()) {
                t += &format!(" · {name}");
            }
            if let Some(mm3) = r.label.as_ref().and_then(|l| l.volume_mm3).filter(|_| label_volume) {
                t += &format!(" · {} cm³", format_number(mm3 / 1000.0, 3));
            }
            t
        }
        (_, _, Some(v)) => format_value(v, 4),
    };
    if world {
        let w: Vec<String> = r.world_mm.iter().map(|&c| if c.is_finite() { to_fixed(c, 1) } else { "?".into() }).collect();
        text += &format!("\n{} mm", w.join(", "));
    }
    text
}

/// "Volume 12 / 63 · 24 s", and the decompression progress of a volume
/// asked for but not shown yet (`volumeText`).
pub fn volume_text(h: &Value, k: usize, shown: usize, progress: Option<f64>) -> String {
    let n = h["n_volumes"].as_u64().unwrap_or(1);
    let mut text = format!("Volume {k} / {}", n.saturating_sub(1));
    let is_4d = h["shape"].as_array().is_some_and(|s| s.len() == 4);
    if let Some(tr) = num(&h["repetition_time"]).filter(|tr| is_4d && tr.is_finite() && *tr > 0.0) {
        let unit = time_unit_symbol(str_of(h, "time_units"));
        text += &format!(" · {}{}", format_number(k as f64 * tr, 4), if unit.is_empty() { String::new() } else { format!(" {unit}") });
    }
    if let Some(p) = progress.filter(|_| k != shown) {
        text += &format!(" · decompressing {} %", (100.0 * p).floor());
    }
    text
}

/// A label of the list: its name ("label N" without one) and its volume
/// (cm³, or voxels when the voxel size is unknown) (`labelRows`).
pub fn label_line(row: &LabelRow) -> (String, String) {
    let name = row.name.clone().unwrap_or_else(|| format!("label {}", js_string(&row.value.to_label_json())));
    let volume = match row.volume_mm3 {
        Some(mm3) => format!("{} cm³", format_number(mm3 / 1000.0, 3)),
        None => format!("{} voxels", row.voxels),
    };
    (name, volume)
}

/// How a series is named: "3 · T1 MPRAGE · MR · 176 images", with its
/// study ("2024-05-02 Brain — …") when `with_study` (`seriesLabel`).
pub fn series_label(s: &SeriesEntry, with_study: bool) -> String {
    let mut parts = Vec::new();
    if let Some(n) = s.number {
        parts.push(n.to_string());
    }
    parts.push(if s.description.is_empty() { "Series".to_string() } else { s.description.clone() });
    if !s.modality.is_empty() {
        parts.push(s.modality.clone());
    }
    parts.push(format!("{} image{}", s.images, if s.images == 1 { "" } else { "s" }));
    let mut text = parts.join(" · ");
    if with_study {
        let d = &s.study_date;
        let date = if d.len() == 8 && d.bytes().all(|c| c.is_ascii_digit()) { format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..]) } else { d.clone() };
        let study = [date, s.study_description.clone()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ");
        if !study.is_empty() {
            text = format!("{study} — {text}");
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_as_javascript_writes_them() {
        // String(x)
        for (x, s) in [
            (0.0, "0"), (-0.0, "0"), (1.0, "1"), (123.456, "123.456"), (0.1 + 0.2, "0.30000000000000004"),
            (1e21, "1e+21"), (1.5e21, "1.5e+21"), (123456789012345680000.0, "123456789012345680000"),
            (1e-6, "0.000001"), (1e-7, "1e-7"), (-2.5e-8, "-2.5e-8"), (f64::NAN, "NaN"),
        ] {
            assert_eq!(js_number(x), s, "{x:e}");
        }
        // formatNumber: four significant digits, integers whole.
        for (x, s) in [
            (412.537, "412.5"), (0.9375, "0.9375"), (1.0625, "1.063"), (2.5, "2.5"), (1234567.8, "1235000"),
            (0.000123456, "0.0001235"), (1e-9 * 1.23456, "1.235e-9"), (-7.25381, "-7.254"), (1e300, "1e+300"),
            (99999.5, "100000"), (0.46875, "0.4688"),
        ] {
            assert_eq!(format_number(x, 4), s, "{x:e}");
        }
        assert_eq!(format_number(1.0005, 4), "1", "1.0005 is stored below the tie");
        // An exact tie (5 then only zeros: the exact expansion decides) and
        // the double just above it.
        let near = 0.5 + 2f64.powi(-52);
        assert_eq!(to_fixed(near, 0), "1");
        assert_eq!(to_fixed(0.5, 0), "1");
        assert_eq!(to_fixed(-0.5, 0), "-1");
        assert_eq!(to_fixed(1e20, 20), format!("{:.20}", 1e20), "beyond 36 digits: no silent zero");
        assert_eq!(format_value(&json!(null), 4), "–");
        assert_eq!(format_value(&json!("18446744073709551615"), 4), "18446744073709551615");
        assert_eq!(format_value(&json!(-42), 4), "-42");
        // toFixed: ties away from zero on the exact value.
        for (x, f, s) in [(1.25, 1, "1.3"), (1.35, 1, "1.4"), (-1.25, 1, "-1.3"), (0.05, 1, "0.1"), (2.5, 0, "3"), (0.0, 2, "0.00"), (-0.04, 1, "-0.0"), (1e-7, 4, "0.0000")] {
            assert_eq!(to_fixed(x, f), s, "{x}.toFixed({f})");
        }
    }

    #[test]
    fn bytes_and_affine() {
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1280), "1.3 KiB");
        assert_eq!(format_bytes(10 * 1024), "10 KiB");
        assert_eq!(format_bytes(3 << 30), "3.0 GiB");
        let a = json!([[-1.0, 0.0, 0.0, 90.0], [0.0, 1.0000001, 3e-5, -126.0], [0.0, 0.0, 1.0, null], [0.0, 0.0, 0.0, 1.0]]);
        assert_eq!(affine_text(&a), "-1  0  0    90\n 0  1  0  -126\n 0  0  1     –");
    }

    #[test]
    fn series_names() {
        let s = SeriesEntry {
            uid: "1.2".into(), number: Some(3), description: "T1 MPRAGE".into(), modality: "MR".into(), study_uid: "1".into(),
            study_description: "Brain".into(), study_date: "20240502".into(), patient: String::new(), images: 176, files: Vec::new(),
        };
        assert_eq!(series_label(&s, false), "3 · T1 MPRAGE · MR · 176 images");
        assert_eq!(series_label(&s, true), "2024-05-02 Brain — 3 · T1 MPRAGE · MR · 176 images");
    }
}
