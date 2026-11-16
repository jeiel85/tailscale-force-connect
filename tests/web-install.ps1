# docs\get.ps1·remove.ps1(한 줄 설치·제거)의 무결성 확인 흐름을 관리자 권한 없이 시험한다.
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\web-install.ps1 [-Network]
# 관리자 전용 폴더를 만드는 부분(소유자 Administrators)은 관리자 권한이 있어야 해서, 시험에서는 그 두 줄만 바꿔 현재 사용자로 돌린다.
# -Network: GitHub 최신 릴리스를 실제로 받아 SHA-256 확인까지 시험한다(약 5MB).
param([switch]$Network)
$ErrorActionPreference = 'Stop'

$script:failures = 0
function Assert-Equal($name, $actual, $expected) {
    if ("$actual" -ceq "$expected") { Write-Host "ok    $name" }
    else { Write-Host "FAIL  $name : '$actual' (기대값 '$expected')" -ForegroundColor Red; $script:failures++ }
}

$docs = Join-Path $PSScriptRoot '..\docs'
$getText = [IO.File]::ReadAllText((Join-Path $docs 'get.ps1'))
$removeText = [IO.File]::ReadAllText((Join-Path $docs 'remove.ps1'))

# --- 두 파일 공통 ---
$sharedPattern = '(?s)# ---- shared with remove\.ps1.*?# ---- end of shared block ----'
Assert-Equal '공통 블록이 두 파일에서 같음' ([regex]::Match($getText, $sharedPattern).Value -ceq [regex]::Match($removeText, $sharedPattern).Value) $true
Assert-Equal '공통 블록이 있음' ([regex]::Match($getText, $sharedPattern).Value.Length -gt 1000) $true
Assert-Equal 'get.ps1은 ASCII만'    ($getText -cmatch '[^\x00-\x7F]') $false
Assert-Equal 'remove.ps1은 ASCII만' ($removeText -cmatch '[^\x00-\x7F]') $false
foreach ($pair in @(@('get.ps1', $getText), @('remove.ps1', $removeText))) {
    $errs = $null
    [void][Management.Automation.Language.Parser]::ParseInput($pair[1], [ref]$null, [ref]$errs)
    Assert-Equal "$($pair[0]) 구문 오류 없음" $errs.Count 0
}

# get.ps1의 함수만 불러온다(마지막 줄의 설치 실행은 뺀다).
. ([scriptblock]::Create(($getText -replace '(?m)^Invoke-TailscaleForceConnectInstall\s*$', '')))

$root = Join-Path ([IO.Path]::GetTempPath()) ("tfc web o'test " + [guid]::NewGuid().ToString('N').Substring(0, 8))   # 따옴표·공백이 든 경로
$stageRoot = Join-Path $root 'stage-root'
New-Item -ItemType Directory -Path $root, $stageRoot | Out-Null
try {
    # 관리자 전용 폴더 대신 현재 사용자도 쓸 수 있는 시험 폴더를 쓴다.
    $orig = Get-TfcElevatedTemplate
    $me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $patched = $orig.Replace('$acl.SetOwner($admins)', '').
        Replace('foreach ($sid in $system, $admins)', 'foreach ($sid in $system, $admins, [Security.Principal.WindowsIdentity]::GetCurrent().User)').
        Replace("@('S-1-5-32-544', 'S-1-5-18') -notcontains `$owner", "@('S-1-5-32-544', 'S-1-5-18', '$me') -notcontains `$owner").
        Replace("Join-Path `$env:SystemRoot ('Temp\tailscale-force-connect-'", "Join-Path '$($stageRoot.Replace("'", "''"))' ('tailscale-force-connect-'")
    Assert-Equal '시험용 치환이 모두 적용됨' (($patched -split "`n").Count -eq ($orig -split "`n").Count -and $patched -notmatch 'SetOwner' -and $patched -match [regex]::Escape($me) -and -not $patched.Contains("Join-Path `$env:SystemRoot ('Temp")) $true
    function Get-TfcElevatedTemplate { $patched }

    # 가짜 패키지: install.ps1이 받은 인자와 실행 위치를 결과 파일에 쓰고 종료 코드 7로 끝난다.
    $pkgSrc = Join-Path $root 'pkgsrc\tailscale-force-connect'
    New-Item -ItemType Directory -Path $pkgSrc | Out-Null
    Set-Content -LiteralPath (Join-Path $pkgSrc 'install.ps1') -Encoding ASCII -Value @'
param([switch]$NoTest, [string]$ReconnectAfter = '1h', [string]$ResultFile)
$fs = [IO.File]::Open($ResultFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
$w = New-Object IO.StreamWriter($fs)
$w.WriteLine("NoTest=$NoTest Reconnect=$ReconnectAfter")
$w.WriteLine("root=$PSScriptRoot")
$w.Dispose()
exit 7
'@
    $dl = Join-Path $root 'download'
    New-Item -ItemType Directory -Path $dl | Out-Null
    $zip = Join-Path $dl 'package.zip'
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::CreateFromDirectory((Join-Path $root 'pkgsrc'), $zip)
    $sha = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLower()

    function Invoke-Step([string]$Command) {
        # 실제로는 Start-Process -Verb RunAs로 실행한다. 명령줄 따옴표 처리도 같게 하려고 같은 형식으로 넘긴다.
        $p = Start-Process powershell.exe -ArgumentList ('-NoProfile -ExecutionPolicy Bypass -Command "' + $Command + '"') -PassThru -Wait -WindowStyle Hidden
        $p.ExitCode
    }

    # 1) 정상: 인자 전달, 종료 코드 전달, 관리자 전용(시험용) 폴더에서 실행, 끝나면 폴더 삭제
    $result = Join-Path $root 'result-1.txt'
    $cmd = New-TfcElevatedCommand -Dir $dl -Zip $zip -Sha256 $sha -RunName 'install.ps1' -RunArgs @('-NoTest', '-ReconnectAfter', "30m") -ResultFile $result
    Assert-Equal '명령에 큰따옴표 없음' ($cmd.Contains('"')) $false
    Assert-Equal '종료 코드 전달' (Invoke-Step $cmd) 7
    $report = Get-Content -LiteralPath $result
    Assert-Equal '인자 전달' $report[0] 'NoTest=True Reconnect=30m'
    Assert-Equal '임시 폴더가 아니라 별도 폴더에서 실행' ($report[1] -like "root=$stageRoot\tailscale-force-connect-*\tailscale-force-connect") $true
    Assert-Equal '끝나면 폴더 삭제' @(Get-ChildItem -LiteralPath $stageRoot -Force).Count 0

    # 2) 받은 뒤 zip이 바뀜 → 실행하지 않고 결과 파일에 사유
    $result = Join-Path $root 'result-2.txt'
    $cmd = New-TfcElevatedCommand -Dir $dl -Zip $zip -Sha256 ('0' * 64) -RunName 'install.ps1' -RunArgs @() -ResultFile $result
    Assert-Equal 'zip이 바뀌면 실패' (Invoke-Step $cmd) 1
    Assert-Equal 'zip이 바뀌면 사유를 남김' ((Get-Content -LiteralPath $result -Raw) -like '*package changed after it was downloaded*') $true
    Assert-Equal '실패해도 폴더 삭제' @(Get-ChildItem -LiteralPath $stageRoot -Force).Count 0

    # 3) 승인을 기다리는 사이 승격 단계 파일이 바뀜 → 실행하지 않음
    $result = Join-Path $root 'result-3.txt'
    $cmd = New-TfcElevatedCommand -Dir $dl -Zip $zip -Sha256 $sha -RunName 'install.ps1' -RunArgs @() -ResultFile $result
    Add-Content -LiteralPath (Join-Path $dl 'elevated-step.ps1') -Value "`r`nWrite-Host 'injected'"
    Assert-Equal '승격 단계가 바뀌면 실패' (Invoke-Step $cmd) 1
    Assert-Equal '승격 단계가 바뀌면 아무것도 실행 안 함' (Test-Path -LiteralPath $result) $false

    # 4) 패키지에 없는 스크립트
    $result = Join-Path $root 'result-4.txt'
    $cmd = New-TfcElevatedCommand -Dir $dl -Zip $zip -Sha256 $sha -RunName 'uninstall.ps1' -RunArgs @() -ResultFile $result
    Assert-Equal '스크립트가 없으면 실패' (Invoke-Step $cmd) 1
    Assert-Equal '스크립트가 없으면 사유를 남김' ((Get-Content -LiteralPath $result -Raw) -like '*uninstall.ps1 was not found*') $true

    if ($Network) {
        $net = Join-Path $root 'net'
        New-Item -ItemType Directory -Path $net | Out-Null
        $pkg = Get-TfcLatestPackage -Dir $net
        Assert-Equal 'GitHub 최신 릴리스 SHA-256 확인' ((Get-FileHash -LiteralPath $pkg.Zip -Algorithm SHA256).Hash.ToLower()) $pkg.Sha256
        Assert-Equal '태그 형식' ($pkg.Tag -match '^v\d+\.\d+\.\d+$') $true
    }
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($script:failures) { Write-Host "$($script:failures)개 실패" -ForegroundColor Red; exit 1 }
Write-Host '모두 통과'
exit 0
