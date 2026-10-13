//! Where things are on screen: column places, hit tests, animation and the frame.

use std::collections::HashSet;
use std::time::Instant;

use windows::Win32::UI::WindowsAndMessaging::*;

use super::*;
use crate::render::{BAR, HEAD, ROW, SEARCH_Y, TITLE, TOP};

pub(super) enum Hit {
    None,
    Header(Col),
    Edge(usize),      // separator at the left of shown column k (k >= 1): drag to resize
    Row(usize, bool), // row, on its chevron
    Thumb,
    Track,
    Nav(usize), // 0 the menu button, 1 Processes, 2 Performance, 3 Windows Task Manager
    Search,
    SearchClear,
    Command(usize), // 0 Run new task, 1 End task
    GraphMode(usize), // the CPU card switch: 0 overall, 1 per core
    Caption(usize), // 0 minimize, 1 maximize, 2 close
    Title,          // empty title bar: drags the window
}

impl App {
    /// Shown columns at their places.
    pub(super) fn layout_cols(&self) -> Vec<(Col, f32, f32)> {
        let s = &self.settings;
        render::columns(self.nav_w, self.size().0, &s.widths, &s.order, &s.hidden)
    }

    /// Shown columns where they are drawn: sliding to their places, or following the mouse.
    pub(super) fn drawn_cols(&self) -> Vec<(Col, f32, f32)> {
        let mut cols = self.layout_cols();
        for c in &mut cols {
            let x = match self.drag {
                Drag::Reorder(d, grab) if d == c.0 => self.mouse.map_or(c.1, |m| m.0 - grab),
                _ => c.1 + self.col_off[ci(c.0)],
            };
            (c.1, c.2) = (x, x + c.2 - c.1);
        }
        cols
    }

    /// Runs `f`, which moves or shows columns; the columns slide from where they were drawn.
    pub(super) fn relayout(&mut self, f: impl FnOnce(&mut Self)) {
        let before = self.drawn_cols();
        f(self);
        for (c, x0, _) in self.layout_cols() {
            self.col_off[ci(c)] = before.iter().find(|b| b.0 == c).map_or(0.0, |b| b.1 - x0);
        }
        self.dirty = true;
    }

    pub(super) fn hit(&self, x: f32, y: f32) -> Hit {
        let (w, h) = self.size();
        if y < TITLE {
            if let Some(k) = render::caption_at(x, y, w) {
                return Hit::Caption(k);
            }
            let (l, r) = render::search(w);
            return match (l..r).contains(&x) && (SEARCH_Y.0..SEARCH_Y.1).contains(&y) {
                true if !self.edit.text.is_empty() && x >= r - render::CLEAR => Hit::SearchClear,
                true => Hit::Search,
                false => Hit::Title,
            };
        }
        if let Some(k) = render::nav_at(x, y, self.nav_w, h) {
            return Hit::Nav(k);
        }
        if x < self.nav_w {
            return Hit::None;
        }
        if y < TOP {
            return self
                .renderer
                .commands
                .iter()
                .find(|(_, r)| (r.left..r.right).contains(&x) && (r.top..r.bottom).contains(&y))
                .map_or(Hit::None, |&(k, _)| Hit::Command(k));
        }
        if self.performance {
            let card = render::cards(self.nav_w, w, h)[0];
            return render::graph_modes(card)
                .iter()
                .position(|r| (r.left..r.right).contains(&x) && (r.top..r.bottom).contains(&y))
                .map_or(Hit::None, Hit::GraphMode);
        }
        let rows_top = TOP + HEAD;
        if x >= w - BAR && y >= rows_top {
            return match render::thumb(h, self.table.rows.len(), self.table.scroll) {
                Some((y0, y1)) if (y0..y1).contains(&y) => Hit::Thumb,
                Some(_) => Hit::Track,
                None => Hit::None,
            };
        }
        let cols = self.layout_cols();
        if (TOP..rows_top).contains(&y) {
            if let Some(k) = (1..cols.len()).find(|&k| (x - cols[k].1).abs() <= 6.0) {
                return Hit::Edge(k);
            }
            return cols.iter().find(|c| (c.1..c.2).contains(&x) || (c.0 == Col::Name && x < c.2)).map_or(Hit::None, |c| Hit::Header(c.0));
        }
        if let Some(i) = self.row_at(y) {
            let row = &self.table.rows[i];
            let cx = render::chevron_x(row, cols[0].1);
            return Hit::Row(i, row.expandable && (cx - 2.0..cx + 18.0).contains(&x));
        }
        Hit::None
    }

    pub(super) fn row_at(&self, y: f32) -> Option<usize> {
        let top = TOP + HEAD;
        if y < top {
            return None;
        }
        let i = (self.table.scroll + (y - top) / ROW).floor() as usize;
        (i < self.table.rows.len()).then_some(i)
    }

    pub(super) fn pill_target(&self) -> f32 {
        let (t, b) = render::nav_row(if self.performance { 2 } else { 1 }, self.size().1);
        (t + b) / 2.0
    }

    /// Moves the animated values one frame on. True while any still moves.
    pub(super) fn animate(&mut self) -> bool {
        let now = Instant::now();
        // After an idle gap, the first frame of a motion steps as if one frame passed.
        let gap = (now - self.last_frame).as_secs_f32();
        let dt = if gap > 0.05 { 0.010 } else { gap };
        self.last_frame = now;
        let mut moving = false;
        if let Drag::Select { .. } = self.drag {
            moving |= self.autoscroll(dt);
        }
        let before = self.table.scroll;
        if !matches!(self.drag, Drag::Thumb(_)) {
            moving |= approach(&mut self.table.scroll, self.scroll_to, 22.0, dt, 0.002);
        }
        if self.table.scroll != before {
            self.changed();
            // The rows moved under a still mouse: the hover follows.
            if let (Some((_, y)), Drag::None) = (self.mouse, &self.drag) {
                self.table.hover = self.row_at(y);
            }
        }
        let nav_before = self.nav_w;
        moving |= approach(&mut self.nav_w, render::nav_width(self.nav_open), 20.0, dt, 0.3);
        if self.nav_w != nav_before {
            self.changed();
        }
        let pill = self.pill_target();
        moving |= approach(&mut self.nav_pill, pill, 20.0, dt, 0.3);
        for o in &mut self.col_off {
            moving |= approach(o, 0.0, 18.0, dt, 0.3);
        }
        moving
    }

    /// While a selection drag is above or below the rows: scrolls toward the mouse, faster the
    /// farther it is, and selects up to the row under it.
    pub(super) fn autoscroll(&mut self, dt: f32) -> bool {
        let (Some((_, y)), Drag::Select { anchor, base }) = (self.mouse, &self.drag) else { return false };
        let (_, h) = self.size();
        let (top, bottom) = (TOP + HEAD, h);
        let d = if y < top { y - top } else if y > bottom { y - bottom } else { 0.0 };
        let (anchor, base) = (*anchor, base.clone());
        if d != 0.0 {
            let speed = (d.abs() / ROW * 10.0).clamp(4.0, 80.0) * d.signum(); // rows per second
            self.scroll_to += speed * dt;
            self.changed();
            self.table.scroll = self.scroll_to;
        }
        self.select_drag(anchor, y.clamp(top, bottom - 1.0), &base);
        d != 0.0
    }

    pub(super) fn select_drag(&mut self, anchor: usize, y: f32, base: &HashSet<Key>) {
        let last = self.table.rows.len().saturating_sub(1);
        let top = TOP + HEAD;
        let j = ((self.table.scroll + (y - top) / ROW).floor().max(0.0) as usize).min(last);
        self.table.mark_range(anchor, j, base);
        self.table.selected = self.table.rows.get(j).map(|r| r.key.clone());
        self.dirty = true;
    }

    pub(super) fn paint(&mut self) {
        self.dirty = false;
        let moving = self.animate();
        let cols = self.drawn_cols();
        let marked = self.end_rows().len();
        let frame = render::Frame {
            table: &self.table,
            system: &self.snap.system,
            icons: &self.snap.icons,
            app_icon: self.app_icon.as_ref(),
            cols: &cols,
            dragging: if let Drag::Reorder(c, _) = self.drag { Some(c) } else { None },
            processes: self.snap.procs.len(),
            performance: self.performance,
            history: &self.snap.history,
            nav_w: self.nav_w,
            nav_pill: self.nav_pill,
            search: &self.edit,
            caret: self.caret_on && self.active,
            focus: self.focus,
            focus_visible: self.focus_visible && self.active,
            hot: self.hot,
            can_end: marked > 0,
            ending: marked,
            maximized: unsafe { IsZoomed(self.hwnd) }.as_bool(),
            cores: self.settings.cores,
            tooltip: self.tooltip,
        };
        let _ = self.renderer.paint(self.hwnd, dpi(self.hwnd), &self.theme, &frame);
        self.dirty |= moving;
    }

    /// Moves a column one place left or right (Ctrl+Left/Right on a header).
    pub(super) fn move_column(&mut self, c: Col, right: bool) {
        let Some(i) = self.settings.order.iter().position(|&k| k == c) else { return };
        // The neighbor among the shown columns; hidden ones in between keep their places.
        let shown = |a: &App, j: usize| !a.settings.hidden[a.settings.order[j].num().unwrap()];
        let j = if right { (i + 1..6).find(|&j| shown(self, j)) } else { (0..i).rev().find(|&j| shown(self, j)) };
        if let Some(j) = j {
            self.relayout(|a| a.settings.order.swap(i, j));
            self.save_later();
        }
    }

    /// The CPU card graph: one per logical processor, or overall.
    pub(super) fn set_graph_mode(&mut self, cores: bool) {
        self.settings.cores = cores;
        self.dirty = true;
        self.save_later();
    }

    /// Fits a column's width to its widest cell (double-click on its right separator).
    pub(super) fn autofit(&mut self, c: Col) {
        let Some(n) = c.num() else { return };
        let w = self.fit_width(c);
        self.relayout(|a| a.settings.widths[n] = w);
        self.save_later();
    }

    /// Width that shows a column's widest cell, total and label in full.
    pub(super) fn fit_width(&mut self, c: Col) -> f32 {
        let cells: HashSet<String> = self.table.rows.iter().filter(|r| !matches!(r.key, Key::Section(_))).map(|r| render::cell(r, c)).collect();
        let mut w = self.renderer.measure(&render::total(&self.snap.system, c), true).max(self.renderer.measure(c.label(), false));
        for s in &cells {
            w = w.max(self.renderer.measure(s, false));
        }
        (w + 20.0).ceil().clamp(render::MIN_WIDTH, 600.0)
    }

    /// While a column is dragged: it swaps places with a neighbor once its middle passes the
    /// neighbor's middle. The neighbor slides over.
    pub(super) fn reorder_step(&mut self, c: Col, x: f32, grab: f32) {
        loop {
            let cols = self.layout_cols();
            let Some(k) = cols.iter().position(|col| col.0 == c) else { return };
            let w = cols[k].2 - cols[k].1;
            let mid = x - grab + w / 2.0;
            let target = if k > 1 && mid < (cols[k - 1].1 + cols[k - 1].2) / 2.0 {
                cols[k - 1].0
            } else if k + 1 < cols.len() && mid > (cols[k + 1].1 + cols[k + 1].2) / 2.0 {
                cols[k + 1].0
            } else {
                return;
            };
            let o = &self.settings.order;
            let (i, j) = (o.iter().position(|&k| k == c).unwrap(), o.iter().position(|&k| k == target).unwrap());
            self.relayout(|a| a.settings.order.swap(i, j));
        }
    }

    /// Caret position in the search text under client x.
    pub(super) fn search_pos(&mut self, x: f32) -> usize {
        let (l, _) = render::search_text(self.size().0, !self.edit.text.is_empty());
        let u = self.renderer.search_index(&self.edit.text, x - l);
        self.edit.from_utf16(u)
    }
}
