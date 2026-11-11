# Tailscale 부팅 시 강제 연결
# 예약 작업 "Tailscale Auto Connect"(시작 시, SYSTEM)이 실행한다.
# 수동으로 Disconnect 한 상태(WantRunning=false)로 재부팅해도 다시 연결한다.
#
# -Watch -ReconnectAfterMinutes N: 예약 작업 "Tailscale Auto Reconnect"(SYSTEM, 주기 실행)이 실행한다.
#   끊긴 상태(Stopped)가 처음 보인 시각을 기록해 두고, N분이 지나도 끊겨 있으면 다시 연결한다.
#   Tailscale 공식 정책 ReconnectAfter는 1.104.5에서 'tailscale down' 뒤 재연결하지 않는 것을 확인해 쓰지 않는다.
#
# 종료 코드: 0 연결됨, 1 시간 안에 연결 실패, 2 로그인 필요, 3 tailscale.exe 없음
# 결과는 connect.log와 Windows 이벤트 로그(응용 프로그램, 원본 TailscaleForceConnect)에 남긴다.
param([switch]$Watch, [int]$ReconnectAfterMinutes = 60)

# Windows PowerShell 5.1은 네이티브 명령의 stderr를 Stop 모드에서 예외로 바꾸므로 Continue로 둔다.
$ErrorActionPreference = 'Continue'
$logFile     = Join-Path $PSScriptRoot 'connect.log'
$eventSource = 'TailscaleForceConnect'
$deadline    = (Get-Date).AddMinutes(10)   # 네트워크가 늦게 잡히는 경우를 위해 최대 10분 재시도 (작업 제한은 15분)
$upWait      = 30000                       # tailscale up 한 번에 기다리는 최대 시간(ms)

function Log($msg) {
    $line = '{0:yyyy-MM-dd HH:mm:ss}  {1}' -f (Get-Date), $msg
    Add-Content -Path $logFile -Value $line -Encoding UTF8
}

# 이벤트 로그 원본은 install.ps1이 만든다. 없으면(예전 설치) 조용히 건너뛴다.
function Write-Result($eventId, $entryType, $msg) {
    Log $msg
    try {
        Write-EventLog -LogName Application -Source $eventSource -EventId $eventId -EntryType $entryType -Message $msg -ErrorAction Stop
    } catch { }
}

# tailscale.exe는 서비스(tailscaled.exe)와 같은 폴더에 있다. 기본 경로가 아닌 설치도 찾는다.
function Find-TailscaleExe {
    try {
        $img = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Services\Tailscale' -ErrorAction Stop).ImagePath
        $img = [Environment]::ExpandEnvironmentVariables($img)
        $daemon = if ($img -match '^\s*"([^"]+)"') { $Matches[1] } else { ($img.Trim() -split '\s+')[0] }
        $candidate = Join-Path (Split-Path $daemon -Parent) 'tailscale.exe'
        if (Test-Path $candidate) { return $candidate }
    } catch { }
    $default = Join-Path $env:ProgramFiles 'Tailscale\tailscale.exe'
    if (Test-Path $default) { return $default }
    return $null
}

function Get-BackendState {
    try {
        $json = & $ts status --json 2>$null
        if ($LASTEXITCODE -ne 0 -or -not $json) { return $null }
        return ($json | Out-String | ConvertFrom-Json).BackendState
    } catch { return $null }
}

# 로그가 너무 커지지 않게 최근 500줄만 유지
# 파이프라인으로 같은 파일을 읽으면서 쓰면 '사용 중' 오류가 나므로, 먼저 다 읽은 뒤 쓴다.
if ((Test-Path $logFile) -and ((Get-Item $logFile).Length -gt 200KB)) {
    try {
        $tail = Get-Content $logFile -Tail 500 -Encoding UTF8
        Set-Content -Path $logFile -Value $tail -Encoding UTF8
    } catch { }
}

$ts = Find-TailscaleExe

if ($Watch) {
    # 주기 감시: 연결돼 있으면 아무것도 남기지 않고 끝낸다(로그가 불어나지 않도록).
    $sinceFile = Join-Path $PSScriptRoot 'stopped-since.txt'
    if (-not $ts) { exit 3 }
    $state = Get-BackendState
    if ($state -ne 'Stopped') {
        # Running이면 기록을 지운다. 로그인 필요 등 다른 상태는 'tailscale up'으로 풀 수 없으므로 건드리지 않는다.
        if (Test-Path -LiteralPath $sinceFile) { Remove-Item -LiteralPath $sinceFile -Force -ErrorAction SilentlyContinue }
        exit 0
    }
    $now = Get-Date
    $since = $null
    if (Test-Path -LiteralPath $sinceFile) {
        try { $since = [datetime]::Parse((Get-Content -LiteralPath $sinceFile -TotalCount 1), [Globalization.CultureInfo]::InvariantCulture) } catch { $since = $null }
    }
    if (-not $since -or $since -gt $now) {
        Set-Content -LiteralPath $sinceFile -Value $now.ToString('o') -Encoding ASCII
        Log "감시: 끊긴 상태 확인 - $ReconnectAfterMinutes 분 뒤에도 끊겨 있으면 다시 연결"
        exit 0
    }
    # 작업 실행 시각이 매번 조금씩 어긋나므로 30초 여유를 둔다(그렇지 않으면 판정이 한 주기 늦어진다).
    $elapsed = ($now - $since).TotalMinutes
    if ($elapsed + 0.5 -lt $ReconnectAfterMinutes) { exit 0 }
    $mins = [int][Math]::Round($elapsed)

    Log "감시: $mins 분 동안 끊겨 있음 -> tailscale up"
    $p = Start-Process -FilePath $ts -ArgumentList 'up' -NoNewWindow -PassThru
    $null = $p.Handle
    if (-not $p.WaitForExit($upWait)) { $p.Kill() }
    Start-Sleep -Seconds 3
    if ((Get-BackendState) -eq 'Running') {
        Remove-Item -LiteralPath $sinceFile -Force -ErrorAction SilentlyContinue
        Write-Result 1004 Information "직접 끊은 지 $mins 분이 지나 다시 연결함"
        exit 0
    }
    # 실패하면 기록을 남겨 두어 다음 주기에 다시 시도한다.
    Write-Result 1001 Error "감시: $mins 분 끊김 뒤 다시 연결하지 못함 (상태 '$(Get-BackendState)')"
    exit 1
}

Log '--- 시작 ---'
if (-not $ts) { Write-Result 1003 Error 'tailscale.exe를 찾지 못함 - Tailscale이 설치되어 있는지 확인하세요'; exit 3 }

$i = 0
while ((Get-Date) -lt $deadline) {
    $i++
    try {
        $svc = Get-Service -Name Tailscale -ErrorAction Stop
        if ($svc.Status -ne 'Running') {
            Log "서비스 상태 $($svc.Status) -> 시작 시도"
            Start-Service -Name Tailscale -ErrorAction Stop
            Start-Sleep -Seconds 5
        }

        $state = Get-BackendState
        if ($state -eq 'Running')    { Write-Result 1000 Information "연결됨 (시도 $i)"; exit 0 }
        if ($state -eq 'NeedsLogin') { Write-Result 1002 Error '로그인이 필요함(키 만료 등) - 자동 연결 불가'; exit 2 }

        Log "상태 '$state' -> tailscale up (시도 $i)"
        # 플래그 없는 up: 기존 설정(unattended 등)을 그대로 두고 WantRunning만 켠다.
        $p = Start-Process -FilePath $ts -ArgumentList 'up' -NoNewWindow -PassThru
        $null = $p.Handle   # 핸들을 잡아 두지 않으면 WaitForExit(timeout) 뒤 ExitCode가 비어 있다
        if (-not $p.WaitForExit($upWait)) {
            $p.Kill()
            Log 'tailscale up 시간 초과'
        } elseif ($p.ExitCode -ne 0) {
            Log "tailscale up 종료 코드 $($p.ExitCode)"
        }
    } catch {
        Log "오류: $($_.Exception.Message)"
    }
    Start-Sleep -Seconds 10
}

Write-Result 1001 Error "실패: 10분 동안 $i 회 시도했지만 연결되지 않음 (마지막 상태 '$(Get-BackendState)')"
exit 1
