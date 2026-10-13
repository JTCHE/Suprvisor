//! The swapchain and the Direct2D device context that draws into it.

use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{Interface, Result};

use super::pixel_format;

/// A flip-model swapchain shown through DirectComposition. DWM picks a flip-model frame up at
/// the next vblank; the GDI-redirected HWND render target took one to two more (~16 ms at 100 Hz).
/// Composition swapchains also carry per-pixel alpha, so Mica shows through.
pub(super) struct Target {
    pub(super) dc: ID2D1DeviceContext,
    pub(super) swapchain: IDXGISwapChain1,
    /// Signaled when the swapchain can take a frame. Waiting on it before drawing replaces the
    /// wait inside Present, so the UI thread waits only where it can also take input.
    pub(super) waitable: HANDLE,
    pub(super) dpi: u32,
    _composition: (IDCompositionDevice, IDCompositionTarget, IDCompositionVisual),
}

impl Target {
    pub(super) fn new(factory: &ID2D1Factory1, hwnd: HWND, dpi: u32) -> Result<Self> {
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
    pub(super) fn bind(&self) -> Result<()> {
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

    pub(super) fn resize(&self, width: u32, height: u32) -> Result<()> {
        unsafe {
            self.dc.SetTarget(None);
            self.swapchain.ResizeBuffers(0, width.max(1), height.max(1), DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT)?;
        }
        self.bind()
    }

    /// Writes the drawn back buffer to a 32-bit BMP: the test harness reads the frame from the app,
    /// so it needs no screen capture and the window can stay off screen and in the background.
    pub(super) fn save(&self, path: &str) -> Result<()> {
        let sc = &self.swapchain;
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
}

impl Drop for Target {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.waitable) };
    }
}
