# Perf audit

## Run
```powershell
powershell -ExecutionPolicy Bypass -File audit/run.ps1
```
One UAC prompt. It runs the idle suite, then the load suite (about 8 minutes).
Do not touch the mouse or keyboard while it runs: it sends real input.
Output: `results/<time>-taskmgr-{idle,load}.json` and `results/audit-<time>.log`.

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
- Launch: Ctrl+Shift+Esc via SendInput (t0), then 25 s of capture.
  - First paint: first frame that differs >2% from the screen before t0.
  - VC90: first frame after which the window stays within 10% of its settled image at 25 s.
- Sort: click the Name header (reverses the list) 10 times. Time from button-up to the first
  frame that changes >10% of the window. The click point is fixed (`--sort-at x,y`, window
  relative px), because taskmgr's XAML is not visible to UI Automation.
- Scroll: wheel events every 10 ms for 2 s; intervals between changed frames (p50/p99).
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
