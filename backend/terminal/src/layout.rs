//! Where the views go: each view is a box of a title row, a row of
//! orientation letters above and below its image and a column of letters
//! on each side. The views share one scale (the same millimetres per cell),
//! in a row, a column or two rows, whichever shows them largest.

use crate::screen::Rect;

/// Rows and columns of a box around its image.
pub const BOX_ROWS: usize = 3;
pub const BOX_COLS: usize = 2;
/// Columns or rows between two boxes.
pub const GAP: usize = 1;

/// A view on the screen: its box and the cells of its image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewBox {
    /// Index of the view in the list given to `arrange`.
    pub view: usize,
    pub frame: Rect,
    pub image: Rect,
}

/// The physical size of a view (mm, any unit as long as all views share it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Extent {
    pub w: f64,
    pub h: f64,
}

/// Arrange views of `extents` in `area`. `cell_aspect` is the width of a
/// cell over its height (about 0.5); `rows_of` lists the arrangements tried
/// (rows of view indices). None when not even one cell per view fits.
pub fn arrange_in(area: Rect, extents: &[Extent], cell_aspect: f64, arrangements: &[Vec<Vec<usize>>]) -> Option<Vec<ViewBox>> {
    let mut best: Option<(f64, Vec<ViewBox>)> = None;
    for rows in arrangements {
        if let Some(boxes) = place(area, extents, cell_aspect, rows) {
            let shown: f64 = boxes.iter().map(|b| (b.image.w * b.image.h) as f64).sum();
            if best.as_ref().is_none_or(|(a, _)| shown > *a) {
                best = Some((shown, boxes));
            }
        }
    }
    best.map(|(_, b)| b)
}

/// The arrangements worth trying for `n` views.
pub fn arrangements(n: usize) -> Vec<Vec<Vec<usize>>> {
    match n {
        0 => Vec::new(),
        1 => vec![vec![vec![0]]],
        2 => vec![vec![vec![0, 1]], vec![vec![0], vec![1]]],
        _ => vec![vec![vec![0, 1, 2]], vec![vec![0, 1], vec![2]], vec![vec![0], vec![1], vec![2]]],
    }
}

/// The views in one row (the quick look).
pub fn row(n: usize) -> Vec<Vec<Vec<usize>>> {
    vec![vec![(0..n).collect()]]
}

fn place(area: Rect, extents: &[Extent], cell_aspect: f64, rows: &[Vec<usize>]) -> Option<Vec<ViewBox>> {
    let n_rows = rows.len();
    let fixed_h = BOX_ROWS * n_rows + GAP * n_rows.saturating_sub(1);
    let avail_h = area.h.checked_sub(fixed_h)? as f64;
    // Cells per mm across: limited by the widest row, then by the height.
    let mut k = f64::INFINITY;
    for r in rows {
        let fixed_w = BOX_COLS * r.len() + GAP * r.len().saturating_sub(1);
        let avail_w = area.w.checked_sub(fixed_w)? as f64;
        let sum: f64 = r.iter().map(|&v| extents[v].w).sum();
        k = k.min(avail_w / sum);
    }
    let tallest: f64 = rows.iter().map(|r| r.iter().map(|&v| extents[v].h).fold(0.0, f64::max)).sum();
    k = k.min(avail_h / (tallest * cell_aspect));
    if !(k.is_finite() && k > 0.0) {
        return None;
    }
    // At least one cell each (a line of voxels keeps one row).
    let size = |v: usize| {
        let w = ((extents[v].w * k + 1e-9).floor() as usize).max(1);
        let h = ((extents[v].h * k * cell_aspect + 1e-9).floor() as usize).max(1);
        (w, h)
    };
    let total_h: usize = rows.iter().map(|r| r.iter().map(|&v| size(v).1).max().unwrap_or(0)).sum::<usize>() + fixed_h;
    let too_wide = rows.iter().any(|r| r.iter().map(|&v| size(v).0 + BOX_COLS).sum::<usize>() + GAP * r.len().saturating_sub(1) > area.w);
    if total_h > area.h || too_wide {
        return None;
    }
    let mut boxes = Vec::new();
    let mut y = area.y;
    for r in rows {
        let row_h = r.iter().map(|&v| size(v).1).max().unwrap_or(0) + BOX_ROWS;
        let row_w: usize = r.iter().map(|&v| size(v).0 + BOX_COLS).sum::<usize>() + GAP * r.len().saturating_sub(1);
        let mut x = area.x + (area.w - row_w) / 2;
        for &v in r {
            let (w, h) = size(v);
            let frame = Rect { x, y, w: w + BOX_COLS, h: row_h };
            boxes.push(ViewBox { view: v, frame, image: Rect { x: x + 1, y: y + 2, w, h } });
            x += w + BOX_COLS + GAP;
        }
        y += row_h + GAP;
    }
    boxes.sort_by_key(|b| b.view);
    Some(boxes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: usize, h: usize) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }

    const BRAIN: [Extent; 3] = [Extent { w: 180.0, h: 150.0 }, Extent { w: 220.0, h: 150.0 }, Extent { w: 180.0, h: 220.0 }];

    #[test]
    fn same_scale_and_aspect() {
        let boxes = arrange_in(area(200, 60), &BRAIN, 0.5, &arrangements(3)).unwrap();
        assert_eq!(boxes.len(), 3);
        // One scale: widths follow the physical widths, heights the heights.
        let ratio = |b: &ViewBox, e: &Extent| b.image.w as f64 / e.w;
        let r0 = ratio(&boxes[0], &BRAIN[0]);
        for (b, e) in boxes.iter().zip(&BRAIN) {
            assert!((ratio(b, e) - r0).abs() / r0 < 0.03, "{boxes:?}");
            // A cell is twice as tall as wide: rows are half the columns per mm.
            let aspect = b.image.w as f64 / (2.0 * b.image.h as f64);
            assert!((aspect - e.w / e.h).abs() < 0.1 * e.w / e.h + 0.1, "{b:?} {e:?}");
        }
        // Everything inside the area, boxes apart.
        for b in &boxes {
            assert!(b.frame.x + b.frame.w <= 200 && b.frame.y + b.frame.h <= 60);
            assert!(b.frame.contains(b.image.x, b.image.y));
        }
    }

    #[test]
    fn narrow_terminals_stack_views() {
        let boxes = arrange_in(area(60, 80), &BRAIN, 0.5, &arrangements(3)).unwrap();
        assert!(boxes.iter().any(|b| b.frame.y > 0), "not all in one row: {boxes:?}");
        let row = arrange_in(area(60, 80), &BRAIN, 0.5, &row(3)).unwrap();
        assert!(row.iter().all(|b| b.frame.y == 0));
    }

    #[test]
    fn tiny_and_degenerate() {
        assert!(arrange_in(area(1, 1), &BRAIN, 0.5, &arrangements(3)).is_none());
        assert!(arrange_in(area(10, 5), &BRAIN, 0.5, &arrangements(3)).is_none());
        assert!(arrange_in(area(0, 0), &BRAIN, 0.5, &arrangements(3)).is_none());
        let one = arrange_in(area(12, 6), &BRAIN[..1], 0.5, &arrangements(1)).unwrap();
        assert!(one[0].image.w >= 1 && one[0].image.h >= 1);
        // A line of voxels still gets one row.
        let line = [Extent { w: 500.0, h: 1.0 }];
        let b = arrange_in(area(100, 30), &line, 0.5, &arrangements(1)).unwrap();
        assert_eq!((b[0].image.w, b[0].image.h), (98, 1));
        // Huge terminals are fine.
        let big = arrange_in(area(500, 200), &BRAIN, 0.5, &arrangements(3)).unwrap();
        assert!(big.iter().all(|b| b.frame.x + b.frame.w <= 500 && b.frame.y + b.frame.h <= 200));
    }
}
