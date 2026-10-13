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

/// Widths after dragging a separator by `d` DIPs from `start`, as in Task Manager. The column the
/// separator moves into shrinks, but not under its content width `fit`. Past that, to the left,
/// Name gives the space and the columns between slide; to the right, the separator stops.
/// `left` and `right` are the columns beside it, by `Col::num`; `left` None is Name, which keeps
/// at least NAME_MIN of its `name` width.
pub fn resize(start: [f32; 6], left: Option<usize>, right: usize, d: f32, name: f32, fit: [f32; 6]) -> [f32; 6] {
    const NAME_MIN: f32 = 120.0;
    // A column already narrower than its content does not jump to it.
    let room = |i: usize| (start[i] - fit[i].min(start[i])).max(0.0);
    let mut w = start;
    if d >= 0.0 {
        let d = d.min(room(right));
        if let Some(l) = left {
            w[l] += d; // else Name grows: it takes the rest
        }
        w[right] -= d;
    } else {
        let shrink = left.map_or(0.0, |l| (-d).min(room(l)));
        if let Some(l) = left {
            w[l] -= shrink;
        }
        w[right] += shrink + (-d - shrink).min((name - NAME_MIN).max(0.0));
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
    // The bottom item keeps the same 4 DIP inset from the bottom edge as from the left edge.
    let top = if k == 3 { (height - NAV_ROW).max(NAV_TOP + 3.0 * NAV_ROW) } else { NAV_TOP + k as f32 * NAV_ROW };
    (top, top + NAV_ROW - 4.0)
}

pub const NAV_ITEMS: usize = 4;

/// Navigation pane labels, by row. The open pane shows them; the closed pane, as tooltips.
pub const NAV_LABELS: [&str; NAV_ITEMS] = ["", "Processes", "Performance", "Windows Task Manager"];

/// The six Performance cards, in a grid under the page header that fits the window width.
pub fn cards(nav_w: f32, width: f32, height: f32) -> Vec<D2D_RECT_F> {
    const GAP: f32 = 12.0;
    let (x0, y0, x1, y1) = (nav_w + PAD, TOP + PAD, width - PAD, height - PAD);
    let cols = if x1 - x0 >= 900.0 { 3 } else if x1 - x0 >= 560.0 { 2 } else { 1 };
    let rows = 6usize.div_ceil(cols);
    let w = (x1 - x0 - GAP * (cols - 1) as f32) / cols as f32;
    let h = (y1 - y0 - GAP * (rows - 1) as f32) / rows as f32;
    (0..6)
        .map(|i| {
            let (l, t) = (x0 + (i % cols) as f32 * (w + GAP), y0 + (i / cols) as f32 * (h + GAP));
            rect(l, t, l + w, t + h)
        })
        .collect()
}

/// The CPU card's graph switch, right of its detail line, by mode: 0 overall, 1 per core.
pub const GRAPH_MODES: [&str; 2] = ["Overall", "Per core"];

pub fn graph_modes(card: D2D_RECT_F) -> [D2D_RECT_F; 2] {
    let (t, b, r) = (card.top + 28.0, card.top + 50.0, card.right - 12.0);
    [rect(r - 136.0, t, r - 68.0, b), rect(r - 68.0, t, r, b)]
}

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

/// Page header buttons, laid out right to left from their label widths: (id, rect). 0 Run new
/// task, 1 End task. A button is its icon and label with even padding, as in Task Manager.
pub fn commands(width: f32, label_widths: &[f32]) -> Vec<(usize, D2D_RECT_F)> {
    let mut right = width - PAD;
    let mut out: Vec<(usize, D2D_RECT_F)> = Vec::new();
    for (k, w) in label_widths.iter().enumerate().rev() {
        let left = (right - COMMAND_LABEL_X - w - COMMAND_PAD).floor();
        out.insert(0, (k, rect(left, TITLE + 10.0, right, TITLE + 42.0)));
        right = left - 4.0;
    }
    out
}

/// Page header button insets: the icon starts at COMMAND_PAD, the label at COMMAND_LABEL_X.
pub const COMMAND_PAD: f32 = 11.0;
pub const COMMAND_LABEL_X: f32 = 39.0;

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
        let fit = [40.0; 6];
        assert_eq!(super::resize(w, Some(0), 1, 20.0, 400.0, fit), [92.0, 76.0, 64.0, 96.0, 128.0, 64.0]);
        // Too far: Memory stops at its content width.
        assert_eq!(super::resize(w, Some(0), 1, 500.0, 400.0, fit)[..2], [128.0, 40.0]);
        // Name|CPU 30 left: CPU wider (Name gives the space), never Name under 120.
        assert_eq!(super::resize(w, None, 0, -30.0, 400.0, fit)[0], 102.0);
        assert_eq!(super::resize(w, None, 0, -1000.0, 400.0, fit)[0], 352.0);
        assert_eq!(super::resize(w, None, 0, 1000.0, 400.0, fit)[0], 40.0);
        // Disk|PID 100 left, Disk content 80: Disk shrinks to 80, then Name gives the other 52
        // and the columns left of Disk slide.
        let fit = [40.0, 40.0, 40.0, 40.0, 80.0, 40.0];
        assert_eq!(super::resize(w, Some(4), 5, -100.0, 400.0, fit), [72.0, 96.0, 64.0, 96.0, 80.0, 164.0]);
        // A column already under its content does not grow back: Name gives it all.
        let fit = [40.0, 40.0, 40.0, 40.0, 200.0, 40.0];
        assert_eq!(super::resize(w, Some(4), 5, -100.0, 400.0, fit)[4..], [128.0, 164.0]);
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
