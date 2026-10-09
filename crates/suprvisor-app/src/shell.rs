//! Small Win32 services for the table: top-level windows, exe icons, clipboard, Explorer.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING};

/// Title of each process's top-level app window (visible, not owned, not a tool window, not cloaked).
/// Processes with one count as Apps, like in Task Manager.
pub fn window_titles() -> HashMap<u32, String> {
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        let out = unsafe { &mut *(lp.0 as *mut HashMap<u32, String>) };
        unsafe {
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&raw mut cloaked).cast(), 4);
            if !IsWindowVisible(hwnd).as_bool() || GetWindow(hwnd, GW_OWNER).is_ok() || ex & WS_EX_TOOLWINDOW.0 != 0 || cloaked != 0 {
                return true.into();
            }
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(hwnd, &mut buf);
            if n > 0 {
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                // EnumWindows goes top to bottom in z-order: keep the topmost title.
                out.entry(pid).or_insert_with(|| String::from_utf16_lossy(&buf[..n as usize]));
            }
        }
        true.into()
    }
    let mut out = HashMap::new();
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&raw mut out as isize)) };
    out
}

/// Premultiplied BGRA pixels, top row first.
pub struct Icon {
    pub size: u32,
    pub bgra: Vec<u8>,
}

/// The exe's first icon, at the system's large icon size (32 px at 100%).
pub fn icon(path: &str) -> Option<Icon> {
    let mut h = HICON::default();
    if unsafe { ExtractIconExW(&HSTRING::from(path), 0, Some(&mut h), None, 1) } == 0 || h.is_invalid() {
        return None;
    }
    let pixels = pixels(h);
    let _ = unsafe { DestroyIcon(h) };
    pixels
}

fn pixels(h: HICON) -> Option<Icon> {
    let mut info = ICONINFO::default();
    unsafe { GetIconInfo(h, &mut info) }.ok()?;
    let mut bm = BITMAP::default();
    unsafe { GetObjectW(info.hbmColor.into(), size_of::<BITMAP>() as i32, Some((&raw mut bm).cast())) };
    let size = bm.bmWidth.max(0) as u32;
    let read = |b: HBITMAP| {
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            ..Default::default()
        };
        let mut px = vec![0u8; (size * size * 4) as usize];
        let dc = unsafe { GetDC(None) };
        let n = unsafe { GetDIBits(dc, b, 0, size, Some(px.as_mut_ptr().cast()), &mut bi, DIB_RGB_COLORS) };
        unsafe { ReleaseDC(None, dc) };
        (n == size as i32).then_some(px)
    };
    let color = if size > 0 && !info.hbmColor.is_invalid() { read(info.hbmColor) } else { None };
    let mask = color.as_ref().and_then(|c| c.chunks(4).all(|p| p[3] == 0).then(|| read(info.hbmMask)).flatten());
    unsafe {
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
    }
    let mut bgra = color?;
    for (i, p) in bgra.chunks_mut(4).enumerate() {
        // Old icons have no alpha: the AND mask says which pixels are transparent.
        if let Some(m) = &mask {
            p[3] = if m[i * 4] == 0 { 255 } else { 0 };
        }
        let a = p[3] as u32;
        for c in &mut p[..3] {
            *c = (*c as u32 * a / 255) as u8;
        }
    }
    Some(Icon { size, bgra })
}

pub fn copy(hwnd: HWND, text: &str) {
    let w: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let dst = GlobalLock(mem) as *mut u16;
            if !dst.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), dst, w.len());
                let _ = GlobalUnlock(mem);
                let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

/// Opens Explorer on the exe's folder with the file selected.
pub fn open_location(path: &str) {
    let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{path}\"")).spawn();
}
