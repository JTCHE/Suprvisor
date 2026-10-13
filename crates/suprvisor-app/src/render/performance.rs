//! The Performance tab: 60 s graphs and one small graph per logical processor.

use std::collections::VecDeque;

use suprvisor_core::System;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::core::Result;

use super::text::*;
use super::*;

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
/// with a width about `aspect` times their height. A count that fills every row wins when its
/// cells are at most 15% smaller.
pub fn grid(n: usize, w: f32, h: f32, aspect: f32) -> usize {
    let n = n.max(1);
    let size = |c: usize| (w / c as f32 / aspect).min(h / n.div_ceil(c) as f32);
    let by_size = |&a: &usize, &b: &usize| size(a).total_cmp(&size(b));
    let best = (1..=n).max_by(by_size).unwrap_or(1);
    (1..=n).filter(|c| n % c == 0 && size(*c) >= 0.85 * size(best)).max_by(by_size).unwrap_or(best)
}

impl Renderer {
    /// A path through `pts`; closed and filled, or open for a line.
    pub(super) fn path(&self, pts: &[windows_numerics::Vector2], filled: bool) -> Result<ID2D1PathGeometry1> {
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
    pub(super) fn plot(&self, t: &ID2D1DeviceContext, r: D2D_RECT_F, series: &[f64], max: f64, b: &Brushes, grid: bool, width: f32) -> Result<()> {
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

    /// The Performance tab: six cards with 60 s graphs. The CPU card shows the overall graph or
    /// one small graph per logical processor.
    pub(super) fn performance(&mut self, t: &ID2D1DeviceContext, size: D2D_SIZE_F, f: &Frame, b: &Brushes) -> Result<()> {
        let cards = cards(f.nav_w, size.width, size.height);
        for (i, (g, c)) in graphs(f.system, f.history).iter().zip(&cards).enumerate() {
            let (l, top, r) = (c.left, c.top, c.right);
            unsafe { t.FillRoundedRectangle(&rounded(l, top, r, c.bottom, 6.0), &b.hover) };
            self.text(t, g.name, SECTION, rect(l + 12.0, top + 6.0, r - 12.0, top + 30.0), &b.text);
            self.text(t, &g.value, HEAD_VALUE, rect(l + 12.0, top + 6.0, r - 12.0, top + 30.0), &b.text);
            self.text(t, &g.detail, HEAD_LABEL_LEFT, rect(l + 12.0, top + 30.0, r - 12.0, top + 48.0), &b.dim);
            let area = rect(l + 12.0, top + 58.0, r - 12.0, c.bottom - 12.0);
            if area.bottom - area.top < 8.0 {
                continue;
            }
            if i == 0 && f.cores {
                self.cores(t, area, f, b)?;
            } else {
                self.plot(t, area, &g.series, g.max, b, true, 1.5)?;
            }
        }
        self.graph_switch(t, cards[0], f, b);
        Ok(())
    }

    /// One small graph per logical processor, in a grid that fills `area`.
    fn cores(&mut self, t: &ID2D1DeviceContext, area: D2D_RECT_F, f: &Frame, b: &Brushes) -> Result<()> {
        const CELL_GAP: f32 = 4.0;
        let cores = &f.system.cores;
        let n = cores.len();
        if n == 0 {
            return Ok(());
        }
        let (aw, ah) = (area.right - area.left, area.bottom - area.top);
        let c = grid(n, aw, ah, 1.6);
        let r = n.div_ceil(c);
        let cw = (aw - CELL_GAP * (c - 1) as f32) / c as f32;
        let ch = (ah - CELL_GAP * (r - 1) as f32) / r as f32;
        for (k, v) in cores.iter().enumerate() {
            let l = area.left + (k % c) as f32 * (cw + CELL_GAP);
            let tp = area.top + (k / c) as f32 * (ch + CELL_GAP);
            let series: Vec<f64> = f.history.iter().map(|s| s.cores.get(k).copied().unwrap_or(0.0) as f64).collect();
            self.plot(t, rect(l, tp, l + cw, tp + ch), &series, 100.0, b, false, 1.0)?;
            if ch >= 22.0 && cw >= 44.0 {
                self.text(t, &format!("{v:.0}%"), TINY_VALUE, rect(l + 4.0, tp + 1.0, l + cw - 4.0, tp + 14.0), &b.dim);
            }
        }
        Ok(())
    }

    /// The Overall / Per core switch on the CPU card.
    fn graph_switch(&mut self, t: &ID2D1DeviceContext, card: D2D_RECT_F, f: &Frame, b: &Brushes) {
        let segs = graph_modes(card);
        let all = rect(segs[0].left, segs[0].top, segs[1].right, segs[1].bottom);
        unsafe { t.DrawRoundedRectangle(&rounded(all.left + 0.5, all.top + 0.5, all.right - 0.5, all.bottom - 0.5, 4.0), &b.line, 1.0, None) };
        for (k, r) in segs.iter().enumerate() {
            let on = (k == 1) == f.cores;
            let fill = if on { Some(&b.selected) } else if f.hot == Hot::GraphMode(k) { Some(&b.hover) } else { None };
            if let Some(fill) = fill {
                unsafe { t.FillRoundedRectangle(&rounded(r.left + 2.0, r.top + 2.0, r.right - 2.0, r.bottom - 2.0, 3.0), fill) };
            }
            let label = GRAPH_MODES[k];
            let inset = ((r.right - r.left - self.width(label, HEAD_LABEL_LEFT)) / 2.0).max(0.0);
            self.text(t, label, HEAD_LABEL_LEFT, rect(r.left + inset, r.top, r.right, r.bottom), if on { &b.text } else { &b.dim });
        }
        if self.ring_on(f, Focus::GraphMode) {
            self.ring(t, all, 4.0, b);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn graph_scale_is_readable() {
        assert_eq!(super::nice(0.0), 1.0);
        assert_eq!(super::nice(1e6), 1e6);
        assert_eq!(super::nice(1.2e6), 2e6);
        assert_eq!(super::nice(3e6), 5e6);
        assert_eq!(super::nice(7e6), 1e7);
        assert_eq!(super::grid(24, 1100.0, 140.0, 1.6), 12, "24 cores in a wide strip: two rows of 12");
        assert_eq!(super::grid(1, 100.0, 100.0, 1.6), 1);
        assert_eq!(super::grid(24, 338.0, 200.0, 1.6), 6, "a CPU card: four full rows of 6, not five rows of 5");
    }
}