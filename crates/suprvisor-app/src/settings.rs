//! View settings kept between runs, in `%APPDATA%\suprvisor\settings.ini` (key=value lines).

use std::path::PathBuf;

use crate::table::{Col, NUMS};

pub struct Settings {
    pub sort: Col,
    pub desc: bool,
    pub grouped: bool,
    /// The Performance tab is open (else Processes).
    pub performance: bool,
    /// The navigation pane shows labels (else icons only).
    pub nav: bool,
    /// Widths in DIPs of the columns after Name (CPU, RAM, GPU, VRAM, Disk, PID). Name takes the rest.
    pub widths: [f32; 6],
    /// Display order of the columns after Name (Name stays first).
    pub order: [Col; 6],
    /// Hidden columns, by `Col::num`.
    pub hidden: [bool; 6],
    /// Normal (restored) window rect in pixels, and whether the window was maximized.
    pub window: Option<([i32; 4], bool)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { sort: Col::Name, desc: false, grouped: true, performance: false, nav: false, widths: DEFAULT_WIDTHS, order: NUMS, hidden: [false; 6], window: None }
    }
}

pub const DEFAULT_WIDTHS: [f32; 6] = [72.0, 96.0, 64.0, 96.0, 128.0, 64.0];

fn file() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join(if crate::test() { "suprvisor-test" } else { "suprvisor" }).join("settings.ini"))
}

fn nums<T: std::str::FromStr, const N: usize>(v: &str) -> Option<[T; N]> {
    let parts: Vec<T> = v.split(',').map(|s| s.trim().parse().ok()).collect::<Option<_>>()?;
    parts.try_into().ok()
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Self::default();
        let text = file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "sort" => s.sort = Col::from_id(v.trim()).unwrap_or(s.sort),
                "desc" => s.desc = v.trim() == "1",
                "grouped" => s.grouped = v.trim() != "0",
                "tab" => s.performance = v.trim() == "performance",
                "nav" => s.nav = v.trim() == "open",
                "columns" => s.widths = nums(v).unwrap_or(s.widths).map(|w: f32| w.clamp(32.0, 600.0)),
                "order" => {
                    let o: Vec<Col> = v.split(',').filter_map(|c| Col::from_id(c.trim())).filter(|c| c.num().is_some()).collect();
                    // Each column once, or the file is ignored.
                    if o.len() == 6 && NUMS.iter().all(|c| o.contains(c)) {
                        s.order.copy_from_slice(&o);
                    }
                }
                "hidden" => {
                    for c in v.split(',').filter_map(|c| Col::from_id(c.trim())) {
                        if let Some(i) = c.num() {
                            s.hidden[i] = true;
                        }
                    }
                }
                "window" => s.window = nums::<i32, 5>(v).map(|[l, t, r, b, max]| ([l, t, r, b], max != 0)),
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let Some(f) = file() else { return };
        let w = self.widths.map(|w| w.round().to_string()).join(",");
        let tab = if self.performance { "performance" } else { "processes" };
        let nav = if self.nav { "open" } else { "closed" };
        let order = self.order.map(|c| c.id()).join(",");
        let hidden: Vec<&str> = NUMS.iter().zip(self.hidden).filter(|(_, h)| *h).map(|(c, _)| c.id()).collect();
        let mut text = format!(
            "sort={}\ndesc={}\ngrouped={}\ntab={tab}\nnav={nav}\ncolumns={w}\norder={order}\nhidden={}\n",
            self.sort.id(),
            self.desc as u8,
            self.grouped as u8,
            hidden.join(",")
        );
        if let Some(([l, t, r, b], max)) = self.window {
            text += &format!("window={l},{t},{r},{b},{}\n", max as u8);
        }
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(f, text);
    }
}
