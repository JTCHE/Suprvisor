//! Direct2D + DirectWrite drawing. Layout is in DIPs; the render target scales to the window DPI.

mod chrome;
mod layout;
mod performance;
mod rows;
mod target;
mod text;
mod theme;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use suprvisor_core::System;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::DXGI_PRESENT;
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::core::Result;

use crate::edit::Edit;
use crate::shell::Icon;
use crate::table::{Col, Table};
use chrome::Brushes;
use text::LayoutKey;
use target::Target;

pub use layout::*;
pub use text::{cell, total};
pub use theme::Theme;

// The window, like Task Manager: a title bar with the search box and a navigation pane on Mica,
// around a solid content layer with the page header, the column headers and the rows.

/// Title bar: app name, search box, caption buttons.
pub const TITLE: f32 = 48.0;
/// Page header in the content layer: page title, End task.
const PAGE: f32 = 52.0;
/// Top of the column headers.
pub const TOP: f32 = TITLE + PAGE;
/// Column headers: sort arrow, total, name.
pub const HEAD: f32 = 52.0;
pub const ROW: f32 = 28.0;
pub const PAD: f32 = 12.0;
/// Scroll bar strip at the right edge.
pub const BAR: f32 = 12.0;
const INDENT: f32 = 20.0;
/// Caption button size, as in Windows 11.
const CAPTION: (f32, f32) = (46.0, 32.0);
/// Search box top and bottom.
pub const SEARCH_Y: (f32, f32) = (8.0, 40.0);
/// Navigation pane rows: the menu button, Processes, Performance.
const NAV_TOP: f32 = TITLE + 4.0;
const NAV_ROW: f32 = 40.0;

/// The element under the mouse, for hover states.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hot {
    None,
    Header(Col),
    Nav(usize),
    Search,
    SearchClear,
    Command(usize),
    Caption(usize),
}

/// Where the keyboard is. Tab and Shift+Tab go through these in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Search,
    Nav(usize),
    Command(usize),
    Header(usize), // index in the shown columns
    Rows,
}

pub struct Renderer {
    factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    formats: [IDWriteTextFormat; 11], // indexed by NAME, SECTION, ...
    // Text layouts of this frame and the previous one. Shaping text is most of the paint time,
    // and most cells show the same text as one frame ago.
    layouts: HashMap<LayoutKey, IDWriteTextLayout>,
    prev: HashMap<LayoutKey, IDWriteTextLayout>,
    dim: u32, // color baked into cached layouts (the dimmed detail text)
    target: Option<Target>,                         // created on first paint, dropped when the device is lost
    bitmaps: HashMap<String, Option<ID2D1Bitmap1>>, // per exe path; belong to the target
    /// How far the search text is scrolled left in its box, so the caret stays in view.
    search_off: f32,
    search_layout: Option<(String, IDWriteTextLayout)>,
    /// The caller already waited on `waitable()`: the frame slot is taken.
    pub ready: bool,
    /// Test instances (SUPRVISOR_TEST): write the next frame to this BMP file.
    pub snap: Option<String>,
    started: std::time::Instant, // this paint, for the trace
}

pub struct Frame<'a> {
    pub table: &'a Table,
    pub system: &'a System,
    pub icons: &'a HashMap<String, Arc<Icon>>,
    pub app_icon: Option<&'a Icon>,
    /// Shown columns at their drawn positions (moving while a column is dragged or slides).
    pub cols: &'a [(Col, f32, f32)],
    /// The column being dragged to a new place: drawn last, lifted.
    pub dragging: Option<Col>,
    pub processes: usize,
    pub performance: bool,
    pub history: &'a VecDeque<System>,
    pub nav_w: f32,    // navigation pane width, animated
    pub nav_pill: f32, // center of the selection pill in the navigation pane, animated
    pub search: &'a Edit,
    pub caret: bool, // the caret is in its on phase
    pub focus: Option<Focus>,
    pub focus_visible: bool, // the keyboard moved the focus: show the focus ring
    pub hot: Hot,
    pub can_end: bool, // a process or group is selected
    pub ending: usize, // selected rows End task acts on, for the button label
    pub maximized: bool,
}

fn pixel_format() -> D2D1_PIXEL_FORMAT {
    D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn color(c: u32, a: f32) -> D2D1_COLOR_F {
    let ch = |s: u32| ((c >> s) & 0xFF) as f32 / 255.0;
    D2D1_COLOR_F { r: ch(16), g: ch(8), b: ch(0), a }
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn rounded(l: f32, t: f32, r: f32, b: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: rect(l, t, r, b), radiusX: radius, radiusY: radius }
}

fn v2(x: f32, y: f32) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: x, Y: y }
}

impl Renderer {
    pub fn resize(&mut self, width: u32, height: u32) {
        if let Some(t) = &self.target
            && t.resize(width, height).is_err()
        {
            self.target = None;
        }
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        if let Some(t) = &mut self.target {
            t.dpi = dpi;
            unsafe { t.dc.SetDpi(dpi as f32, dpi as f32) };
        }
    }

    /// The swapchain's frame slot object, once the swapchain exists.
    pub fn waitable(&self) -> Option<HANDLE> {
        self.target.as_ref().map(|t| t.waitable)
    }

    pub(super) fn target(&mut self, hwnd: HWND, dpi: u32) -> Result<ID2D1DeviceContext> {
        if self.target.is_none() {
            self.target = Some(Target::new(&self.factory, hwnd, dpi)?);
            self.bitmaps.clear();
        }
        Ok(self.target.as_ref().unwrap().dc.clone())
    }

    pub(super) fn bitmap(&mut self, t: &ID2D1DeviceContext, path: &str, icon: Option<&Icon>) -> Option<ID2D1Bitmap1> {
        if let Some(b) = self.bitmaps.get(path) {
            return b.clone();
        }
        let icon = icon?; // not loaded yet: try again next paint
        let props = D2D1_BITMAP_PROPERTIES1 { pixelFormat: pixel_format(), dpiX: 96.0, dpiY: 96.0, ..Default::default() };
        let size = D2D_SIZE_U { width: icon.size, height: icon.size };
        let b = unsafe { t.CreateBitmap(size, Some(icon.bgra.as_ptr().cast()), icon.size * 4, &props) }.ok();
        self.bitmaps.insert(path.to_owned(), b.clone());
        b
    }

    pub fn paint(&mut self, hwnd: HWND, dpi: u32, theme: &Theme, f: &Frame) -> Result<()> {
        self.started = std::time::Instant::now();
        let t = self.target(hwnd, dpi)?;
        if !std::mem::take(&mut self.ready) {
            // Painting outside the message loop (resize, menus): take the frame slot here. A
            // timeout, so a stalled compositor cannot hang the window.
            unsafe { WaitForSingleObject(self.waitable().unwrap(), 100) };
        }
        let size = unsafe { t.GetSize() };
        let b = Brushes::new(&t, theme)?;
        if self.dim != theme.text_secondary {
            (self.dim, self.layouts, self.prev) = (theme.text_secondary, HashMap::new(), HashMap::new()); // theme changed
        }
        unsafe {
            t.BeginDraw();
            t.Clear(Some(&color(0, 0.0))); // Mica shows through
        }
        self.chrome(&t, size, f, &b);
        if f.performance {
            self.performance(&t, size, f, &b)?;
        } else {
            self.table(&t, size, f, &b);
        }
        self.present(&t)
    }

    pub(super) fn present(&mut self, t: &ID2D1DeviceContext) -> Result<()> {
        crate::trace::record("draw", self.started, self.layouts.len() as u64);
        self.prev = std::mem::take(&mut self.layouts);
        let at = std::time::Instant::now();
        let drawn = unsafe { t.EndDraw(None, None) };
        crate::trace::record("enddraw", at, 0);
        if let Some(path) = self.snap.take() {
            let _ = self.target.as_ref().unwrap().save(&path);
        }
        let at = std::time::Instant::now();
        let presented = drawn.and_then(|_| unsafe { self.target.as_ref().unwrap().swapchain.Present(1, DXGI_PRESENT(0)).ok() });
        crate::trace::record("present", at, 0);
        if presented.is_err() {
            self.target = None; // device lost: recreate on the next paint
        }
        Ok(())
    }
}
