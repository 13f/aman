# Windows build environment setup for aman gateway
#
# Installs all required native dependencies:
#   - Rust toolchain (via rustup)
#   - MSVC Build Tools (C++ workload, provides link.exe)
#   - Node.js (provides npm for frontend builds)
#   - protoc (Protocol Buffers compiler for gRPC)
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts/setup-windows.ps1

$ErrorActionPreference = "Stop"

function Test-Command($Name) {
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

function Install-Rust {
    if (Test-Command "cargo") {
        Write-Host "[OK] Rust already installed: $(cargo --version)" -ForegroundColor Green
        return
    }
    Write-Host "==> Installing Rust..." -ForegroundColor Cyan
    $rustupInit = "$env:TEMP\rustup-init.exe"
    Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $rustupInit
    & $rustupInit -y --default-toolchain stable --profile minimal
    Remove-Item $rustupInit -ErrorAction SilentlyContinue
    $env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")
    Write-Host "[OK] Rust installed: $(cargo --version)" -ForegroundColor Green
}

function Install-MsvcBuildTools {
    $linkExe = Get-ChildItem "C:\Program Files (x86)\Microsoft Visual Studio\2022\VC\Tools\MSVC\*\bin\Hostx64\x64\link.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $linkExe) {
        $linkExe = Get-ChildItem "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Tools\MSVC\*\bin\Hostx64\x64\link.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    }
    if ($linkExe) {
        Write-Host "[OK] MSVC Build Tools already installed: $($linkExe.FullName)" -ForegroundColor Green
        return
    }
    Write-Host "==> Installing MSVC Build Tools (C++ workload)..." -ForegroundColor Cyan
    Write-Host "    This requires ~5-8 GB disk space and may take several minutes." -ForegroundColor Yellow
    winget install Microsoft.VisualStudio.2022.BuildTools --accept-source-agreements --accept-package-agreements `
        --override "--wait --add Microsoft.VisualStudio.Workload.VCTools"
    Write-Host "[OK] MSVC Build Tools installed" -ForegroundColor Green
}

function Install-NodeJs {
    if (Test-Command "node") {
        Write-Host "[OK] Node.js already installed: $(node --version)" -ForegroundColor Green
        return
    }
    Write-Host "==> Installing Node.js..." -ForegroundColor Cyan
    winget install OpenJS.NodeJS.LTS --accept-source-agreements --accept-package-agreements
    $env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")
    Write-Host "[OK] Node.js installed: $(node --version)" -ForegroundColor Green
}

function Install-Protoc {
    if (Test-Command "protoc") {
        Write-Host "[OK] protoc already installed: $(protoc --version)" -ForegroundColor Green
        return
    }
    Write-Host "==> Installing protoc..." -ForegroundColor Cyan
    winget install Google.Protobuf --accept-source-agreements --accept-package-agreements
    $env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")
    Write-Host "[OK] protoc installed: $(protoc --version)" -ForegroundColor Green
}

function Install-FrontendDeps {
    $workspaceRoot = Split-Path -Parent $PSScriptRoot
    $frontendDirs = @(
        "shared/frontend/chat-input",
        "shared/frontend/agent-selector"
    )
    foreach ($dir in $frontendDirs) {
        $fullPath = Join-Path $workspaceRoot $dir
        $nodeModules = Join-Path $fullPath "node_modules"
        if (Test-Path $nodeModules) {
            Write-Host "[OK] $dir dependencies already installed" -ForegroundColor Green
            continue
        }
        Write-Host "==> Installing $dir dependencies..." -ForegroundColor Cyan
        Push-Location $fullPath
        npm install
        Pop-Location
        Write-Host "[OK] $dir dependencies installed" -ForegroundColor Green
    }
}

# ── Main ────────────────────────────────────────────────────────────

Write-Host ""
Write-Host "=== aman Windows Build Environment Setup ===" -ForegroundColor Cyan
Write-Host ""

if (-not (Test-Command "winget")) {
    Write-Host "[FATAL] winget not found. Install App Installer from Microsoft Store first." -ForegroundColor Red
    exit 1
}

Install-Rust
Install-MsvcBuildTools
Install-NodeJs
Install-Protoc
Install-FrontendDeps

Write-Host ""
Write-Host "=== All dependencies installed ===" -ForegroundColor Green
Write-Host ""
Write-Host "You may need to restart your terminal for PATH changes to take effect."
Write-Host "Then run: .\scripts\install-gateway.ps1 -Release -Run"
