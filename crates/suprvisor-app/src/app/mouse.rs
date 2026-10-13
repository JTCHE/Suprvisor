//! Mouse: press, move and release on the client area.

use std::collections::HashSet;

use windows::Win32::UI::Input::KeyboardAndMouse::*;

use super::*;
use super::keys::{ctrl, shift};
use super::view::Hit;
use crate::render::{HEAD, TOP};

impl App {
    pub(super) fn mouse_down(&mut self, x: f32, y: f32, double: bool) {
        let hit = self.hit(x, y);
        let (ctrl, shift) = (ctrl(), shift());
        match hit {
            Hit::Search => {}
            Hit::Header(_) | Hit::Edge(_) | Hit::Row(..) => self.set_focus(Some(if matches!(hit, Hit::Row(..)) { Focus::Rows } else { Focus::Header(0) }), false),
            _ => self.set_focus(None, false),
        }
        match hit {
            Hit::Search => self.press_search(x, double, shift),
            Hit::Edge(k) if double => {
                // Double-click on a separator fits the column on its left (CPU for Name|CPU).
                let cols = self.layout_cols();
                self.autofit(if k == 1 { cols[1].0 } else { cols[k - 1].0 });
            }
            Hit::Edge(k) => {
                let cols = self.layout_cols();
                let (left, right) = (cols[k - 1].0.num(), cols[k].0.num().unwrap());
                self.drag = Drag::Column { left, right, x, start: self.settings.widths, name: cols[0].2 - cols[0].1 };
                unsafe { SetCapture(self.hwnd) };
            }
            // No capture for a header press: a click is the common case, and SetCapture and
            // ReleaseCapture go into the window manager, which can make the thread wait when
            // other processes hold it. A release outside the window does not sort.
            Hit::Header(c) => {
                if let Some(k) = self.layout_cols().iter().position(|col| col.0 == c) {
                    self.focus = Some(Focus::Header(k));
                }
                self.drag = Drag::Header(c, x);
            }
            Hit::Row(i, chevron) => self.press_row(i, y, chevron || double, ctrl, shift),
            Hit::Thumb => {
                if let Some((y0, _)) = render::thumb(self.size().1, self.table.rows.len(), self.table.scroll) {
                    self.drag = Drag::Thumb(y - y0);
                    unsafe { SetCapture(self.hwnd) };
                }
            }
            Hit::Nav(k) => self.activate_nav(k),
            Hit::Command(k) if k == 0 || !self.end_rows().is_empty() => self.activate_command(k),
            Hit::SearchClear => {
                self.edit.clear();
                self.set_focus(Some(Focus::Search), false);
                self.filter_changed();
            }
            Hit::Track => {
                let up = render::thumb(self.size().1, self.table.rows.len(), self.table.scroll).is_some_and(|(y0, _)| y < y0);
                let page = self.page() as f32;
                self.scroll_by(if up { -page } else { page });
            }
            _ => {}
        }
    }

    /// A press in the search box: places the caret, or selects the word on a double-click.
    fn press_search(&mut self, x: f32, double: bool, shift: bool) {
        self.set_focus(Some(Focus::Search), false);
        let i = self.search_pos(x);
        if double {
            self.edit.word_at(i);
        } else {
            self.edit.move_to(i, shift);
        }
        self.drag = Drag::Text;
        unsafe { SetCapture(self.hwnd) };
        self.caret_input();
    }

    /// A press on row `i`; `toggle`: on its chevron, or a double-click.
    fn press_row(&mut self, i: usize, y: f32, toggle: bool, ctrl: bool, shift: bool) {
        if toggle {
            self.select(i);
            self.table.toggle(i);
            self.changed();
            return;
        }
        if ctrl {
            self.toggle_mark(i);
        } else if shift {
            self.extend(i);
        } else {
            self.select(i);
        }
        self.drag = Drag::Row(i, y);
    }

    pub(super) fn mouse_move(&mut self, x: f32, y: f32) {
        self.mouse = Some((x, y));
        match self.drag {
            Drag::Column { left, right, x: x0, start, name } => {
                self.settings.widths = render::resize(start, left, right, x - x0, name);
                self.dirty = true;
            }
            Drag::Thumb(grab) => {
                let (_, h) = self.size();
                let rows = self.table.rows.len();
                if let Some((y0, y1)) = render::thumb(h, rows, 0.0) {
                    let span = h - TOP - HEAD - (y1 - y0);
                    let max = rows.saturating_sub(self.page()) as f32;
                    self.scroll_to = ((y - grab - y0) / span * max).clamp(0.0, max);
                    self.table.scroll = self.scroll_to;
                    self.changed();
                }
            }
            Drag::Header(c, x0) if c != Col::Name && (x - x0).abs() > 5.0 => {
                let col = self.layout_cols().into_iter().find(|col| col.0 == c);
                if let Some((_, l, _)) = col {
                    self.drag = Drag::Reorder(c, x0 - l);
                    self.set_hot(Hot::None);
                    unsafe { SetCapture(self.hwnd) };
                    self.reorder_step(c, x, x0 - l);
                }
            }
            Drag::Reorder(c, grab) => {
                self.reorder_step(c, x, grab);
                self.dirty = true;
            }
            Drag::Row(i, y0) if (y - y0).abs() > 4.0 => {
                let base = if ctrl() { self.table.marked.clone() } else { HashSet::new() };
                unsafe { SetCapture(self.hwnd) };
                self.select_drag(i, y, &base);
                self.drag = Drag::Select { anchor: i, base };
            }
            Drag::Select { .. } => {
                self.autoscroll(0.0);
                self.dirty = true; // the frame loop scrolls while the mouse is outside the rows
            }
            Drag::Text => {
                let i = self.search_pos(x);
                self.edit.move_to(i, true);
                self.caret_input();
            }
            Drag::Header(..) | Drag::Row(..) => {}
            Drag::None => self.hover(x, y),
        }
    }

    pub(super) fn mouse_up(&mut self, x: f32, y: f32) {
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Column { .. } => self.save_later(),
            Drag::Header(c, _) if matches!(self.hit(x, y), Hit::Header(h) if h == c) => self.sort_by(c),
            Drag::Reorder(c, grab) => {
                // Drop: the column slides from the mouse into its place.
                if let Some((_, l, _)) = self.layout_cols().into_iter().find(|col| col.0 == c) {
                    self.col_off[ci(c)] = x - grab - l;
                }
                self.dirty = true;
                self.save_later();
            }
            _ => {}
        }
        if unsafe { GetCapture() } == self.hwnd {
            let _ = unsafe { ReleaseCapture() };
        }
    }

    /// No button down: hover states follow the mouse.
    fn hover(&mut self, x: f32, y: f32) {
        let hit = self.hit(x, y);
        self.set_hot(match hit {
            Hit::Header(c) => Hot::Header(c),
            Hit::Nav(k) => Hot::Nav(k),
            Hit::Search => Hot::Search,
            Hit::SearchClear => Hot::SearchClear,
            Hit::Command(k) => Hot::Command(k),
            _ => Hot::None,
        });
        let hover = match hit {
            Hit::Row(i, _) => Some(i),
            _ => None,
        };
        if hover != self.table.hover {
            self.table.hover = hover;
            self.dirty = true;
        }
        let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
        let _ = unsafe { TrackMouseEvent(&mut t) };
    }
}
