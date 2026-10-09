//! Direct2D + DirectWrite drawing. Layout is in DIPs; the render target scales to the window DPI.

use suprvisor_core::System;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{Result, w};

use crate::theme::Theme;

pub struct Renderer {
    factory: ID2D1Factory1,
    body: IDWriteTextFormat,
    value: IDWriteTextFormat,
    label: IDWriteTextFormat,
    target: Option<ID2D1HwndRenderTarget>, // created on first paint, dropped when the device is lost
}

fn format(dwrite: &IDWriteFactory, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
    let f = unsafe {
        dwrite.CreateTextFormat(
            w!("Segoe UI Variable Text"),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-us"),
        )?
    };
    unsafe { f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)? };
    Ok(f)
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn color(c: u32) -> D2D1_COLOR_F {
    let ch = |s: u32| ((c >> s) & 0xFF) as f32 / 255.0;
    D2D1_COLOR_F { r: ch(16), g: ch(8), b: ch(0), a: 1.0 }
}

fn gb(b: u64) -> f64 {
    b as f64 / (1u64 << 30) as f64
}

fn rate(bytes_per_sec: u64) -> String {
    let mb = bytes_per_sec as f64 / 1e6;
    if mb >= 10.0 { format!("{mb:.0} MB/s") } else { format!("{mb:.1} MB/s") }
}

impl Renderer {
    pub fn new() -> Result<Self> {
        let factory: ID2D1Factory1 = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Self {
            body: format(&dwrite, 14.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            value: format(&dwrite, 20.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            label: format(&dwrite, 12.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            factory,
            target: None,
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
            // Premultiplied alpha: transparent pixels let the Mica backdrop show through.
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            // Software (WARP): 21 MB private vs 65 MB with the GPU driver loaded, same CPU,
            // and drawing does not stall when a render or sim saturates the GPU.
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
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
        Ok(t)
    }

    pub fn paint(&mut self, hwnd: HWND, dpi: u32, theme: &Theme, sys: &System, processes: usize) -> Result<()> {
        let t = self.target(hwnd, dpi)?;
        let size = unsafe { t.GetSize() };
        let text = unsafe { t.CreateSolidColorBrush(&color(theme.text), None)? };
        let dim = unsafe { t.CreateSolidColorBrush(&color(theme.text_secondary), None)? };
        let accent = unsafe { t.CreateSolidColorBrush(&color(theme.accent), None)? };

        unsafe {
            t.BeginDraw();
            t.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 })); // Mica shows through

            // Headroom strip: one cell per resource, value on top, label under it.
            let cells = [
                (format!("{:.0}%", sys.cpu), "CPU".to_owned()),
                (format!("{:.1} / {:.0} GB", gb(sys.ram_used), gb(sys.ram_total)), "Memory".to_owned()),
                (format!("{:.0}%", sys.gpu), "GPU".to_owned()),
                (format!("{:.1} / {:.0} GB", gb(sys.vram_used), gb(sys.vram_total)), "VRAM".to_owned()),
                (rate(sys.disk_read + sys.disk_write), "Disk".to_owned()),
                (rate(sys.net_recv + sys.net_sent), "Network".to_owned()),
            ];
            let cell_w = ((size.width - 32.0) / cells.len() as f32).max(80.0);
            for (i, (v, l)) in cells.iter().enumerate() {
                let x = 16.0 + i as f32 * cell_w;
                let v: Vec<u16> = v.encode_utf16().collect();
                let l: Vec<u16> = l.encode_utf16().collect();
                t.DrawText(&v, &self.value, &rect(x, 16.0, x + cell_w, 44.0), &text, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                t.DrawText(&l, &self.label, &rect(x, 46.0, x + cell_w, 64.0), &dim, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
            t.FillRectangle(&rect(16.0, 76.0, size.width - 16.0, 77.0), &dim);
            let line: Vec<u16> = format!("{processes} processes").encode_utf16().collect();
            t.DrawText(&line, &self.body, &rect(16.0, 88.0, size.width - 16.0, 110.0), &accent, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);

            if t.EndDraw(None, None).is_err() {
                self.target = None; // device lost: recreate on the next paint
            }
        }
        Ok(())
    }
}
