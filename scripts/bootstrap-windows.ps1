# RafikX Windows bootstrap - run this AFTER cloning the repo.
# Installs missing prerequisites (Rust toolchain, MSVC build tools) via winget,
# then builds & installs the rafikx terminal binary.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\bootstrap-windows.ps1

$ErrorActionPreference = "Continue"

function Have($cmd) { [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }

Write-Host "== RafikX Windows bootstrap ==" -ForegroundColor Yellow

if (-not (Have "winget")) {
    Write-Warning "winget 이 없습니다. '앱 설치 관리자'(Microsoft Store)를 업데이트하거나 수동 설치가 필요합니다."
}

# 1) Rust toolchain
if (-not (Have "cargo")) {
    Write-Host "> Rust 미발견 - rustup 설치"
    if (Have "winget") {
        winget install -e --id Rustlang.Rustup --accept-source-agreements --accept-package-agreements
    } else {
        Invoke-RestMethod https://win.rustup.rs/x86_64 -OutFile "$env:TEMP\rustup-init.exe"
        & "$env:TEMP\rustup-init.exe" -y --default-toolchain stable
    }
    $env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path
} else {
    Write-Host "OK cargo ($(cargo --version))"
}

# 2) MSVC linker (Visual Studio Build Tools) - link.exe 가 없으면 빌드 실패
$needMsvc = $true
foreach ($p in @(
    "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC",
    "$env:ProgramFiles\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC",
    "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2019\BuildTools\VC\Tools\MSVC",
    "$env:ProgramFiles\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC"
)) {
    if (Test-Path $p) { $needMsvc = $false }
}
if ($needMsvc) {
    Write-Host "> MSVC Build Tools 미발견 - 설치 (수 분 소요, 재부팅 요청될 수 있음)"
    if (Have "winget") {
        winget install -e --id Microsoft.VisualStudio.2022.BuildTools --accept-source-agreements --accept-package-agreements --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
    } else {
        Write-Warning "https://visualstudio.microsoft.com/visual-cpp-build-tools/ 에서 수동 설치 후 'C++ 데스크톱 개발' 워크로드를 선택하세요."
    }
} else {
    Write-Host "OK MSVC Build Tools"
}

# 3) 빌드 & 설치
Write-Host "> rafikx 빌드·설치 (몇 분 소요)"
Push-Location "$PSScriptRoot\..\agent-harness"
cargo install --path . --force
Pop-Location

Write-Host ""
Write-Host "== 완료 ==" -ForegroundColor Green
Write-Host "확인 : rafikx --version"
Write-Host "연결 : rafikx model      (마법사 - 서비스 선택/키 등록/모델 선택)"
Write-Host "상태 : rafikx status     · 진단: rafikx doctor"
Write-Host "대화 : rafikx            (TUI, /connect 로도 연결 가능)"
Write-Host ""
Write-Host "데스크탑 앱이 필요하면:" -ForegroundColor DarkGray
Write-Host "  powershell -ExecutionPolicy Bypass -File scripts\build-desktop.ps1" -ForegroundColor DarkGray
