//! What the quick look and the interactive viewer draw alike: a view's
//! box (title, orientation letters, image), the panel's sections as text,
//! the list of labels.

use gnome_nifti_preview::orientation::ViewPlan;
use gnome_nifti_preview::panel::{self, Section};
use gnome_nifti_preview::view::LabelRow;

use crate::blocks;
use crate::caps::{Caps, Graphics};
use crate::color::Color;
use crate::layout::ViewBox;
use crate::picture::Picture;
use crate::screen::{Cell, Glyph, Rect, Screen, Style};
use crate::text;

/// Largest side of a picture the terminal scales itself (Kitty, iTerm2).
pub const MAX_PICTURE_SIDE: usize = 2048;
/// Largest side of a Sixel picture, drawn pixel for pixel to fill its cells.
pub const MAX_SIXEL_SIDE: usize = 8192;

/// The pixels of a picture filling `image` with this rendering, for a slice
/// of `slice` pixels (w, h). Kitty and iTerm2 scale the picture to the cells
/// themselves: it is sent at most twice the slice's size, so that bytes go to
/// detail the slice has. Sixel is drawn as it is: it fills the cells exactly.
pub fn picture_size(caps: &Caps, image: Rect, slice: (usize, usize)) -> (usize, usize) {
    let fit = |w: usize, h: usize, max_w: usize, max_h: usize| {
        let f = (max_w as f64 / w as f64).min(max_h as f64 / h as f64).min(1.0);
        (((w as f64 * f).round() as usize).max(1), ((h as f64 * f).round() as usize).max(1))
    };
    let (w, h) = (image.w * caps.cell_px.0 as usize, image.h * caps.cell_px.1 as usize);
    match caps.graphics {
        Graphics::Blocks => blocks::half_block_pixels(image),
        Graphics::Ascii => (image.w, image.h),
        Graphics::Sixel => fit(w, h, MAX_SIXEL_SIDE, MAX_SIXEL_SIDE),
        Graphics::Kitty | Graphics::Iterm2 => {
            let (sw, sh) = (slice.0.max(1) * 2, slice.1.max(1) * 2);
            fit(w, h, sw.clamp(16, MAX_PICTURE_SIDE), sh.clamp(16, MAX_PICTURE_SIDE))
        }
    }
}

/// The crosshair of a view drawn with an image protocol: the cells of the
/// margins in line with the cursor, in the crosshair colour (the picture
/// stays the same while the cursor moves in its plane, and is not sent
/// again). `(col, row)` is the cursor's pixel in the slice of `plan`.
pub fn margin_marks(screen: &mut Screen, image: Rect, plan: &ViewPlan, (col, row): (usize, usize), depth: crate::color::Depth) {
    let x = image.x + ((col as f64 + 0.5) * image.w as f64 / plan.width as f64) as usize;
    let y = image.y + ((row as f64 + 0.5) * image.h as f64 / plan.height as f64) as usize;
    let x = x.min(image.x + image.w - 1);
    let y = y.min(image.y + image.h - 1);
    let mark = depth.reduce(crate::picture::CROSSHAIR);
    for (cx, cy) in [(x, image.y - 1), (x, image.y + image.h), (image.x - 1, y), (image.x + image.w, y)] {
        if cx < screen.w && cy < screen.h {
            let mut cell = screen.get(cx, cy);
            cell.style = Style { fg: Color::Rgb(0, 0, 0), bg: mark, bold: true, ..Style::default() };
            if cell.glyph == Glyph::Image || cell.glyph == Glyph::Continuation {
                cell.glyph = Glyph::Char(' ');
            }
            screen.set(cx, cy, cell);
        }
    }
}

/// The title of a view: "Axial 96/191".
pub fn title(plan: &ViewPlan, index: usize) -> String {
    format!("{} {index}/{}", plan.title, plan.n_slices.saturating_sub(1))
}

/// Title and orientation letters of a view (letters only when known).
pub fn view_frame(screen: &mut Screen, b: &ViewBox, plan: &ViewPlan, index: usize, active: bool) {
    let style = if active { Style::default().bold().reverse() } else { Style::default().bold() };
    let t = format!(" {} ", title(plan, index));
    screen.put(b.frame.x, b.frame.y, &t, style, b.frame.w);
    let letter = Style::fg(Color::Rgb(0x61, 0xad, 0xff)).bold();
    let i = b.image;
    let mid_x = i.x + i.w / 2;
    let mid_y = i.y + i.h / 2;
    let known = |l: &str| !l.is_empty() && l != "?";
    if known(plan.top) {
        screen.put(mid_x, i.y - 1, plan.top, letter, 1);
    }
    if known(plan.bottom) {
        screen.put(mid_x, i.y + i.h, plan.bottom, letter, 1);
    }
    if known(plan.left) {
        screen.put(i.x - 1, mid_y, plan.left, letter, 1);
    }
    if known(plan.right) {
        screen.put(i.x + i.w, mid_y, plan.right, letter, 1);
    }
}

/// Draw a picture into the image cells of a box with text cells, or mark
/// those cells as covered by an image the protocol draws.
pub fn view_cells(screen: &mut Screen, caps: &Caps, image: Rect, pic: &Picture) {
    match caps.graphics {
        Graphics::Blocks => blocks::half_blocks(pic, caps.depth, screen, image),
        Graphics::Ascii => blocks::ascii(pic, screen, image),
        _ => screen.fill(image, Cell { glyph: Glyph::Image, style: Style::default() }),
    }
}

/// The panel's sections as lines of text, keys aligned per section.
pub fn section_lines(sections: &[Section]) -> Vec<(String, Style)> {
    let mut lines = Vec::new();
    for s in sections {
        lines.push((s.title.clone(), Style::default().bold()));
        let key_w = s.rows.iter().map(|(k, _)| text::str_width(k)).max().unwrap_or(0);
        for (k, v) in &s.rows {
            let pad = " ".repeat(key_w - text::str_width(k));
            lines.push((format!("  {k}{pad}  {}", text::clean(v)), Style::default()));
        }
        if let Some(t) = &s.text {
            for l in t.lines() {
                lines.push((format!("  {}", text::clean(l)), Style::default()));
            }
        }
    }
    lines
}

/// One label: a swatch in its colour, its value, name and volume.
pub fn label_line(screen: &mut Screen, x: usize, y: usize, width: usize, row: &LabelRow, depth: crate::color::Depth, highlight: bool) {
    let [r, g, b] = row.rgb;
    let swatch = Style::fg(depth.reduce([r, g, b]));
    screen.put(x, y, "■", swatch, 1);
    let (_, volume) = panel::label_line(row);
    let value = panel::format_value(&row.value.to_label_json(), 6);
    let name = row.name.clone().unwrap_or_default();
    let style = if highlight { Style::default().reverse() } else { Style::default() };
    let vol_w = text::str_width(&volume);
    let left = format!(" {value:>5}  {}", text::clean(&name));
    let room = width.saturating_sub(2 + vol_w);
    let used = screen.put(x + 1, y, &text::fit(&left, room), style, room);
    if highlight {
        screen.put(x + 1 + used, y, &" ".repeat(room - used), style, room - used);
    }
    screen.put(x + width.saturating_sub(vol_w), y, &volume, Style::default().dim(), vol_w);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Depth;

    fn caps(g: Graphics) -> Caps {
        Caps { graphics: g, depth: Depth::TrueColor, cell_px: (10, 20), tmux: false, registers: 256 }
    }

    #[test]
    fn picture_sizes() {
        let image = Rect { x: 1, y: 2, w: 60, h: 30 };
        // Text cells: two pixels per cell, or one.
        assert_eq!(picture_size(&caps(Graphics::Blocks), image, (256, 256)), (60, 60));
        assert_eq!(picture_size(&caps(Graphics::Ascii), image, (256, 256)), (60, 30));
        // Sixel fills its cells exactly, whatever the slice.
        assert_eq!(picture_size(&caps(Graphics::Sixel), image, (64, 64)), (600, 600));
        // Kitty and iTerm2 scale it themselves: at most twice the slice.
        assert_eq!(picture_size(&caps(Graphics::Kitty), image, (64, 64)), (128, 128));
        assert_eq!(picture_size(&caps(Graphics::Iterm2), image, (1000, 1000)), (600, 600));
    }
}
