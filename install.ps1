# Installs Suprvisor for the current user: builds it, copies it to Program Files, registers the
# logon task, and redirects taskmgr.exe to it (IFEO). Asks for elevation once (UAC).
# Undo everything with uninstall.ps1.
# Run the Task Manager audit (audit/run.ps1) before installing: after this, taskmgr.exe opens Suprvisor.
#   powershell -ExecutionPolicy Bypass -File install.ps1
param([string]$User, [switch]$Elevated)
$ErrorActionPreference = 'Stop'
$dir = Join-Path $env:ProgramFiles 'Suprvisor'
$exe = Join-Path $dir 'suprvisor.exe'
$tasks = '\Suprvisor\'
$ifeo = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe'

if (-not $Elevated) {
    cargo build -p suprvisor-app --release --manifest-path "$PSScriptRoot\Cargo.toml"
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
    $me = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    $p = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Elevated', '-User', "`"$me`""
    exit $p.ExitCode
}

try {
    # 1. The binary. Program Files is writable by admins only, so a normal process cannot replace
    #    the exe that the tasks and the IFEO redirect run elevated.
    if (Test-Path $exe) { & $exe --exit; Start-Sleep -Milliseconds 500 }
    New-Item -ItemType Directory -Force $dir | Out-Null
    Copy-Item "$PSScriptRoot\target\release\suprvisor.exe" $exe -Force

    # 2. Two tasks that run elevated with no UAC prompt: the resident process at logon, and the real
    #    Task Manager on demand (the IFEO fallback when Suprvisor does not answer).
    #    Priority 1 = high: the resident process must answer when the machine is saturated.
    $principal = New-ScheduledTaskPrincipal -UserId $User -LogonType Interactive -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable `
        -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -Priority 1
    Register-ScheduledTask -TaskPath $tasks -TaskName Resident -Force -Principal $principal -Settings $settings `
        -Action (New-ScheduledTaskAction -Execute $exe -Argument '--resident') `
        -Trigger (New-ScheduledTaskTrigger -AtLogOn -User $User) | Out-Null
    Register-ScheduledTask -TaskPath $tasks -TaskName Taskmgr -Force -Principal $principal -Settings $settings `
        -Action (New-ScheduledTaskAction -Execute $exe -Argument '--real-taskmgr') | Out-Null

    # 3. Every taskmgr.exe start runs `suprvisor.exe --ifeo` instead. Keep a Debugger value that
    #    another tool set (Process Explorer does this), so uninstall can restore it.
    if (-not (Test-Path $ifeo)) { New-Item $ifeo | Out-Null }
    $ours = "`"$exe`" --ifeo"
    $old = (Get-ItemProperty $ifeo -ErrorAction SilentlyContinue).Debugger
    if ($old -and $old -ne $ours) { Set-ItemProperty $ifeo SuprvisorPreviousDebugger $old }
    Set-ItemProperty $ifeo Debugger $ours

    Start-ScheduledTask -TaskPath $tasks -TaskName Resident
    Write-Host "Installed $exe. Ctrl+Shift+Esc opens Suprvisor."
}
catch {
    Write-Host $_
    Read-Host 'Install failed. Press Enter'
    exit 1
}
