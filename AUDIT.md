# Perf audit

## Run
Task Manager (before `install.ps1`: after it, taskmgr.exe opens Suprvisor):
```powershell
powershell -ExecutionPolicy Bypass -File audit/run.ps1
```
One UAC prompt. It runs the idle suite, then the load suite (about 8 minutes).
Exit Suprvisor first (`suprvisor.exe --exit`): its keyboard hook takes Ctrl+Shift+Esc.

Suprvisor (no elevation needed while it runs as the current user):
```powershell
powershell -ExecutionPolicy Bypass -File audit/run.ps1 --target suprvisor
```
`--launch exe` measures the IFEO path (start `suprvisor.exe`) instead of the hotkey.

Do not touch the mouse or keyboard while it runs: it sends real input.
Output: `results/<time>-<target>-{idle,load}.json` and `results/audit-<time>.log`.

Needs the .NET 10 SDK in `%LOCALAPPDATA%\Microsoft\dotnet` (user install, `dotnet-install.ps1 -Channel 10.0`).

## Why elevated
`Taskmgr.exe` always restarts itself elevated for an admin user, even with `RunAsInvoker`.
Windows UIPI then blocks input, cursor moves and process kills from a non-elevated harness.
Suprvisor also runs as admin. Launch capture alone works without admin.

## How it measures
Everything is measured on screen pixels, so the numbers mean the same for any app.
- Capture: DXGI Desktop Duplication of the target's window rectangle. Each frame carries the
  DWM present time (QPC). The harness stores the fraction of changed pixels per frame.
- A dark "curtain" window sits under the target, so only the target's pixels change.
- Launch: Ctrl+Shift+Esc via SendInput (t0), then 25 s of capture (5 s for Suprvisor, which
  shows its last snapshot at once). Suprvisor's "close" is a hide, so its launch is show-from-tray.
  - First paint: first frame that changes >2% of the window from the frame before.
  - VC90: first frame after which the window stays within 10% of its settled image.
  - VC90 content: the same, but a pixel counts as changed only when a channel moves by more than
    24/255. Mica's activation crossfade (~250 ms) shifts the backdrop tint by less, so this shows
    when the content (rows, text, icons) is complete.
- Sort: click the Name header (reverses the list) 10 times. Time from button-up to the first frame
  that changes >2% of the Name column's rows. Live numbers in other columns never change that
  area. A full reversal changes only 3-6% of Suprvisor's whole window, so a whole-window threshold
  misses it. Taskmgr's click point is fixed (`--sort-at x,y`, window relative px), because its
  XAML is not visible to UI Automation; Suprvisor's comes from its layout.
- Scroll: wheel events every 10 ms for 2 s, direction flipped every 10 notches so the list never
  hits an end; intervals between changed frames (p50/p99). The harness sets a 1 ms timer
  resolution, or `Thread.Sleep(10)` sleeps ~16 ms.
- Visible cost: CPU time over 10 s with the window open, as % of one core; private bytes.
- Load suite: all logical CPUs busy at normal priority, 16 GB RAM committed, GPU compute loop,
  70% of dedicated VRAM filled. All inside the harness process, so it stops with it.

## Budgets (Suprvisor)
Every metric must pass its fixed budget and must also beat the Task Manager baseline.

| Metric | Budget |
|---|---|
| Hotkey to visually complete (VC90) | < 50 ms |
| Sort click to sorted frame | < 10 ms (display is 100 Hz) |
| Scroll frame interval p99, under load | < 10 ms |
| Hidden | 1 wakeup/s, < 0.1% of one core |
| Visible | < 1% of one core |
| Private memory | < 50 MB |

Taskmgr has no hidden state (closed = 0 cost), so the hidden budget is absolute only.

## When
After each milestone, and after any change to the collector or the renderer.
A failed budget means the task is not done.

## Collector cost (M1, `cargo run -p suprvisor-core --release --example bench`)
Measured 2026-10-09 on the dev machine: 1,070 processes, 43,000 threads, CPU 96% busy (Houdini cook).
CPU per call, median: `system` 0.85 ms, `refresh` (30 busiest rows) 6.0 ms, `scan` 34 ms, `scan_full` 65 ms.
- Per-process CPU time needs a walk over the process's threads in the kernel, so cost follows the
  thread count. `NtQuerySystemInformation` also copies every thread (3.3 MB here), so it is the slowest.
- Global GPU engine counters cost 1-2.6 M cycles each; only busy engines are read on the fast tick.
- With a 500 ms fast tick, `scan` every 3 s, `scan_full` every 60 s: about 2.6% of one core
  (Task Manager: about 50-60%). Above the 1% visible budget on this machine; see open points.

## Suprvisor results (M3/M4, 2026-10-09)
All runs had the machine at 60-100% CPU from a Houdini cook (not the synthetic load). Numbers
move by +-1% of a core and by tens of ms between runs. A clean idle run and the load suite are pending.

| Metric | Budget | Suprvisor | Notes |
|---|---|---|---|
| Hotkey to first frame / VC90 content | < 50 ms | 30-66 ms, median 38 | `ShowWindow` alone blocks 30-70 ms in the activation handshake |
| VC90 (strict) | < 50 ms | ~300 ms | Mica's activation crossfade; the content is complete in the first frame |
| Sort | < 10 ms | 10-38 ms, median ~20 | ~3 ms input, ~5 ms rebuild + paint, then the next vblank; outliers to 1.2 s under the cook |
| Scroll p50 / p99 | p99 < 10 ms | 10.1 / 20.8 ms | one frame per notch at 100 Hz; p99 = one missed vblank |
| Hidden | < 0.1% | 0.091% (60 s, by CPU cycles) | v1.1: one totals sample per second (~0.45 ms) plus a full GPU engine read every 60 s (~11 ms) |
| Visible, Processes | < 1% | 2.7-3.4% | the collector alone is ~2-3% (see above) |
| Visible, Performance | < 1% | 0.94% | v1.1: one sample and one repaint per second |
| Private memory | < 50 MB | 23-25 MB | single-threaded WARP (was 35-46 MB) |

What changed the numbers:
- Text layouts cached across frames: paint 6 ms to 3.5 ms.
- Flip-model swapchain through DirectComposition (instead of an HWND render target, which DWM
  shows one or two vblanks late): sort median 30 ms to 19 ms, scroll one frame per notch.
- Keyboard hook for Ctrl+Shift+Esc instead of a process start: first frame median ~70 to 38 ms.
- WARP without its worker pool (`D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS`):
  one worker per core woke on every frame. Performance tab 3.7% to 1.8% (then 0.94% at 1 Hz),
  private memory -8 MB. Table paint 3.4 to 4.4 ms; an A/B audit showed the same sort and scroll
  latency.
- PDH for disk and network cost ~4 ms per read; `IOCTL_DISK_PERFORMANCE` and `GetIfEntry2` on the
  physical adapters cost ~0.35 ms.
- Bug found by the hidden measurement: at `--resident` start, the creation `WM_SIZE` told the
  collector the window was visible, so a hidden instance listed processes every 500 ms until the
  first show/hide.
- Tried and dropped: hiding by DWM cloak (the Mica crossfade comes from activation, not from
  showing); `SW_SHOWNA` before activation (no gain); D2D software rendering copied into the
  swapchain through WIC (same CPU as WARP, more code).
