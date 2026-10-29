//! The quick look: one line about the image, its views side by side (with
//! their titles and orientation letters), optionally the metadata and the
//! labels, printed once. With text cells it is plain lines that scroll with
//! the shell; with an image protocol the rows are printed first, then each
//! picture is drawn into them.

use std::path::Path;

use gnome_nifti_preview::error::Result;
use gnome_nifti_preview::panel;
use gnome_nifti_preview::thumbnail;
use gnome_nifti_preview::view;
use serde_json::{json, Value};

use crate::args::Options;
use crate::caps::Caps;
use crate::draw;
use crate::graphics;
use crate::layout::{self, Extent};
use crate::picture::{self, Picture};
use crate::screen::{Rect, Screen, Style};
use crate::text;
use crate::viewer::{self, Viewer};

/// Rows a quick look may use when the output is not a terminal.
const UNBOUNDED_ROWS: usize = 10_000;

/// Print the quick look of `path` into `out`. `cols` is the width to fill,
/// `rows` the height of the terminal (None when the output is not one).
pub fn run(o: &Options, path: &Path, caps: &Caps, cols: usize, rows: Option<usize>, out: &mut Vec<u8>) -> Result<()> {
    let mut v = viewer::open(path, o)?;
    v.load()?;
    if let Some(k) = o.volume {
        v.show_volume(k)?;
    }
    v.place_cursor(o)?;
    if o.panel_json {
        return panel_json(&mut v, o, out);
    }
    let mut head = Screen::new(cols, 1);
    let summary = format!("{}  {}", v.name(), v.summary()?);
    head.put(0, 0, &text::fit(&summary, cols), Style::default().bold(), cols);
    head.lines(caps.depth, out);

    // A single-voxel spectrum has no views: its plot.
    if v.vol.mrs.is_some() && v.vol.shape3 == [1, 1, 1] {
        let opts = thumbnail::Options { size: (cols * caps.cell_px.0 as usize).clamp(thumbnail::MIN_SIZE, 512), ..Default::default() };
        let pic = thumbnail::render(&mut v.vol, &opts)?;
        let pic = Picture { w: pic.width, h: pic.height, rgb: pic.rgb };
        let extent = [Extent { w: pic.w as f64, h: pic.h as f64 }];
        let area = Rect { x: 0, y: 0, w: cols, h: rows.map_or(UNBOUNDED_ROWS, |r| r.saturating_sub(3)) };
        if let Some(b) = layout::arrange_in(area, &extent, caps.cell_aspect(), &layout::row(1)) {
            let (w, h) = draw::picture_size(caps, b[0].image, (pic.w, pic.h));
            let scaled = Picture { w, h, rgb: thumbnail::resample(&pic.rgb, pic.w, pic.h, w, h, false)? };
            let mut s = Screen::new(cols, b[0].frame.h);
            s.put(b[0].frame.x, 0, " Spectrum ", Style::default().bold(), b[0].frame.w);
            let image = Rect { y: b[0].image.y, ..b[0].image };
            draw::view_cells(&mut s, caps, image, &scaled);
            print_screen(&s, &[(image, scaled)], caps, out);
        }
    } else {
        let views = v.views(o.view);
        let extents: Vec<Extent> = views.iter().map(|&i| {
            let p = &v.plans[i];
            let a = picture::physical_aspect(p);
            Extent { w: a, h: 1.0 }
        }).collect();
        // Same physical scale across views: use real extents when known.
        let extents = scaled_extents(&v, &views).unwrap_or(extents);
        let area = Rect { x: 0, y: 0, w: cols, h: rows.map_or(UNBOUNDED_ROWS, |r| r.saturating_sub(3)) };
        match layout::arrange_in(area, &extents, caps.cell_aspect(), &layout::row(views.len())) {
            None => match rows {
                Some(r) => eprintln!("nitty: {}: a terminal of {cols} × {r} cells is too small for the images", v.name()),
                None => eprintln!("nitty: {}: {cols} columns are too narrow for the images", v.name()),
            },
            Some(boxes) => {
                let height = boxes.iter().map(|b| b.frame.y + b.frame.h).max().unwrap_or(0);
                let mut s = Screen::new(cols, height);
                let mut images = Vec::new();
                for b in &boxes {
                    let i = views[b.view];
                    let plan = v.plans[i].clone();
                    draw::view_frame(&mut s, b, &plan, v.index(i), false);
                    let (w, h) = draw::picture_size(caps, b.image, (plan.width, plan.height));
                    let pic = v.picture(i, w, h, false)?;
                    draw::view_cells(&mut s, caps, b.image, &pic);
                    images.push((b.image, pic));
                }
                print_screen(&s, &images, caps, out);
            }
        }
    }
    if o.info {
        let mut lines = draw::warning_lines(&v.warnings()?, cols);
        lines.extend(draw::section_lines(&v.sections(o.patient)?));
        let mut s = Screen::new(cols, lines.len());
        for (y, (l, style)) in lines.iter().enumerate() {
            s.put(0, y, l, *style, cols);
        }
        s.lines(caps.depth, out);
    }
    if o.legend {
        let list = view::label_rows(&mut v.vol)?;
        let mut s = Screen::new(cols, list.rows.len() + usize::from(!list.complete));
        for (y, row) in list.rows.iter().enumerate() {
            draw::label_line(&mut s, 0, y, cols.min(60), row, caps.depth, false);
        }
        if !list.complete {
            s.put(0, list.rows.len(), "(more labels than listed)", Style::default().dim(), cols);
        }
        s.lines(caps.depth, out);
    }
    Ok(())
}

/// The physical extents of the views (mm), so that they share one scale.
fn scaled_extents(v: &Viewer, views: &[usize]) -> Option<Vec<Extent>> {
    let size = |x: f64| if x.is_finite() && x > 0.0 { Some(x) } else { None };
    views
        .iter()
        .map(|&i| {
            let p = &v.plans[i];
            Some(Extent { w: p.width as f64 * size(p.pixel_w)?, h: p.height as f64 * size(p.pixel_h)? })
        })
        .collect()
}

/// Print a screen; with an image protocol, then draw each picture into its
/// cells (the cursor goes back up to them and down again at the end).
fn print_screen(s: &Screen, images: &[(Rect, Picture)], caps: &Caps, out: &mut Vec<u8>) {
    s.lines(caps.depth, out);
    if !caps.graphics.is_image() {
        return;
    }
    for (r, pic) in images {
        // From the line after the screen: up to the picture's first row.
        let up = s.h - r.y;
        out.extend_from_slice(format!("\x1b[{up}A\r").as_bytes());
        if r.x > 0 {
            out.extend_from_slice(format!("\x1b[{}C", r.x).as_bytes());
        }
        graphics::draw(pic, *r, caps, 0, out);
        out.extend_from_slice(format!("\r\x1b[{up}B").as_bytes());
    }
}

/// The panel, the readout at the cursor and the labels as JSON (compared
/// with the Sushi viewer's own in tests/gjs).
fn panel_json(v: &mut Viewer, o: &Options, out: &mut Vec<u8>) -> Result<()> {
    let sections: Vec<Value> = v
        .sections(o.patient)?
        .iter()
        .map(|s| match &s.text {
            Some(t) => json!({"title": s.title, "text": t}),
            None => json!({"title": s.title, "rows": s.rows.iter().map(|(k, x)| json!([k, x])).collect::<Vec<_>>()}),
        })
        .collect();
    let (cmap, _) = v.shown()?;
    let r = v.readout()?;
    let labels = view::label_rows(&mut v.vol)?;
    let labels: Vec<Value> = labels.rows.iter().map(|row| {
        let (name, volume) = panel::label_line(row);
        json!([name, volume])
    }).collect();
    // The line beside the volume slider, for an image with volumes to browse.
    let volume = v.vol.browsable().then(|| {
        let k = v.vol.current_volume();
        panel::volume_text(&v.header(), k, k, None)
    });
    let doc = json!({
        "cursor": v.cursor, "effective_colormap": cmap, "sections": sections, "warnings": v.warnings()?,
        "readout": panel::readout_text(&r, cmap, true, false), "labels": labels, "volume": volume,
    });
    out.extend_from_slice(serde_json::to_string_pretty(&doc).unwrap_or_default().as_bytes());
    out.push(b'\n');
    Ok(())
}
