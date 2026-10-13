//! Light or dark, and the accent color, from the Windows personalization settings.

use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::{PCWSTR, w};

/// Colors as 0xRRGGBB.
pub struct Theme {
    pub dark: bool,
    pub text: u32,
    pub text_secondary: u32,
    pub accent: u32,
    /// The solid content layer; Mica shows around it (title bar, navigation pane).
    pub layer: u32,
}

fn dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let (mut v, mut size) = (0u32, 4u32);
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key, value, RRF_RT_REG_DWORD, None, Some((&raw mut v).cast()), Some(&mut size)) };
    r.is_ok().then_some(v)
}

impl Theme {
    pub fn current() -> Self {
        let dark = dword(w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"), w!("AppsUseLightTheme")) == Some(0);
        // AccentColor is 0xAABBGGRR.
        let accent = dword(w!(r"Software\Microsoft\Windows\DWM"), w!("AccentColor"))
            .map(|c| (c & 0xFF) << 16 | (c & 0xFF00) | (c >> 16) & 0xFF)
            .unwrap_or(0x0078D4);
        if dark {
            Self { dark, text: 0xFFFFFF, text_secondary: 0xC5C5C5, accent, layer: 0x191919 }
        } else {
            Self { dark, text: 0x1A1A1A, text_secondary: 0x5D5D5D, accent, layer: 0xF9F9F9 }
        }
    }
}
