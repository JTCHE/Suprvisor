# Suprvisor

A fast replacement for the Windows 11 Task Manager, written in Rust.

The stock Task Manager lags at launch, freezes for over a second when you sort a column,
and draws broken rows while it loads (see `docs/taskmgr-render-glitch.png`). It also hides
per-process VRAM use. Suprvisor stays resident, opens on Ctrl+Shift+Esc in one frame,
and shows CPU, RAM, GPU and VRAM per process.

Status: M0 (perf audit harness and Task Manager baseline). See `AUDIT.md`.

## Layout
- `audit/` – C# perf audit harness (screen capture based) and `run.ps1`.
- `results/` – audit results (JSON + logs).
- `docs/` – evidence and notes.

Design decisions live in the Obsidian vault: `side projects/Suprvisor - Task Manager Rewrite/Suprvisor - Decisions.md`.
