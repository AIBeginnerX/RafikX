# Build RafikX desktop installers on Windows (NSIS / optional MSI).
# Requires: Rust, WebView2 (Windows 10+), and cargo-tauri (installed below if missing).

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

python "$PSScriptRoot\gen-desktop-icons.py"

$tauri = Get-Command tauri -ErrorAction SilentlyContinue
if (-not $tauri) {
    Write-Host "Installing tauri-cli 2..."
    cargo install tauri-cli --locked --version "^2"
}

$bundles = if ($args.Count -gt 0) { $args } else { @("nsis") }
$bundleArg = $bundles -join ","

Write-Host "Building bundles: $bundleArg"
Set-Location "$Root\desktop\src-tauri"
cargo tauri build --bundles $bundleArg

$bundleRoot = if ($env:CARGO_TARGET_DIR) {
    Join-Path $env:CARGO_TARGET_DIR "release\bundle"
} else {
    Join-Path $Root "desktop\src-tauri\target\release\bundle"
}

Write-Host ""
Write-Host "Installers (if the build succeeded):"
Get-ChildItem -Recurse $bundleRoot -ErrorAction SilentlyContinue |
    Where-Object { $_.Extension -match '\.(exe|msi)$' } |
    ForEach-Object { $_.FullName }
