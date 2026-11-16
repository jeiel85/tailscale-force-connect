# connect.ps1의 감시 판정(Get-WatchVerdict, Get-StuckAction)을 실제 Tailscale 없이 시험한다.
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\watch-verdict.ps1
#   (PowerShell 7에서도 돌아간다. 예약 작업은 Windows PowerShell 5.1로 실행되므로 두 쪽 모두 시험하는 것이 좋다.)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\connect.ps1')

$script:failures = 0
function Assert-Equal($name, $actual, $expected) {
    if ("$actual" -ceq "$expected") { Write-Host "ok    $name" }
    else { Write-Host "FAIL  $name : '$actual' (기대값 '$expected')" -ForegroundColor Red; $script:failures++ }
}
function New-Status($backend, $online, $health) {
    [pscustomobject]@{
        TimedOut = $false
        Status   = [pscustomobject]@{ BackendState = $backend; Self = [pscustomobject]@{ Online = $online }; Health = $health }
    }
}
$reachable   = { $true }
$unreachable = { $false }
$mustNotCall = { throw '조정 서버 확인을 부르면 안 되는 경우' }

# --- Get-WatchVerdict ---
Assert-Equal '정상 연결'                 (Get-WatchVerdict 'Running' 'Automatic' (New-Status 'Running' $true) $mustNotCall).Kind 'healthy'
Assert-Equal '직접 끊음'                 (Get-WatchVerdict 'Running' 'Automatic' (New-Status 'Stopped' $false) $mustNotCall).Kind 'stopped'
Assert-Equal '로그인 필요는 손대지 않음' (Get-WatchVerdict 'Running' 'Automatic' (New-Status 'NeedsLogin' $false) $mustNotCall).Kind 'ignore'

$v = Get-WatchVerdict 'Running' 'Automatic' (New-Status 'Running' $false @('Tailscale cannot connect because the network is down.')) $reachable
Assert-Equal 'Running인데 오프라인 + 인터넷 됨 = 멈춤' $v.Kind 'stuck'
Assert-Equal '멈춤 사유에 Health 메시지 포함' ($v.Reason -like '*network is down*') $true
Assert-Equal 'Running인데 오프라인 + 인터넷 안 됨 = 네트워크 문제' (Get-WatchVerdict 'Running' 'Automatic' (New-Status 'Running' $false) $unreachable).Kind 'network'
Assert-Equal 'Self가 없음 + 인터넷 됨 = 멈춤'  (Get-WatchVerdict 'Running' 'Automatic' ([pscustomobject]@{ TimedOut = $false; Status = [pscustomobject]@{ BackendState = 'Running' } }) $reachable).Kind 'stuck'
Assert-Equal 'Starting에 머묾 + 인터넷 됨'   (Get-WatchVerdict 'Running' 'Automatic' (New-Status 'Starting' $false) $reachable).Kind 'stuck'
Assert-Equal 'Starting + 인터넷 안 됨'       (Get-WatchVerdict 'Running' 'Automatic' (New-Status 'Starting' $false) $unreachable).Kind 'network'
Assert-Equal 'NoState + 인터넷 됨'           (Get-WatchVerdict 'Running' 'Automatic' (New-Status 'NoState' $false) $reachable).Kind 'stuck'
Assert-Equal 'status 응답 없음 = 멈춤'       (Get-WatchVerdict 'Running' 'Automatic' ([pscustomobject]@{ TimedOut = $true; Status = $null }) $mustNotCall).Kind 'stuck'
Assert-Equal 'status 읽기 실패 = 멈춤'       (Get-WatchVerdict 'Running' 'Automatic' ([pscustomobject]@{ TimedOut = $false; Status = $null }) $mustNotCall).Kind 'stuck'
Assert-Equal '서비스 꺼짐 = 멈춤'            (Get-WatchVerdict 'Stopped' 'Automatic' $null $mustNotCall).Kind 'stuck'
Assert-Equal '서비스 사용 안 함은 손대지 않음' (Get-WatchVerdict 'Stopped' 'Disabled' $null $mustNotCall).Kind 'ignore'
Assert-Equal '서비스 없음은 손대지 않음'     (Get-WatchVerdict $null $null $null $mustNotCall).Kind 'ignore'

# --- Get-StuckAction (시각은 유닉스 초, 기준 3분) ---
$t = 1800000000
$rec = { param($sinceAgo, $lastAgo) [pscustomobject]@{ since = $t - $sinceAgo; last = $t - $lastAgo } }
Assert-Equal '처음 봄'                     (Get-StuckAction $null $t 3 $null) 'start'
Assert-Equal '1분째'                       (Get-StuckAction (& $rec 60 60) $t 3 $null) 'wait'
Assert-Equal '2분 20초째(30초 여유를 더해도 3분 전)' (Get-StuckAction (& $rec 140 60) $t 3 $null) 'wait'
Assert-Equal '2분 31초째(30초 여유로 재시작)' (Get-StuckAction (& $rec 151 60) $t 3 $null) 'heal'
Assert-Equal '3분째'                       (Get-StuckAction (& $rec 180 60) $t 3 $null) 'heal'
Assert-Equal '절전 뒤(확인이 22분 비었음)는 새로 셈' (Get-StuckAction (& $rec 1500 1320) $t 3 $null) 'start'
Assert-Equal '5분 주기 감시는 이어서 셈'   (Get-StuckAction (& $rec 600 300) $t 3 $null) 'heal'
Assert-Equal '시계가 뒤로 감'              (Get-StuckAction (& $rec 60 -30) $t 3 $null) 'start'
Assert-Equal '10분 전에 재시작했으면 쉼'   (Get-StuckAction (& $rec 600 60) $t 3 ($t - 600)) 'cooldown'
Assert-Equal '31분 전에 재시작했으면 다시' (Get-StuckAction (& $rec 600 60) $t 3 ($t - 1860)) 'heal'

if ($script:failures) { Write-Host "$($script:failures)개 실패" -ForegroundColor Red; exit 1 }
Write-Host '모두 통과'
exit 0
