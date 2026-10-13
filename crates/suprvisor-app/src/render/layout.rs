//! Layout in DIPs: where each part of the window is. No drawing here, so it is testable.

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

use super::*;
use crate::table::{Col, Row};

/// Navigation pane width: icons only, or icons and labels.
pub fn nav_width(open: bool) -> f32 {
    if open { 200.0 } else { 48.0 }
}

/// Left and right edges of the shown columns, Name first, then the others in `order`. Numeric
/// columns are anchored right; Name takes the rest, from the content layer's left edge `x0`.
pub fn columns(x0: f32, width: f32, widths: &[f32; 6], order: &[Col; 6], hidden: &[bool; 6]) -> Vec<(Col, f32, f32)> {
    let shown: Vec<Col> = order.iter().copied().filter(|c| !c.num().is_some_and(|i| hidden[i])).collect();
    let mut out = vec![(Col::Name, 0.0, 0.0); shown.len() + 1];
    let mut x = width - PAD - BAR;
    for (k, c) in shown.iter().enumerate().rev() {
        let w = widths[c.num().unwrap()];
        out[k + 1] = (*c, x - w, x);
        x -= w;
    }
    out[0] = (Col::Name, x0 + PAD, x.max(x0 + PAD));
    out
}

/// Widths after dragging a separator by `d` DIPs, from `start`. Only that separator moves: the
/// columns on its two sides (`left`, `right`, by `Col::num`; `left` None is Name) trade width.
/// Name keeps at least NAME_MIN of its `name` width.
pub fn resize(start: [f32; 6], left: Option<usize>, right: usize, d: f32, name: f32) -> [f32; 6] {
    const NAME_MIN: f32 = 120.0;
    let mut w = start;
    match left {
        None => w[right] = (start[right] - d).clamp(MIN_WIDTH, start[right].max(MIN_WIDTH) + (name - NAME_MIN).max(0.0)),
        Some(l) => {
            let d = d.clamp(-(start[l] - MIN_WIDTH).max(0.0), (start[right] - MIN_WIDTH).max(0.0));
            w[l] += d;
            w[right] -= d;
        }
    }
    w
}

pub const MIN_WIDTH: f32 = 40.0;

/// Search box left and right edges: centered in the title bar, between the app name and the
/// caption buttons.
pub fn search(width: f32) -> (f32, f32) {
    let w = (width - 140.0 - 3.0 * CAPTION.0 - 16.0).clamp(120.0, 412.0);
    let l = ((width - w) / 2.0).max(140.0);
    (l, l + w)
}

/// Width of the clear button at the search box's right end, shown while there is text.
pub const CLEAR: f32 = 32.0;

/// Caption button under a point: 0 minimize, 1 maximize, 2 close.
pub fn caption_at(x: f32, y: f32, width: f32) -> Option<usize> {
    let x0 = width - 3.0 * CAPTION.0;
    (y < CAPTION.1 && x >= x0).then(|| (((x - x0) / CAPTION.0) as usize).min(2))
}

/// Navigation pane rows: 0 the menu button, 1 Processes, 2 Performance, 3 Windows Task Manager
/// (at the bottom). Top and bottom of row `k` in a window `height` DIPs tall.
pub fn nav_row(k: usize, height: f32) -> (f32, f32) {
    let top = if k == 3 { (height - 4.0 - NAV_ROW).max(NAV_TOP + 3.0 * NAV_ROW) } else { NAV_TOP + k as f32 * NAV_ROW };
    (top, top + NAV_ROW - 4.0)
}

pub const NAV_ITEMS: usize = 4;

/// Navigation pane row under a point.
pub fn nav_at(x: f32, y: f32, nav_w: f32, height: f32) -> Option<usize> {
    if x >= nav_w || y < NAV_TOP {
        return None;
    }
    (0..NAV_ITEMS).find(|&k| {
        let (t, b) = nav_row(k, height);
        (t..b).contains(&y)
    })
}

/// Page header buttons, right to left: (id, rect). 0 Run new task, 1 End task (Processes only).
pub fn commands(width: f32, performance: bool) -> Vec<(usize, D2D_RECT_F)> {
    let end = rect(width - PAD - 108.0, TITLE + 10.0, width - PAD, TITLE + 42.0);
    if performance {
        return vec![(0, rect(width - PAD - 140.0, TITLE + 10.0, width - PAD, TITLE + 42.0))];
    }
    vec![(0, rect(end.left - 8.0 - 140.0, end.top, end.left - 8.0, end.bottom)), (1, end)]
}

/// Search box text area, left and right edges: after the magnifier, before the clear button.
pub fn search_text(width: f32, has_text: bool) -> (f32, f32) {
    let (l, r) = search(width);
    (l + 34.0, r - if has_text { CLEAR + 4.0 } else { 10.0 })
}

/// Rows that fit in a client area `height` DIPs tall.
pub fn rows_visible(height: f32) -> usize {
    ((height - TOP - HEAD) / ROW).floor().max(0.0) as usize
}

/// Top and bottom of the scroll bar thumb; None when every row fits.
pub fn thumb(height: f32, rows: usize, scroll: f32) -> Option<(f32, f32)> {
    let n = rows_visible(height);
    if rows <= n {
        return None;
    }
    let (top, h) = (TOP + HEAD, height - TOP - HEAD);
    let len = (h * n as f32 / rows as f32).max(24.0);
    let y = top + (h - len) * (scroll / (rows - n) as f32).clamp(0.0, 1.0);
    Some((y, y + len))
}

/// Left edge of a row's expand chevron. The icon follows 20 DIPs later, the label 24 after that.
/// Each level indents by one chevron, so a group's rows start under its label.
pub fn chevron_x(row: &Row, name_x0: f32) -> f32 {
    name_x0 + row.depth as f32 * INDENT
}

#[cfg(test)]
mod tests {
    use crate::table::{Col, NUMS};

    #[test]
    fn resize_moves_one_separator() {
        let w = [72.0, 96.0, 64.0, 96.0, 128.0, 64.0];
        // CPU|Memory 20 right: CPU wider, Memory narrower, the rest still.
        assert_eq!(super::resize(w, Some(0), 1, 20.0, 400.0), [92.0, 76.0, 64.0, 96.0, 128.0, 64.0]);
        // Too far: Memory stops at the minimum.
        assert_eq!(super::resize(w, Some(0), 1, 500.0, 400.0)[..2], [128.0, 40.0]);
        // Name|CPU 30 left: CPU wider (Name gives the space), never Name under 120.
        assert_eq!(super::resize(w, None, 0, -30.0, 400.0)[0], 102.0);
        assert_eq!(super::resize(w, None, 0, -1000.0, 400.0)[0], 352.0);
        assert_eq!(super::resize(w, None, 0, 1000.0, 400.0)[0], 40.0);
    }

    #[test]
    fn columns_follow_order_and_visibility() {
        let w = [72.0, 96.0, 64.0, 96.0, 128.0, 64.0];
        let mut order = NUMS;
        order.swap(0, 5); // PID first, CPU last
        let mut hidden = [false; 6];
        hidden[2] = true; // GPU
        let c = super::columns(48.0, 1000.0, &w, &order, &hidden);
        let names: Vec<Col> = c.iter().map(|c| c.0).collect();
        assert_eq!(names, [Col::Name, Col::Pid, Col::Ram, Col::Vram, Col::Disk, Col::Cpu]);
        assert_eq!(c.last().unwrap().2, 1000.0 - super::PAD - super::BAR, "anchored right");
        assert!(c.windows(2).all(|p| p[0].2 == p[1].1), "no gaps");
        assert_eq!(c[0].1, 48.0 + super::PAD);
    }
}
