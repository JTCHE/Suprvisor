//! Title bar, search box, navigation pane, page header, focus rings.

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;

use super::text::*;
use super::*;

/// The brushes of one frame.
pub(super) struct Brushes {
    pub(super) text: ID2D1SolidColorBrush,
    pub(super) dim: ID2D1SolidColorBrush,
    pub(super) strong: ID2D1SolidColorBrush, // control borders
    pub(super) off: ID2D1SolidColorBrush,    // disabled text
    pub(super) accent: ID2D1SolidColorBrush,
    pub(super) hover: ID2D1SolidColorBrush,
    pub(super) selected: ID2D1SolidColorBrush,
    pub(super) band: ID2D1SolidColorBrush, // the sorted column, under the rows
    pub(super) line: ID2D1SolidColorBrush,
    pub(super) layer: ID2D1SolidColorBrush,
    pub(super) lifted: ID2D1SolidColorBrush, // a dragged column
    pub(super) shadow: ID2D1SolidColorBrush,
    pub(super) close: ID2D1SolidColorBrush, // close button, hovered
    pub(super) white: ID2D1SolidColorBrush,
    pub(super) ring: ID2D1SolidColorBrush, // keyboard focus
    pub(super) ring_in: ID2D1SolidColorBrush,
    pub(super) fill: ID2D1LinearGradientBrush, // area under a graph line
}

impl Brushes {
    pub(super) fn new(t: &ID2D1DeviceContext, theme: &Theme) -> Result<Self> {
        let dim = theme.text_secondary;
        let brush = |c: u32, a: f32| unsafe { t.CreateSolidColorBrush(&color(c, a), None) };
        let wash = if theme.dark { 0xFFFFFF } else { 0x000000 };
        let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: color(theme.accent, 0.38) }, D2D1_GRADIENT_STOP { position: 1.0, color: color(theme.accent, 0.04) }];
        let fill = unsafe {
            let s = ID2D1RenderTarget::CreateGradientStopCollection(t, &stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let p = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: v2(0.0, 0.0), endPoint: v2(0.0, 1.0) };
            t.CreateLinearGradientBrush(&p, None, &s)?
        };
        Ok(Self {
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
        })
    }
}

impl Renderer {
    /// The keyboard focus ring of Windows 11: a 2 DIP outer stroke and a 1 DIP inner one.
    pub(super) fn ring(&self, t: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, b: &Brushes) {
        unsafe {
            t.DrawRoundedRectangle(&rounded(r.left - 1.0, r.top - 1.0, r.right + 1.0, r.bottom + 1.0, radius + 1.0), &b.ring, 2.0, None);
            t.DrawRoundedRectangle(&rounded(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5, (radius - 0.5).max(0.0)), &b.ring_in, 1.0, None);
        }
    }

    pub(super) fn ring_on(&self, f: &Frame, focus: Focus) -> bool {
        f.focus_visible && f.focus == Some(focus)
    }

    /// The search box: text, selection, caret, clear button.
    pub(super) fn search_box(&mut self, t: &ID2D1DeviceContext, w: f32, f: &Frame, b: &Brushes) {
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
        self.search_content(t, f, b, (tl, tr));
    }

    /// The search text, scrolled so the caret stays inside the box, with its selection and caret.
    fn search_content(&mut self, t: &ID2D1DeviceContext, f: &Frame, b: &Brushes, (tl, tr): (f32, f32)) {
        let (st, sb) = SEARCH_Y;
        let focused = f.focus == Some(Focus::Search);
        let e = f.search;
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
    pub(super) fn chrome(&mut self, t: &ID2D1DeviceContext, size: D2D_SIZE_F, f: &Frame, b: &Brushes) {
        let (w, h) = (size.width, size.height);
        let cl = f.nav_w;
        unsafe {
            // Content layer: solid, rounded at its top-left corner. Its other corners are off screen.
            t.FillRoundedRectangle(&rounded(cl, TITLE, w + 8.0, h + 8.0, 8.0), &b.layer);
            t.DrawRoundedRectangle(&rounded(cl + 0.5, TITLE + 0.5, w + 8.0, h + 8.0, 8.0), &b.line, 1.0, None);
        }
        self.title_bar(t, w, f, b);
        self.nav_pane(t, h, f, b);
        self.page_header(t, w, f, b);
    }

    /// App icon and name, search box, caption buttons.
    fn title_bar(&mut self, t: &ID2D1DeviceContext, w: f32, f: &Frame, b: &Brushes) {
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
    }

    fn nav_pane(&mut self, t: &ID2D1DeviceContext, h: f32, f: &Frame, b: &Brushes) {
        let cl = f.nav_w;
        // Navigation pane. Labels show as it opens; the pill slides to the open page.
        unsafe { t.PushAxisAlignedClip(&rect(0.0, TITLE, cl, h), D2D1_ANTIALIAS_MODE_ALIASED) };
        let glyphs = ["\u{E700}", "\u{E71D}", "\u{E9D9}", "\u{EE40}"];
        for (k, (glyph, label)) in glyphs.into_iter().zip(NAV_LABELS).enumerate() {
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
    }

    /// A navigation item's name next to the closed pane, in the style of a Windows tooltip.
    pub(super) fn tooltip(&mut self, t: &ID2D1DeviceContext, height: f32, k: usize, f: &Frame, b: &Brushes) {
        let label = NAV_LABELS[k];
        let (top, bot) = nav_row(k, height);
        let (l, cy) = (f.nav_w + 6.0, (top + bot) / 2.0);
        let r = rect(l, cy - 15.0, l + self.width(label, HEAD_LABEL_LEFT).ceil() + 18.0, cy + 15.0);
        unsafe {
            t.FillRoundedRectangle(&rounded(r.left - 1.0, r.top + 1.0, r.right + 1.0, r.bottom + 2.0, 5.0), &b.shadow);
            t.FillRoundedRectangle(&rounded(r.left, r.top, r.right, r.bottom, 4.0), &b.lifted);
            t.DrawRoundedRectangle(&rounded(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5, 4.0), &b.line, 1.0, None);
        }
        self.text(t, label, HEAD_LABEL_LEFT, rect(r.left + 9.0, r.top, r.right, r.bottom), &b.text);
    }

    /// Page title, process count, Run new task and End task.
    fn page_header(&mut self, t: &ID2D1DeviceContext, w: f32, f: &Frame, b: &Brushes) {
        let cl = f.nav_w;
        let x = cl + PAD + 4.0;
        self.text(t, if f.performance { "Performance" } else { "Processes" }, SECTION, rect(x, TITLE, x + 160.0, TOP), &b.text);
        if !f.performance {
            self.text(t, &format!("{} processes", f.processes), NAME, rect(x + 96.0, TITLE, x + 260.0, TOP), &b.dim);
        }
        let mut buttons = vec![("\u{E768}", "Run new task".to_owned())];
        if !f.performance {
            buttons.push(("\u{E733}", if f.ending > 1 { format!("End {} tasks", f.ending) } else { "End task".to_owned() }));
        }
        let widths: Vec<f32> = buttons.iter().map(|(_, label)| self.width(label, NAME).ceil()).collect();
        self.commands = commands(w, &widths);
        for (k, r) in self.commands.clone() {
            let enabled = k == 0 || f.can_end;
            if enabled && f.hot == Hot::Command(k) {
                unsafe { t.FillRoundedRectangle(&rounded(r.left, r.top, r.right, r.bottom, 4.0), &b.hover) };
            }
            let fg = if enabled { &b.text } else { &b.off };
            let (glyph, label) = &buttons[k];
            self.text(t, glyph, ICON, rect(r.left + COMMAND_PAD - 4.0, r.top, r.left + COMMAND_PAD + 20.0, r.bottom), fg);
            self.text(t, label, NAME, rect(r.left + COMMAND_LABEL_X, r.top, r.right, r.bottom), fg);
            if self.ring_on(f, Focus::Command(k)) {
                self.ring(t, r, 4.0, b);
            }
        }
        unsafe { t.FillRectangle(&rect(cl + 1.0, TOP - 1.0, w, TOP), &b.line) };
    }
}
