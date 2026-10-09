# Returns Windows to stock: stops Suprvisor, removes the taskmgr.exe redirect (restoring a previous
# Debugger value) and the tasks, and sends the program folder to the Recycle Bin.
# Keeps the settings in %APPDATA%\suprvisor. Asks for elevation once (UAC).
#   powershell -ExecutionPolicy Bypass -File uninstall.ps1
param([switch]$Elevated)
$ErrorActionPreference = 'Stop'
$dir = Join-Path $env:ProgramFiles 'Suprvisor'
$exe = Join-Path $dir 'suprvisor.exe'
$tasks = '\Suprvisor\'
$ifeo = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe'

if (-not $Elevated) {
    $p = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Elevated'
    exit $p.ExitCode
}

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
    Write-Host 'Uninstalled. Ctrl+Shift+Esc opens Task Manager again.'
}
catch {
    Write-Host $_
    Read-Host 'Uninstall failed. Press Enter'
    exit 1
}
