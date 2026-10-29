//! The series around a DICOM file: those of its folder (read from the
//! headers of the neighbouring files), or those a DICOMDIR lists. A series
//! is opened from its files.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use super::parse::{self, DataSet, Extent, Parsed};
use super::tags;
use crate::error::{bail, Result};

/// Directory entries listed, and files read, at most, in a folder.
pub const MAX_LISTED: usize = 50_000;
pub const MAX_READ: usize = 10_000;

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesEntry {
    pub uid: String,
    pub number: Option<i64>,
    pub description: String,
    pub modality: String,
    pub study_uid: String,
    pub study_description: String,
    pub study_date: String,
    pub patient: String,
    /// Images: files, or frames of multi-frame files.
    pub images: usize,
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    /// The folder read, or the DICOMDIR.
    pub origin: PathBuf,
    pub dicomdir: bool,
    pub series: Vec<SeriesEntry>,
    /// The folder held more files than were read.
    pub incomplete: bool,
}

impl Catalog {
    pub fn find(&self, uid: &str) -> Option<&SeriesEntry> {
        self.series.iter().find(|s| s.uid == uid)
    }

    /// The series offered next to `current`: every series of a DICOMDIR (it
    /// is made to browse patients and studies), those of the same study in a
    /// folder (the other sequences of the exam, not unrelated files that
    /// happen to share the folder).
    pub fn related(&self, current: Option<&str>) -> Vec<&SeriesEntry> {
        if self.dicomdir {
            return self.series.iter().collect();
        }
        let study = current.and_then(|uid| self.find(uid)).map(|s| s.study_uid.as_str());
        match study {
            Some(study) => self.series.iter().filter(|s| s.study_uid == study).collect(),
            None => self.series.iter().collect(),
        }
    }

    fn sort(&mut self) {
        self.series.sort_by(|a, b| {
            (&a.study_date, &a.study_uid, a.number.unwrap_or(i64::MAX), &a.description, &a.uid).cmp(&(
                &b.study_date,
                &b.study_uid,
                b.number.unwrap_or(i64::MAX),
                &b.description,
                &b.uid,
            ))
        });
        for s in &mut self.series {
            s.files.sort();
        }
    }
}

fn entry_of(set: &DataSet, uid: String) -> SeriesEntry {
    let text = |t| set.text(t).unwrap_or_default();
    SeriesEntry {
        uid,
        number: set.int(tags::SERIES_NUMBER),
        description: text(tags::SERIES_DESCRIPTION),
        modality: text(tags::MODALITY),
        study_uid: text(tags::STUDY_INSTANCE),
        study_description: text(tags::STUDY_DESCRIPTION),
        study_date: text(tags::STUDY_DATE),
        patient: text(tags::PATIENT_NAME),
        images: 0,
        files: Vec::new(),
    }
}

/// An image file of the folder.
pub fn is_image(p: &Parsed) -> bool {
    p.pixels.is_some() && p.set.int(tags::ROWS).is_some()
}

/// The image series of a folder. The files of series `keep` are returned
/// parsed (the file already parsed, `known`, is not read again).
pub fn scan_folder(dir: &Path, keep: Option<&str>, known: Option<(&Path, &Parsed)>) -> Result<(Catalog, Vec<(PathBuf, Parsed)>)> {
    let mut names: Vec<PathBuf> = Vec::new();
    let mut incomplete = false;
    let entries = std::fs::read_dir(if dir.as_os_str().is_empty() { Path::new(".") } else { dir })
        .map_err(|e| crate::error::PreviewError::new("not_found", format!("Cannot list {}: {e}", dir.display())))?;
    for e in entries.flatten() {
        if names.len() >= MAX_LISTED {
            incomplete = true;
            break;
        }
        let name = e.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            names.push(e.path());
        }
    }
    names.sort();
    if names.len() > MAX_READ {
        incomplete = true;
        names.truncate(MAX_READ);
    }
    let mut series: HashMap<String, SeriesEntry> = HashMap::new();
    let mut kept = Vec::new();
    for path in names {
        let owned;
        let parsed = match known {
            Some((p, parsed)) if p == path => parsed,
            _ => {
                let Ok(file) = crate::source::open_data_file(&path) else { continue };
                let Ok(meta) = file.metadata() else { continue };
                if meta.len() < 132 {
                    continue;
                }
                match parse::parse_file(&file, meta.len(), Extent::Image) {
                    Ok(p) => {
                        owned = p;
                        &owned
                    }
                    Err(_) => continue,
                }
            }
        };
        if !is_image(parsed) {
            continue;
        }
        let Some(uid) = parsed.set.text(tags::SERIES_INSTANCE) else { continue };
        let frames = parsed.set.int(tags::NUMBER_OF_FRAMES).unwrap_or(1).max(1) as usize;
        let e = series.entry(uid.clone()).or_insert_with(|| entry_of(&parsed.set, uid.clone()));
        e.images += frames;
        e.files.push(path.clone());
        if keep == Some(uid.as_str()) {
            // Copies of the same instance are one image (the file opened wins).
            let instance = parsed.set.text(tags::SOP_INSTANCE);
            let clicked = known.is_some_and(|(p, _)| p == path);
            match kept.iter().position(|(_, q): &(PathBuf, Parsed)| instance.is_some() && q.set.text(tags::SOP_INSTANCE) == instance) {
                Some(i) if clicked => kept[i] = (path, parsed.clone()),
                Some(_) => {}
                None => kept.push((path, parsed.clone())),
            }
        }
    }
    let mut catalog = Catalog { origin: dir.to_path_buf(), dicomdir: false, series: series.into_values().collect(), incomplete };
    catalog.sort();
    Ok((catalog, kept))
}

/// Whether a parsed file is a DICOMDIR.
pub fn is_dicomdir(p: &Parsed) -> bool {
    p.meta.string(tags::MEDIA_STORAGE_SOP_CLASS).as_deref() == Some("1.2.840.10008.1.3.10") || p.set.contains(tags::DIRECTORY_RECORDS)
}

/// A file named by a DICOMDIR: path components below the DICOMDIR's
/// folder (never outside it, also through a symbolic link), matched without
/// regard to case as media written on another system often differ. Links
/// are checked here, when the DICOMDIR is read.
pub fn referenced_file(root: &Path, components: &[String]) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    if components.is_empty() {
        return None;
    }
    for c in components {
        let c = c.trim();
        let candidate = Path::new(c);
        if c.is_empty() || candidate.components().count() != 1 || !matches!(candidate.components().next(), Some(Component::Normal(_))) {
            return None;
        }
        let exact = path.join(c);
        if exact.exists() {
            path = exact;
            continue;
        }
        let found = std::fs::read_dir(&path).ok()?.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(c))?;
        path = found.path();
    }
    let folder = if root.as_os_str().is_empty() { Path::new(".") } else { root };
    let inside = std::fs::canonicalize(&path).ok()?.starts_with(std::fs::canonicalize(folder).ok()?);
    (inside && path.is_file()).then_some(path)
}

/// The series a DICOMDIR lists (records patient → study → series → image).
pub fn dicomdir_catalog(path: &Path, parsed: &Parsed) -> Result<Catalog> {
    let root = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let records = parsed.set.items(tags::DIRECTORY_RECORDS);
    if records.is_empty() {
        bail!("invalid_image", "This DICOMDIR lists no files");
    }
    let by_offset: HashMap<u64, usize> = records.iter().enumerate().map(|(i, r)| (r.offset, i)).collect();
    let kind = |i: usize| records[i].set.string(tags::RECORD_TYPE).unwrap_or_default().to_ascii_uppercase();
    let offset_of = |i: usize, t| records[i].set.int(t).filter(|&o| o > 0).map(|o| o as u64);
    // The records reached from `first` through "next" offsets.
    let siblings = |first: u64| -> Vec<usize> {
        let mut out = Vec::new();
        let mut at = Some(first);
        while let Some(o) = at {
            let Some(&i) = by_offset.get(&o) else { break };
            if out.contains(&i) || out.len() > records.len() {
                break;
            }
            out.push(i);
            at = offset_of(i, tags::NEXT_RECORD);
        }
        out
    };
    // Parent of each record: from the "lower level" offsets when they are
    // consistent, else from the order of the records.
    let mut parent: Vec<Option<usize>> = vec![None; records.len()];
    let mut linked = false;
    for i in 0..records.len() {
        if let Some(lower) = offset_of(i, tags::LOWER_RECORD) {
            for child in siblings(lower) {
                if child != i {
                    parent[child] = Some(i);
                    linked = true;
                }
            }
        }
    }
    if !linked {
        let level = |k: &str| match k {
            "PATIENT" => 0,
            "STUDY" => 1,
            "SERIES" => 2,
            _ => 3,
        };
        let mut stack: Vec<(usize, usize)> = Vec::new();
        for (i, up) in parent.iter_mut().enumerate() {
            let l = level(&kind(i));
            while stack.last().is_some_and(|&(_, sl)| sl >= l) {
                stack.pop();
            }
            *up = stack.last().map(|&(p, _)| p);
            stack.push((i, l));
        }
    }
    let ancestor = |mut i: usize, want: &str| -> Option<usize> {
        for _ in 0..8 {
            i = parent[i]?;
            if kind(i) == want {
                return Some(i);
            }
        }
        None
    };
    let mut series: Vec<SeriesEntry> = Vec::new();
    let mut index: HashMap<usize, usize> = HashMap::new();
    for i in 0..records.len() {
        let r = &records[i].set;
        let components = r.strings(tags::REFERENCED_FILE_ID);
        if components.is_empty() || kind(i) == "SERIES" || kind(i) == "STUDY" || kind(i) == "PATIENT" {
            continue;
        }
        let Some(file) = referenced_file(&root, &components) else { continue };
        let Some(s) = ancestor(i, "SERIES") else { continue };
        let k = *index.entry(s).or_insert_with(|| {
            let sr = &records[s].set;
            let uid = sr.text(tags::SERIES_INSTANCE).unwrap_or_else(|| format!("record-{s}"));
            let mut e = entry_of(sr, uid);
            if let Some(st) = ancestor(s, "STUDY") {
                let st = &records[st].set;
                e.study_uid = st.text(tags::STUDY_INSTANCE).unwrap_or_default();
                e.study_description = st.text(tags::STUDY_DESCRIPTION).unwrap_or_default();
                e.study_date = st.text(tags::STUDY_DATE).unwrap_or_default();
            }
            if let Some(p) = ancestor(s, "PATIENT") {
                e.patient = records[p].set.text(tags::PATIENT_NAME).unwrap_or_default();
            }
            series.push(e);
            series.len() - 1
        });
        series[k].images += r.int(tags::NUMBER_OF_FRAMES).unwrap_or(1).max(1) as usize;
        series[k].files.push(file);
    }
    if series.is_empty() {
        bail!("not_found", "None of the images this DICOMDIR lists could be found");
    }
    let mut catalog = Catalog { origin: path.to_path_buf(), dicomdir: true, series, incomplete: false };
    catalog.sort();
    Ok(catalog)
}
