//! Direct2D + DirectWrite drawing. Layout is in DIPs; the render target scales to the window DPI.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use suprvisor_core::System;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{Interface, PCWSTR, Result, w};

use crate::edit::Edit;
use crate::shell::Icon;
use crate::table::{Col, Key, Row, Table};
use crate::theme::Theme;

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

/// A flip-model swapchain shown through DirectComposition. DWM picks a flip-model frame up at
/// the next vblank; the GDI-redirected HWND render target took one to two more (~16 ms at 100 Hz).
/// Composition swapchains also carry per-pixel alpha, so Mica shows through.
struct Target {
    dc: ID2D1DeviceContext,
    swapchain: IDXGISwapChain1,
    /// Signaled when the swapchain can take a frame. Waiting on it before drawing replaces the
    /// wait inside Present, so the UI thread waits only where it can also take input.
    waitable: HANDLE,
    dpi: u32,
    _composition: (IDCompositionDevice, IDCompositionTarget, IDCompositionVisual),
}

fn pixel_format() -> D2D1_PIXEL_FORMAT {
    D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }
}

impl Target {
    fn new(factory: &ID2D1Factory1, hwnd: HWND, dpi: u32) -> Result<Self> {
        let mut rc = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rc)? };
        let mut device = None;
        // WARP: software rendering on the CPU, so the GPU driver (40 MB private) never loads and
        // drawing does not wait behind a render or sim that saturates the GPU. Single-threaded:
        // WARP's worker pool (one thread per core) cost more to wake than it saved on these small
        // frames: Performance tab 3.7% to 1.8% of one core, private memory -8 MB. Table paint goes
        // from 3.4 to 4.4 ms, which leaves sort and scroll latency the same.
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS;
        unsafe { D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), flags, None, D3D11_SDK_VERSION, Some(&mut device), None, None)? };
        let dxgi: IDXGIDevice1 = device.unwrap().cast()?;
        unsafe { dxgi.SetMaximumFrameLatency(1)? };
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: (rc.right - rc.left).max(1) as u32,
            Height: (rc.bottom - rc.top).max(1) as u32,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
            ..Default::default()
        };
        let dxgi_factory: IDXGIFactory2 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))? };
        let swapchain = unsafe { dxgi_factory.CreateSwapChainForComposition(&dxgi, &desc, None)? };
        let waitable = unsafe {
            let s: IDXGISwapChain2 = swapchain.cast()?;
            s.SetMaximumFrameLatency(1)?;
            s.GetFrameLatencyWaitableObject()
        };
        let composition: IDCompositionDevice = unsafe { DCompositionCreateDevice(&dxgi)? };
        let target = unsafe { composition.CreateTargetForHwnd(hwnd, true)? };
        let visual = unsafe { composition.CreateVisual()? };
        unsafe {
            visual.SetContent(&swapchain)?;
            target.SetRoot(&visual)?;
            composition.Commit()?;
        }
        let dc = unsafe { factory.CreateDevice(&dxgi)?.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };
        let t = Self { dc, swapchain, waitable, dpi, _composition: (composition, target, visual) };
        t.bind()?;
        Ok(t)
    }

    /// Points the device context at the swapchain's back buffer.
    fn bind(&self) -> Result<()> {
        let surface: IDXGISurface = unsafe { self.swapchain.GetBuffer(0)? };
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: pixel_format(),
            dpiX: self.dpi as f32,
            dpiY: self.dpi as f32,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            ..Default::default()
        };
        let bitmap = unsafe { self.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))? };
        unsafe {
            self.dc.SetTarget(&bitmap);
            self.dc.SetDpi(self.dpi as f32, self.dpi as f32);
        }
        Ok(())
    }

    fn resize(&self, width: u32, height: u32) -> Result<()> {
        unsafe {
            self.dc.SetTarget(None);
            self.swapchain.ResizeBuffers(0, width.max(1), height.max(1), DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT)?;
        }
        self.bind()
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.waitable) };
    }
}

const NAME: usize = 0;
const SECTION: usize = 1;
const NUM: usize = 2;
const HEAD_VALUE: usize = 3;
const HEAD_LABEL: usize = 4;
const HEAD_LABEL_LEFT: usize = 5;
const GLYPH: usize = 6;
const ICON: usize = 7; // navigation and button icons
const SEARCH: usize = 8; // the search text: no trimming
const TINY: usize = 9; // core numbers on the CPU cores graphs
const TINY_VALUE: usize = 10;

/// Format, box size in 1/4 DIPs, where the dimmed part starts, text.
type LayoutKey = (usize, u32, u32, u32, String);

fn format(dwrite: &IDWriteFactory, font: PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT) -> Result<IDWriteTextFormat> {
    unsafe {
        let f = dwrite.CreateTextFormat(font, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        f.SetTextAlignment(align)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        // Too long: cut with an ellipsis.
        let sign = dwrite.CreateEllipsisTrimmingSign(&f)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
        f.SetTrimming(&trim, &sign)?;
        Ok(f)
    }
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

pub fn pct(v: f32) -> String {
    if v < 0.05 {
        "0%".into()
    } else if v < 10.0 {
        format!("{v:.1}%")
    } else {
        format!("{v:.0}%")
    }
}

pub fn bytes(b: u64) -> String {
    let mb = b as f64 / (1u64 << 20) as f64;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if mb >= 100.0 || mb == 0.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

fn mbs(b: u64) -> String {
    let mb = b as f64 / 1e6;
    if mb < 0.05 {
        "0".into()
    } else if mb >= 10.0 {
        format!("{mb:.0}")
    } else {
        format!("{mb:.1}")
    }
}

fn gb(b: u64) -> f64 {
    b as f64 / (1u64 << 30) as f64
}

/// The text of a numeric cell.
pub fn cell(row: &Row, c: Col) -> String {
    let v = &row.v;
    match c {
        Col::Name => row.label.clone(),
        Col::Cpu => pct(v.cpu),
        Col::Ram => bytes(v.ram),
        Col::Gpu => pct(v.gpu),
        Col::Vram => bytes(v.vram),
        Col::Disk => match (mbs(v.read), mbs(v.write)) {
            (r, w) if r == "0" && w == "0" => "0 MB/s".into(),
            (r, w) => format!("{r} / {w} MB/s"),
        },
        Col::Pid => row.pid.map(|p| p.to_string()).unwrap_or_default(),
    }
}

/// The machine total over a column header.
pub fn total(s: &System, c: Col) -> String {
    let share = |used: u64, total: u64| if total > 0 { format!("{:.0}%", used as f64 * 100.0 / total as f64) } else { String::new() };
    match c {
        Col::Name | Col::Pid => String::new(),
        Col::Cpu => pct(s.cpu),
        Col::Ram => share(s.ram_used, s.ram_total),
        Col::Gpu => pct(s.gpu),
        Col::Vram => share(s.vram_used, s.vram_total),
        Col::Disk => format!("{} MB/s", mbs(s.disk_read + s.disk_write)),
    }
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

/// The brushes of one frame.
struct Brushes {
    text: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    strong: ID2D1SolidColorBrush, // control borders
    off: ID2D1SolidColorBrush,    // disabled text
    accent: ID2D1SolidColorBrush,
    hover: ID2D1SolidColorBrush,
    selected: ID2D1SolidColorBrush,
    band: ID2D1SolidColorBrush, // the sorted column, under the rows
    line: ID2D1SolidColorBrush,
    layer: ID2D1SolidColorBrush,
    lifted: ID2D1SolidColorBrush, // a dragged column
    shadow: ID2D1SolidColorBrush,
    close: ID2D1SolidColorBrush, // close button, hovered
    white: ID2D1SolidColorBrush,
    ring: ID2D1SolidColorBrush, // keyboard focus
    ring_in: ID2D1SolidColorBrush,
    fill: ID2D1LinearGradientBrush, // area under a graph line
}

fn rounded(l: f32, t: f32, r: f32, b: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: rect(l, t, r, b), radiusX: radius, radiusY: radius }
}

fn v2(x: f32, y: f32) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: x, Y: y }
}

/// One Performance graph.
struct Graph {
    name: &'static str,
    value: String,
    detail: String,
    max: f64, // value at the top of the graph
    series: Vec<f64>,
}

/// 1, 2 or 5 times a power of ten, at least `v`: a readable top for an auto-scaled graph.
fn nice(v: f64) -> f64 {
    let p = 10f64.powf(v.max(1.0).log10().floor());
    [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * p).find(|&m| m >= v).unwrap_or(10.0 * p)
}

fn graphs(s: &System, h: &VecDeque<System>) -> [Graph; 6] {
    let series = |f: &dyn Fn(&System) -> f64| h.iter().map(f).collect::<Vec<f64>>();
    let share = |used: u64, total: u64| if total > 0 { format!("{:.0}% in use", used as f64 * 100.0 / total as f64) } else { String::new() };
    let rate = |f: &dyn Fn(&System) -> f64| nice(series(f).into_iter().fold(1e6, f64::max));
    let disk = |s: &System| (s.disk_read + s.disk_write) as f64;
    let net = |s: &System| (s.net_recv + s.net_sent) as f64;
    [
        Graph { name: "CPU", value: pct(s.cpu), detail: format!("{} logical processors", s.cores.len()), max: 100.0, series: series(&|s| s.cpu as f64) },
        Graph {
            name: "Memory",
            value: format!("{:.1} / {:.0} GB", gb(s.ram_used), gb(s.ram_total)),
            detail: share(s.ram_used, s.ram_total),
            max: s.ram_total as f64,
            series: series(&|s| s.ram_used as f64),
        },
        Graph { name: "GPU", value: pct(s.gpu), detail: "busiest engine".into(), max: 100.0, series: series(&|s| s.gpu as f64) },
        Graph {
            name: "VRAM",
            value: format!("{:.1} / {:.0} GB", gb(s.vram_used), gb(s.vram_total)),
            detail: share(s.vram_used, s.vram_total),
            max: s.vram_total as f64,
            series: series(&|s| s.vram_used as f64),
        },
        Graph {
            name: "Disk",
            value: format!("{} MB/s", mbs(s.disk_read + s.disk_write)),
            detail: format!("read {}  ·  write {} MB/s  ·  scale {} MB/s", mbs(s.disk_read), mbs(s.disk_write), mbs(rate(&disk) as u64)),
            max: rate(&disk),
            series: series(&disk),
        },
        Graph {
            name: "Network",
            value: format!("{} MB/s", mbs(s.net_recv + s.net_sent)),
            detail: format!("receive {}  ·  send {} MB/s  ·  scale {} MB/s", mbs(s.net_recv), mbs(s.net_sent), mbs(rate(&net) as u64)),
            max: rate(&net),
            series: series(&net),
        },
    ]
}

/// Columns for a grid of `n` cells in a `w` x `h` area: the count that gives the biggest cells
/// with a width about `aspect` times their height.
pub fn grid(n: usize, w: f32, h: f32, aspect: f32) -> usize {
    let size = |c: usize| (w / c as f32 / aspect).min(h / n.div_ceil(c) as f32);
    (1..=n.max(1)).max_by(|&a, &b| size(a).total_cmp(&size(b))).unwrap_or(1)
}

impl Renderer {
    pub fn new() -> Result<Self> {
        let factory: ID2D1Factory1 = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let text = w!("Segoe UI Variable Text");
        let (lead, trail) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING);
        let search = format(&dwrite, text, 13.0, DWRITE_FONT_WEIGHT_NORMAL, lead)?;
        // The search text scrolls inside its box: never cut it with an ellipsis.
        unsafe { search.SetTrimming(&DWRITE_TRIMMING::default(), None)? };
        Ok(Self {
            formats: [
                format(&dwrite, text, 13.0, DWRITE_FONT_WEIGHT_NORMAL, lead)?,
                format(&dwrite, text, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, lead)?,
                format(&dwrite, text, 13.0, DWRITE_FONT_WEIGHT_NORMAL, trail)?,
                format(&dwrite, text, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, trail)?,
                format(&dwrite, text, 12.0, DWRITE_FONT_WEIGHT_NORMAL, trail)?,
                format(&dwrite, text, 12.0, DWRITE_FONT_WEIGHT_NORMAL, lead)?,
                format(&dwrite, w!("Segoe Fluent Icons"), 10.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER)?,
                format(&dwrite, w!("Segoe Fluent Icons"), 16.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER)?,
                search,
                format(&dwrite, text, 10.0, DWRITE_FONT_WEIGHT_NORMAL, lead)?,
                format(&dwrite, text, 10.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, trail)?,
            ],
            layouts: HashMap::new(),
            prev: HashMap::new(),
            dim: 0,
            dwrite,
            factory,
            target: None,
            bitmaps: HashMap::new(),
            search_off: 0.0,
            search_layout: None,
            ready: false,
            snap: None,
            started: std::time::Instant::now(),
        })
    }

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

    fn target(&mut self, hwnd: HWND, dpi: u32) -> Result<ID2D1DeviceContext> {
        if self.target.is_none() {
            self.target = Some(Target::new(&self.factory, hwnd, dpi)?);
            self.bitmaps.clear();
        }
        Ok(self.target.as_ref().unwrap().dc.clone())
    }

    fn bitmap(&mut self, t: &ID2D1DeviceContext, path: &str, icon: Option<&Icon>) -> Option<ID2D1Bitmap1> {
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

    /// A path through `pts`; closed and filled, or open for a line.
    fn path(&self, pts: &[windows_numerics::Vector2], filled: bool) -> Result<ID2D1PathGeometry1> {
        unsafe {
            let g = self.factory.CreatePathGeometry()?;
            let sink = g.Open()?;
            sink.BeginFigure(pts[0], if filled { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW });
            sink.AddLines(&pts[1..]);
            sink.EndFigure(if filled { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
            sink.Close()?;
            Ok(g)
        }
    }

    /// A 60 s graph of `series` (oldest first, one value per second) in `r`, 0 at the bottom and
    /// `max` at the top: a line over a fill that fades down.
    #[allow(clippy::too_many_arguments)]
    fn plot(&self, t: &ID2D1DeviceContext, r: D2D_RECT_F, series: &[f64], max: f64, b: &Brushes, grid: bool, width: f32) -> Result<()> {
        unsafe {
            t.DrawRectangle(&rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), &b.line, 1.0, None);
            if grid {
                for k in 1..4 {
                    let y = r.top + (r.bottom - r.top) * k as f32 / 4.0;
                    t.DrawLine(v2(r.left, y), v2(r.right, y), &b.line, 0.5, None);
                }
            }
        }
        if series.len() < 2 || max <= 0.0 {
            return Ok(());
        }
        // 60 one-second slots; the newest sample at the right edge.
        let step = (r.right - r.left) / 59.0;
        let n = series.len();
        let y = |v: f64| r.bottom - ((v / max).clamp(0.0, 1.0) as f32) * (r.bottom - r.top);
        let mut pts: Vec<windows_numerics::Vector2> = series.iter().enumerate().map(|(k, &v)| v2(r.right - (n - 1 - k) as f32 * step, y(v))).collect();
        let line = self.path(&pts, false)?;
        pts.push(v2(r.right, r.bottom));
        pts.push(v2(r.right - (n - 1) as f32 * step, r.bottom));
        let area = self.path(&pts, true)?;
        unsafe {
            b.fill.SetStartPoint(v2(0.0, r.top));
            b.fill.SetEndPoint(v2(0.0, r.bottom));
            t.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_ALIASED);
            t.FillGeometry(&area, &b.fill, None);
            t.DrawGeometry(&line, &b.accent, width, None);
            t.PopAxisAlignedClip();
        }
        Ok(())
    }

    /// The Performance tab: a grid of 60 s graphs under the page header, and a row with one small
    /// graph per logical processor.
    fn performance(&mut self, t: &ID2D1DeviceContext, size: D2D_SIZE_F, f: &Frame, b: &Brushes) -> Result<()> {
        const GAP: f32 = 12.0;
        let (x0, y0, x1, y1) = (f.nav_w + PAD, TOP + PAD, size.width - PAD, size.height - PAD);
        let cols = if x1 - x0 >= 900.0 { 3 } else if x1 - x0 >= 560.0 { 2 } else { 1 };
        let rows = 6usize.div_ceil(cols) + 1;
        let w = (x1 - x0 - GAP * (cols - 1) as f32) / cols as f32;
        let h = (y1 - y0 - GAP * (rows - 1) as f32) / rows as f32;
        let tile = |t: &ID2D1DeviceContext, l: f32, top: f32, r: f32| unsafe { t.FillRoundedRectangle(&rounded(l, top, r, top + h, 6.0), &b.hover) };
        for (i, g) in graphs(f.system, f.history).iter().enumerate() {
            let l = x0 + (i % cols) as f32 * (w + GAP);
            let top = y0 + (i / cols) as f32 * (h + GAP);
            let r = l + w;
            tile(t, l, top, r);
            self.text(t, g.name, SECTION, rect(l + 12.0, top + 6.0, r - 12.0, top + 30.0), &b.text);
            self.text(t, &g.value, HEAD_VALUE, rect(l + 12.0, top + 6.0, r - 12.0, top + 30.0), &b.text);
            self.text(t, &g.detail, HEAD_LABEL_LEFT, rect(l + 12.0, top + 30.0, r - 12.0, top + 48.0), &b.dim);
            let g_r = rect(l + 12.0, top + 54.0, r - 12.0, top + h - 12.0);
            if g_r.bottom - g_r.top >= 8.0 {
                self.plot(t, g_r, &g.series, g.max, b, true, 1.5)?;
            }
        }

        // One graph per logical processor, across the last row.
        let top = y0 + (rows - 1) as f32 * (h + GAP);
        tile(t, x0, top, x1);
        let cores = &f.system.cores;
        let busiest = cores.iter().copied().fold(0.0, f32::max);
        self.text(t, "CPU cores", SECTION, rect(x0 + 12.0, top + 6.0, x1 - 12.0, top + 30.0), &b.text);
        self.text(t, &format!("busiest {}", pct(busiest)), HEAD_VALUE, rect(x0 + 12.0, top + 6.0, x1 - 12.0, top + 30.0), &b.text);
        let (gl, gt, gr, gb) = (x0 + 12.0, top + 36.0, x1 - 12.0, top + h - 12.0);
        let n = cores.len();
        if n == 0 || gb - gt < 16.0 {
            return Ok(());
        }
        const CELL_GAP: f32 = 6.0;
        let c = grid(n, gr - gl, gb - gt, 1.6);
        let r = n.div_ceil(c);
        let cw = (gr - gl - CELL_GAP * (c - 1) as f32) / c as f32;
        let ch = (gb - gt - CELL_GAP * (r - 1) as f32) / r as f32;
        for (k, v) in cores.iter().enumerate() {
            let l = gl + (k % c) as f32 * (cw + CELL_GAP);
            let tp = gt + (k / c) as f32 * (ch + CELL_GAP);
            let series: Vec<f64> = f.history.iter().map(|s| s.cores.get(k).copied().unwrap_or(0.0) as f64).collect();
            self.plot(t, rect(l, tp, l + cw, tp + ch), &series, 100.0, b, false, 1.0)?;
            if ch >= 22.0 && cw >= 40.0 {
                self.text(t, &k.to_string(), TINY, rect(l + 4.0, tp + 1.0, l + cw - 4.0, tp + 14.0), &b.dim);
                self.text(t, &format!("{v:.0}%"), TINY_VALUE, rect(l + 4.0, tp + 1.0, l + cw - 4.0, tp + 14.0), &b.dim);
            }
        }
        Ok(())
    }

    fn text(&mut self, t: &ID2D1DeviceContext, s: &str, f: usize, r: D2D_RECT_F, b: &ID2D1SolidColorBrush) {
        self.text_dim(t, s, f, r, b, None);
    }

    /// Draws `s` in `r`; from UTF-16 index `dim.0` on, in the `dim.1` brush.
    fn text_dim(&mut self, t: &ID2D1DeviceContext, s: &str, f: usize, r: D2D_RECT_F, b: &ID2D1SolidColorBrush, dim: Option<(u32, &ID2D1SolidColorBrush)>) {
        if let Some(layout) = self.layout(s, f, r, dim) {
            unsafe { t.DrawTextLayout(v2(r.left, r.top), &layout, b, D2D1_DRAW_TEXT_OPTIONS_CLIP) };
        }
    }

    /// The layout of `s` in `r`, from this frame's or the previous frame's cache.
    fn layout(&mut self, s: &str, f: usize, r: D2D_RECT_F, dim: Option<(u32, &ID2D1SolidColorBrush)>) -> Option<IDWriteTextLayout> {
        let (w, h) = ((r.right - r.left).max(0.0), (r.bottom - r.top).max(0.0));
        let key = (f, (w * 4.0) as u32, (h * 4.0) as u32, dim.map_or(u32::MAX, |d| d.0), s.to_owned());
        let layout = match self.prev.remove(&key).or_else(|| self.layouts.get(&key).cloned()) {
            Some(l) => l,
            None => {
                let l = unsafe { self.dwrite.CreateTextLayout(&utf16(s), &self.formats[f], w, h) }.ok()?;
                if let Some((start, brush)) = dim {
                    let _ = unsafe { l.SetDrawingEffect(brush, DWRITE_TEXT_RANGE { startPosition: start, length: u32::MAX - start }) };
                }
                l
            }
        };
        self.layouts.insert(key, layout.clone());
        Some(layout)
    }

    /// Width of `s` as a cell (or a header total), for fitting a column to its content.
    pub fn measure(&mut self, s: &str, header: bool) -> f32 {
        let f = if header { HEAD_VALUE } else { NUM };
        let Ok(l) = (unsafe { self.dwrite.CreateTextLayout(&utf16(s), &self.formats[f], 4000.0, 20.0) }) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { l.GetMetrics(&mut m) }.map_or(0.0, |_| m.width)
    }

    /// The search text's layout: unbounded width, so it never wraps or trims; the box clips it.
    fn search_layout(&mut self, s: &str) -> Option<IDWriteTextLayout> {
        if let Some((text, l)) = &self.search_layout
            && text == s
        {
            return Some(l.clone());
        }
        let l = unsafe { self.dwrite.CreateTextLayout(&utf16(s), &self.formats[SEARCH], 100_000.0, SEARCH_Y.1 - SEARCH_Y.0) }.ok()?;
        self.search_layout = Some((s.to_owned(), l.clone()));
        Some(l)
    }

    /// X of UTF-16 position `pos` in the search text, from the text's left edge.
    fn search_x(&mut self, s: &str, pos: u32) -> f32 {
        let Some(l) = self.search_layout(s) else { return 0.0 };
        let (mut x, mut y, mut m) = (0.0, 0.0, DWRITE_HIT_TEST_METRICS::default());
        let _ = unsafe { l.HitTestTextPosition(pos, false, &mut x, &mut y, &mut m) };
        x
    }

    /// UTF-16 position in the search text nearest to `x` DIPs from the text area's left edge.
    pub fn search_index(&mut self, s: &str, x: f32) -> u32 {
        let off = self.search_off;
        let Some(l) = self.search_layout(s) else { return 0 };
        let (mut trailing, mut inside, mut m) = (Default::default(), Default::default(), DWRITE_HIT_TEST_METRICS::default());
        let _ = unsafe { l.HitTestPoint(x + off, 10.0, &mut trailing, &mut inside, &mut m) };
        m.textPosition + if trailing.as_bool() { m.length } else { 0 }
    }

    /// The keyboard focus ring of Windows 11: a 2 DIP outer stroke and a 1 DIP inner one.
    fn ring(&self, t: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, b: &Brushes) {
        unsafe {
            t.DrawRoundedRectangle(&rounded(r.left - 1.0, r.top - 1.0, r.right + 1.0, r.bottom + 1.0, radius + 1.0), &b.ring, 2.0, None);
            t.DrawRoundedRectangle(&rounded(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5, (radius - 0.5).max(0.0)), &b.ring_in, 1.0, None);
        }
    }

    fn ring_on(&self, f: &Frame, focus: Focus) -> bool {
        f.focus_visible && f.focus == Some(focus)
    }

    /// The search box: text, selection, caret, clear button.
    fn search_box(&mut self, t: &ID2D1DeviceContext, w: f32, f: &Frame, b: &Brushes) {
        let (sl, sr) = search(w);
        let (st, sb) = SEARCH_Y;
        let focused = f.focus == Some(Focus::Search);
        let e = f.search;
        unsafe {
            // Fluent text box: lighter on hover; on focus, the layer color and an accent underline.
            let fill = if focused { &b.layer } else if matches!(f.hot, Hot::Search | Hot::SearchClear) { &b.selected } else { &b.hover };
            t.FillRoundedRectangle(&rounded(sl, st, sr, sb, 4.0), fill);
            t.DrawRoundedRectangle(&rounded(sl + 0.5, st + 0.5, sr - 0.5, sb - 0.5, 4.0), &b.line, 1.0, None);
            let under = if focused { 2.0 } else { 1.0 };
            t.FillRectangle(&rect(sl + 3.0, sb - under, sr - 3.0, sb), if focused { &b.accent } else { &b.strong });
        }
        self.text(t, "\u{E721}", GLYPH, rect(sl + 8.0, st, sl + 28.0, sb), &b.dim);
        let (tl, tr) = search_text(w, !e.text.is_empty());
        if e.text.is_empty() {
            self.search_off = 0.0;
            self.text(t, "Type a name, window title, or PID to filter", NAME, rect(tl, st, tr, sb), &b.dim);
        } else {
            let r = rect(sr - CLEAR, st + 4.0, sr - 4.0, sb - 4.0);
            if f.hot == Hot::SearchClear {
                unsafe { t.FillRoundedRectangle(&rounded(r.left, r.top, r.right, r.bottom, 3.0), &b.hover) };
            }
            self.text(t, "\u{E894}", GLYPH, r, &b.dim);
        }
        let Some(layout) = self.search_layout(&e.text) else { return };
        // Scroll the text so the caret stays inside the box, and no space is left at the end.
        let box_w = tr - tl;
        let caret_x = self.search_x(&e.text, e.utf16(e.caret));
        let mut m = DWRITE_TEXT_METRICS::default();
        let text_w = unsafe { layout.GetMetrics(&mut m) }.map_or(0.0, |_| m.widthIncludingTrailingWhitespace);
        let mut off = self.search_off.min((text_w - box_w + 2.0).max(0.0));
        if caret_x - off > box_w - 2.0 {
            off = caret_x - box_w + 2.0;
        }
        if caret_x - off < 0.0 {
            off = caret_x;
        }
        self.search_off = off.max(0.0);
        let x = tl - self.search_off;
        let (a, z) = e.selection();
        let (ua, uz) = (e.utf16(a), e.utf16(z));
        let shown = focused && a != z;
        let (xa, xz) = if shown { (self.search_x(&e.text, ua), self.search_x(&e.text, uz)) } else { (0.0, 0.0) };
        unsafe {
            t.PushAxisAlignedClip(&rect(tl - 1.0, st, tr + 1.0, sb), D2D1_ANTIALIAS_MODE_ALIASED);
            if shown {
                // Selected text: white on the accent, like the Windows edit control.
                t.FillRectangle(&rect(x + xa, st + 7.0, x + xz, sb - 7.0), &b.accent);
                let _ = layout.SetDrawingEffect(&b.white, DWRITE_TEXT_RANGE { startPosition: ua, length: uz - ua });
            }
            t.DrawTextLayout(v2(x, st), &layout, &b.text, D2D1_DRAW_TEXT_OPTIONS_NONE);
            if shown {
                let _ = layout.SetDrawingEffect(&b.text, DWRITE_TEXT_RANGE { startPosition: 0, length: u32::MAX });
            }
            if focused && f.caret {
                let cx = (x + caret_x).round() + 0.5;
                t.DrawLine(v2(cx, st + 8.0), v2(cx, sb - 8.0), &b.text, 1.0, None);
            }
            t.PopAxisAlignedClip();
        }
    }

    /// Title bar, navigation pane, content layer and page header.
    fn chrome(&mut self, t: &ID2D1DeviceContext, size: D2D_SIZE_F, f: &Frame, b: &Brushes) {
        let (w, h) = (size.width, size.height);
        let cl = f.nav_w;
        unsafe {
            // Content layer: solid, rounded at its top-left corner. Its other corners are off screen.
            t.FillRoundedRectangle(&rounded(cl, TITLE, w + 8.0, h + 8.0, 8.0), &b.layer);
            t.DrawRoundedRectangle(&rounded(cl + 0.5, TITLE + 0.5, w + 8.0, h + 8.0, 8.0), &b.line, 1.0, None);
        }

        // Title bar.
        match f.app_icon.and_then(|i| self.bitmap(t, "\0app", Some(i))) {
            Some(bm) => unsafe { t.DrawBitmap(&bm, Some(&rect(16.0, 16.0, 32.0, 32.0)), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None) },
            None => self.text(t, "\u{E9D9}", ICON, rect(12.0, 0.0, 36.0, TITLE), &b.accent),
        }
        self.text(t, "Suprvisor", HEAD_LABEL_LEFT, rect(44.0, 0.0, 140.0, TITLE), &b.text);
        self.search_box(t, w, f, b);
        let x0 = w - 3.0 * CAPTION.0;
        for k in 0..3 {
            let r = rect(x0 + k as f32 * CAPTION.0, 0.0, x0 + (k + 1) as f32 * CAPTION.0, CAPTION.1);
            let hot = f.hot == Hot::Caption(k);
            if hot {
                unsafe { t.FillRectangle(&r, if k == 2 { &b.close } else { &b.hover }) };
            }
            let glyph = ["\u{E921}", if f.maximized { "\u{E923}" } else { "\u{E922}" }, "\u{E8BB}"][k];
            self.text(t, glyph, GLYPH, r, if hot && k == 2 { &b.white } else { &b.text });
        }

        // Navigation pane. Labels show as it opens; the pill slides to the open page.
        unsafe { t.PushAxisAlignedClip(&rect(0.0, TITLE, cl, h), D2D1_ANTIALIAS_MODE_ALIASED) };
        let items = [("\u{E700}", ""), ("\u{E71D}", "Processes"), ("\u{E9D9}", "Performance"), ("\u{E8A7}", "Windows Task Manager")];
        for (k, (glyph, label)) in items.into_iter().enumerate() {
            let (top, bot) = nav_row(k, h);
            let (l, r) = (4.0, cl - 4.0);
            let on = k > 0 && k < 3 && (k == 2) == f.performance;
            unsafe {
                if on {
                    t.FillRoundedRectangle(&rounded(l, top, r, bot, 4.0), &b.selected);
                } else if f.hot == Hot::Nav(k) {
                    t.FillRoundedRectangle(&rounded(l, top, r, bot, 4.0), &b.hover);
                }
            }
            self.text(t, glyph, ICON, rect(l, top, l + 40.0, bot), &b.text);
            if cl > 60.0 {
                self.text(t, label, NAME, rect(l + 44.0, top, l + 190.0, bot), &b.text);
            }
            if self.ring_on(f, Focus::Nav(k)) {
                self.ring(t, rect(l, top, r, bot), 4.0, b);
            }
        }
        unsafe {
            let cy = f.nav_pill;
            t.FillRoundedRectangle(&rounded(4.0, cy - 8.0, 7.0, cy + 8.0, 1.5), &b.accent);
            t.PopAxisAlignedClip();
        }

        // Page header.
        let x = cl + PAD + 4.0;
        self.text(t, if f.performance { "Performance" } else { "Processes" }, SECTION, rect(x, TITLE, x + 160.0, TOP), &b.text);
        if !f.performance {
            self.text(t, &format!("{} processes", f.processes), NAME, rect(x + 96.0, TITLE, x + 260.0, TOP), &b.dim);
        }
        for (k, r) in commands(w, f.performance) {
            let enabled = k == 0 || f.can_end;
            if enabled && f.hot == Hot::Command(k) {
                unsafe { t.FillRoundedRectangle(&rounded(r.left, r.top, r.right, r.bottom, 4.0), &b.hover) };
            }
            let fg = if enabled { &b.text } else { &b.off };
            let (glyph, label) = match k {
                0 => ("\u{E768}", "Run new task".to_owned()),
                _ if f.ending > 1 => ("\u{E733}", format!("End {} tasks", f.ending)),
                _ => ("\u{E733}", "End task".to_owned()),
            };
            self.text(t, glyph, ICON, rect(r.left + 4.0, r.top, r.left + 32.0, r.bottom), fg);
            self.text(t, &label, NAME, rect(r.left + 36.0, r.top, r.right - 4.0, r.bottom), fg);
            if self.ring_on(f, Focus::Command(k)) {
                self.ring(t, r, 4.0, b);
            }
        }
        unsafe { t.FillRectangle(&rect(cl + 1.0, TOP - 1.0, w, TOP), &b.line) };
    }

    /// One column header; `k` is its place among the shown columns.
    #[allow(clippy::too_many_arguments)]
    fn header(&mut self, t: &ID2D1DeviceContext, f: &Frame, b: &Brushes, k: usize, c: Col, x0: f32, x1: f32) {
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

    pub fn paint(&mut self, hwnd: HWND, dpi: u32, theme: &Theme, f: &Frame) -> Result<()> {
        self.started = std::time::Instant::now();
        let t = self.target(hwnd, dpi)?;
        if !std::mem::take(&mut self.ready) {
            // Painting outside the message loop (resize, menus): take the frame slot here. A
            // timeout, so a stalled compositor cannot hang the window.
            unsafe { WaitForSingleObject(self.waitable().unwrap(), 100) };
        }
        let size = unsafe { t.GetSize() };
        let dim = theme.text_secondary;
        let brush = |c: u32, a: f32| unsafe { t.CreateSolidColorBrush(&color(c, a), None) };
        let wash = if theme.dark { 0xFFFFFF } else { 0x000000 };
        let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: color(theme.accent, 0.38) }, D2D1_GRADIENT_STOP { position: 1.0, color: color(theme.accent, 0.04) }];
        let fill = unsafe {
            let s = ID2D1RenderTarget::CreateGradientStopCollection(&t, &stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let p = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: v2(0.0, 0.0), endPoint: v2(0.0, 1.0) };
            t.CreateLinearGradientBrush(&p, None, &s)?
        };
        let b = Brushes {
            text: brush(theme.text, 1.0)?,
            dim: brush(dim, 1.0)?,
            strong: brush(dim, 0.55)?,
            off: brush(dim, 0.45)?,
            accent: brush(theme.accent, 1.0)?,
            hover: brush(wash, if theme.dark { 0.06 } else { 0.04 })?,
            selected: brush(wash, if theme.dark { 0.09 } else { 0.06 })?,
            band: brush(wash, if theme.dark { 0.03 } else { 0.02 })?,
            line: brush(dim, 0.25)?,
            layer: brush(theme.layer, 1.0)?,
            lifted: brush(if theme.dark { 0x2C2C2C } else { 0xFFFFFF }, 1.0)?,
            shadow: brush(0x000000, if theme.dark { 0.45 } else { 0.14 })?,
            close: brush(0xC42B1C, 1.0)?,
            white: brush(0xFFFFFF, 1.0)?,
            ring: brush(if theme.dark { 0xFFFFFF } else { 0x000000 }, 1.0)?,
            ring_in: brush(if theme.dark { 0x000000 } else { 0xFFFFFF }, 0.7)?,
            fill,
        };
        if self.dim != dim {
            (self.dim, self.layouts, self.prev) = (dim, HashMap::new(), HashMap::new()); // theme changed
        }
        let tb = f.table;

        unsafe {
            t.BeginDraw();
            t.Clear(Some(&color(0, 0.0))); // Mica shows through
        }
        self.chrome(&t, size, f, &b);
        if f.performance {
            self.performance(&t, size, f, &b)?;
            return self.present(&t);
        }

        // Column headers: sort arrow on top, then the machine total, then the name.
        let cols = f.cols;
        unsafe { t.PushAxisAlignedClip(&rect(f.nav_w + 1.0, TOP, size.width, size.height), D2D1_ANTIALIAS_MODE_ALIASED) };
        if let Some(&(c, x0, x1)) = cols.iter().find(|c| c.0 == tb.sort && Some(c.0) != f.dragging) {
            let fx0 = if c == Col::Name { x0 - PAD } else { x0 };
            unsafe { t.FillRectangle(&rect(fx0, TOP, x1, size.height), &b.band) };
        }
        for (k, &(c, x0, x1)) in cols.iter().enumerate() {
            if Some(c) != f.dragging {
                self.header(&t, f, &b, k, c, x0, x1);
            }
        }
        let (name_x0, name_x1) = (cols[0].1, cols[0].2);
        unsafe { t.FillRectangle(&rect(name_x0, TOP + HEAD - 1.0, size.width - PAD, TOP + HEAD), &b.line) };

        // Rows.
        let top = TOP + HEAD;
        if tb.rows.is_empty() && !tb.filter.is_empty() {
            self.text(&t, &format!("No process matches \u{201C}{}\u{201D}", tb.filter), NAME, rect(name_x0 + 8.0, top + 8.0, size.width - PAD, top + 8.0 + ROW), &b.dim);
        }
        unsafe { t.PushAxisAlignedClip(&rect(0.0, top, size.width, size.height), D2D1_ANTIALIAS_MODE_ALIASED) };
        let cursor = tb.selected_index();
        let first = tb.scroll.floor().max(0.0) as usize;
        let row_y = |i: usize| top + (i as f32 - tb.scroll) * ROW;
        let shown = rows_visible(size.height) + 2;
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
                self.ring(&t, band.rect, 4.0, &b);
            }
            let mut x = chevron_x(row, name_x0);
            if row.expandable {
                let g = if row.expanded { "\u{E70D}" } else { "\u{E76C}" };
                self.text(&t, g, GLYPH, rect(x, y, x + 16.0, y + ROW), &b.dim);
            }
            x += 20.0;
            if matches!(row.key, Key::Section(_)) {
                self.text(&t, &format!("{} {}", row.label, row.detail), SECTION, rect(x, y, name_x1 - 8.0, y + ROW), &b.text);
                continue;
            }
            let path = row.rep.map(|p| tb.procs[p].path.as_str()).unwrap_or("");
            match (!path.is_empty()).then(|| self.bitmap(&t, path, f.icons.get(path).map(|i| &**i))).flatten() {
                Some(bm) => unsafe { t.DrawBitmap(&bm, Some(&rect(x, y + 6.0, x + 16.0, y + 22.0)), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None) },
                None => unsafe { t.DrawRoundedRectangle(&rounded(x + 2.5, y + 8.5, x + 13.5, y + 19.5, 2.0), &b.line, 1.0, None) },
            }
            x += 24.0;
            // Label, then the detail (window title or member count) dimmed.
            let s = if row.detail.is_empty() { row.label.clone() } else { format!("{}  {}", row.label, row.detail) };
            let start = row.label.encode_utf16().count() as u32;
            self.text_dim(&t, &s, NAME, rect(x, y, name_x1 - 8.0, y + ROW), &b.text, Some((start, &b.dim)));
            for &(c, x0, x1) in &cols[1..] {
                if Some(c) != f.dragging {
                    self.text(&t, &cell(row, c), NUM, rect(x0 + 8.0, y, x1 - 8.0, y + ROW), &b.text);
                }
            }
        }
        unsafe { t.PopAxisAlignedClip() };

        // The dragged column, lifted over the others: header and cells follow the mouse.
        if let Some(k) = cols.iter().position(|col| Some(col.0) == f.dragging) {
            let (c, x0, x1) = cols[k];
            unsafe {
                t.FillRoundedRectangle(&rounded(x0 - 3.0, TOP + 3.0, x1 + 5.0, size.height + 8.0, 7.0), &b.shadow);
                t.FillRoundedRectangle(&rounded(x0, TOP, x1, size.height + 8.0, 6.0), &b.lifted);
                t.DrawRoundedRectangle(&rounded(x0 + 0.5, TOP + 0.5, x1 - 0.5, size.height + 8.0, 6.0), &b.line, 1.0, None);
            }
            self.header(&t, f, &b, k, c, x0, x1);
            unsafe { t.PushAxisAlignedClip(&rect(x0, top, x1, size.height), D2D1_ANTIALIAS_MODE_ALIASED) };
            for (i, row) in tb.rows.iter().enumerate().skip(first).take(shown) {
                if !matches!(row.key, Key::Section(_)) {
                    self.text(&t, &cell(row, c), NUM, rect(x0 + 8.0, row_y(i), x1 - 8.0, row_y(i) + ROW), &b.text);
                }
            }
            unsafe { t.PopAxisAlignedClip() };
        }

        if let Some((y0, y1)) = thumb(size.height, tb.rows.len(), tb.scroll) {
            let x = size.width - BAR / 2.0;
            unsafe { t.FillRoundedRectangle(&rounded(x - 1.5, y0 + 2.0, x + 1.5, y1 - 2.0, 1.5), &b.dim) };
        }
        unsafe { t.PopAxisAlignedClip() };
        self.present(&t)
    }

    /// Writes the drawn back buffer to a 32-bit BMP: the test harness reads the frame from the app,
    /// so it needs no screen capture and the window can stay off screen and in the background.
    fn save_frame(&self, path: &str) -> Result<()> {
        let sc = &self.target.as_ref().unwrap().swapchain;
        unsafe {
            let tex: ID3D11Texture2D = sc.GetBuffer(0)?;
            let device: ID3D11Device = sc.GetDevice()?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            tex.GetDesc(&mut desc);
            (desc.Usage, desc.BindFlags, desc.CPUAccessFlags, desc.MiscFlags) = (D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32, 0);
            let mut staging = None;
            device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            let staging = staging.unwrap();
            let ctx = device.GetImmediateContext()?;
            ctx.CopyResource(&staging, &tex);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let (w, h) = (desc.Width as usize, desc.Height as usize);
            let mut out = Vec::with_capacity(54 + w * h * 4);
            let le = |v: u32| v.to_le_bytes();
            out.extend(b"BM");
            out.extend(le((54 + w * h * 4) as u32));
            out.extend(le(0));
            out.extend(le(54));
            out.extend(le(40));
            out.extend(le(w as u32));
            out.extend(le((-(h as i32)) as u32)); // top-down
            out.extend([1, 0, 32, 0]);
            out.extend([0u8; 24]);
            for y in 0..h {
                let row = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), w * 4);
                out.extend(row);
            }
            ctx.Unmap(&staging, 0);
            let _ = std::fs::write(path, out);
        }
        Ok(())
    }

    fn present(&mut self, t: &ID2D1DeviceContext) -> Result<()> {
        crate::trace::record("draw", self.started, self.layouts.len() as u64);
        self.prev = std::mem::take(&mut self.layouts);
        let at = std::time::Instant::now();
        let drawn = unsafe { t.EndDraw(None, None) };
        crate::trace::record("enddraw", at, 0);
        if let Some(path) = self.snap.take() {
            let _ = self.save_frame(&path);
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

    #[test]
    fn graph_scale_is_readable() {
        assert_eq!(super::nice(0.0), 1.0);
        assert_eq!(super::nice(1e6), 1e6);
        assert_eq!(super::nice(1.2e6), 2e6);
        assert_eq!(super::nice(3e6), 5e6);
        assert_eq!(super::nice(7e6), 1e7);
        assert_eq!(super::grid(24, 1100.0, 140.0, 1.6), 12, "24 cores in a wide strip: two rows of 12");
        assert_eq!(super::grid(1, 100.0, 100.0, 1.6), 1);
    }
}
