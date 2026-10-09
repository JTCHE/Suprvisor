//! Direct2D + DirectWrite drawing. Layout is in DIPs; the render target scales to the window DPI.

use std::collections::HashMap;
use std::sync::Arc;

use suprvisor_core::System;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{PCWSTR, Result, w};

use crate::shell::Icon;
use crate::table::{COLS, Col, Key, Row, Table};
use crate::theme::Theme;

/// Filter box and totals line.
pub const TOP: f32 = 44.0;
/// Column headers: total on the first line, name on the second.
pub const HEAD: f32 = 48.0;
pub const ROW: f32 = 28.0;
pub const PAD: f32 = 12.0;
/// Scroll bar strip at the right edge.
pub const BAR: f32 = 12.0;
const INDENT: f32 = 20.0;

/// Left and right edges of each column, in `COLS` order. Numeric columns are anchored right;
/// Name takes the rest.
pub fn columns(width: f32, widths: &[f32; 6]) -> [(f32, f32); 7] {
    let mut out = [(0.0, 0.0); 7];
    let mut x = width - PAD - BAR;
    for i in (0..6).rev() {
        out[i + 1] = (x - widths[i], x);
        x -= widths[i];
    }
    out[0] = (PAD, x);
    out
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
pub fn chevron_x(t: &Table, row: &Row, name_x0: f32) -> f32 {
    let section = matches!(row.key, Key::Section(_));
    let depth = if t.grouped && !section { row.depth.saturating_sub(1) } else { row.depth };
    name_x0 + depth as f32 * INDENT
}

pub struct Renderer {
    factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    formats: [IDWriteTextFormat; 7], // indexed by NAME, SECTION, ...
    // Text layouts of this frame and the previous one. Shaping text is most of the paint time,
    // and most cells show the same text as one frame ago.
    layouts: HashMap<LayoutKey, IDWriteTextLayout>,
    prev: HashMap<LayoutKey, IDWriteTextLayout>,
    dim: u32, // color baked into cached layouts (the dimmed detail text)
    target: Option<ID2D1HwndRenderTarget>, // created on first paint, dropped when the device is lost
    bitmaps: HashMap<String, Option<ID2D1Bitmap>>, // per exe path; belong to the target
}

const NAME: usize = 0;
const SECTION: usize = 1;
const NUM: usize = 2;
const HEAD_VALUE: usize = 3;
const HEAD_LABEL: usize = 4;
const HEAD_LABEL_LEFT: usize = 5;
const GLYPH: usize = 6;

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
                format(&dwrite, text, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, lead)?,
                format(&dwrite, text, 13.0, DWRITE_FONT_WEIGHT_NORMAL, trail)?,
                format(&dwrite, text, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, trail)?,
                format(&dwrite, text, 12.0, DWRITE_FONT_WEIGHT_NORMAL, trail)?,
                format(&dwrite, text, 12.0, DWRITE_FONT_WEIGHT_NORMAL, lead)?,
                format(&dwrite, w!("Segoe Fluent Icons"), 10.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER)?,
            ],
            layouts: HashMap::new(),
            prev: HashMap::new(),
            dim: 0,
            dwrite,
            factory,
            target: None,
            bitmaps: HashMap::new(),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if let Some(t) = &self.target {
            let _ = unsafe { t.Resize(&D2D_SIZE_U { width, height }) };
        }
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        if let Some(t) = &self.target {
            unsafe { t.SetDpi(dpi as f32, dpi as f32) };
        }
    }

    fn target(&mut self, hwnd: HWND, dpi: u32) -> Result<ID2D1HwndRenderTarget> {
        if let Some(t) = &self.target {
            return Ok(t.clone());
        }
        let mut rc = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rc)? };
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            // Software (WARP): 21 MB private vs 65 MB with the GPU driver loaded, same CPU,
            // and drawing does not stall when a render or sim saturates the GPU.
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            // Premultiplied alpha: transparent pixels let the Mica backdrop show through.
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: dpi as f32,
            dpiY: dpi as f32,
            ..Default::default()
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2D_SIZE_U { width: (rc.right - rc.left) as u32, height: (rc.bottom - rc.top) as u32 },
            // Do not block the UI thread on vblank; DWM composes the frame anyway.
            presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
        };
        let t = unsafe { self.factory.CreateHwndRenderTarget(&props, &hwnd_props)? };
        self.target = Some(t.clone());
        self.bitmaps.clear();
        Ok(t)
    }

    fn bitmap(&mut self, t: &ID2D1HwndRenderTarget, path: &str, icons: &HashMap<String, Arc<Icon>>) -> Option<ID2D1Bitmap> {
        if let Some(b) = self.bitmaps.get(path) {
            return b.clone();
        }
        let icon = icons.get(path)?; // not loaded yet: try again next paint
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let size = D2D_SIZE_U { width: icon.size, height: icon.size };
        let b = unsafe { t.CreateBitmap(size, Some(icon.bgra.as_ptr().cast()), icon.size * 4, &props) }.ok();
        self.bitmaps.insert(path.to_owned(), b.clone());
        b
    }

    fn text(&mut self, t: &ID2D1HwndRenderTarget, s: &str, f: usize, r: D2D_RECT_F, b: &ID2D1SolidColorBrush) {
        self.text_dim(t, s, f, r, b, None);
    }

    /// Draws `s` in `r`; from UTF-16 index `dim.0` on, in the `dim.1` brush.
    fn text_dim(&mut self, t: &ID2D1HwndRenderTarget, s: &str, f: usize, r: D2D_RECT_F, b: &ID2D1SolidColorBrush, dim: Option<(u32, &ID2D1SolidColorBrush)>) {
        let (w, h) = ((r.right - r.left).max(0.0), (r.bottom - r.top).max(0.0));
        let key = (f, (w * 4.0) as u32, (h * 4.0) as u32, dim.map_or(u32::MAX, |d| d.0), s.to_owned());
        let layout = match self.prev.remove(&key).or_else(|| self.layouts.get(&key).cloned()) {
            Some(l) => l,
            None => {
                let Ok(l) = (unsafe { self.dwrite.CreateTextLayout(&utf16(s), &self.formats[f], w, h) }) else { return };
                if let Some((start, brush)) = dim {
                    let _ = unsafe { l.SetDrawingEffect(brush, DWRITE_TEXT_RANGE { startPosition: start, length: u32::MAX - start }) };
                }
                l
            }
        };
        unsafe { t.DrawTextLayout(windows_numerics::Vector2 { X: r.left, Y: r.top }, &layout, b, D2D1_DRAW_TEXT_OPTIONS_CLIP) };
        self.layouts.insert(key, layout);
    }

    pub fn paint(&mut self, hwnd: HWND, dpi: u32, theme: &Theme, f: &Frame) -> Result<()> {
        let t = self.target(hwnd, dpi)?;
        let size = unsafe { t.GetSize() };
        let (fg, dim) = (theme.text, theme.text_secondary);
        let brush = |c: u32, a: f32| unsafe { t.CreateSolidColorBrush(&color(c, a), None) };
        let text = brush(fg, 1.0)?;
        let dim_b = brush(dim, 1.0)?;
        let accent = brush(theme.accent, 1.0)?;
        let wash = if theme.dark { 0xFFFFFF } else { 0x000000 };
        let hover = brush(wash, if theme.dark { 0.06 } else { 0.04 })?;
        let selected = brush(wash, if theme.dark { 0.09 } else { 0.06 })?;
        let line = brush(dim, 0.25)?;
        if self.dim != dim {
            (self.dim, self.layouts, self.prev) = (dim, HashMap::new(), HashMap::new()); // theme changed
        }
        let tb = f.table;
        let cols = columns(size.width, f.widths);

        unsafe {
            t.BeginDraw();
            t.Clear(Some(&color(0, 0.0))); // Mica shows through

            // Filter box, then exact totals.
            let r = D2D1_ROUNDED_RECT { rect: rect(PAD, 7.0, PAD + 240.0, 37.0), radiusX: 4.0, radiusY: 4.0 };
            t.FillRoundedRectangle(&r, &hover);
            t.DrawRoundedRectangle(&r, if tb.filter.is_empty() { &line } else { &accent }, 1.0, None);
            self.text(&t, "\u{E721}", GLYPH, rect(PAD + 4.0, 7.0, PAD + 28.0, 37.0), &dim_b);
            let (q, qb) = if tb.filter.is_empty() { ("Type to filter", &dim_b) } else { (tb.filter.as_str(), &text) };
            self.text(&t, q, NAME, rect(PAD + 30.0, 7.0, PAD + 232.0, 37.0), qb);
            let s = f.system;
            let totals = format!(
                "CPU {}  ·  Memory {:.1} / {:.0} GB  ·  GPU {}  ·  VRAM {:.1} / {:.0} GB  ·  Disk {} MB/s  ·  Network {} MB/s  ·  {} processes",
                pct(s.cpu),
                gb(s.ram_used),
                gb(s.ram_total),
                pct(s.gpu),
                gb(s.vram_used),
                gb(s.vram_total),
                mbs(s.disk_read + s.disk_write),
                mbs(s.net_recv + s.net_sent),
                f.processes,
            );
            self.text(&t, &totals, NUM, rect(PAD + 260.0, 7.0, size.width - PAD, 37.0), &dim_b);

            // Column headers: headroom total on top, name under it.
            let share = |used: u64, total: u64| if total > 0 { format!("{:.0}%", used as f64 * 100.0 / total as f64) } else { String::new() };
            for (c, &(x0, x1)) in COLS.iter().zip(&cols) {
                let (value, label) = match c {
                    Col::Name => (String::new(), "Name"),
                    Col::Pid => (String::new(), "PID"),
                    Col::Cpu => (pct(s.cpu), "CPU"),
                    Col::Ram => (share(s.ram_used, s.ram_total), "Memory"),
                    Col::Gpu => (pct(s.gpu), "GPU"),
                    Col::Vram => (share(s.vram_used, s.vram_total), "VRAM"),
                    Col::Disk => (format!("{} MB/s", mbs(s.disk_read + s.disk_write)), "Disk r / w"),
                };
                let (l, r) = (x0 + 8.0, x1 - 8.0);
                self.text(&t, &value, HEAD_VALUE, rect(l, TOP + 2.0, r, TOP + 26.0), &text);
                let sorted = tb.sort == *c;
                let fmt = if *c == Col::Name { HEAD_LABEL_LEFT } else { HEAD_LABEL };
                // Sorted: the arrow goes right of the label, so a right-aligned label moves left.
                let r_label = if sorted && *c != Col::Name { r - 14.0 } else { r };
                self.text(&t, label, fmt, rect(l, TOP + 26.0, r_label, TOP + HEAD - 4.0), if sorted { &accent } else { &dim_b });
                if sorted {
                    let g = if tb.desc { "\u{E70D}" } else { "\u{E70E}" };
                    let gx = if *c == Col::Name { l + 44.0 } else { r - 5.0 };
                    self.text(&t, g, GLYPH, rect(gx - 8.0, TOP + 26.0, gx + 8.0, TOP + HEAD - 4.0), &accent);
                }
            }
            t.FillRectangle(&rect(PAD, TOP + HEAD - 1.0, size.width - PAD, TOP + HEAD), &line);

            // Rows.
            let top = TOP + HEAD;
            t.PushAxisAlignedClip(&rect(0.0, top, size.width, size.height), D2D1_ANTIALIAS_MODE_ALIASED);
            let sel = tb.selected_index();
            let (name_x0, name_x1) = cols[0];
            for (i, row) in tb.rows.iter().enumerate().skip(tb.scroll).take(rows_visible(size.height) + 1) {
                let y = top + (i - tb.scroll) as f32 * ROW;
                let band = D2D1_ROUNDED_RECT { rect: rect(PAD - 4.0, y + 1.0, size.width - PAD - BAR + 4.0, y + ROW - 1.0), radiusX: 4.0, radiusY: 4.0 };
                if sel == Some(i) {
                    t.FillRoundedRectangle(&band, &selected);
                    let pill = D2D1_ROUNDED_RECT { rect: rect(PAD - 4.0, y + 8.0, PAD - 1.0, y + ROW - 8.0), radiusX: 1.5, radiusY: 1.5 };
                    t.FillRoundedRectangle(&pill, &accent);
                } else if tb.hover == Some(i) {
                    t.FillRoundedRectangle(&band, &hover);
                }
                let is_section = matches!(row.key, Key::Section(_));
                let mut x = chevron_x(tb, row, name_x0);
                if row.expandable {
                    let g = if row.expanded { "\u{E70D}" } else { "\u{E76C}" };
                    self.text(&t, g, GLYPH, rect(x, y, x + 16.0, y + ROW), &dim_b);
                }
                x += 20.0;
                if is_section {
                    self.text(&t, &format!("{} {}", row.label, row.detail), SECTION, rect(x, y, name_x1 - 8.0, y + ROW), &text);
                    continue;
                }
                let path = row.rep.map(|p| tb.procs[p].path.as_str()).unwrap_or("");
                match (!path.is_empty()).then(|| self.bitmap(&t, path, f.icons)).flatten() {
                    Some(b) => t.DrawBitmap(&b, Some(&rect(x, y + 6.0, x + 16.0, y + 22.0)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None),
                    None => t.DrawRoundedRectangle(
                        &D2D1_ROUNDED_RECT { rect: rect(x + 2.5, y + 8.5, x + 13.5, y + 19.5), radiusX: 2.0, radiusY: 2.0 },
                        &line,
                        1.0,
                        None,
                    ),
                }
                x += 24.0;
                // Label, then the detail (window title or member count) dimmed.
                let s = if row.detail.is_empty() { row.label.clone() } else { format!("{}  {}", row.label, row.detail) };
                let start = row.label.encode_utf16().count() as u32;
                self.text_dim(&t, &s, NAME, rect(x, y, name_x1 - 8.0, y + ROW), &text, Some((start, &dim_b)));
                let v = &row.v;
                let cells = [
                    row.pid.map(|p| p.to_string()).unwrap_or_default(),
                    pct(v.cpu),
                    bytes(v.ram),
                    pct(v.gpu),
                    bytes(v.vram),
                    match (mbs(v.read), mbs(v.write)) {
                        (r, w) if r == "0" && w == "0" => "0 MB/s".into(),
                        (r, w) => format!("{r} / {w} MB/s"),
                    },
                ];
                for (s, &(x0, x1)) in cells.iter().zip(&cols[1..]) {
                    self.text(&t, s, NUM, rect(x0 + 8.0, y, x1 - 8.0, y + ROW), &text);
                }
            }
            if let Some((y0, y1)) = thumb(size.height, tb.rows.len(), tb.scroll) {
                let x = size.width - BAR / 2.0;
                let r = D2D1_ROUNDED_RECT { rect: rect(x - 1.5, y0 + 2.0, x + 1.5, y1 - 2.0), radiusX: 1.5, radiusY: 1.5 };
                t.FillRoundedRectangle(&r, &dim_b);
            }
            t.PopAxisAlignedClip();

            self.prev = std::mem::take(&mut self.layouts);
            if t.EndDraw(None, None).is_err() {
                self.target = None; // device lost: recreate on the next paint
            }
        }
        Ok(())
    }
}
