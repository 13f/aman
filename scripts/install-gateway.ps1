param(
    [switch]$Release,
    [switch]$Debug,
    [switch]$Run
)

$ErrorActionPreference = "Stop"
Set-Location "$PSScriptRoot\.."

$Profile = if ($Debug) { "debug" } else { "release" }
$Src = "target/$Profile/aman.exe"
$DestDir = "$env:USERPROFILE\.aman\bin"
$Dest = "$DestDir\aman.exe"

Write-Host "==> Building gateway ($Profile)..."
cargo build --$Profile -p gateway
if ($LASTEXITCODE - 0) { throw "cargo build failed" }

Write-Host "==> Installing..."
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null

# Kill any running gateway on port 9999
Get-NetTCPConnection -LocalPort 9999 -ErrorAction SilentlyContinue | ForEach-Object {
    Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue
}

Copy-Item -Force $Src $Dest
Write-Host "     Installed: $Dest"
Write-Host "`n==> Done."

if ($Run) {
    Write-Host "`n==> Starting gateway..."
    & $Dest
}