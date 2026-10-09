# Builds the harness and runs the stress test against target/release/suprvisor.exe, as the current user.
# Output: the table on screen and in results/stress-<time>.log
#   powershell -ExecutionPolicy Bypass -File audit/stress.ps1 [--seconds N] [--only sort,wheel] [--churn N] [--load]
$ErrorActionPreference = 'Stop'
$dotnet = Join-Path $env:LOCALAPPDATA 'Microsoft\dotnet\dotnet.exe'
$env:DOTNET_ROOT = Split-Path $dotnet
& $dotnet build "$PSScriptRoot\Audit.csproj" -c Release --nologo -v q
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$log = Join-Path (Split-Path $PSScriptRoot) "results\stress-$(Get-Date -Format yyyyMMdd-HHmmss).log"
& $dotnet "$PSScriptRoot\bin\Release\net10.0-windows\Audit.dll" stress @args *>&1 | Tee-Object -FilePath $log
