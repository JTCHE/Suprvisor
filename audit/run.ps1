# Builds the harness as the current user, then runs it elevated (one UAC prompt).
# Output: results/*.json and results/audit-<time>.log
#   powershell -ExecutionPolicy Bypass -File audit/run.ps1 [harness args]
$ErrorActionPreference = 'Stop'
$dotnet = Join-Path $env:LOCALAPPDATA 'Microsoft\dotnet\dotnet.exe'
$env:DOTNET_ROOT = Split-Path $dotnet
& $dotnet build "$PSScriptRoot\Audit.csproj" -c Release --nologo -v q
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$dll = "$PSScriptRoot\bin\Release\net10.0-windows\Audit.dll"
$log = Join-Path (Split-Path $PSScriptRoot) "results\audit-$(Get-Date -Format yyyyMMdd-HHmmss).log"
New-Item -ItemType Directory -Force (Split-Path $log) | Out-Null
$cmd = "`$env:DOTNET_ROOT='$env:DOTNET_ROOT'; & '$dotnet' '$dll' $($args -join ' ') *>&1 | Tee-Object -FilePath '$log'; exit `$LASTEXITCODE"
$p = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', $cmd
Write-Host "exit $($p.ExitCode), log $log"
exit $p.ExitCode
