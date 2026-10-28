//! Command-line options.

use std::path::PathBuf;

use gnome_nifti_preview::orientation::{MODES, VIEWS};
use gnome_nifti_preview::render::COLORMAPS;
use gnome_nifti_preview::window::PALETTES;

use crate::caps::Graphics;
use crate::color::Depth;

pub const USAGE: &str = "\
usage: nitty [OPTIONS] FILE...

Shows medical images in the terminal: NIfTI, FreeSurfer MGH/MGZ, MRtrix,
NRRD, MetaImage and DICOM (a file, its series, a folder or a DICOMDIR).
Interactive when run in a terminal; prints a quick look with -p, or when the
output is not a terminal.

  -p, --print            print the three views and return
  -i, --interactive      the interactive viewer, even with -p
      --view V           coronal, sagittal, axial or all (default)
      --at I,J,K         cursor at this voxel (default: the centre, or the
                         labels of a segmentation)
      --slice N          slice N of the view chosen with --view
      --volume K         volume K of a 4D image
      --mode M           neurological (default), radiological or stored
      --colormap C       auto (default), gray, labels or rgb
      --palette P        gray (default), hot, inferno or viridis
      --window LOW,HIGH  grey levels from LOW to HIGH
      --percentiles L,H  automatic window between these percentiles (0.5,99.5)
      --labels L         label colours: auto, freesurfer or random
      --lut FILE         label colours and names (FreeSurferColorLUT format)
      --protocol P       auto, kitty, sixel, iterm2, blocks or ascii
      --colors C         auto, truecolor, 256 or 16
      --width COLS       width of the quick look (default: the terminal's)
      --info             also print the metadata
      --legend           also print the labels of a segmentation
      --patient          show who a DICOM image is of
      --no-series        a DICOM file alone, without its series
      --no-mouse         leave the mouse to the terminal
  -h, --help             this help
  -V, --version          the version

Environment: NITTY_PROTOCOL and NITTY_COLORS set the defaults of --protocol
and --colors; NIFTI_PREVIEW_MAX_BYTES bounds the memory of an image.";

/// Options of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub files: Vec<PathBuf>,
    pub print: bool,
    pub interactive: bool,
    /// None: all views (or the one a single-slice image has).
    pub view: Option<&'static str>,
    pub at: Option<[i64; 3]>,
    pub slice: Option<usize>,
    pub volume: Option<usize>,
    pub mode: &'static str,
    pub colormap: &'static str,
    pub palette: &'static str,
    pub window: Option<(f64, f64)>,
    pub percentiles: Option<(f64, f64)>,
    pub labels: &'static str,
    pub lut: Option<PathBuf>,
    pub graphics: Option<Graphics>,
    pub depth: Option<Depth>,
    pub width: Option<usize>,
    pub info: bool,
    pub legend: bool,
    pub patient: bool,
    pub no_series: bool,
    pub no_mouse: bool,
    /// The panel and readout as JSON (for the tests against the Sushi viewer).
    pub panel_json: bool,
    /// Bytes of voxel data an image may take (NIFTI_PREVIEW_MAX_BYTES, read
    /// once by `main`, before the terminal is taken).
    pub max_bytes: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            files: Vec::new(),
            print: false,
            interactive: false,
            view: None,
            at: None,
            slice: None,
            volume: None,
            mode: "neurological",
            colormap: "auto",
            palette: "gray",
            window: None,
            percentiles: None,
            labels: "auto",
            lut: None,
            graphics: None,
            depth: None,
            width: None,
            info: false,
            legend: false,
            patient: false,
            no_series: false,
            no_mouse: false,
            panel_json: false,
            max_bytes: gnome_nifti_preview::volume::DEFAULT_MAX_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Run(Box<Options>),
    Help,
    Version,
}

fn choice(value: &str, choices: &[&'static str], flag: &str) -> Result<&'static str, String> {
    choices.iter().find(|c| **c == value).copied().ok_or_else(|| format!("{flag} must be one of {}", choices.join(", ")))
}

fn pair(value: &str, flag: &str) -> Result<(f64, f64), String> {
    let p = value.split_once(',').and_then(|(a, b)| Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?)));
    match p {
        Some((a, b)) if a.is_finite() && b.is_finite() && a < b => Ok((a, b)),
        _ => Err(format!("{flag} must be LOW,HIGH with LOW < HIGH")),
    }
}

/// Parse the arguments (without the program name); `env` reads the
/// variables that set defaults.
pub fn parse(raw: &[String], env: impl Fn(&str) -> Option<String>) -> Result<Command, String> {
    let mut o = Options::default();
    let graphics = |v: &str, from: &str| -> Result<Option<Graphics>, String> {
        if v == "auto" {
            return Ok(None);
        }
        Graphics::from_name(v).map(Some).ok_or_else(|| format!("{from} must be auto, {}", Graphics::NAMES.join(", ")))
    };
    let depth = |v: &str, from: &str| -> Result<Option<Depth>, String> {
        if v == "auto" {
            return Ok(None);
        }
        Depth::from_name(v).map(Some).ok_or_else(|| format!("{from} must be auto, truecolor, 256 or 16"))
    };
    if let Some(v) = env("NITTY_PROTOCOL").filter(|v| !v.is_empty()) {
        o.graphics = graphics(&v, "NITTY_PROTOCOL")?;
    }
    if let Some(v) = env("NITTY_COLORS").filter(|v| !v.is_empty()) {
        o.depth = depth(&v, "NITTY_COLORS")?;
    }
    let mut it = raw.iter();
    let mut only_files = false;
    while let Some(a) = it.next() {
        if only_files || !a.starts_with('-') || a == "-" {
            o.files.push(PathBuf::from(a));
            continue;
        }
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = || -> Result<String, String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => it.next().cloned().ok_or_else(|| format!("{flag} needs a value")),
            }
        };
        match flag {
            "--" => only_files = true,
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "-p" | "--print" => o.print = true,
            "-i" | "--interactive" => o.interactive = true,
            "--view" => {
                let v = value()?;
                o.view = if v == "all" { None } else { Some(choice(&v, &VIEWS, "--view")?) };
            }
            "--at" => {
                let v = value()?;
                let ijk: Vec<i64> = v.split(',').map(|x| x.trim().parse::<i64>()).collect::<Result<_, _>>().map_err(|_| "--at must be I,J,K".to_string())?;
                let [i, j, k] = ijk[..] else { return Err("--at must be I,J,K".into()) };
                o.at = Some([i, j, k]);
            }
            "--slice" => o.slice = Some(value()?.parse().map_err(|_| "--slice must be a slice number".to_string())?),
            "--volume" => o.volume = Some(value()?.parse().map_err(|_| "--volume must be a volume number".to_string())?),
            "--mode" => o.mode = choice(&value()?, &MODES, "--mode")?,
            "--colormap" => o.colormap = choice(&value()?, &COLORMAPS, "--colormap")?,
            "--palette" => o.palette = choice(&value()?, &PALETTES, "--palette")?,
            "--window" => o.window = Some(pair(&value()?, "--window")?),
            "--percentiles" => {
                let (a, b) = pair(&value()?, "--percentiles")?;
                if a < 0.0 || b > 100.0 {
                    return Err("--percentiles must satisfy 0 <= LOW < HIGH <= 100".into());
                }
                o.percentiles = Some((a, b));
            }
            "--labels" => o.labels = choice(&value()?, &["auto", "freesurfer", "random"], "--labels")?,
            "--lut" => o.lut = Some(PathBuf::from(value()?)),
            "--protocol" => o.graphics = graphics(&value()?, "--protocol")?,
            "--colors" | "--colours" => o.depth = depth(&value()?, "--colors")?,
            "--width" => {
                let w: usize = value()?.parse().map_err(|_| "--width must be a number of columns".to_string())?;
                if !(1..=10_000).contains(&w) {
                    return Err("--width must be 1 to 10000".into());
                }
                o.width = Some(w);
            }
            "--info" => o.info = true,
            "--legend" => o.legend = true,
            "--patient" => o.patient = true,
            "--no-series" => o.no_series = true,
            "--no-mouse" => o.no_mouse = true,
            "--panel-json" => o.panel_json = true,
            other => return Err(format!("unknown option {other}")),
        }
    }
    if o.files.is_empty() {
        return Err("no file given".into());
    }
    if o.slice.is_some() && o.view.is_none() {
        return Err("--slice needs --view".into());
    }
    Ok(Command::Run(Box::new(o)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, String> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), |_| None)
    }

    #[test]
    fn options() {
        let Command::Run(o) = p(&["-p", "--view", "axial", "--slice=12", "--window", "0,100", "a.nii", "--mode", "radiological", "--", "-b.nii"]).unwrap() else { panic!() };
        assert!(o.print);
        assert_eq!((o.view, o.slice, o.window, o.mode), (Some("axial"), Some(12), Some((0.0, 100.0)), "radiological"));
        assert_eq!(o.files, [PathBuf::from("a.nii"), PathBuf::from("-b.nii")]);
        let Command::Run(o) = p(&["--at", "1, 2,3", "--protocol", "sixel", "--colors", "256", "x"]).unwrap() else { panic!() };
        assert_eq!((o.at, o.graphics, o.depth), (Some([1, 2, 3]), Some(Graphics::Sixel), Some(Depth::Ansi256)));
        assert_eq!(p(&["--help", "x"]), Ok(Command::Help));
        assert_eq!(p(&["-V"]), Ok(Command::Version));
    }

    #[test]
    fn mistakes_are_refused() {
        for bad in [
            &["--view", "top", "x"][..], &["--mode"], &["--window", "5,1", "x"], &["--window", "a,b", "x"], &["--at", "1,2", "x"],
            &["--percentiles", "-1,50", "x"], &["--protocol", "png", "x"], &["--width", "0", "x"], &["--slice", "3", "x"],
            &["--frobnicate", "x"], &[],
        ] {
            assert!(p(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn environment_defaults() {
        let env = |k: &str| match k {
            "NITTY_PROTOCOL" => Some("blocks".to_string()),
            "NITTY_COLORS" => Some("16".to_string()),
            _ => None,
        };
        let Command::Run(o) = parse(&["x".to_string()], env).unwrap() else { panic!() };
        assert_eq!((o.graphics, o.depth), (Some(Graphics::Blocks), Some(Depth::Ansi16)));
        let Command::Run(o) = parse(&["--protocol".into(), "auto".into(), "x".into()], env).unwrap() else { panic!() };
        assert_eq!(o.graphics, None);
        assert!(parse(&["x".to_string()], |k| (k == "NITTY_COLORS").then(|| "lots".to_string())).is_err());
    }
}
