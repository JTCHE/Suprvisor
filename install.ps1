# Installs Suprvisor for the current user: builds it, copies it to Program Files, registers the
# elevated tasks, and redirects taskmgr.exe to it (IFEO). Asks for elevation once (UAC).
# Undo everything with uninstall.ps1.
# Run the Task Manager audit (audit/run.ps1) before installing: after this, taskmgr.exe opens Suprvisor.
#   powershell -ExecutionPolicy Bypass -File install.ps1
param([string]$User, [string]$Log, [switch]$Elevated)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$dir = Join-Path $env:ProgramFiles 'Suprvisor'
$exe = Join-Path $dir 'suprvisor.exe'
$tasks = '\Suprvisor\'
$ifeo = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe'

if (-not $Elevated) {
    cargo build -p suprvisor-app --release --manifest-path "$PSScriptRoot\Cargo.toml"
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
    $me = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    # The elevated half runs without a window and reports through this file: a second console
    # window can be paused by a click in it (Quick Edit), and it stayed open on failure.
    $log = Join-Path $env:TEMP 'suprvisor-install.log'
    Set-Content $log ''
    $p = Start-Process powershell -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Elevated', '-User', "`"$me`"", '-Log', "`"$log`""
    Get-Content $log
    exit $p.ExitCode
}

function Say($text) { Add-Content $Log $text }
try {
    # 1. The binary. Program Files is writable by admins only, so a normal process cannot replace
    #    the exe that the tasks and the IFEO redirect run elevated.
    if (Test-Path $exe) { & $exe --exit; Start-Sleep -Milliseconds 500 }
    New-Item -ItemType Directory -Force $dir | Out-Null
    Copy-Item "$PSScriptRoot\target\release\suprvisor.exe" $exe -Force
    Say "Copied $exe"

    # 2. Two tasks that run elevated with no UAC prompt: Suprvisor, hidden in the tray (at logon, and
    #    from the IFEO stub when it does not run), and the real Task Manager. Priority 1 = high: it must answer when the
    #    machine is saturated. Tasks from an earlier install go first.
    Get-ScheduledTask -TaskPath $tasks -ErrorAction SilentlyContinue | Unregister-ScheduledTask -Confirm:$false
    $principal = New-ScheduledTaskPrincipal -UserId $User -LogonType Interactive -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable `
        -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -Priority 1
    Register-ScheduledTask -TaskPath $tasks -TaskName Start -Force -Principal $principal -Settings $settings `
        -Action (New-ScheduledTaskAction -Execute $exe -Argument '--resident') `
        -Trigger (New-ScheduledTaskTrigger -AtLogOn -User $User) | Out-Null
    Register-ScheduledTask -TaskPath $tasks -TaskName Taskmgr -Force -Principal $principal -Settings $settings `
        -Action (New-ScheduledTaskAction -Execute $exe -Argument '--real-taskmgr') | Out-Null
    Say 'Registered the tasks'

    # 3. Every taskmgr.exe start runs `suprvisor.exe --ifeo` instead. Keep a Debugger value that
    #    another tool set (Process Explorer does this), so uninstall can restore it.
    if (-not (Test-Path $ifeo)) { New-Item $ifeo | Out-Null }
    $ours = "`"$exe`" --ifeo"
    $old = (Get-ItemProperty $ifeo -ErrorAction SilentlyContinue).Debugger
    if ($old -and $old -ne $ours) { Set-ItemProperty $ifeo SuprvisorPreviousDebugger $old }
    Set-ItemProperty $ifeo Debugger $ours

    Start-ScheduledTask -TaskPath $tasks -TaskName Start
    Say 'Installed. Ctrl+Shift+Esc opens Suprvisor.'
}
catch {
    Say "Install failed: $_"
    exit 1
}
