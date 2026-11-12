# 릴리스 zip 만들기 (PowerShell 7에서 실행)
#   pwsh -File scripts\build-release.ps1 [-Ref v1.3.0]
# 1) 관리 창을 릴리스 빌드한다.  2) git archive로 설치에 필요한 파일만 꺼낸다(.gitattributes의 export-ignore).
# 3) 실행 파일을 넣어 dist\tailscale-force-connect.zip을 만든다.
param([string]$Ref = 'HEAD')

$ErrorActionPreference = 'Stop'
$root  = Resolve-Path (Join-Path $PSScriptRoot '..')
$dist  = Join-Path $root 'dist'
$stage = Join-Path $dist 'stage'
$zip   = Join-Path $dist 'tailscale-force-connect.zip'

Push-Location (Join-Path $root 'gui')
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build 실패 ($LASTEXITCODE)" }
} finally { Pop-Location }
$exe = Join-Path $root 'gui\target\release\TailscaleForceConnect.exe'

if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Path $stage | Out-Null
$src = Join-Path $dist 'src.zip'
git -C $root archive --format=zip --prefix=tailscale-force-connect/ -o $src $Ref
if ($LASTEXITCODE -ne 0) { throw "git archive 실패 ($LASTEXITCODE)" }
Expand-Archive -Path $src -DestinationPath $stage
Remove-Item $src

$pkg = Join-Path $stage 'tailscale-force-connect'
Copy-Item -LiteralPath $exe -Destination $pkg
Compress-Archive -Path $pkg -DestinationPath $zip
Remove-Item -Recurse -Force $stage

$size = [Math]::Round((Get-Item $zip).Length / 1KB)
Write-Host "만듦: $zip ($size KB)"
(Get-FileHash $zip -Algorithm SHA256).Hash
