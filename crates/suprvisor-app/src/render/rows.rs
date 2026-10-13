//! The process table: column headers, rows, the dragged column, the scroll bar.

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;

use super::text::*;
use super::*;
use crate::table::{Col, Key, Row};

impl Renderer {
    /// One column header; `k` is its place among the shown columns.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn header(&mut self, t: &ID2D1DeviceContext, f: &Frame, b: &Brushes, k: usize, c: Col, x0: f32, x1: f32) {
        let tb = f.table;
        let sorted = tb.sort == c;
        // Name's fills start at the content layer's edge, not at its text inset.
        let fx0 = if k == 0 { x0 - PAD } else { x0 };
        let hot = f.hot == Hot::Header(c);
        unsafe {
            if sorted || hot || f.dragging == Some(c) {
                t.FillRectangle(&rect(fx0, TOP, x1, TOP + HEAD), if sorted && hot { &b.selected } else { &b.hover });
            }
            if k > 0 {
                t.FillRectangle(&rect(x0, TOP + 8.0, x0 + 1.0, TOP + HEAD - 8.0), &b.line);
            }
        }
        // The sorted column: filled header, full-contrast total. The other totals are dimmed, so
        // the sorted one reads first.
        let fg = if sorted { &b.text } else { &b.dim };
        let (l, r) = (x0 + 8.0, x1 - 8.0);
        self.text(t, &total(f.system, c), HEAD_VALUE, rect(l, TOP + 10.0, r, TOP + 32.0), fg);
        let fmt = if c == Col::Name { HEAD_LABEL_LEFT } else { HEAD_LABEL };
        self.text(t, c.label(), fmt, rect(l, TOP + 32.0, r, TOP + HEAD - 4.0), fg);
        if sorted {
            let g = if tb.desc { "\u{E70D}" } else { "\u{E70E}" };
            let cx = (x0 + x1) / 2.0;
            self.text(t, g, GLYPH, rect(cx - 8.0, TOP, cx + 8.0, TOP + 12.0), &b.dim);
        }
        if self.ring_on(f, Focus::Header(k)) {
            self.ring(t, rect(fx0 + 2.0, TOP + 2.0, x1 - 2.0, TOP + HEAD - 2.0), 4.0, b);
        }
    }

    /// Column headers (sort arrow, machine total, name), the rows, the dragged column, the thumb.
    pub(super) fn table(&mut self, t: &ID2D1DeviceContext, size: D2D_SIZE_F, f: &Frame, b: &Brushes) {
        let (tb, cols) = (f.table, f.cols);
        unsafe { t.PushAxisAlignedClip(&rect(f.nav_w + 1.0, TOP, size.width, size.height), D2D1_ANTIALIAS_MODE_ALIASED) };
        if let Some(&(c, x0, x1)) = cols.iter().find(|c| c.0 == tb.sort && Some(c.0) != f.dragging) {
            let fx0 = if c == Col::Name { x0 - PAD } else { x0 };
            unsafe { t.FillRectangle(&rect(fx0, TOP, x1, size.height), &b.band) };
        }
        for (k, &(c, x0, x1)) in cols.iter().enumerate() {
            if Some(c) != f.dragging {
                self.header(t, f, b, k, c, x0, x1);
            }
        }
        unsafe { t.FillRectangle(&rect(cols[0].1, TOP + HEAD - 1.0, size.width - PAD, TOP + HEAD), &b.line) };
        if tb.rows.is_empty() && !tb.filter.is_empty() {
            let top = TOP + HEAD + 8.0;
            self.text(t, &format!("No process matches \u{201C}{}\u{201D}", tb.filter), NAME, rect(cols[0].1 + 8.0, top, size.width - PAD, top + ROW), &b.dim);
        }
        self.rows(t, size, f, b);
        self.lifted(t, size, f, b);
        if let Some((y0, y1)) = thumb(size.height, tb.rows.len(), tb.scroll) {
            let x = size.width - BAR / 2.0;
            unsafe { t.FillRoundedRectangle(&rounded(x - 1.5, y0 + 2.0, x + 1.5, y1 - 2.0, 1.5), &b.dim) };
        }
        if let Some((x0, x1)) = f.hbar {
            let y = size.height - BAR / 2.0;
            unsafe { t.FillRoundedRectangle(&rounded(x0 + 2.0, y - 1.5, x1 - 2.0, y + 1.5, 1.5), &b.dim) };
        }
        unsafe { t.PopAxisAlignedClip() };
    }

    /// The rows on screen: index of the first, how many, and the top of row `i`.
    fn visible(f: &Frame, height: f32) -> (usize, usize, impl Fn(usize) -> f32) {
        let scroll = f.table.scroll;
        let row_y = move |i: usize| TOP + HEAD + (i as f32 - scroll) * ROW;
        (scroll.floor().max(0.0) as usize, rows_visible(height) + 2, row_y)
    }

    fn rows(&mut self, t: &ID2D1DeviceContext, size: D2D_SIZE_F, f: &Frame, b: &Brushes) {
        let tb = f.table;
        let (name_x0, name_x1) = (f.cols[0].1, f.cols[0].2);
        unsafe { t.PushAxisAlignedClip(&rect(0.0, TOP + HEAD, size.width, size.height), D2D1_ANTIALIAS_MODE_ALIASED) };
        let cursor = tb.selected_index();
        let (first, shown, row_y) = Self::visible(f, size.height);
        for (i, row) in tb.rows.iter().enumerate().skip(first).take(shown) {
            let y = row_y(i);
            let band = rounded(name_x0 - 4.0, y + 1.0, size.width - PAD - BAR + 4.0, y + ROW - 1.0, 4.0);
            unsafe {
                if tb.marked.contains(&row.key) {
                    t.FillRoundedRectangle(&band, &b.selected);
                    t.FillRoundedRectangle(&rounded(name_x0 - 4.0, y + 8.0, name_x0 - 1.0, y + ROW - 8.0, 1.5), &b.accent);
                } else if tb.hover == Some(i) {
                    t.FillRoundedRectangle(&band, &b.hover);
                }
            }
            if cursor == Some(i) && self.ring_on(f, Focus::Rows) {
                self.ring(t, band.rect, 4.0, b);
            }
            self.row(t, f, b, row, y, name_x1);
        }
        unsafe { t.PopAxisAlignedClip() };
    }

    /// One row's chevron, icon, label and cells.
    fn row(&mut self, t: &ID2D1DeviceContext, f: &Frame, b: &Brushes, row: &Row, y: f32, name_x1: f32) {
        let mut x = chevron_x(row, f.cols[0].1);
        if row.expandable {
            let g = if row.expanded { "\u{E70D}" } else { "\u{E76C}" };
            self.text(t, g, GLYPH, rect(x, y, x + 16.0, y + ROW), &b.dim);
        }
        x += 20.0;
        if matches!(row.key, Key::Section(_)) {
            self.text(t, &format!("{} {}", row.label, row.detail), SECTION, rect(x, y, name_x1 - 8.0, y + ROW), &b.text);
            return;
        }
        let path = row.rep.map(|p| f.table.procs[p].path.as_str()).unwrap_or("");
        match (!path.is_empty()).then(|| self.bitmap(t, path, f.icons.get(path).map(|i| &**i))).flatten() {
            Some(bm) => unsafe { t.DrawBitmap(&bm, Some(&rect(x, y + 6.0, x + 16.0, y + 22.0)), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None) },
            None => unsafe { t.DrawRoundedRectangle(&rounded(x + 2.5, y + 8.5, x + 13.5, y + 19.5, 2.0), &b.line, 1.0, None) },
        }
        x += 24.0;
        // Label, then the detail (window title or member count) dimmed.
        let s = if row.detail.is_empty() { row.label.clone() } else { format!("{}  {}", row.label, row.detail) };
        let start = row.label.encode_utf16().count() as u32;
        self.text_dim(t, &s, NAME, rect(x, y, name_x1 - 8.0, y + ROW), &b.text, Some((start, &b.dim)));
        for &(c, x0, x1) in &f.cols[1..] {
            if Some(c) != f.dragging {
                self.text(t, &cell(row, c), NUM, rect(x0 + 8.0, y, x1 - 8.0, y + ROW), &b.text);
            }
        }
    }

    /// The dragged column, lifted over the others: header and cells follow the mouse.
    fn lifted(&mut self, t: &ID2D1DeviceContext, size: D2D_SIZE_F, f: &Frame, b: &Brushes) {
        let Some(k) = f.cols.iter().position(|col| Some(col.0) == f.dragging) else { return };
        let (c, x0, x1) = f.cols[k];
        unsafe {
            t.FillRoundedRectangle(&rounded(x0 - 3.0, TOP + 3.0, x1 + 5.0, size.height + 8.0, 7.0), &b.shadow);
            t.FillRoundedRectangle(&rounded(x0, TOP, x1, size.height + 8.0, 6.0), &b.lifted);
            t.DrawRoundedRectangle(&rounded(x0 + 0.5, TOP + 0.5, x1 - 0.5, size.height + 8.0, 6.0), &b.line, 1.0, None);
        }
        self.header(t, f, b, k, c, x0, x1);
        unsafe { t.PushAxisAlignedClip(&rect(x0, TOP + HEAD, x1, size.height), D2D1_ANTIALIAS_MODE_ALIASED) };
        let (first, shown, row_y) = Self::visible(f, size.height);
        for (i, row) in f.table.rows.iter().enumerate().skip(first).take(shown) {
            if !matches!(row.key, Key::Section(_)) {
                self.text(t, &cell(row, c), NUM, rect(x0 + 8.0, row_y(i), x1 - 8.0, row_y(i) + ROW), &b.text);
            }
        }
        unsafe { t.PopAxisAlignedClip() };
    }
}
