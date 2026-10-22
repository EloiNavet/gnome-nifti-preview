//! `nifti-preview`: the GNOME NIfTI Preview backend binary.
//!
//!   nifti-preview serve                      protocol on stdin/stdout (used by the plugin)
//!   nifti-preview info FILE [--json] [--mode M] [--data]
//!   nifti-preview render FILE -o OUT.png [--view V|all] [--index N] [--mode M] [--colormap C]
//!   nifti-preview bench FILE...              per-stage timings (JSON lines)
//!   nifti-preview --version

use std::io::{BufReader, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use gnome_nifti_preview::error::PreviewError;
use gnome_nifti_preview::orientation::VIEWS;
use gnome_nifti_preview::render::render_slice;
use gnome_nifti_preview::volume::{self, Volume};
use gnome_nifti_preview::{sandbox, server};
use serde_json::json;

const USAGE: &str = "usage: nifti-preview serve | info FILE [--json] [--data] [--mode M] | \
render FILE -o OUT.png [--view V|all] [--index N] [--mode M] [--colormap C] | bench FILE... | --version";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(raw: &[String], with_value: &[&str]) -> Result<Args, String> {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut it = raw.iter();
        while let Some(a) = it.next() {
            if a.starts_with('-') && a.len() > 1 {
                if with_value.contains(&a.as_str()) {
                    let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
                    flags.push((a.clone(), Some(v.clone())));
                } else {
                    flags.push((a.clone(), None));
                }
            } else {
                positional.push(a.clone());
            }
        }
        Ok(Args { positional, flags })
    }
    fn has(&self, f: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == f)
    }
    fn get(&self, f: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(k, _)| k == f).and_then(|(_, v)| v.as_deref())
    }
}

fn fail(e: PreviewError) -> ExitCode {
    eprintln!("error [{}]: {}", e.code, e.message);
    ExitCode::from(2)
}

fn cmd_info(args: &Args) -> ExitCode {
    let Some(file) = args.positional.first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    };
    let mode = args.get("--mode").unwrap_or("neurological");
    let mut vol = match Volume::open(Path::new(file)) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let mut info = vol.header_info();
    let mode = gnome_nifti_preview::orientation::effective_mode(mode, vol.orientation_known, &vol.affine);
    match vol.plans(mode) {
        Ok(plans) => {
            info["plans"] = json!({
                plans[0].view: plans[0].to_json(), plans[1].view: plans[1].to_json(), plans[2].view: plans[2].to_json()
            })
        }
        Err(e) => return fail(e),
    }
    if args.has("--data") {
        match vol.stats() {
            Ok(s) => {
                info["stats"] = json!({
                    "window": {"low": s.window.low, "high": s.window.high, "status": s.window.status.as_str()},
                    "labels": {"is_labels": s.labels.is_labels, "binary": s.labels.binary, "n_labels": s.labels.n_labels},
                    "non_finite": s.non_finite, "min": s.min, "max": s.max, "notes": s.notes,
                })
            }
            Err(e) => return fail(e),
        }
    }
    if args.has("--json") {
        println!("{}", serde_json::to_string_pretty(&info).unwrap());
        return ExitCode::SUCCESS;
    }
    println!(
        "{}  ({}, {}, {} bytes)",
        info["name"].as_str().unwrap_or(""),
        info["format"].as_str().unwrap_or(""),
        if vol.compressed { "gzip" } else { "uncompressed" },
        vol.file_size
    );
    println!("  shape        {:?}   volumes: {}", vol.header.shape, info["n_volumes"]);
    println!("  spacing      {:?} {}", &vol.header.zooms()[..vol.header.zooms().len().min(3)], vol.header.space_units());
    println!("  datatype     {} ({}), slope {}, inter {}", info["datatype"].as_str().unwrap_or(""), info["dtype"].as_str().unwrap_or(""), vol.header.slope, vol.header.inter);
    println!("  orientation  {}  (affine from {})", info["orientation"].as_str().unwrap_or(""), vol.header.affine_source());
    for row in vol.affine.iter() {
        println!("               {}", row.iter().map(|x| format!("{x:10.4}")).collect::<Vec<_>>().join("  "));
    }
    for view in VIEWS {
        let p = &info["plans"][view];
        let l = &p["labels"];
        println!(
            "  {:<9}    {}x{}, {} slices; left {} right {} top {} bottom {}",
            view, p["width"], p["height"], p["n_slices"], l["left"].as_str().unwrap_or(""), l["right"].as_str().unwrap_or(""),
            l["top"].as_str().unwrap_or(""), l["bottom"].as_str().unwrap_or("")
        );
    }
    if let Some(s) = info.get("stats") {
        println!("  window       [{}, {}] ({})", s["window"]["low"], s["window"]["high"], s["window"]["status"].as_str().unwrap_or(""));
        println!("  labels       {}", s["labels"]);
    }
    for w in info["warnings"].as_array().into_iter().flatten().chain(info["header_fixes"].as_array().into_iter().flatten()) {
        println!("  warning      {}", w.as_str().unwrap_or(""));
    }
    ExitCode::SUCCESS
}

fn cmd_render(args: &Args) -> ExitCode {
    let (Some(file), Some(out)) = (args.positional.first(), args.get("-o").or(args.get("--output"))) else {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    };
    let mode = args.get("--mode").unwrap_or("neurological");
    let colormap = args.get("--colormap").unwrap_or("auto");
    let view = args.get("--view").unwrap_or("all");
    if view != "all" && !VIEWS.contains(&view) {
        eprintln!("--view must be coronal, sagittal, axial or all");
        return ExitCode::from(64);
    }
    let index: Option<usize> = match args.get("--index").map(|s| s.parse()) {
        Some(Ok(i)) => Some(i),
        Some(Err(_)) => {
            eprintln!("--index must be an integer");
            return ExitCode::from(64);
        }
        None => None,
    };
    let mut vol = match Volume::open(Path::new(file)) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let mode = gnome_nifti_preview::orientation::effective_mode(mode, vol.orientation_known, &vol.affine);
    let plans = match vol.plans(mode) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let mut scratch = Vec::new();
    for plan in plans.iter().filter(|p| view == "all" || p.view == view) {
        let idx = index.unwrap_or(plan.n_slices / 2);
        let (rgb, cmap) = match render_slice(&mut vol, plan, idx, colormap, &mut scratch) {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        let path = if view == "all" {
            let p = Path::new(out);
            let stem = p.with_extension("");
            format!("{}-{}.png", stem.display(), plan.view)
        } else {
            out.to_string()
        };
        if let Err(e) = std::fs::write(&path, gnome_nifti_preview::png::encode_rgb(plan.width, plan.height, &rgb)) {
            eprintln!("cannot write {path}: {e}");
            return ExitCode::from(1);
        }
        println!(
            "{path}: {} slice {idx}/{} {}x{} [{cmap}] left={} right={} top={} bottom={}",
            plan.view,
            plan.n_slices - 1,
            plan.width,
            plan.height,
            plan.left,
            plan.right,
            plan.top,
            plan.bottom
        );
    }
    ExitCode::SUCCESS
}

fn cmd_bench(args: &Args) -> ExitCode {
    for file in &args.positional {
        let t0 = Instant::now();
        let mut vol = match Volume::open(Path::new(file)) {
            Ok(v) => v,
            Err(e) => {
                println!("{}", json!({"file": file, "error": e.code}));
                continue;
            }
        };
        let header_ms = t0.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        if let Err(e) = vol.ensure_data() {
            println!("{}", json!({"file": file, "error": e.code}));
            continue;
        }
        let load_ms = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let _ = vol.stats();
        let stats_ms = t.elapsed().as_secs_f64() * 1e3;
        let plans = vol.plans("neurological").unwrap();
        let mut scratch = Vec::new();
        let t = Instant::now();
        for p in plans.iter() {
            let _ = render_slice(&mut vol, p, p.n_slices / 2, "auto", &mut scratch);
        }
        let first_ms = t.elapsed().as_secs_f64() * 1e3;
        let ready_ms = t0.elapsed().as_secs_f64() * 1e3;
        let n = 300;
        let t = Instant::now();
        let mut x = 0x9e3779b97f4a7c15u64;
        for i in 0..n {
            let p = &plans[i % 3];
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let _ = render_slice(&mut vol, p, (x as usize) % p.n_slices, "auto", &mut scratch);
        }
        let slice_ms = t.elapsed().as_secs_f64() * 1e3 / n as f64;
        println!(
            "{}",
            json!({"file": file, "header_ms": header_ms, "decompress_ms": load_ms, "stats_ms": stats_ms,
                   "first_3_slices_ms": first_ms, "ready_ms": ready_ms, "slice_mean_ms": slice_ms,
                   "peak_rss_mb": peak_rss_mb()})
        );
    }
    ExitCode::SUCCESS
}

fn peak_rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<f64>().ok())
        })
        .map(|kb| kb / 1024.0)
        .unwrap_or(f64::NAN)
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = raw.first().cloned() else {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    };
    let rest = &raw[1..];
    let args = match Args::parse(rest, &["-o", "--output", "--mode", "--colormap", "--view", "--index"]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(64);
        }
    };
    match cmd.as_str() {
        "serve" => {
            sandbox::close_inherited_descriptors();
            server::die_with_parent();
            let limit = sandbox::data_limit(volume::max_bytes_from_env(), server::cache_bytes_from_env() as u64);
            let report = sandbox::apply(limit);
            if std::env::var_os("NIFTI_PREVIEW_DEBUG").is_some() {
                let _ = writeln!(std::io::stderr(), "nifti-preview: sandbox {report:?}");
            }
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            let input = BufReader::with_capacity(1 << 16, stdin.lock());
            let output = std::io::BufWriter::with_capacity(1 << 20, stdout.lock());
            match server::serve(input, output) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                Err(e) => {
                    let _ = writeln!(std::io::stderr(), "nifti-preview: {e}");
                    ExitCode::from(1)
                }
            }
        }
        "info" => cmd_info(&args),
        "render" => cmd_render(&args),
        "bench" => cmd_bench(&args),
        "--version" | "-V" => {
            println!("nifti-preview {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "--help" | "-h" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command {other:?}\n{USAGE}");
            ExitCode::from(64)
        }
    }
}
