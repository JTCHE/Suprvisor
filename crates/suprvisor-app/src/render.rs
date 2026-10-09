//! Direct2D + DirectWrite drawing. Layout is in DIPs; the render target scales to the window DPI.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use suprvisor_core::System;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use windows::Win32::Graphics::Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS, D3D11_SDK_VERSION, D3D11CreateDevice};
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{Interface, PCWSTR, Result, w};

use crate::shell::Icon;
use crate::table::{COLS, Col, Key, Row, Table};
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

/// Left and right edges of each column, in `COLS` order. Numeric columns are anchored right;
/// Name takes the rest, from the content layer's left edge `x0`.
pub fn columns(x0: f32, width: f32, widths: &[f32; 6]) -> [(f32, f32); 7] {
    let mut out = [(0.0, 0.0); 7];
    let mut x = width - PAD - BAR;
    for i in (0..6).rev() {
        out[i + 1] = (x - widths[i], x);
        x -= widths[i];
    }
    out[0] = (x0 + PAD, x);
    out
}

/// Widths after dragging the separator at the left edge of numeric column `i` by `d` DIPs, from
/// `start`. Only that separator moves: the columns on its two sides trade width. The first
/// separator trades with Name, which keeps at least NAME_MIN of its `name` width.
pub fn resize(start: [f32; 6], i: usize, d: f32, name: f32) -> [f32; 6] {
    const MIN: f32 = 32.0;
    const NAME_MIN: f32 = 120.0;
    let mut w = start;
    if i == 0 {
        w[0] = (start[0] - d).clamp(MIN, start[0].max(MIN) + (name - NAME_MIN).max(0.0));
    } else {
        let d = d.clamp(-(start[i - 1] - MIN).max(0.0), (start[i] - MIN).max(0.0));
        w[i - 1] += d;
        w[i] -= d;
    }
    w
}

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

/// Navigation pane row under a point: 0 the menu button, 1 Processes, 2 Performance.
pub fn nav_at(x: f32, y: f32, open: bool) -> Option<usize> {
    if x >= nav_width(open) || y < NAV_TOP {
        return None;
    }
    let k = ((y - NAV_TOP) / NAV_ROW) as usize;
    (k < 3 && y - NAV_TOP - k as f32 * NAV_ROW < NAV_ROW - 4.0).then_some(k)
}

/// The End task button in the page header.
pub fn end_task(width: f32) -> D2D_RECT_F {
    rect(width - PAD - 108.0, TITLE + 10.0, width - PAD, TITLE + 42.0)
}

/// The element under the mouse, for hover states.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hot {
    None,
    Header(Col),
    Nav(usize),
    Search,
    SearchClear,
    EndTask,
    Caption(usize),
}

/// Rows that fit in a client area `height` DIPs tall.
pub fn rows_visible(height: f32) -> usize {
    ((height - TOP - HEAD) / ROW).floor().max(0.0) as usize
}

/// Top and bottom of the scroll bar thumb; None when every row fits.
pub fn thumb(height: f32, rows: usize, scroll: usize) -> Option<(f32, f32)> {
    let n = rows_visible(height);
    if rows <= n {
        return None;
    }
    let (top, h) = (TOP + HEAD, height - TOP - HEAD);
    let len = (h * n as f32 / rows as f32).max(24.0);
    let y = top + (h - len) * scroll as f32 / (rows - n) as f32;
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
    formats: [IDWriteTextFormat; 8], // indexed by NAME, SECTION, ...
    // Text layouts of this frame and the previous one. Shaping text is most of the paint time,
    // and most cells show the same text as one frame ago.
    layouts: HashMap<LayoutKey, IDWriteTextLayout>,
    prev: HashMap<LayoutKey, IDWriteTextLayout>,
    dim: u32, // color baked into cached layouts (the dimmed detail text)
    target: Option<Target>,                         // created on first paint, dropped when the device is lost
    bitmaps: HashMap<String, Option<ID2D1Bitmap1>>, // per exe path; belong to the target
    /// The caller already waited on `waitable()`: the frame slot is taken.
    pub ready: bool,
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

pub struct Frame<'a> {
    pub table: &'a Table,
    pub system: &'a System,
    pub icons: &'a HashMap<String, Arc<Icon>>,
    pub widths: &'a [f32; 6],
    pub processes: usize,
    pub performance: bool,
    pub history: &'a VecDeque<System>,
    pub nav_open: bool,
    pub focus: bool, // the search box has the keyboard
    pub hot: Hot,
    pub can_end: bool, // a process or group is selected
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
    close: ID2D1SolidColorBrush, // close button, hovered
    white: ID2D1SolidColorBrush,
}

fn rounded(l: f32, t: f32, r: f32, b: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: rect(l, t, r, b), radiusX: radius, radiusY: radius }
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
        Graph { name: "CPU", value: pct(s.cpu), detail: String::new(), max: 100.0, series: series(&|s| s.cpu as f64) },
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

impl Renderer {
    pub fn new() -> Result<Self> {
        let factory: ID2D1Factory1 = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let text = w!("Segoe UI Variable Text");
        let (lead, trail) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING);
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
            ],
            layouts: HashMap::new(),
            prev: HashMap::new(),
            dim: 0,
            dwrite,
            factory,
            target: None,
            bitmaps: HashMap::new(),
            ready: false,
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

    fn bitmap(&mut self, t: &ID2D1DeviceContext, path: &str, icons: &HashMap<String, Arc<Icon>>) -> Option<ID2D1Bitmap1> {
        if let Some(b) = self.bitmaps.get(path) {
            return b.clone();
        }
        let icon = icons.get(path)?; // not loaded yet: try again next paint
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

    /// The Performance tab: a grid of 60 s graphs under the page header.
    fn performance(&mut self, t: &ID2D1DeviceContext, size: D2D_SIZE_F, f: &Frame, b: &Brushes) -> Result<()> {
        const GAP: f32 = 12.0;
        let (text, dim, accent, tile, line) = (&b.text, &b.dim, &b.accent, &b.hover, &b.line);
        let fill = unsafe { t.CreateSolidColorBrush(&color(0, 0.0), None)? };
        unsafe { fill.SetColor(&{ let mut c = accent.GetColor(); c.a = 0.2; c }) };
        let (x0, y0, x1, y1) = (nav_width(f.nav_open) + PAD, TOP + PAD, size.width - PAD, size.height - PAD);
        let cols = if x1 - x0 >= 900.0 { 3 } else if x1 - x0 >= 560.0 { 2 } else { 1 };
        let rows = 6usize.div_ceil(cols);
        let w = (x1 - x0 - GAP * (cols - 1) as f32) / cols as f32;
        let h = (y1 - y0 - GAP * (rows - 1) as f32) / rows as f32;
        for (i, g) in graphs(f.system, f.history).iter().enumerate() {
            let l = x0 + (i % cols) as f32 * (w + GAP);
            let top = y0 + (i / cols) as f32 * (h + GAP);
            let r = l + w;
            unsafe { t.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(l, top, r, top + h), radiusX: 6.0, radiusY: 6.0 }, tile) };
            self.text(t, g.name, SECTION, rect(l + 12.0, top + 6.0, r - 12.0, top + 30.0), text);
            self.text(t, &g.value, HEAD_VALUE, rect(l + 12.0, top + 6.0, r - 12.0, top + 30.0), text);
            self.text(t, &g.detail, HEAD_LABEL_LEFT, rect(l + 12.0, top + 30.0, r - 12.0, top + 48.0), dim);
            let (gl, gt, gr, gb) = (l + 12.0, top + 54.0, r - 12.0, top + h - 12.0);
            if gb - gt < 8.0 {
                continue;
            }
            unsafe {
                t.DrawRectangle(&rect(gl + 0.5, gt + 0.5, gr - 0.5, gb - 0.5), line, 1.0, None);
                for k in 1..4 {
                    let y = gt + (gb - gt) * k as f32 / 4.0;
                    t.DrawLine(windows_numerics::Vector2 { X: gl, Y: y }, windows_numerics::Vector2 { X: gr, Y: y }, line, 0.5, None);
                }
            }
            if g.series.len() < 2 || g.max <= 0.0 {
                continue;
            }
            // 60 one-second slots; the newest sample at the right edge.
            let step = (gr - gl) / 59.0;
            let n = g.series.len();
            let mut pts: Vec<windows_numerics::Vector2> = g
                .series
                .iter()
                .enumerate()
                .map(|(k, v)| windows_numerics::Vector2 { X: gr - (n - 1 - k) as f32 * step, Y: gb - ((v / g.max).clamp(0.0, 1.0) as f32) * (gb - gt) })
                .collect();
            let line_path = self.path(&pts, false)?;
            pts.push(windows_numerics::Vector2 { X: gr, Y: gb });
            pts.push(windows_numerics::Vector2 { X: gr - (n - 1) as f32 * step, Y: gb });
            let area = self.path(&pts, true)?;
            unsafe {
                t.PushAxisAlignedClip(&rect(gl, gt, gr, gb), D2D1_ANTIALIAS_MODE_ALIASED);
                t.FillGeometry(&area, &fill, None);
                t.DrawGeometry(&line_path, accent, 1.5, None);
                t.PopAxisAlignedClip();
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
            unsafe { t.DrawTextLayout(windows_numerics::Vector2 { X: r.left, Y: r.top }, &layout, b, D2D1_DRAW_TEXT_OPTIONS_CLIP) };
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

    /// Title bar, navigation pane, content layer and page header.
    fn chrome(&mut self, t: &ID2D1DeviceContext, size: D2D_SIZE_F, f: &Frame, b: &Brushes) {
        let (w, h) = (size.width, size.height);
        let cl = nav_width(f.nav_open);
        unsafe {
            // Content layer: solid, rounded at its top-left corner. Its other corners are off screen.
            t.FillRoundedRectangle(&rounded(cl, TITLE, w + 8.0, h + 8.0, 8.0), &b.layer);
            t.DrawRoundedRectangle(&rounded(cl + 0.5, TITLE + 0.5, w + 8.0, h + 8.0, 8.0), &b.line, 1.0, None);

            // Title bar.
            self.text(t, "\u{E9D9}", ICON, rect(12.0, 0.0, 36.0, TITLE), &b.accent);
            self.text(t, "Suprvisor", HEAD_LABEL_LEFT, rect(44.0, 0.0, 140.0, TITLE), &b.text);
            let (sl, sr) = search(w);
            let (st, sb) = SEARCH_Y;
            // Fluent text box: lighter on hover; on focus, the layer color and an accent underline.
            let fill = if f.focus { &b.layer } else if matches!(f.hot, Hot::Search | Hot::SearchClear) { &b.selected } else { &b.hover };
            t.FillRoundedRectangle(&rounded(sl, st, sr, sb, 4.0), fill);
            t.DrawRoundedRectangle(&rounded(sl + 0.5, st + 0.5, sr - 0.5, sb - 0.5, 4.0), &b.line, 1.0, None);
            let under = if f.focus { 2.0 } else { 1.0 };
            t.FillRectangle(&rect(sl + 3.0, sb - under, sr - 3.0, sb), if f.focus { &b.accent } else { &b.strong });
            self.text(t, "\u{E721}", GLYPH, rect(sl + 8.0, st, sl + 28.0, sb), &b.dim);
            let filter = f.table.filter.as_str();
            let tr = rect(sl + 34.0, st, sr - if filter.is_empty() { 10.0 } else { CLEAR + 4.0 }, sb);
            if filter.is_empty() {
                self.text(t, "Type a name, window title, or PID to filter", NAME, tr, &b.dim);
            } else {
                self.text(t, filter, NAME, tr, &b.text);
                let r = rect(sr - CLEAR, st + 4.0, sr - 4.0, sb - 4.0);
                if f.hot == Hot::SearchClear {
                    t.FillRoundedRectangle(&rounded(r.left, r.top, r.right, r.bottom, 3.0), &b.hover);
                }
                self.text(t, "\u{E894}", GLYPH, r, &b.dim);
            }
            if f.focus {
                let mut m = DWRITE_TEXT_METRICS::default();
                let width = self.layout(filter, NAME, tr, None).and_then(|l| l.GetMetrics(&mut m).ok()).map_or(0.0, |_| m.widthIncludingTrailingWhitespace);
                let x = (tr.left + width).min(tr.right).round() + 0.5;
                t.DrawLine(windows_numerics::Vector2 { X: x, Y: st + 8.0 }, windows_numerics::Vector2 { X: x, Y: sb - 8.0 }, &b.text, 1.0, None);
            }
            let x0 = w - 3.0 * CAPTION.0;
            for k in 0..3 {
                let r = rect(x0 + k as f32 * CAPTION.0, 0.0, x0 + (k + 1) as f32 * CAPTION.0, CAPTION.1);
                let hot = f.hot == Hot::Caption(k);
                if hot {
                    t.FillRectangle(&r, if k == 2 { &b.close } else { &b.hover });
                }
                let glyph = ["\u{E921}", if f.maximized { "\u{E923}" } else { "\u{E922}" }, "\u{E8BB}"][k];
                self.text(t, glyph, GLYPH, r, if hot && k == 2 { &b.white } else { &b.text });
            }

            // Navigation pane.
            for (k, (glyph, label)) in [("\u{E700}", ""), ("\u{E71D}", "Processes"), ("\u{E9D9}", "Performance")].into_iter().enumerate() {
                let top = NAV_TOP + k as f32 * NAV_ROW;
                let (l, r, bot) = (4.0, cl - 4.0, top + NAV_ROW - 4.0);
                let on = k > 0 && (k == 2) == f.performance;
                if on {
                    t.FillRoundedRectangle(&rounded(l, top, r, bot, 4.0), &b.selected);
                    let cy = (top + bot) / 2.0;
                    t.FillRoundedRectangle(&rounded(l, cy - 8.0, l + 3.0, cy + 8.0, 1.5), &b.accent);
                } else if f.hot == Hot::Nav(k) {
                    t.FillRoundedRectangle(&rounded(l, top, r, bot, 4.0), &b.hover);
                }
                self.text(t, glyph, ICON, rect(l, top, l + 40.0, bot), &b.text);
                if f.nav_open {
                    self.text(t, label, NAME, rect(l + 44.0, top, r - 8.0, bot), &b.text);
                }
            }

            // Page header.
            let x = cl + PAD + 4.0;
            self.text(t, if f.performance { "Performance" } else { "Processes" }, SECTION, rect(x, TITLE, x + 160.0, TOP), &b.text);
            if !f.performance {
                self.text(t, &format!("{} processes", f.processes), NAME, rect(x + 96.0, TITLE, x + 260.0, TOP), &b.dim);
                let r = end_task(w);
                if f.can_end && f.hot == Hot::EndTask {
                    t.FillRoundedRectangle(&rounded(r.left, r.top, r.right, r.bottom, 4.0), &b.hover);
                }
                let fg = if f.can_end { &b.text } else { &b.off };
                self.text(t, "\u{E733}", ICON, rect(r.left + 4.0, r.top, r.left + 32.0, r.bottom), fg);
                self.text(t, "End task", NAME, rect(r.left + 36.0, r.top, r.right - 8.0, r.bottom), fg);
            }
            t.FillRectangle(&rect(cl + 1.0, TOP - 1.0, w, TOP), &b.line);
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
            close: brush(0xC42B1C, 1.0)?,
            white: brush(0xFFFFFF, 1.0)?,
        };
        let (text, dim_b, accent, hover, selected, line) = (&b.text, &b.dim, &b.accent, &b.hover, &b.selected, &b.line);
        if self.dim != dim {
            (self.dim, self.layouts, self.prev) = (dim, HashMap::new(), HashMap::new()); // theme changed
        }
        let tb = f.table;
        let cols = columns(nav_width(f.nav_open), size.width, f.widths);

        unsafe {
            t.BeginDraw();
            t.Clear(Some(&color(0, 0.0))); // Mica shows through
            self.chrome(&t, size, f, &b);
            if f.performance {
                self.performance(&t, size, f, &b)?;
                return self.present(&t);
            }

            // Column headers: sort arrow on top, then the machine total, then the name.
            let s = f.system;
            let share = |used: u64, total: u64| if total > 0 { format!("{:.0}%", used as f64 * 100.0 / total as f64) } else { String::new() };
            for (i, (c, &(x0, x1))) in COLS.iter().zip(&cols).enumerate() {
                let (value, label) = match c {
                    Col::Name => (String::new(), "Name"),
                    Col::Pid => (String::new(), "PID"),
                    Col::Cpu => (pct(s.cpu), "CPU"),
                    Col::Ram => (share(s.ram_used, s.ram_total), "Memory"),
                    Col::Gpu => (pct(s.gpu), "GPU"),
                    Col::Vram => (share(s.vram_used, s.vram_total), "VRAM"),
                    Col::Disk => (format!("{} MB/s", mbs(s.disk_read + s.disk_write)), "Disk r / w"),
                };
                // The sorted column: filled header, full-contrast total, a band down the rows.
                // The other totals are dimmed, so the sorted one reads first.
                // Name's fills start at the content layer's edge, not at its text inset.
                let sorted = tb.sort == *c;
                let fx0 = if i == 0 { x0 - PAD } else { x0 };
                if sorted {
                    t.FillRectangle(&rect(fx0, TOP, x1, size.height), &b.band);
                }
                if sorted || f.hot == Hot::Header(*c) {
                    t.FillRectangle(&rect(fx0, TOP, x1, TOP + HEAD), if sorted && f.hot == Hot::Header(*c) { selected } else { hover });
                }
                if i > 0 {
                    t.FillRectangle(&rect(x0, TOP + 8.0, x0 + 1.0, TOP + HEAD - 8.0), line);
                }
                let (l, r) = (x0 + 8.0, x1 - 8.0);
                self.text(&t, &value, HEAD_VALUE, rect(l, TOP + 10.0, r, TOP + 32.0), if sorted { text } else { dim_b });
                let fmt = if *c == Col::Name { HEAD_LABEL_LEFT } else { HEAD_LABEL };
                self.text(&t, label, fmt, rect(l, TOP + 32.0, r, TOP + HEAD - 4.0), if sorted { text } else { dim_b });
                if sorted {
                    let g = if tb.desc { "\u{E70D}" } else { "\u{E70E}" };
                    let cx = (x0 + x1) / 2.0;
                    self.text(&t, g, GLYPH, rect(cx - 8.0, TOP, cx + 8.0, TOP + 12.0), dim_b);
                }
            }
            t.FillRectangle(&rect(cols[0].0, TOP + HEAD - 1.0, size.width - PAD, TOP + HEAD), line);

            // Rows.
            let top = TOP + HEAD;
            if tb.rows.is_empty() && !tb.filter.is_empty() {
                self.text(&t, &format!("No process matches \u{201C}{}\u{201D}", tb.filter), NAME, rect(cols[0].0 + 8.0, top + 8.0, size.width - PAD, top + 8.0 + ROW), dim_b);
            }
            t.PushAxisAlignedClip(&rect(0.0, top, size.width, size.height), D2D1_ANTIALIAS_MODE_ALIASED);
            let sel = tb.selected_index();
            let (name_x0, name_x1) = cols[0];
            for (i, row) in tb.rows.iter().enumerate().skip(tb.scroll).take(rows_visible(size.height) + 1) {
                let y = top + (i - tb.scroll) as f32 * ROW;
                let band = D2D1_ROUNDED_RECT { rect: rect(name_x0 - 4.0, y + 1.0, size.width - PAD - BAR + 4.0, y + ROW - 1.0), radiusX: 4.0, radiusY: 4.0 };
                if sel == Some(i) {
                    t.FillRoundedRectangle(&band, selected);
                    let pill = D2D1_ROUNDED_RECT { rect: rect(name_x0 - 4.0, y + 8.0, name_x0 - 1.0, y + ROW - 8.0), radiusX: 1.5, radiusY: 1.5 };
                    t.FillRoundedRectangle(&pill, accent);
                } else if tb.hover == Some(i) {
                    t.FillRoundedRectangle(&band, hover);
                }
                let is_section = matches!(row.key, Key::Section(_));
                let mut x = chevron_x(row, name_x0);
                if row.expandable {
                    let g = if row.expanded { "\u{E70D}" } else { "\u{E76C}" };
                    self.text(&t, g, GLYPH, rect(x, y, x + 16.0, y + ROW), dim_b);
                }
                x += 20.0;
                if is_section {
                    self.text(&t, &format!("{} {}", row.label, row.detail), SECTION, rect(x, y, name_x1 - 8.0, y + ROW), text);
                    continue;
                }
                let path = row.rep.map(|p| tb.procs[p].path.as_str()).unwrap_or("");
                match (!path.is_empty()).then(|| self.bitmap(&t, path, f.icons)).flatten() {
                    Some(b) => t.DrawBitmap(&b, Some(&rect(x, y + 6.0, x + 16.0, y + 22.0)), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None),
                    None => t.DrawRoundedRectangle(
                        &D2D1_ROUNDED_RECT { rect: rect(x + 2.5, y + 8.5, x + 13.5, y + 19.5), radiusX: 2.0, radiusY: 2.0 },
                        line,
                        1.0,
                        None,
                    ),
                }
                x += 24.0;
                // Label, then the detail (window title or member count) dimmed.
                let s = if row.detail.is_empty() { row.label.clone() } else { format!("{}  {}", row.label, row.detail) };
                let start = row.label.encode_utf16().count() as u32;
                self.text_dim(&t, &s, NAME, rect(x, y, name_x1 - 8.0, y + ROW), text, Some((start, dim_b)));
                let v = &row.v;
                let cells = [
                    pct(v.cpu),
                    bytes(v.ram),
                    pct(v.gpu),
                    bytes(v.vram),
                    match (mbs(v.read), mbs(v.write)) {
                        (r, w) if r == "0" && w == "0" => "0 MB/s".into(),
                        (r, w) => format!("{r} / {w} MB/s"),
                    },
                    row.pid.map(|p| p.to_string()).unwrap_or_default(),
                ];
                for (s, &(x0, x1)) in cells.iter().zip(&cols[1..]) {
                    self.text(&t, s, NUM, rect(x0 + 8.0, y, x1 - 8.0, y + ROW), text);
                }
            }
            if let Some((y0, y1)) = thumb(size.height, tb.rows.len(), tb.scroll) {
                let x = size.width - BAR / 2.0;
                let r = D2D1_ROUNDED_RECT { rect: rect(x - 1.5, y0 + 2.0, x + 1.5, y1 - 2.0), radiusX: 1.5, radiusY: 1.5 };
                t.FillRoundedRectangle(&r, dim_b);
            }
            t.PopAxisAlignedClip();

        }
        self.present(&t)
    }

    fn present(&mut self, t: &ID2D1DeviceContext) -> Result<()> {
        crate::trace::record("draw", self.started, self.layouts.len() as u64);
        self.prev = std::mem::take(&mut self.layouts);
        let at = std::time::Instant::now();
        let drawn = unsafe { t.EndDraw(None, None) };
        crate::trace::record("enddraw", at, 0);
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
    #[test]
    fn resize_moves_one_separator() {
        let w = [72.0, 96.0, 64.0, 96.0, 128.0, 64.0];
        // CPU|Memory 20 right: CPU wider, Memory narrower, the rest still.
        assert_eq!(super::resize(w, 1, 20.0, 400.0), [92.0, 76.0, 64.0, 96.0, 128.0, 64.0]);
        // Too far: Memory stops at the minimum.
        assert_eq!(super::resize(w, 1, 500.0, 400.0)[..2], [136.0, 32.0]);
        // Name|CPU 30 left: CPU wider (Name gives the space), never Name under 120.
        assert_eq!(super::resize(w, 0, -30.0, 400.0)[0], 102.0);
        assert_eq!(super::resize(w, 0, -1000.0, 400.0)[0], 352.0);
        assert_eq!(super::resize(w, 0, 1000.0, 400.0)[0], 32.0);
    }

    #[test]
    fn graph_scale_is_readable() {
        assert_eq!(super::nice(0.0), 1.0);
        assert_eq!(super::nice(1e6), 1e6);
        assert_eq!(super::nice(1.2e6), 2e6);
        assert_eq!(super::nice(3e6), 5e6);
        assert_eq!(super::nice(7e6), 1e7);
    }
}
