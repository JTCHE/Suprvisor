//! Context menus: rows, column headers, the search box.

use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::core::HSTRING;

use super::*;
use crate::render::{HEAD, ROW, SEARCH_Y, TOP};
use crate::table::NUMS;
use super::view::Hit;

const MENU_END: usize = 10;
const MENU_OPEN: usize = 11;
const MENU_COPY_PATH: usize = 12;
const MENU_COPY_CMD: usize = 13;
const MENU_GROUPED: usize = 14;
const MENU_RESET: usize = 15;
const MENU_COLUMN: usize = 20; // + Col::num
const MENU_UNDO: usize = 30;
const MENU_CUT: usize = 31;
const MENU_COPY: usize = 32;
const MENU_PASTE: usize = 33;
const MENU_DELETE: usize = 34;
const MENU_SELECT_ALL: usize = 35;

impl App {
    /// Screen point for a menu: the mouse, or for a keyboard menu, the focused element.
    pub(super) fn menu_at(&self, keyboard: bool) -> Option<POINT> {
        if !keyboard {
            return None;
        }
        let (x, y) = match self.focus {
            Some(Focus::Rows) => {
                let i = self.table.selected_index().unwrap_or(0) as f32;
                (self.layout_cols()[0].1 + 40.0, TOP + HEAD + (i - self.table.scroll + 1.0) * ROW)
            }
            Some(Focus::Header(k)) => (self.layout_cols().get(k).map_or(0.0, |c| c.1 + 8.0), TOP + HEAD),
            Some(Focus::Search) => (render::search(self.size().0).0 + 34.0, SEARCH_Y.1),
            _ => return None,
        };
        let s = dpi(self.hwnd) as f32 / 96.0;
        let mut p = POINT { x: (x * s) as i32, y: (y * s) as i32 };
        let _ = unsafe { ClientToScreen(self.hwnd, &mut p) };
        Some(p)
    }

    pub(super) fn row_menu(&mut self, keyboard: bool) {
        let rows = self.end_rows();
        let Some(&first) = rows.first() else { return };
        let row = &self.table.rows[first];
        let end = match (rows.len(), &row.key) {
            (n, _) if n > 1 => format!("End {n} tasks"),
            (_, Key::Group(..)) => format!("End all {} {}", row.procs.len(), row.label),
            _ => "End task".to_owned(),
        };
        let has_path = self.cursor_proc().is_some_and(|p| !p.path.is_empty());
        let path_flags = if has_path { MF_STRING } else { MF_STRING | MF_GRAYED };
        let at = self.menu_at(keyboard);
        let cmd = self.popup(&[(MENU_END, MF_STRING, &end), (0, MF_SEPARATOR, ""), (MENU_OPEN, path_flags, "Open file location"), (MENU_COPY_PATH, path_flags, "Copy path\tCtrl+C"), (MENU_COPY_CMD, MF_STRING, "Copy command line")], at);
        if cmd == MENU_END {
            return self.end_task();
        }
        let Some(p) = self.cursor_proc() else { return };
        let (pid, path) = (p.pid, p.path.clone());
        match cmd {
            MENU_OPEN => shell::open_location(&path),
            MENU_COPY_PATH => shell::copy(self.hwnd, &path),
            MENU_COPY_CMD => shell::copy(self.hwnd, &suprvisor_core::command_line(pid).unwrap_or_default()),
            _ => {}
        }
    }

    /// Group by type, and which columns show.
    pub(super) fn header_menu(&mut self, keyboard: bool) {
        let check = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
        let mut items = vec![(MENU_GROUPED, check(self.table.grouped), "Group by type".to_owned()), (0, MF_SEPARATOR, String::new())];
        for (i, c) in NUMS.iter().enumerate() {
            items.push((MENU_COLUMN + i, check(!self.settings.hidden[i]), c.label().to_owned()));
        }
        items.push((0, MF_SEPARATOR, String::new()));
        items.push((MENU_RESET, MF_STRING, "Reset columns".to_owned()));
        let refs: Vec<(usize, MENU_ITEM_FLAGS, &str)> = items.iter().map(|(id, f, s)| (*id, *f, s.as_str())).collect();
        let at = self.menu_at(keyboard);
        match self.popup(&refs, at) {
            MENU_GROUPED => {
                self.table.grouped = !self.table.grouped;
                self.rebuild();
            }
            MENU_RESET => self.relayout(|a| {
                let s = &mut a.settings;
                (s.widths, s.name_width, s.order, s.hidden) = (settings::DEFAULT_WIDTHS, settings::DEFAULT_NAME_WIDTH, NUMS, [false; 6]);
            }),
            id if (MENU_COLUMN..MENU_COLUMN + 6).contains(&id) => self.relayout(|a| a.settings.hidden[id - MENU_COLUMN] ^= true),
            _ => return,
        }
        if let Some(Focus::Header(k)) = self.focus {
            self.focus = Some(Focus::Header(k.min(self.layout_cols().len() - 1)));
        }
        self.save_later();
    }

    /// The search box's menu, like a Windows edit control's.
    pub(super) fn edit_menu(&mut self, keyboard: bool) {
        let some = |on: bool| if on { MF_STRING } else { MF_STRING | MF_GRAYED };
        let sel = !self.edit.selected().is_empty();
        let at = self.menu_at(keyboard);
        let cmd = self.popup(
            &[
                (MENU_UNDO, MF_STRING, "Undo\tCtrl+Z"),
                (0, MF_SEPARATOR, ""),
                (MENU_CUT, some(sel), "Cut\tCtrl+X"),
                (MENU_COPY, some(sel), "Copy\tCtrl+C"),
                (MENU_PASTE, MF_STRING, "Paste\tCtrl+V"),
                (MENU_DELETE, some(sel), "Delete\tDel"),
                (0, MF_SEPARATOR, ""),
                (MENU_SELECT_ALL, some(!self.edit.text.is_empty()), "Select all\tCtrl+A"),
            ],
            at,
        );
        let key = match cmd {
            MENU_UNDO => VK_Z,
            MENU_CUT => VK_X,
            MENU_COPY => VK_C,
            MENU_PASTE => VK_V,
            MENU_SELECT_ALL => VK_A,
            MENU_DELETE => {
                self.edit_key(VK_DELETE, false, false);
                return;
            }
            _ => return,
        };
        self.edit_key(key, true, false);
    }

    /// A native popup menu at `at` (screen pixels) or the cursor. Returns the chosen id, 0 for none.
    pub(super) fn popup(&mut self, items: &[(usize, MENU_ITEM_FLAGS, &str)], at: Option<POINT>) -> usize {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return 0 };
            for &(id, flags, text) in items {
                let _ = AppendMenuW(menu, flags, id, &HSTRING::from(text));
            }
            let p = at.unwrap_or_else(|| {
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                p
            });
            let _ = SetForegroundWindow(self.hwnd); // so the menu closes when clicking elsewhere
            let id = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD, p.x, p.y, Some(0), self.hwnd, None).0 as usize;
            let _ = DestroyMenu(menu);
            id
        }
    }

    /// The menu for where the mouse is (`keyboard` false) or for the focused element.
    pub(super) fn context_menu(&mut self, hit: Option<Hit>) {
        let keyboard = hit.is_none();
        let target = match hit {
            Some(Hit::Row(i, _)) => {
                if !self.table.marked.contains(&self.table.rows[i].key) {
                    self.select(i);
                }
                self.set_focus(Some(Focus::Rows), false);
                Some(Focus::Rows)
            }
            Some(Hit::Header(_) | Hit::Edge(_)) => Some(Focus::Header(0)),
            Some(Hit::Search) | Some(Hit::SearchClear) => {
                self.set_focus(Some(Focus::Search), false);
                Some(Focus::Search)
            }
            Some(_) => None,
            None => self.focus,
        };
        match target {
            Some(Focus::Rows) if !self.performance => self.row_menu(keyboard),
            Some(Focus::Header(_)) if !self.performance => self.header_menu(keyboard),
            Some(Focus::Search) => self.edit_menu(keyboard),
            _ => {}
        }
    }
}
