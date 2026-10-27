//! Label colour tables: FreeSurfer's (embedded, `freesurfer_lut.tsv`) or one
//! given by the user (a file in FreeSurferColorLUT.txt format), with names
//! for the readout and the list of structures.
//!
//! A segmentation is shown with FreeSurfer's colours when it looks like a
//! FreeSurfer parcellation (MIQ's rule): at least 3 labels, at least half of
//! them in the table, and one of the structures every whole-brain
//! segmentation has and a naive 1, 2, 3... labelling never reaches
//! (right-hemisphere structures 41–54, corpus callosum 251–255, cortical
//! parcels from 1000). A tissue map 1 = CSF, 2 = GM, 3 = WM does not.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::error::{bail, Result};
use crate::header::clean_text;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub rgb: [u8; 3],
}

pub type Table = BTreeMap<i128, Entry>;

/// Lines of the table: `label<TAB>name<TAB>red<TAB>green<TAB>blue`.
const FREESURFER_TSV: &str = include_str!("freesurfer_lut.tsv");

pub fn freesurfer() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| parse(FREESURFER_TSV).expect("embedded FreeSurfer table"))
}

/// A FreeSurferColorLUT-style text: `label name r g b [a]` per line (tabs or
/// spaces), `#` comments. Unparsable lines are skipped; no entry at all is an
/// error.
pub fn parse(text: &str) -> Result<Table> {
    let mut table = Table::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        let (Ok(label), Ok(r), Ok(g), Ok(b)) = (fields[0].parse::<i128>(), fields[2].parse::<u8>(), fields[3].parse::<u8>(), fields[4].parse::<u8>()) else {
            continue;
        };
        if table.len() >= MAX_ENTRIES {
            bail!("bad_request", "Label table with more than {MAX_ENTRIES} entries");
        }
        table.insert(label, Entry { name: clean_text(fields[1]).chars().take(64).collect(), rgb: [r, g, b] });
    }
    if table.is_empty() {
        bail!("bad_request", "No label in the colour table");
    }
    Ok(table)
}

/// Entries of a user table, at most.
pub const MAX_ENTRIES: usize = 1 << 16;
/// Largest user table file read.
pub const MAX_FILE_BYTES: u64 = 4 << 20;

/// A user table file (read only; regular file of at most 4 MB).
pub fn read_file(path: &std::path::Path) -> Result<Table> {
    let file = crate::source::open_data_file(path)?;
    let size = file.metadata().map_err(crate::source::io_error)?.len();
    if size > MAX_FILE_BYTES {
        bail!("bad_request", "Label table {} is larger than 4 MB", path.display());
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::BufReader::new(file), &mut text)
        .map_err(|e| crate::error::PreviewError::new("bad_request", format!("Label table {}: {e}", path.display())))?;
    parse(&text)
}

fn signature(label: i128) -> bool {
    (41..=54).contains(&label) || (251..=255).contains(&label) || label >= 1000
}

/// Whether the labels present (0 excluded) look like a FreeSurfer parcellation.
pub fn looks_like_freesurfer(labels: &[i128]) -> bool {
    let table = freesurfer();
    let present: Vec<i128> = labels.iter().copied().filter(|&l| l != 0).collect();
    let known = present.iter().filter(|l| table.contains_key(l)).count();
    let marked = present.iter().any(|&l| table.contains_key(&l) && signature(l));
    present.len() >= 3 && known * 2 >= present.len() && marked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_table() {
        let t = freesurfer();
        assert!(t.len() > 300);
        assert_eq!(t[&17], Entry { name: "Left-Hippocampus".into(), rgb: [220, 216, 20] });
        assert_eq!(t[&2035].name, "ctx-rh-insula");
        assert_eq!(t[&12175].name, "ctx_rh_S_temporal_transverse");
    }

    #[test]
    fn detection_rule() {
        // aseg: subcortical structures of both hemispheres.
        assert!(looks_like_freesurfer(&[0, 2, 3, 4, 17, 41, 42, 53]));
        // aparc+aseg: cortical parcels.
        assert!(looks_like_freesurfer(&[2, 41, 1001, 1002, 2001]));
        // A tissue map, a mask, arbitrary labels.
        assert!(!looks_like_freesurfer(&[0, 1, 2, 3]));
        assert!(!looks_like_freesurfer(&[1]));
        assert!(!looks_like_freesurfer(&[100, 200, 300, 400, 500, 41]));
    }

    #[test]
    fn user_tables() {
        let t = parse("# my LUT\n0  Unknown 0 0 0 0\n1\tLesion\t255 0 0 0\nbad line\n7 Tumour 0 255 0\n").unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t[&1].name, "Lesion");
        assert!(parse("nothing here\n").is_err());
    }
}
