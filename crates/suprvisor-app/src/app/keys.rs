//! Keyboard: shortcuts, focus moves, the search box keys.

use windows::Win32::UI::Input::KeyboardAndMouse::*;

use super::*;
use crate::test;

// Test instances only (SUPRVISOR_TEST): the harness posts input as messages, so it never moves the
// user's mouse or takes the keyboard focus. These carry what posted messages cannot.
pub(super) static TEST_MODS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(super) fn down(vk: VIRTUAL_KEY) -> bool {
    if test() {
        let m = TEST_MODS.load(std::sync::atomic::Ordering::Relaxed);
        let bit = match vk {
            VK_CONTROL => 1,
            VK_SHIFT => 2,
            VK_MENU => 4,
            _ => 0,
        };
        if m & bit != 0 {
            return true;
        }
    }
    (unsafe { GetKeyState(vk.0 as i32) }) < 0
}

pub(super) fn ctrl() -> bool {
    down(VK_CONTROL)
}

pub(super) fn shift() -> bool {
    down(VK_SHIFT)
}

impl App {
    /// The filter text changed: the list starts again at the top.
    pub(super) fn filter_changed(&mut self) {
        self.caret_input();
        if self.table.filter == self.edit.text {
            return;
        }
        self.table.filter = self.edit.text.clone();
        (self.scroll_to, self.table.scroll) = (0.0, 0.0);
        if self.performance {
            self.set_tab(false); // rebuilds
        } else {
            self.rebuild();
        }
    }

    /// Text box keys. Returns whether the key was one.
    pub(super) fn edit_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let e = &mut self.edit;
        match vk {
            VK_LEFT | VK_RIGHT => e.arrow(vk == VK_RIGHT, ctrl, shift),
            VK_HOME => e.move_to(0, shift),
            VK_END => e.move_to(e.text.len(), shift),
            VK_BACK => {
                e.delete(false, ctrl);
            }
            VK_DELETE if shift && !e.selected().is_empty() => {
                // Shift+Delete cuts, as in Windows text boxes.
                shell::copy(self.hwnd, e.selected());
                e.delete(true, false);
            }
            VK_DELETE => {
                e.delete(true, ctrl);
            }
            VK_INSERT if ctrl && !e.selected().is_empty() => shell::copy(self.hwnd, e.selected()),
            VK_INSERT if shift => {
                if let Some(s) = shell::paste(self.hwnd) {
                    e.insert(&s.replace(['\r', '\n'], " "), false);
                }
            }
            VK_A if ctrl => e.select_all(),
            VK_C if ctrl => {
                if !e.selected().is_empty() {
                    shell::copy(self.hwnd, e.selected());
                }
            }
            VK_X if ctrl => {
                if !e.selected().is_empty() {
                    shell::copy(self.hwnd, e.selected());
                    e.delete(true, false);
                }
            }
            VK_V if ctrl => {
                if let Some(s) = shell::paste(self.hwnd) {
                    e.insert(s.trim_end_matches(['\r', '\n']).replace(['\r', '\n'], " ").as_str(), false);
                }
            }
            VK_Z if ctrl => {
                e.undo();
            }
            _ => return false,
        }
        self.filter_changed();
        true
    }

    /// Tab stops, in order. The navigation pane, the column headers and the rows are one stop each;
    /// arrow keys move inside them.
    pub(super) fn tab_stops(&self) -> Vec<Focus> {
        let mut v = vec![Focus::Search, Focus::Nav(if self.performance { 2 } else { 1 }), Focus::Command(0)];
        if self.performance {
            v.push(Focus::GraphMode);
        } else {
            if !self.end_rows().is_empty() {
                v.push(Focus::Command(1));
            }
            let sorted = self.layout_cols().iter().position(|c| c.0 == self.table.sort).unwrap_or(0);
            v.extend([Focus::Header(sorted), Focus::Rows]);
        }
        v
    }

    pub(super) fn tab(&mut self, back: bool) {
        let stops = self.tab_stops();
        let same = |a: &Focus, b: &Focus| std::mem::discriminant(a) == std::mem::discriminant(b) && (!matches!(a, Focus::Command(_)) || a == b);
        let n = stops.len();
        let next = match self.focus.and_then(|f| stops.iter().position(|s| same(s, &f))) {
            Some(i) if back => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None if back => n - 1,
            None => 0,
        };
        if stops[next] == Focus::Rows {
            return self.focus_rows();
        }
        self.set_focus(Some(stops[next]), true);
    }

    pub(super) fn key(&mut self, vk: VIRTUAL_KEY) -> bool {
        let (ctrl, shift) = (ctrl(), shift());
        // Window-wide shortcuts.
        match vk {
            VK_TAB if ctrl => self.set_tab(!self.performance),
            VK_1 | VK_2 if ctrl => self.set_tab(vk == VK_2),
            VK_F if ctrl => {
                self.set_focus(Some(Focus::Search), true);
                self.edit.select_all();
            }
            VK_W if ctrl => self.close(),
            VK_N if ctrl => self.run_new_task(),
            VK_F5 => {
                let _ = self.signal.send(Signal::Rescan);
            }
            VK_TAB => self.tab(shift),
            VK_ESCAPE if !matches!(self.drag, Drag::None) => {
                if matches!(self.drag, Drag::Reorder(..) | Drag::Column { .. } | Drag::Thumb(_) | Drag::HThumb(_) | Drag::Select { .. } | Drag::Text) {
                    let _ = unsafe { ReleaseCapture() };
                }
                self.drag = Drag::None;
                self.dirty = true;
            }
            VK_ESCAPE if !self.edit.text.is_empty() => {
                self.edit.clear();
                self.filter_changed();
            }
            VK_ESCAPE => self.close(),
            _ => return self.focus_key(vk, ctrl, shift),
        }
        true
    }

    /// Keys for the focused element.
    pub(super) fn focus_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let activate = vk == VK_RETURN || vk == VK_SPACE;
        match self.focus {
            Some(Focus::Search) => match vk {
                VK_DOWN | VK_NEXT | VK_RETURN if !self.performance => self.focus_rows(),
                _ => return self.edit_key(vk, ctrl, shift),
            },
            Some(Focus::Nav(k)) => match vk {
                VK_UP => self.set_focus(Some(Focus::Nav(k.saturating_sub(1))), true),
                VK_DOWN => self.set_focus(Some(Focus::Nav((k + 1).min(render::NAV_ITEMS - 1))), true),
                _ if activate => self.activate_nav(k),
                _ => return false,
            },
            Some(Focus::Command(k)) => match vk {
                VK_LEFT | VK_RIGHT => {
                    let n = self.renderer.commands.len().max(1);
                    let k = if vk == VK_LEFT { k.saturating_sub(1) } else { (k + 1).min(n - 1) };
                    if k == 0 || !self.end_rows().is_empty() {
                        self.set_focus(Some(Focus::Command(k)), true);
                    }
                }
                _ if activate => self.activate_command(k),
                _ => return false,
            },
            Some(Focus::GraphMode) => match vk {
                VK_LEFT | VK_RIGHT => self.set_graph_mode(vk == VK_RIGHT),
                _ if activate => self.set_graph_mode(!self.settings.cores),
                _ => return false,
            },
            Some(Focus::Header(k)) => {
                let cols = self.layout_cols();
                let k = k.min(cols.len() - 1);
                let c = cols[k].0;
                match vk {
                    VK_LEFT | VK_RIGHT if ctrl => {
                        self.move_column(c, vk == VK_RIGHT);
                        let k = self.layout_cols().iter().position(|col| col.0 == c).unwrap_or(k);
                        self.set_focus(Some(Focus::Header(k)), true);
                    }
                    VK_LEFT => self.set_focus(Some(Focus::Header(k.saturating_sub(1))), true),
                    VK_RIGHT => self.set_focus(Some(Focus::Header((k + 1).min(cols.len() - 1))), true),
                    VK_HOME => self.set_focus(Some(Focus::Header(0)), true),
                    VK_END => self.set_focus(Some(Focus::Header(cols.len() - 1)), true),
                    VK_DOWN => self.focus_rows(),
                    _ if activate => self.sort_by(c),
                    _ => return false,
                }
            }
            Some(Focus::Rows) if !self.performance => return self.row_key(vk, ctrl, shift),
            _ => match vk {
                // Nothing focused yet: the arrows start in the rows, as in Task Manager.
                VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_HOME | VK_END if !self.performance => self.focus_rows(),
                _ => return false,
            },
        }
        true
    }
}
