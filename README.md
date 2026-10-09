# Suprvisor

A fast replacement for the Windows 11 Task Manager, written in Rust.

The stock Task Manager lags at launch, freezes for over a second when you sort a column,
and draws broken rows while it loads (see `docs/taskmgr-render-glitch.png`). It also hides
per-process VRAM use. Suprvisor stays resident, opens on Ctrl+Shift+Esc in one frame,
and shows CPU, RAM, GPU and VRAM per process.

Status: M0-M4 built (audit harness, collector, resident window, process table, hotkey and
install). The Task Manager baseline run and the first install need a UAC click. See `AUDIT.md`.

## Install
```powershell
powershell -ExecutionPolicy Bypass -File install.ps1
```
Builds, copies to `C:\Program Files\Suprvisor`, adds a logon task that starts it hidden and
elevated, and redirects `taskmgr.exe` to it (IFEO). Ctrl+Shift+Esc goes to its keyboard hook;
the taskbar menu and the Run box go through the redirect. If Suprvisor does not answer within
200 ms, the real Task Manager opens. `uninstall.ps1` returns Windows to stock.

## Use
Type to filter (name, window title or PID); Esc clears, then hides. Click a header to sort.
Arrows, Home/End, PgUp/PgDn move; Right/Left/Enter open and close groups. Del ends the task
(a group: all its processes). Right-click: end, open file location, copy path or command line.
Right-click a header: group by type on/off. X and Esc hide to the tray; exit from the tray menu.

## Layout
- `crates/suprvisor-core/` – data collection, no UI. `cargo run -p suprvisor-core --release --example bench` prints the cost of each call.
- `crates/suprvisor-app/` – the Win32 app: Direct2D drawing, table model, tray, hook, IFEO stub.
- `audit/` – C# perf audit harness (screen capture based) and `run.ps1`.
- `results/` – audit results (JSON + logs).
- `docs/` – evidence and notes.

Design decisions live in the Obsidian vault: `side projects/Suprvisor - Task Manager Rewrite/Suprvisor - Decisions.md`.
