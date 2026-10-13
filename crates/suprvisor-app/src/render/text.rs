//! Text: formats, cached layouts, the search text, and the strings of the cells.

use suprvisor_core::System;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{PCWSTR, Result, w};

use std::collections::HashMap;

use super::*;
use crate::table::{Col, Row};

pub(super) const NAME: usize = 0;
pub(super) const SECTION: usize = 1;
pub(super) const NUM: usize = 2;
pub(super) const HEAD_VALUE: usize = 3;
pub(super) const HEAD_LABEL: usize = 4;
pub(super) const HEAD_LABEL_LEFT: usize = 5;
pub(super) const GLYPH: usize = 6;
pub(super) const ICON: usize = 7; // navigation and button icons
pub(super) const SEARCH: usize = 8; // the search text: no trimming
pub(super) const TINY: usize = 9; // core numbers on the CPU cores graphs
pub(super) const TINY_VALUE: usize = 10;

/// Format, box size in 1/4 DIPs, where the dimmed part starts, text.
pub(super) type LayoutKey = (usize, u32, u32, u32, String);

pub(super) fn format(dwrite: &IDWriteFactory, font: PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT) -> Result<IDWriteTextFormat> {
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

pub(super) fn mbs(b: u64) -> String {
    let mb = b as f64 / 1e6;
    if mb < 0.05 {
        "0".into()
    } else if mb >= 10.0 {
        format!("{mb:.0}")
    } else {
        format!("{mb:.1}")
    }
}

pub(super) fn gb(b: u64) -> f64 {
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

    pub(super) fn text(&mut self, t: &ID2D1DeviceContext, s: &str, f: usize, r: D2D_RECT_F, b: &ID2D1SolidColorBrush) {
        self.text_dim(t, s, f, r, b, None);
    }

    /// Draws `s` in `r`; from UTF-16 index `dim.0` on, in the `dim.1` brush.
    pub(super) fn text_dim(&mut self, t: &ID2D1DeviceContext, s: &str, f: usize, r: D2D_RECT_F, b: &ID2D1SolidColorBrush, dim: Option<(u32, &ID2D1SolidColorBrush)>) {
        if let Some(layout) = self.layout(s, f, r, dim) {
            unsafe { t.DrawTextLayout(v2(r.left, r.top), &layout, b, D2D1_DRAW_TEXT_OPTIONS_CLIP) };
        }
    }

    /// The layout of `s` in `r`, from this frame's or the previous frame's cache.
    pub(super) fn layout(&mut self, s: &str, f: usize, r: D2D_RECT_F, dim: Option<(u32, &ID2D1SolidColorBrush)>) -> Option<IDWriteTextLayout> {
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
    pub(super) fn search_layout(&mut self, s: &str) -> Option<IDWriteTextLayout> {
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
    pub(super) fn search_x(&mut self, s: &str, pos: u32) -> f32 {
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
}
