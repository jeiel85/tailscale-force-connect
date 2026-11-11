# Tailscale 부팅 시 강제 연결
# 예약 작업 "Tailscale Auto Connect"(시작 시, SYSTEM)이 실행한다.
# 수동으로 Disconnect 한 상태(WantRunning=false)로 재부팅해도 다시 연결한다.

# Windows PowerShell 5.1은 네이티브 명령의 stderr를 Stop 모드에서 예외로 바꾸므로 Continue로 둔다.
$ErrorActionPreference = 'Continue'
$ts      = 'C:\Program Files\Tailscale\tailscale.exe'
$logFile = Join-Path $PSScriptRoot 'connect.log'
$maxTry  = 30          # 10초 간격, 약 5분
$upWait  = 30000       # tailscale up 한 번에 기다리는 최대 시간(ms)

function Log($msg) {
    $line = '{0:yyyy-MM-dd HH:mm:ss}  {1}' -f (Get-Date), $msg
    Add-Content -Path $logFile -Value $line -Encoding UTF8
}

# 로그가 너무 커지지 않게 최근 500줄만 유지
if ((Test-Path $logFile) -and ((Get-Item $logFile).Length -gt 200KB)) {
    Get-Content $logFile -Tail 500 | Set-Content $logFile -Encoding UTF8
}

function Get-BackendState {
    try {
        $json = & $ts status --json 2>$null
        if ($LASTEXITCODE -ne 0 -or -not $json) { return $null }
        return ($json | Out-String | ConvertFrom-Json).BackendState
    } catch { return $null }
}

Log '--- 시작 ---'
if (-not (Test-Path $ts)) { Log "tailscale.exe 없음: $ts"; exit 3 }

for ($i = 1; $i -le $maxTry; $i++) {
    try {
        $svc = Get-Service -Name Tailscale -ErrorAction Stop
        if ($svc.Status -ne 'Running') {
            Log "서비스 상태 $($svc.Status) -> 시작 시도"
            Start-Service -Name Tailscale -ErrorAction Stop
            Start-Sleep -Seconds 5
        }

        $state = Get-BackendState
        if ($state -eq 'Running')    { Log "연결됨 (시도 $i)"; exit 0 }
        if ($state -eq 'NeedsLogin') { Log '로그인이 필요함(키 만료 등) - 자동 연결 불가'; exit 2 }

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

Log "실패: $maxTry 회 시도 후에도 연결 안 됨 (마지막 상태 '$(Get-BackendState)')"
exit 1
