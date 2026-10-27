//! What a DICOM file without an image holds, shown by the preview in place
//! of slices: the object and its series, and the content of structured
//! reports, radiotherapy structure sets and plans, and waveforms. Names of
//! people (the patient, an observer) are never part of it.

use serde_json::{json, Value};

use super::parse::{DataSet, Parsed};
use super::tags::{self, Tag};
use crate::error::{section, size_text};
use crate::header::clean_text;

/// Lines of a report, at most (the rest is counted).
const MAX_LINES: usize = 400;
/// Rows of a list (structures, beams), at most.
const MAX_ROWS: usize = 200;
/// Characters of one report value, at most.
const MAX_VALUE: usize = 4000;

/// Names of the storage classes that hold no image.
fn class_name(uid: &str) -> Option<&'static str> {
    Some(match uid {
        "1.2.840.10008.5.1.4.1.1.88.11" => "Basic text report",
        "1.2.840.10008.5.1.4.1.1.88.22" => "Enhanced report",
        "1.2.840.10008.5.1.4.1.1.88.33" => "Comprehensive report",
        "1.2.840.10008.5.1.4.1.1.88.34" => "Comprehensive 3D report",
        "1.2.840.10008.5.1.4.1.1.88.35" => "Extensible report",
        "1.2.840.10008.5.1.4.1.1.88.50" => "Mammography CAD report",
        "1.2.840.10008.5.1.4.1.1.88.59" => "Key object selection",
        "1.2.840.10008.5.1.4.1.1.88.67" => "X-ray radiation dose report",
        "1.2.840.10008.5.1.4.1.1.88.68" => "Radiopharmaceutical radiation dose report",
        "1.2.840.10008.5.1.4.1.1.88.71" => "Acquisition context report",
        "1.2.840.10008.5.1.4.1.1.481.3" => "Radiotherapy structure set",
        "1.2.840.10008.5.1.4.1.1.481.5" => "Radiotherapy plan",
        "1.2.840.10008.5.1.4.1.1.481.8" => "Radiotherapy ion plan",
        "1.2.840.10008.5.1.4.1.1.481.4" => "Radiotherapy beams treatment record",
        "1.2.840.10008.5.1.4.1.1.9.1.1" => "12-lead ECG waveform",
        "1.2.840.10008.5.1.4.1.1.9.1.2" => "General ECG waveform",
        "1.2.840.10008.5.1.4.1.1.9.1.3" => "Ambulatory ECG waveform",
        "1.2.840.10008.5.1.4.1.1.9.2.1" => "Hemodynamic waveform",
        "1.2.840.10008.5.1.4.1.1.9.3.1" => "Cardiac electrophysiology waveform",
        "1.2.840.10008.5.1.4.1.1.9.4.1" => "Basic voice audio waveform",
        "1.2.840.10008.5.1.4.1.1.11.1" => "Grayscale presentation state",
        "1.2.840.10008.5.1.4.1.1.11.2" => "Color presentation state",
        "1.2.840.10008.5.1.4.1.1.104.1" => "Encapsulated PDF",
        "1.2.840.10008.5.1.4.1.1.104.2" => "Encapsulated CDA",
        "1.2.840.10008.5.1.4.1.1.4.2" => "MR spectroscopy",
        "1.2.840.10008.5.1.4.1.1.66" => "Raw data",
        "1.2.840.10008.5.1.4.1.1.66.1" => "Spatial registration",
        "1.2.840.10008.5.1.4.1.1.66.3" => "Deformable spatial registration",
        "1.2.840.10008.5.1.4.1.1.67" => "Real world value mapping",
        _ => return None,
    })
}

/// Sections for the preview's panel (see `crate::error::section`).
pub fn about(p: &Parsed, file_size: u64) -> Value {
    let set = &p.set;
    let text = |t| set.text(t).unwrap_or_default();
    let mut file: Vec<(String, String)> = vec![("Format".into(), "DICOM".into()), ("Size".into(), size_text(file_size))];
    let mut push = |k: &str, v: String| {
        if !v.is_empty() {
            file.push((k.to_string(), v));
        }
    };
    let class = set.string(tags::SOP_CLASS).or_else(|| p.meta.string(tags::MEDIA_STORAGE_SOP_CLASS)).unwrap_or_default();
    push("Object", class_name(&class).map(str::to_string).unwrap_or_default());
    push("Modality", text(tags::MODALITY));
    push("Scanner", [text(tags::MANUFACTURER), text(tags::MODEL)].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" "));
    let series = match (set.int(tags::SERIES_NUMBER), set.text(tags::SERIES_DESCRIPTION)) {
        (Some(n), Some(d)) => format!("{n} · {d}"),
        (Some(n), None) => n.to_string(),
        (None, Some(d)) => d,
        (None, None) => String::new(),
    };
    push("Series", series);
    push("Study", [text(tags::STUDY_DESCRIPTION), super::date(&text(tags::STUDY_DATE))].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · "));
    push("Content date", super::date(&text(tags::CONTENT_DATE)));
    push("Title", text(tags::DOCUMENT_TITLE));
    push("Status", [text(tags::COMPLETION_FLAG), text(tags::VERIFICATION_FLAG)].iter().filter(|s| !s.is_empty()).map(|s| s.to_lowercase()).collect::<Vec<_>>().join(" · "));
    let name = p.syntax.name();
    push("Transfer syntax", if name.is_empty() { p.syntax.uid.clone() } else { name.to_string() });

    let mut out: Vec<Value> = section("File", file).into_iter().collect();
    if let Some(r) = report(set) {
        out.push(r);
    }
    out.extend(structures(set));
    out.extend(plan(set));
    out.extend(waveforms(set));
    Value::Array(out)
}

fn meaning(set: &DataSet, sequence: Tag) -> Option<String> {
    set.item(sequence).and_then(|c| c.text(tags::CODE_MEANING))
}

/// The content tree of a structured report, one line per item, indented.
fn report(set: &DataSet) -> Option<Value> {
    if set.items(tags::CONTENT_SEQUENCE).is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    if let Some(title) = meaning(set, tags::CONCEPT_NAME_CODE_SEQUENCE) {
        lines.push(title);
    }
    let mut skipped = 0;
    content(set, 0, &mut lines, &mut skipped);
    if skipped > 0 {
        lines.push(format!("… and {skipped} more items"));
    }
    Some(json!({"title": "Report", "text": lines.join("\n"), "prose": true}))
}

fn content(set: &DataSet, depth: usize, lines: &mut Vec<String>, skipped: &mut usize) {
    for item in set.items(tags::CONTENT_SEQUENCE) {
        let s = &item.set;
        if lines.len() >= MAX_LINES || depth > 10 {
            *skipped += 1;
            continue;
        }
        let name = meaning(s, tags::CONCEPT_NAME_CODE_SEQUENCE);
        let kind = s.text(tags::VALUE_TYPE).unwrap_or_default();
        let value = match kind.as_str() {
            "TEXT" => s.string(tags::TEXT_VALUE).map(|t| {
                let t: String = t.lines().map(clean_text).collect::<Vec<_>>().join(" / ");
                t.chars().take(MAX_VALUE).collect()
            }),
            "CODE" => meaning(s, tags::CONCEPT_CODE_SEQUENCE),
            "NUM" => s.item(tags::MEASURED_VALUE_SEQUENCE).and_then(|m| {
                let v = m.text(tags::NUMERIC_VALUE)?;
                let unit = m.item(tags::MEASUREMENT_UNITS_CODE_SEQUENCE).and_then(|u| u.text(tags::CODE_VALUE)).filter(|u| u != "1");
                Some(match unit {
                    Some(u) => format!("{v} {u}"),
                    None => v,
                })
            }),
            "DATE" => s.text(tags::DATE).map(|d| super::date(&d)),
            "TIME" => s.text(tags::TIME),
            "DATETIME" => s.text(tags::DATETIME),
            "UIDREF" => s.text(tags::UID),
            "IMAGE" | "COMPOSITE" | "WAVEFORM" => Some("(a reference to another object)".into()),
            "SCOORD" | "SCOORD3D" | "TCOORD" => Some("(coordinates)".into()),
            // Containers have no value; names of people (PNAME) are not shown.
            _ => None,
        };
        if kind != "PNAME" {
            let indent = "    ".repeat(depth);
            let line = match (name, value) {
                (Some(n), Some(v)) => format!("{indent}{n}: {v}"),
                (Some(n), None) => format!("{indent}{n}"),
                (None, Some(v)) => format!("{indent}{v}"),
                (None, None) => String::new(),
            };
            if !line.trim().is_empty() {
                lines.push(line);
            }
        }
        content(s, depth + 1, lines, skipped);
    }
}

/// The regions of interest of a radiotherapy structure set.
fn structures(set: &DataSet) -> Option<Value> {
    let rois = set.items(tags::STRUCTURE_SET_ROI_SEQUENCE);
    let mut rows: Vec<(String, String)> = Vec::new();
    if let Some(label) = set.text(tags::STRUCTURE_SET_LABEL) {
        rows.push(("Label".into(), label));
    }
    for item in rois.iter().take(MAX_ROWS) {
        let n = item.set.int(tags::ROI_NUMBER).map(|n| n.to_string()).unwrap_or_default();
        rows.push((n, item.set.text(tags::ROI_NAME).unwrap_or_else(|| "(unnamed)".into())));
    }
    if rois.len() > MAX_ROWS {
        rows.push(("…".into(), format!("{} more", rois.len() - MAX_ROWS)));
    }
    section(&format!("Structures · {}", rois.len()), if rois.is_empty() { Vec::new() } else { rows })
}

/// A radiotherapy plan: its label, fractions and beams.
fn plan(set: &DataSet) -> Option<Value> {
    let beams: Vec<_> = set.items(tags::BEAM_SEQUENCE).iter().chain(set.items(tags::ION_BEAM_SEQUENCE)).collect();
    let label = set.text(tags::RT_PLAN_LABEL);
    if label.is_none() && beams.is_empty() {
        return None;
    }
    let mut rows: Vec<(String, String)> = Vec::new();
    for (k, t) in [("Label", tags::RT_PLAN_LABEL), ("Name", tags::RT_PLAN_NAME), ("Intent", tags::PLAN_INTENT)] {
        if let Some(v) = set.text(t) {
            rows.push((k.into(), if k == "Intent" { v.to_lowercase() } else { v }));
        }
    }
    let fractions: i64 = set.items(tags::FRACTION_GROUP_SEQUENCE).iter().filter_map(|g| g.set.int(tags::NUMBER_OF_FRACTIONS_PLANNED)).sum();
    if fractions > 0 {
        rows.push(("Fractions".into(), fractions.to_string()));
    }
    rows.push(("Beams".into(), beams.len().to_string()));
    for (i, b) in beams.iter().take(MAX_ROWS).enumerate() {
        let name = b.set.text(tags::BEAM_NAME).unwrap_or_else(|| format!("beam {}", i + 1));
        let kind = b.set.text(tags::RADIATION_TYPE).map(|t| format!(" · {}", t.to_lowercase())).unwrap_or_default();
        rows.push((format!("{}", i + 1), format!("{name}{kind}")));
    }
    section("Plan", rows)
}

/// Each group of channels of a waveform: how many, how long, which.
fn waveforms(set: &DataSet) -> Option<Value> {
    let groups = set.items(tags::WAVEFORM_SEQUENCE);
    let mut rows: Vec<(String, String)> = Vec::new();
    for (i, g) in groups.iter().take(16).enumerate() {
        let s = &g.set;
        let channels = s.int(tags::NUMBER_OF_WAVEFORM_CHANNELS).unwrap_or(0);
        let samples = s.int(tags::NUMBER_OF_WAVEFORM_SAMPLES).unwrap_or(0);
        let hz = s.number(tags::SAMPLING_FREQUENCY).filter(|f| *f > 0.0);
        let mut what = format!("{channels} channels, {samples} samples");
        if let Some(hz) = hz {
            what.push_str(&format!(" at {} Hz ({} s)", super::number(hz), super::number(samples as f64 / hz)));
        }
        let label = s.text(tags::MULTIPLEX_GROUP_LABEL).unwrap_or_else(|| format!("Group {}", i + 1));
        rows.push((label, what));
        let names: Vec<String> = s.items(tags::CHANNEL_DEFINITION_SEQUENCE).iter().filter_map(|c| meaning(&c.set, tags::CHANNEL_SOURCE_SEQUENCE)).collect();
        if !names.is_empty() {
            let shown: Vec<String> = names.iter().take(24).cloned().collect();
            let more = if names.len() > shown.len() { ", …" } else { "" };
            rows.push(("Channels".into(), format!("{}{more}", shown.join(", "))));
        }
    }
    section("Waveform", rows)
}
