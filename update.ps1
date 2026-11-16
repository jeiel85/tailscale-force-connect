# Tailscale 부팅 시 강제 연결 - 최신 릴리스로 업데이트
#   관리 창의 [업데이트] 버튼이 관리자 권한으로 실행한다. 직접 실행해도 된다(UAC로 스스로 다시 실행).
#   -Relaunch: 끝나면 관리 창을 일반 권한으로 다시 연다.
#   -Force: 최신 릴리스가 설치된 버전과 같거나 낮아도 설치한다(다시 설치·되돌리기용).
# 하는 일:
#   1) GitHub 최신 릴리스가 설치된 버전보다 새것인지 본다(같거나 낮으면 설치하지 않음).
#   2) zip을 관리자만 쓸 수 있는 Program Files 안의 임시 폴더에 받는다.
#   3) GitHub이 릴리스에 기록한 SHA-256과 비교해 검증한다(다르면 중단).
#   4) 지금 설치된 관리 앱 파일을 같은 임시 폴더에 백업한다.
#   5) 기존 자동 재연결 설정을 유지한 채 install.ps1 -NoTest로 설치한다. 연결은 끊지 않는다.
#   6) 설치가 실패했는데 파일이 이미 바뀌었으면, 백업해 둔 이전 버전의 install.ps1로 다시 설치해 되돌린다.
param([switch]$Relaunch, [switch]$Force)

$ErrorActionPreference = 'Stop'
$repo     = 'jeiel85/tailscale-force-connect'
$asset    = 'tailscale-force-connect.zip'
$appDir   = Join-Path $env:ProgramFiles 'Tailscale Force Connect'
$appName  = 'TailscaleForceConnect.exe'
$appExe   = Join-Path $appDir $appName
$dataDir  = 'C:\ProgramData\TailscaleAutoConnect'
# install.ps1이 Program Files에 두는 파일. 되돌릴 때 이 파일들을 백업에서 다시 설치한다.
$appFiles = @($appName, 'install.ps1', 'uninstall.ps1', 'connect.ps1', 'update.ps1')

# "1.7.0"·"v1.7.0" → [version]. 형식이 다르면 $null.
function ConvertTo-SemVer([string]$Text) {
    if ("$Text".Trim() -match '^v?(\d+)\.(\d+)\.(\d+)$') { return New-Object Version ([int]$Matches[1]), ([int]$Matches[2]), ([int]$Matches[3]) }
    return $null
}

# 업데이트할지 정한다. 결과: install / same(이미 같은 버전) / older(설치된 것보다 낮음) / unknown(최신 버전을 읽지 못함)
# 설치된 버전을 모르면(예전 설치) 설치한다. 관리 창은 새 버전일 때만 버튼을 보이지만, 직접 실행할 때 내려가지 않게 막는다.
function Get-UpdateAction([string]$Latest, [string]$Installed, [bool]$ForceInstall) {
    if ($ForceInstall) { return 'install' }
    $l = ConvertTo-SemVer $Latest
    if (-not $l) { return 'unknown' }
    $i = ConvertTo-SemVer $Installed
    if (-not $i -or $l -gt $i) { return 'install' }
    if ($l -eq $i) { return 'same' }
    return 'older'
}

# 지금 설치된 관리 앱 파일(이전 버전)을 $Dest에 복사한다. 이전 버전의 install.ps1과 실행 파일이 있어야 되돌릴 수 있다.
# 예약 작업이 실행하는 $Data\connect.ps1도 data-connect.ps1로 복사해, 설치가 그 파일을 바꿨는지 비교할 기준으로 쓴다.
function Backup-AppFiles([string]$From, [string]$Dest, [string]$Data) {
    New-Item -ItemType Directory -Path $Dest -Force | Out-Null
    foreach ($f in $appFiles) {
        $src = Join-Path $From $f
        if (Test-Path -LiteralPath $src) { Copy-Item -LiteralPath $src -Destination $Dest -Force }
    }
    $dataScript = Join-Path $Data 'connect.ps1'
    if ($Data -and (Test-Path -LiteralPath $dataScript)) { Copy-Item -LiteralPath $dataScript -Destination (Join-Path $Dest 'data-connect.ps1') -Force }
    return (Test-Path -LiteralPath (Join-Path $Dest 'install.ps1')) -and (Test-Path -LiteralPath (Join-Path $Dest $appName))
}

function Get-HashOrEmpty([string]$Path) {
    if (Test-Path -LiteralPath $Path) { return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash }
    return ''
}

# 실패한 설치가 무엇이라도 바꿨는지 본다(관리 앱 파일, 예약 작업이 실행하는 connect.ps1).
# 아무것도 바뀌지 않았으면(Tailscale 로그인 필요 등으로 처음에 멈춤) 되돌릴 필요가 없다.
function Test-InstallChanged([string]$App, [string]$Backup, [string]$Data) {
    foreach ($f in $appFiles) {
        if ((Get-HashOrEmpty (Join-Path $App $f)) -ne (Get-HashOrEmpty (Join-Path $Backup $f))) { return $true }
    }
    # 설치 전 자기 자신(백업의 data-connect.ps1)과 비교한다. Program Files의 connect.ps1과 비교하면 원래 두 파일이 달랐을 때 바뀐 것으로 잘못 본다.
    return (Get-HashOrEmpty (Join-Path $Data 'connect.ps1')) -ne (Get-HashOrEmpty (Join-Path $Backup 'data-connect.ps1'))
}

# install.ps1이 이번 실행에서 마지막 단계(settings.json에 새 버전 기록)까지 마쳤는지 본다.
# install.ps1은 Unattended 설정 실패처럼 나머지를 다 설치하고도 1로 끝나는 경우가 있어, 그때는 되돌리지 않는다.
# 같은 버전을 다시 설치(-Force)하면 settings.json에 처음부터 그 버전이 있으므로, 설치 전($BeforeHash)과 내용이 달라졌는지도 본다
# (install.ps1은 설치 시각 installedAt을 함께 기록한다).
function Test-InstallCompleted([string]$Data, [string]$NewVersion, [string]$BeforeHash) {
    $path = Join-Path $Data 'settings.json'
    try {
        if ((Get-HashOrEmpty $path) -eq $BeforeHash) { return $false }
        $v = (Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json).version
        return [bool]$NewVersion -and "$v" -eq $NewVersion
    } catch { return $false }
}

# install.ps1을 새 Windows PowerShell로 실행하고 (종료 코드, 출력 줄)을 돌려준다.
function Invoke-Installer([string]$Installer, [string[]]$InstallArgs) {
    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $out = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $Installer @InstallArgs 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally { $ErrorActionPreference = $old }
    return [pscustomobject]@{ Code = $code; Lines = @($out -split "`r?`n" | Where-Object { $_.Trim() }) }
}

$lines = New-Object System.Collections.Generic.List[string]
function Write-Report($m) { Write-Host $m; $lines.Add([string]$m) }

# 새 버전을 설치하고, 실패했는데 파일이 이미 바뀌었으면 백업해 둔 이전 버전의 install.ps1로 다시 설치해
# 새 파일과 이전 예약 작업이 섞이지 않게 한다. 실패하면 예외를 던진다(메시지에 되돌렸는지 담음).
function Install-WithRollback([string]$Installer, [string[]]$InstallArgs, [string]$App, [string]$Data, [string]$Backup, [bool]$CanRollback, [string]$NewVersion) {
    $settingsBefore = Get-HashOrEmpty (Join-Path $Data 'settings.json')
    $r = Invoke-Installer -Installer $Installer -InstallArgs $InstallArgs
    $r.Lines | ForEach-Object { Write-Report "  $_" }
    if ($r.Code -eq 0) { return }
    if (Test-InstallCompleted -Data $Data -NewVersion $NewVersion -BeforeHash $settingsBefore) {
        throw "install.ps1이 새 버전(v$NewVersion) 설치를 마쳤지만 일부 단계가 실패했습니다(종료 코드 $($r.Code)). 위 기록을 확인하세요. 새 버전은 그대로 둡니다."
    }
    if (-not (Test-InstallChanged -App $App -Backup $Backup -Data $Data)) {
        throw "install.ps1이 실패했습니다 (종료 코드 $($r.Code)). 설치된 파일은 바뀌지 않았습니다."
    }
    if (-not $CanRollback) {
        throw "install.ps1이 실패했습니다 (종료 코드 $($r.Code)). 이전 버전 파일이 없어 되돌리지 못했습니다. 한 줄 설치로 다시 설치하세요."
    }
    Write-Report "설치가 실패해(종료 코드 $($r.Code)) 이전 버전으로 되돌립니다."
    $rb = Invoke-Installer -Installer (Join-Path $Backup 'install.ps1') -InstallArgs $InstallArgs
    $rb.Lines | ForEach-Object { Write-Report "  $_" }
    if ($rb.Code -eq 0) { throw "install.ps1이 실패해(종료 코드 $($r.Code)) 이전 버전으로 되돌렸습니다." }
    throw "install.ps1이 실패했고(종료 코드 $($r.Code)) 이전 버전으로 되돌리는 것도 실패했습니다(종료 코드 $($rb.Code)). 한 줄 설치로 다시 설치하세요."
}

# 시험(tests\update-helpers.ps1)에서 함수만 불러갈 때는 여기서 멈춘다.
if ($MyInvocation.InvocationName -eq '.') { return }

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    $argList = "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    if ($Relaunch) { $argList += ' -Relaunch' }
    if ($Force) { $argList += ' -Force' }
    try {
        $p = Start-Process powershell.exe -Verb RunAs -ArgumentList $argList -PassThru -Wait
    } catch {
        Write-Host '관리자 권한을 받지 못해 업데이트하지 않았습니다.' -ForegroundColor Yellow
        exit 1
    }
    exit $p.ExitCode
}

# 네트워크가 잠깐 끊기는 경우가 있어(실제로 겪음) 30초 제한으로 최대 3번 시도한다.
function Invoke-WithRetry([scriptblock]$Action, [string]$What) {
    for ($i = 1; $i -le 3; $i++) {
        try { return & $Action } catch {
            if ($i -eq 3) { throw "$What 실패(3번 시도): $($_.Exception.Message)" }
            Write-Report "$What 실패, 5초 뒤 다시 시도 ($i/3): $($_.Exception.Message)"
            Start-Sleep -Seconds 5
        }
    }
}

$exitCode = 0
$stage = $null
try {
    try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch { }

    # 기존 설정(자동 재연결, 멈춤 감시)과 설치된 버전. 기록이 없으면(예전 버전) 새 버전의 기본값을 쓴다.
    $reconnect = '1h'
    $heal = $null
    $installedVersion = $null
    try {
        $settings = Get-Content -LiteralPath (Join-Path $dataDir 'settings.json') -Raw -Encoding UTF8 | ConvertFrom-Json
        if ($null -ne $settings.reconnectMinutes) {
            $reconnect = if ([int]$settings.reconnectMinutes -le 0) { '0' } else { "$([int]$settings.reconnectMinutes)m" }
        }
        if ($null -ne $settings.healMinutes) {
            $heal = if ([int]$settings.healMinutes -le 0) { '0' } else { "$([int]$settings.healMinutes)m" }
        }
        $installedVersion = "$($settings.version)"
    } catch { }

    # 1) 최신 릴리스 정보
    $rel = Invoke-WithRetry -What '최신 릴리스 조회' -Action {
        Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'tailscale-force-connect-updater'; 'Accept' = 'application/vnd.github+json' } -UseBasicParsing -TimeoutSec 30
    }
    $zipAsset = $rel.assets | Where-Object { $_.name -eq $asset } | Select-Object -First 1
    if (-not $zipAsset) { throw "최신 릴리스($($rel.tag_name))에 $asset 이(가) 없습니다." }
    Write-Report "최신 릴리스: $($rel.tag_name)$(if ($installedVersion) { " (설치된 버전 v$installedVersion)" })"

    $action = Get-UpdateAction -Latest $rel.tag_name -Installed $installedVersion -ForceInstall $Force.IsPresent
    if ($action -eq 'same') {
        Write-Report '이미 최신 버전입니다. 다시 설치하려면 -Force를 붙여 실행하세요.'
    } elseif ($action -eq 'older') {
        throw "최신 릴리스($($rel.tag_name))가 설치된 버전(v$installedVersion)보다 낮아 업데이트하지 않습니다. 이 버전으로 내리려면 -Force를 붙여 실행하세요."
    } elseif ($action -eq 'unknown') {
        throw "최신 릴리스의 버전($($rel.tag_name))을 읽지 못해 업데이트하지 않습니다."
    } else {
        # 2) 관리자만 쓸 수 있는 곳(Program Files 아래)에 받는다. 사용자 %TEMP%는 승인을 기다리는 사이 바꿔치기될 수 있다.
        if (-not (Test-Path -LiteralPath $appDir)) { New-Item -ItemType Directory -Path $appDir | Out-Null }
        $stage = Join-Path $appDir ('.update-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $stage | Out-Null
        $zip = Join-Path $stage $asset
        $oldProgress = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        try {
            Invoke-WithRetry -What '내려받기' -Action { Invoke-WebRequest -Uri $zipAsset.browser_download_url -OutFile $zip -UseBasicParsing -TimeoutSec 120 } | Out-Null
        } finally { $ProgressPreference = $oldProgress }

        # 3) GitHub이 기록한 SHA-256과 비교
        $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLower()
        if ($zipAsset.digest -match '^sha256:([0-9a-f]{64})$') {
            if ($Matches[1] -ne $actual) { throw "받은 파일의 SHA-256이 릴리스 기록과 다릅니다. 업데이트를 중단합니다. (기대 $($Matches[1]), 실제 $actual)" }
            Write-Report "SHA-256 확인: $actual"
        } else {
            throw '릴리스에 SHA-256 기록이 없어 받은 파일을 검증할 수 없습니다. 업데이트를 중단합니다.'
        }

        # Expand-Archive는 PowerShell 7에서 상속된 모듈 경로 때문에 실패할 수 있어 .NET으로 직접 푼다.
        $pkgDir = Join-Path $stage 'package'
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        [IO.Compression.ZipFile]::ExtractToDirectory($zip, $pkgDir)
        $installer = Join-Path $pkgDir 'tailscale-force-connect\install.ps1'
        if (-not (Test-Path -LiteralPath $installer)) { throw '받은 패키지에 install.ps1이 없습니다.' }

        # 4) 되돌리기용 백업 (지금 설치된 관리 앱 파일)
        $backup = Join-Path $stage 'previous'
        $canRollback = Backup-AppFiles -From $appDir -Dest $backup -Data $dataDir

        # 5) 설치 (연결을 끊는 확인 단계는 건너뜀). 기존 설정을 그대로 넘긴다.
        Write-Report "자동 재연결 설정 유지: $reconnect"
        $installArgs = @('-NoTest', '-ReconnectAfter', $reconnect)
        if ($heal) {
            $installArgs += @('-HealAfter', $heal)
            Write-Report "멈춤 감시 설정 유지: $heal"
        }
        # 6) 실패했는데 파일이 이미 바뀌었으면 이전 버전으로 되돌린다.
        Install-WithRollback -Installer $installer -InstallArgs $installArgs -App $appDir -Data $dataDir -Backup $backup -CanRollback $canRollback -NewVersion ($rel.tag_name.TrimStart('v'))
        Write-Report "업데이트 완료: $($rel.tag_name)"
    }
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
