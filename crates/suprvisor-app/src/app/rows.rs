//! The row selection, and the actions on it: End task, Run new task, the row keys.

use std::collections::HashSet;

use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, w};

use super::*;
use crate::taskmgr;
use suprvisor_core::Proc;

impl App {
    pub(super) fn key_at(&self, i: usize) -> Option<Key> {
        self.table.rows.get(i).map(|r| r.key.clone())
    }

    /// Selects row `i` alone.
    pub(super) fn select(&mut self, i: usize) {
        let Some(k) = self.key_at(i) else { return };
        self.table.marked = HashSet::from([k.clone()]);
        (self.table.selected, self.table.anchor) = (Some(k.clone()), Some(k));
        self.ensure_visible(i);
    }

    /// Selects from the anchor row to row `i` (Shift).
    pub(super) fn extend(&mut self, i: usize) {
        let Some(k) = self.key_at(i) else { return };
        let a = self.table.index_of(&self.table.anchor).unwrap_or(i);
        self.table.mark_range(a, i, &HashSet::new());
        self.table.selected = Some(k);
        self.ensure_visible(i);
    }

    /// Moves the cursor to row `i` and leaves the selection as it is (Ctrl).
    pub(super) fn move_cursor(&mut self, i: usize) {
        self.table.selected = self.key_at(i);
        self.ensure_visible(i);
    }

    /// Adds row `i` to the selection or takes it out (Ctrl+click, Ctrl+Space).
    pub(super) fn toggle_mark(&mut self, i: usize) {
        let Some(k) = self.key_at(i) else { return };
        if !self.table.marked.remove(&k) {
            self.table.marked.insert(k.clone());
        }
        (self.table.selected, self.table.anchor) = (Some(k.clone()), Some(k));
        self.ensure_visible(i);
    }

    /// Focus to the rows: the cursor row, or the first process row.
    pub(super) fn focus_rows(&mut self) {
        self.set_focus(Some(Focus::Rows), true);
        match self.table.selected_index() {
            Some(i) => {
                if self.table.marked.is_empty() {
                    self.select(i);
                }
                self.ensure_visible(i);
            }
            None => {
                let first = self.table.rows.iter().position(|r| !matches!(r.key, Key::Section(_))).unwrap_or(0);
                self.select(first);
            }
        }
    }

    pub(super) fn cursor_proc(&self) -> Option<&Proc> {
        let row = &self.table.rows[self.table.selected_index()?];
        row.rep.map(|i| &self.table.procs[i])
    }

    /// Ends every selected process, and every process of each selected group.
    pub(super) fn end_task(&mut self) {
        let rows = self.end_rows();
        if rows.is_empty() {
            return;
        }
        let mut targets: Vec<(u32, i64)> = Vec::new();
        for &i in &rows {
            match self.table.rows[i].key {
                Key::Proc(pid, created) => targets.push((pid, created)),
                Key::Group(..) => targets.extend(self.table.keys(i..i + 1)),
                Key::Section(_) => {}
            }
        }
        targets.sort_unstable();
        targets.dedup();
        let critical: Vec<String> = targets
            .iter()
            .filter(|&&(pid, _)| suprvisor_core::is_critical(pid))
            .filter_map(|&(pid, _)| self.table.procs.iter().find(|p| p.pid == pid).map(|p| format!("{} ({pid})", p.name)))
            .collect();
        if !critical.is_empty() {
            let text = HSTRING::from(format!(
                "{} {} a critical Windows process. Ending it stops Windows immediately and unsaved data is lost.\n\nEnd it anyway?",
                critical.join(", "),
                if critical.len() == 1 { "is" } else { "are each" }
            ));
            self.modal += 1;
            let answer = unsafe { MessageBoxW(Some(self.hwnd), &text, w!("Suprvisor"), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) };
            self.modal -= 1;
            if answer != IDYES {
                return;
            }
        }
        if targets.iter().map(|&(pid, created)| suprvisor_core::kill(pid, created)).any(|r| r.is_err()) {
            let _ = unsafe { windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_ICONWARNING) };
        }
        let _ = self.signal.send(Signal::Rescan);
    }

    pub(super) fn run_new_task(&mut self) {
        self.modal += 1;
        shell::run_dialog(self.hwnd);
        self.modal -= 1;
        let _ = self.signal.send(Signal::Rescan);
    }

    pub(super) fn activate_nav(&mut self, k: usize) {
        match k {
            0 => {
                self.nav_open = !self.nav_open;
                self.dirty = true;
                self.save_later();
            }
            3 => taskmgr::open_native(),
            k => self.set_tab(k == 2),
        }
    }

    pub(super) fn activate_command(&mut self, k: usize) {
        match k {
            0 => self.run_new_task(),
            _ => self.end_task(),
        }
    }

    pub(super) fn sort_by(&mut self, c: Col) {
        self.table.sort_by(c);
        self.changed();
        self.save_later();
    }

    pub(super) fn row_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let cur = self.table.selected_index();
        let last = self.table.rows.len().saturating_sub(1);
        let page = self.page();
        let to = match vk {
            VK_UP if cur == Some(0) && !shift && !ctrl => {
                // Up from the first row goes back to the search box.
                self.set_focus(Some(Focus::Search), true);
                return true;
            }
            VK_UP => Some(cur.map_or(0, |i| i.saturating_sub(1))),
            VK_DOWN => Some(cur.map_or(0, |i| (i + 1).min(last))),
            VK_PRIOR => Some(cur.map_or(0, |i| i.saturating_sub(page))),
            VK_NEXT => Some(cur.map_or(0, |i| (i + page).min(last))),
            VK_HOME => Some(0),
            VK_END => Some(last),
            _ => None,
        };
        if let Some(i) = to {
            self.focus_visible = true;
            match (shift, ctrl) {
                (true, _) => self.extend(i),
                (false, true) => self.move_cursor(i),
                _ => self.select(i),
            }
            return true;
        }
        self.row_action(vk, ctrl, cur, last)
    }

    /// Row keys that act on the cursor row or the selection.
    fn row_action(&mut self, vk: VIRTUAL_KEY, ctrl: bool, cur: Option<usize>, last: usize) -> bool {
        match vk {
            VK_SPACE => {
                if let Some(i) = cur {
                    if ctrl { self.toggle_mark(i) } else { self.select(i) }
                }
            }
            VK_A if ctrl => {
                self.table.mark_range(0, last, &HashSet::new());
                self.dirty = true;
            }
            VK_LEFT | VK_RIGHT | VK_RETURN => {
                let Some(i) = cur else { return true };
                let row = &self.table.rows[i];
                match vk {
                    VK_LEFT if !row.expanded => {
                        // Go to the parent row.
                        let d = row.depth;
                        if let Some(p) = self.table.rows[..i].iter().rposition(|r| r.depth < d) {
                            self.select(p);
                        }
                    }
                    VK_RIGHT if row.expanded => self.select((i + 1).min(last)),
                    VK_RIGHT if !row.expandable => {}
                    _ => {
                        self.table.toggle(i);
                        self.changed();
                    }
                }
            }
            VK_DELETE => self.end_task(),
            VK_C if ctrl => {
                if let Some(p) = self.cursor_proc() {
                    let path = p.path.clone();
                    shell::copy(self.hwnd, &path);
                }
            }
            _ => return false,
        }
        true
    }
}
