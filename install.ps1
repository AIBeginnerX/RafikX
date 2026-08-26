# RafikX one-line install for Windows PowerShell.
#   irm https://raw.githubusercontent.com/AIBeginnerX/RafikX/master/install.ps1 | iex
$ErrorActionPreference = "Stop"
$Repo = if ($env:RAFIKX_REPO) { $env:RAFIKX_REPO } else { "AIBeginnerX/RafikX" }
$Branch = if ($env:RAFIKX_BRANCH) { $env:RAFIKX_BRANCH } else { "master" }
$Src = if ($env:RAFIKX_SRC) { $env:RAFIKX_SRC } else { Join-Path $HOME ".rafikx-src" }

function Say([string]$m) { Write-Host "`n==> $m" }

Say "Rust 확인"
$cargoBin = Join-Path $HOME ".cargo\bin"
$env:Path = "$cargoBin;" + $env:Path
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Say "Rust가 없어 rustup을 설치합니다"
    $tmp = Join-Path $env:TEMP "rustup-init.exe"
    Invoke-WebRequest -Uri "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe" -OutFile $tmp
    & $tmp -y
    $env:Path = "$cargoBin;" + $env:Path
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo 를 찾지 못했습니다. 새 PowerShell을 연 뒤 다시 실행하세요."
}
rustup toolchain install stable | Out-Null
rustup default stable | Out-Null

Say "소스 받기 ($Repo @$Branch)"
if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    throw "git 이 필요합니다. https://git-scm.com/download/win"
}
if (Test-Path (Join-Path $Src ".git")) {
    git -C $Src fetch --depth 1 origin $Branch
    git -C $Src checkout -q $Branch
    git -C $Src pull --ff-only origin $Branch
} elseif (Test-Path $Src) {
    throw "$Src 가 Git 저장소가 아니어서 덮어쓰지 않았습니다. 경로를 옮기거나 RAFIKX_SRC를 지정하세요."
} else {
    git clone --depth 1 --branch $Branch "https://github.com/$Repo.git" $Src
}

Say "rafikx 설치"
cargo install --path (Join-Path $Src "agent-harness") --locked --force

$exe = Join-Path $cargoBin "rafikx.exe"
if (-not (Test-Path $exe)) { throw "설치는 끝났지만 $exe 를 찾지 못했습니다." }

Say "설치 완료"
& $exe --version
Write-Host ""
Write-Host "새 PowerShell을 열거나 아래를 실행하세요:"
Write-Host '  $env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path'
Write-Host "  rafikx --version"
Write-Host "  rafikx"
Write-Host ""
Write-Host "설정 폴더: $env:USERPROFILE\.rafikx"
