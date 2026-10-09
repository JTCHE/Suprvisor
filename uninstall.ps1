# Returns Windows to stock: stops Suprvisor, removes the taskmgr.exe redirect (restoring a previous
# Debugger value) and the tasks, and sends the program folder to the Recycle Bin.
# Keeps the settings in %APPDATA%\suprvisor. Asks for elevation once (UAC).
#   powershell -ExecutionPolicy Bypass -File uninstall.ps1
param([string]$Log, [switch]$Elevated)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$dir = Join-Path $env:ProgramFiles 'Suprvisor'
$exe = Join-Path $dir 'suprvisor.exe'
$tasks = '\Suprvisor\'
$ifeo = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe'

if (-not $Elevated) {
    # The elevated half runs without a window and reports through this file (see install.ps1).
    $log = Join-Path $env:TEMP 'suprvisor-uninstall.log'
    Set-Content $log ''
    $p = Start-Process powershell -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Elevated', '-Log', "`"$log`""
    Get-Content $log
    exit $p.ExitCode
}

function Say($text) { Add-Content $Log $text }
try {
    if (Test-Path $exe) { & $exe --exit; Start-Sleep -Milliseconds 500 }

    $values = Get-ItemProperty $ifeo -ErrorAction SilentlyContinue
    if ($values.SuprvisorPreviousDebugger) {
        Set-ItemProperty $ifeo Debugger $values.SuprvisorPreviousDebugger
        Remove-ItemProperty $ifeo SuprvisorPreviousDebugger
    }
    elseif ($values.Debugger -like '*suprvisor.exe*') {
        Remove-ItemProperty $ifeo Debugger
    }

    Get-ScheduledTask -TaskPath $tasks -ErrorAction SilentlyContinue | Unregister-ScheduledTask -Confirm:$false

    if (Test-Path $dir) { trash $dir }
    Say 'Uninstalled. Ctrl+Shift+Esc opens Task Manager again.'
}
catch {
    Say "Uninstall failed: $_"
    exit 1
}
