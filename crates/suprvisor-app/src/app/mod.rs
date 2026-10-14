//! The window's state, and the helpers that every part of it uses.

mod keys;
mod menus;
mod messages;
mod mouse;
mod nonclient;
mod rows;
mod tray;
mod view;
mod window;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::collector::{Signal, Snapshot};
use crate::edit::Edit;
use crate::render::{self, Focus, Hot, Theme};
use crate::table::{COLS, Col, Key, Table};
use crate::{settings, shell, trace};

pub use window::run;

const TIMER_SAVE: usize = 1;
const TIMER_CARET: usize = 2;
const TIMER_SHOW: usize = 3;
const TIMER_TOOLTIP: usize = 4;
/// The caret stops blinking after this long without input, like Windows' own text boxes can; each
/// blink is a repaint.
const CARET_BLINK_FOR: Duration = Duration::from_secs(10);

enum Drag {
    None,
    /// Pressed on a header at this x: a release sorts, a move drags the column to a new place.
    Header(Col, f32),
    /// A column follows the mouse; the value is the mouse x minus the column's left edge.
    Reorder(Col, f32),
    /// A separator: the columns on its two sides trade width. Mouse x, widths and Name width at
    /// the start.
    Column { left: Option<usize>, right: usize, x: f32, start: [f32; 6], name: f32, fit: [f32; 6], overflow: f32 },
    HThumb(f32), // mouse x minus the horizontal thumb's left edge
    Thumb(f32), // mouse y minus the thumb top
    /// Pressed on a row at this y: a move selects the rows it passes.
    Row(usize, f32),
    Select { anchor: usize, base: HashSet<Key> },
    Text, // selecting text in the search box
}

struct App {
    hwnd: HWND,
    renderer: render::Renderer,
    theme: Theme,
    data: Arc<Mutex<Snapshot>>,
    wanted: Arc<Mutex<Vec<(u32, i64)>>>, // processes under the visible rows: refreshed fast
    signal: mpsc::Sender<Signal>,
    snap: Snapshot,
    got_data: bool,
    table: Table,
    settings: settings::Settings,
    performance: bool, // the Performance tab is open
    nav_open: bool,    // the navigation pane shows labels
    edit: Edit,
    focus: Option<Focus>,
    focus_visible: bool, // the keyboard moved the focus last: draw the focus ring
    active: bool,        // the window has the keyboard
    caret_on: bool,
    caret_at: Instant, // last input in the search box: the caret blinks for CARET_BLINK_FOR after it
    caret_timer: bool,
    hot: Hot,
    mouse: Option<(f32, f32)>,
    caption_down: u32,         // caption button pressed (HTCLOSE...), 0 for none
    save_due: Option<Instant>, // settings changed: write them at this time (TIMER_SAVE checks)
    dirty: bool,               // a frame is due (the message loop draws it)
    modal: u32,                // inside a size/move, menu or dialog loop, which does not return to ours: paint at once
    drag: Drag,
    // Animation: values that move toward a target a little on each frame.
    scroll_to: f32,
    nav_w: f32,
    nav_pill: f32,
    col_off: [f32; 7], // per column (COLS order): drawn left edge minus its place
    hscroll: f32,      // the columns scroll left by this much when they overflow
    hscroll_to: f32,
    last_frame: Instant,
    app_icon: Option<shell::Icon>,
    pending_show: Option<bool>, // show asked before the first data: shows when it comes (or soon)
    maximize_on_show: bool,     // the window was maximized when it last closed
    shown: bool,
    tooltip: Option<usize>, // navigation item whose tooltip shows
}

fn app<'a>(hwnd: HWND) -> Option<&'a mut App> {
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
    unsafe { p.as_mut() }
}

fn dpi(hwnd: HWND) -> u32 {
    unsafe { GetDpiForWindow(hwnd) }
}

/// Index of a column in `COLS`.
fn ci(c: Col) -> usize {
    COLS.iter().position(|&k| k == c).unwrap_or(0)
}

/// Moves `v` toward `to` with a time constant of 1/`rate` seconds. True while it still moves.
fn approach(v: &mut f32, to: f32, rate: f32, dt: f32, settle: f32) -> bool {
    *v += (to - *v) * (1.0 - (-dt * rate).exp());
    if (to - *v).abs() < settle {
        *v = to;
        return false;
    }
    true
}

impl App {
    fn new(data: Arc<Mutex<Snapshot>>, wanted: Arc<Mutex<Vec<(u32, i64)>>>, signal: mpsc::Sender<Signal>) -> windows::core::Result<Self> {
        let settings = settings::Settings::load();
        let nav_w = render::nav_width(settings.nav);
        Ok(Self {
            hwnd: HWND::default(),
            renderer: render::Renderer::new()?,
            theme: Theme::current(),
            data,
            wanted,
            signal,
            snap: Snapshot::default(),
            got_data: false,
            table: Table::new(settings.sort, settings.desc, settings.grouped),
            performance: settings.performance,
            nav_open: settings.nav,
            edit: Edit::default(),
            focus: None,
            focus_visible: false,
            active: false,
            caret_on: true,
            caret_at: Instant::now(),
            caret_timer: false,
            hot: Hot::None,
            mouse: None,
            caption_down: 0,
            save_due: None,
            dirty: false,
            modal: 0,
            settings,
            drag: Drag::None,
            scroll_to: 0.0,
            nav_w,
            nav_pill: 0.0,
            col_off: [0.0; 7],
            hscroll: 0.0,
            hscroll_to: 0.0,
            last_frame: Instant::now(),
            app_icon: shell::app_icon(32),
            pending_show: None,
            maximize_on_show: false,
            shown: false,
            tooltip: None,
        })
    }

    /// Client size in DIPs.
    fn size(&self) -> (f32, f32) {
        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(self.hwnd, &mut rc) };
        let s = 96.0 / dpi(self.hwnd) as f32;
        (rc.right as f32 * s, rc.bottom as f32 * s)
    }

    /// Mouse position from a mouse message, in DIPs.
    fn point(&self, lp: LPARAM) -> (f32, f32) {
        let s = 96.0 / dpi(self.hwnd) as f32;
        ((lp.0 & 0xFFFF) as i16 as f32 * s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 * s)
    }

    fn page(&self) -> usize {
        render::rows_visible(self.size().1).max(1)
    }

    /// After the rows or the scroll position change: tell the collector what is on screen, repaint.
    fn changed(&mut self) {
        let max = self.table.rows.len().saturating_sub(self.page()) as f32;
        self.scroll_to = self.scroll_to.clamp(0.0, max);
        self.table.scroll = self.table.scroll.clamp(0.0, max);
        let (a, b) = (self.table.scroll.min(self.scroll_to), self.table.scroll.max(self.scroll_to));
        let keys = self.table.keys(a.floor() as usize..b.ceil() as usize + self.page() + 1);
        *self.wanted.lock().unwrap() = keys;
        self.dirty = true; // a flag, not InvalidateRect: no call into the window manager per input
    }

    /// Writes the settings file a second after the last change: a burst of sort clicks or a column
    /// drag writes it once, and no click waits on the disk (or on an antivirus scan of the file).
    /// The timer is armed once per burst: SetTimer goes into the window manager, which can make
    /// the UI thread wait when other processes hold it.
    fn save_later(&mut self) {
        if self.save_due.replace(Instant::now() + Duration::from_secs(1)).is_none() {
            unsafe { SetTimer(Some(self.hwnd), TIMER_SAVE, 250, None) };
        }
    }

    /// Takes the collector's latest snapshot.
    fn load(&mut self) {
        self.snap = self.data.lock().unwrap().clone();
        self.got_data |= !self.snap.procs.is_empty();
        self.table.procs = self.snap.procs.clone();
        self.table.titles = self.snap.titles.clone();
        if self.performance {
            self.dirty = true;
        } else {
            self.rebuild();
        }
        self.paint_in_modal();
    }

    /// Inside a modal loop (a menu, the Run dialog) our loop does not run: paint now.
    fn paint_in_modal(&mut self) {
        if self.modal > 0 && self.dirty && self.shown {
            self.paint();
        }
    }

    fn set_hot(&mut self, hot: Hot) {
        if hot != self.hot {
            self.hot = hot;
            self.dirty = true;
            // An item of the closed navigation pane shows its name after a short hover.
            self.tooltip = None;
            if matches!(hot, Hot::Nav(k) if k > 0) && !self.nav_open {
                unsafe { SetTimer(Some(self.hwnd), TIMER_TOOLTIP, 500, None) };
            }
        }
    }

    /// Moves the keyboard focus; `keyboard`: the keyboard moved it, so the focus ring shows.
    fn set_focus(&mut self, focus: Option<Focus>, keyboard: bool) {
        if focus == Some(Focus::Search) && self.focus != focus {
            self.caret_input();
        }
        self.focus = focus;
        self.focus_visible = keyboard;
        self.dirty = true;
    }

    /// Input in the search box: the caret shows and blinks again.
    fn caret_input(&mut self) {
        self.caret_on = true;
        self.caret_at = Instant::now();
        if !self.caret_timer {
            self.caret_timer = true;
            unsafe { SetTimer(Some(self.hwnd), TIMER_CARET, GetCaretBlinkTime().clamp(200, 1200), None) };
        }
        self.dirty = true;
    }

    /// Rows End task acts on: the selected rows that are not section headers.
    fn end_rows(&self) -> Vec<usize> {
        if self.performance {
            return Vec::new();
        }
        self.table.marked_rows().into_iter().filter(|&i| !matches!(self.table.rows[i].key, Key::Section(_))).collect()
    }

    fn rebuild(&mut self) {
        let t = Instant::now();
        self.table.rebuild();
        trace::record("rebuild", t, self.table.rows.len() as u64);
        self.changed();
    }

    fn scroll_by(&mut self, n: f32) {
        self.scroll_to += n;
        self.changed();
    }

    fn ensure_visible(&mut self, i: usize) {
        let page = self.page() as f32;
        let i = i as f32;
        if i < self.scroll_to {
            self.scroll_to = i;
        } else if i + 1.0 > self.scroll_to + page {
            self.scroll_to = i + 1.0 - page;
        }
        self.changed();
    }

    fn save(&mut self) {
        let s = &mut self.settings;
        (s.sort, s.desc, s.grouped, s.performance, s.nav) = (self.table.sort, self.table.desc, self.table.grouped, self.performance, self.nav_open);
        let mut p = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        if unsafe { GetWindowPlacement(self.hwnd, &mut p) }.is_ok() {
            let r = p.rcNormalPosition;
            s.window = Some(([r.left, r.top, r.right, r.bottom], unsafe { IsZoomed(self.hwnd) }.as_bool()));
        }
        s.save();
    }
}
