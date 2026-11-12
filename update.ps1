# Tailscale 부팅 시 강제 연결 - 최신 릴리스로 업데이트
#   관리 창의 [업데이트] 버튼이 관리자 권한으로 실행한다. 직접 실행해도 된다(UAC로 스스로 다시 실행).
#   -Relaunch: 끝나면 관리 창을 일반 권한으로 다시 연다.
# 하는 일:
#   1) GitHub 최신 릴리스의 zip을 관리자만 쓸 수 있는 Program Files 안의 임시 폴더에 받는다.
#   2) GitHub이 릴리스에 기록한 SHA-256과 비교해 검증한다(다르면 중단).
#   3) 기존 자동 재연결 설정을 유지한 채 install.ps1 -NoTest로 설치한다. 연결은 끊지 않는다.
param([switch]$Relaunch)

$ErrorActionPreference = 'Stop'
$repo    = 'jeiel85/tailscale-force-connect'
$asset   = 'tailscale-force-connect.zip'
$appDir  = Join-Path $env:ProgramFiles 'Tailscale Force Connect'
$appExe  = Join-Path $appDir 'TailscaleForceConnect.exe'
$dataDir = 'C:\ProgramData\TailscaleAutoConnect'

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    $argList = "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    if ($Relaunch) { $argList += ' -Relaunch' }
    try {
        $p = Start-Process powershell.exe -Verb RunAs -ArgumentList $argList -PassThru -Wait
    } catch {
        Write-Host '관리자 권한을 받지 못해 업데이트하지 않았습니다.' -ForegroundColor Yellow
        exit 1
    }
    exit $p.ExitCode
}

$lines = New-Object System.Collections.Generic.List[string]
function Write-Report($m) { Write-Host $m; $lines.Add([string]$m) }

$exitCode = 0
$stage = $null
try {
    try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch { }

    # 1) 최신 릴리스 정보
    $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'tailscale-force-connect-updater'; 'Accept' = 'application/vnd.github+json' } -UseBasicParsing
    $zipAsset = $rel.assets | Where-Object { $_.name -eq $asset } | Select-Object -First 1
    if (-not $zipAsset) { throw "최신 릴리스($($rel.tag_name))에 $asset 이(가) 없습니다." }
    Write-Report "최신 릴리스: $($rel.tag_name)"

    # 2) 관리자만 쓸 수 있는 곳(Program Files 아래)에 받는다. 사용자 %TEMP%는 승인을 기다리는 사이 바꿔치기될 수 있다.
    if (-not (Test-Path -LiteralPath $appDir)) { New-Item -ItemType Directory -Path $appDir | Out-Null }
    $stage = Join-Path $appDir ('.update-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stage | Out-Null
    $zip = Join-Path $stage $asset
    $oldProgress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try { Invoke-WebRequest -Uri $zipAsset.browser_download_url -OutFile $zip -UseBasicParsing } finally { $ProgressPreference = $oldProgress }

    # 3) GitHub이 기록한 SHA-256과 비교
    $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLower()
    if ($zipAsset.digest -match '^sha256:([0-9a-f]{64})$') {
        if ($Matches[1] -ne $actual) { throw "받은 파일의 SHA-256이 릴리스 기록과 다릅니다. 업데이트를 중단합니다. (기대 $($Matches[1]), 실제 $actual)" }
        Write-Report "SHA-256 확인: $actual"
    } else {
        throw '릴리스에 SHA-256 기록이 없어 받은 파일을 검증할 수 없습니다. 업데이트를 중단합니다.'
    }

    # Expand-Archive는 PowerShell 7에서 상속된 모듈 경로 때문에 실패할 수 있어 .NET으로 직접 푼다.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::ExtractToDirectory($zip, $stage)
    $installer = Join-Path $stage 'tailscale-force-connect\install.ps1'
    if (-not (Test-Path -LiteralPath $installer)) { throw '받은 패키지에 install.ps1이 없습니다.' }

    # 4) 기존 자동 재연결 설정 유지
    $reconnect = '1h'
    try {
        $settings = Get-Content -LiteralPath (Join-Path $dataDir 'settings.json') -Raw -Encoding UTF8 | ConvertFrom-Json
        if ($null -ne $settings.reconnectMinutes) {
            $reconnect = if ([int]$settings.reconnectMinutes -le 0) { '0' } else { "$([int]$settings.reconnectMinutes)m" }
        }
    } catch { }
    Write-Report "자동 재연결 설정 유지: $reconnect"

    # 5) 설치 (연결을 끊는 확인 단계는 건너뜀)
    $ErrorActionPreference = 'Continue'
    $out = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $installer -NoTest -ReconnectAfter $reconnect 2>&1 | Out-String
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    $out -split "`r?`n" | Where-Object { $_.Trim() } | ForEach-Object { Write-Report "  $_" }
    if ($code -ne 0) { throw "install.ps1이 실패했습니다 (종료 코드 $code)." }
    Write-Report "업데이트 완료: $($rel.tag_name)"
} catch {
    $exitCode = 1
    Write-Report "업데이트 오류: $($_.Exception.Message)"
} finally {
    if ($stage -and (Test-Path -LiteralPath $stage)) { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue }
}

if ($Relaunch) {
    if ($exitCode -eq 0 -and (Test-Path -LiteralPath $appExe)) {
        # explorer.exe를 거쳐 열면 관리자 권한이 아닌 일반 권한으로 열린다.
        Start-Process -FilePath explorer.exe -ArgumentList "`"$appExe`""
    } else {
        Add-Type -AssemblyName System.Windows.Forms
        $tail = ($lines | Select-Object -Last 15) -join "`r`n"
        [void][System.Windows.Forms.MessageBox]::Show($tail, 'Tailscale Force Connect 업데이트', 'OK', 'Error')
    }
}
exit $exitCode
