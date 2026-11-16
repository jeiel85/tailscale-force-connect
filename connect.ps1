# Tailscale 부팅 시 강제 연결
# 예약 작업 "Tailscale Auto Connect"(시작 시, SYSTEM)이 실행한다.
# 수동으로 Disconnect 한 상태(WantRunning=false)로 재부팅해도 다시 연결한다.
#
# -Watch: 예약 작업 "Tailscale Auto Reconnect"(SYSTEM, 주기 실행)이 실행한다. 두 가지를 본다.
#   -ReconnectAfterMinutes N: 끊긴 상태(Stopped)가 처음 보인 시각을 기록해 두고, N분이 지나도 끊겨 있으면 다시 연결한다(0이면 끔).
#     Tailscale 공식 정책 ReconnectAfter는 1.104.5에서 'tailscale down' 뒤 재연결하지 않는 것을 확인해 쓰지 않는다.
#   -HealAfterMinutes M: 연결된 것처럼 보이지만 실제로는 멈춘 상태가 M분 이어지면 Tailscale 서비스를 다시 시작한다(0이면 끔).
#     멈춘 상태: 'Running'인데 서버와 연결되지 않음(Self.Online=false), 'Starting'에서 넘어가지 않음, 응답 없음, 서비스 꺼짐.
#     실제로 겪은 일: 절전(모던 스탠바이)에서 깨어난 뒤 인터넷은 되는데 Tailscale이 "network is down"에 머물러,
#     트레이 아이콘은 정상인데 다른 기기에서는 offline으로 보였다. 서비스를 다시 시작하자 풀렸다.
# -Heal: 지금 바로 Tailscale 서비스를 다시 시작한다(관리 창의 [Tailscale 다시 시작] 버튼이 관리자 권한으로 실행).
#
# 종료 코드: 0 연결됨, 1 시간 안에 연결 실패, 2 로그인 필요, 3 tailscale.exe 없음
# 결과는 connect.log와 Windows 이벤트 로그(응용 프로그램, 원본 TailscaleForceConnect)에 남긴다.
param([switch]$Watch, [int]$ReconnectAfterMinutes = 60, [int]$HealAfterMinutes = 0, [switch]$Heal, [string]$ResultFile)

# Windows PowerShell 5.1은 네이티브 명령의 stderr를 Stop 모드에서 예외로 바꾸므로 Continue로 둔다.
$ErrorActionPreference = 'Continue'
$logFile     = Join-Path $PSScriptRoot 'connect.log'
$eventSource = 'TailscaleForceConnect'
$deadline    = (Get-Date).AddMinutes(10)   # 네트워크가 늦게 잡히는 경우를 위해 최대 10분 재시도 (작업 제한은 15분)
$upWait      = 30000                       # tailscale up 한 번에 기다리는 최대 시간(ms)
$statusWait  = 20000                       # tailscale status 한 번에 기다리는 최대 시간(ms). 서비스가 멈추면 응답하지 않을 수 있다
$stuckFile   = Join-Path $PSScriptRoot 'stuck-since.json'   # 멈춘 상태를 처음 본 시각(since)과 마지막으로 본 시각(last)
$healFile    = Join-Path $PSScriptRoot 'heal-last.json'     # 마지막 서비스 재시작 결과. 관리 창과 알림이 읽는다
$healCooldownMinutes = 30    # 재시작해도 풀리지 않는 경우(방화벽 등) 계속 재시작하지 않도록 쉬는 시간
$healOkCooldownMinutes = 10  # 재시작으로 풀린 뒤 다시 멈췄을 때 쉬는 시간
$maxGapSeconds = 420         # 마지막 확인 뒤 이만큼 비었으면(절전·꺼짐) 멈춘 시간을 새로 센다. 감시 주기(1~5분)보다 길어야 한다
$controlHost = 'controlplane.tailscale.com'

function Log($msg) {
    if (-not $logFile) { return }   # -Heal에서 설치 폴더를 믿을 수 없을 때
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

# 'tailscale status --json'을 시간 제한을 걸고 읽는다.
# 결과: TimedOut(응답 없음), Status(JSON 객체, 서비스가 꺼졌거나 읽지 못하면 $null)
function Get-TsStatus {
    $r = [pscustomobject]@{ TimedOut = $false; Status = $null }
    if (-not $ts) { return $r }
    try {
        $psi = New-Object Diagnostics.ProcessStartInfo
        $psi.FileName = $ts
        $psi.Arguments = 'status --json'
        $psi.UseShellExecute = $false
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $psi.CreateNoWindow = $true
        $psi.StandardOutputEncoding = New-Object Text.UTF8Encoding $false
        $p = [Diagnostics.Process]::Start($psi)
        # 두 출력을 함께 비동기로 읽어야 버퍼가 차서 멈추는 일이 없다.
        $out = $p.StandardOutput.ReadToEndAsync()
        $null = $p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit($statusWait)) {
            try { $p.Kill() } catch { }
            $r.TimedOut = $true
            return $r
        }
        $json = $out.Result
        if ($json) { $r.Status = $json | ConvertFrom-Json }
    } catch { }
    return $r
}

function Get-BackendState {
    $st = (Get-TsStatus).Status
    if ($st) { return "$($st.BackendState)" }
    return $null
}

# 운영체제 쪽에서 Tailscale 조정 서버까지 TCP 연결이 되는지 본다(5초 제한).
# 되는데도 Tailscale이 연결되지 않았다면 Tailscale 쪽이 멈춘 것이다. 안 되면 진짜 네트워크 문제라 재시작해도 소용없다.
function Test-ControlReachable {
    # 인자 없는 TcpClient는 IPv4 전용이라, IPv6만 되는 망에서도 닿도록 듀얼 모드 소켓을 쓴다(IPv6가 꺼져 있으면 IPv4로).
    try {
        $client = New-Object Net.Sockets.TcpClient([Net.Sockets.AddressFamily]::InterNetworkV6)
        $client.Client.DualMode = $true
    } catch {
        $client = New-Object Net.Sockets.TcpClient
    }
    try {
        $task = $client.ConnectAsync($controlHost, 443)
        return ($task.Wait(5000) -and $client.Connected)
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

# 감시 판정. 입력만 보고 정하므로 tests\watch-verdict.ps1에서 Tailscale 없이 시험한다.
#   $ServiceStatus: 서비스 상태('Running', 'Stopped' 등, 서비스가 없으면 $null), $StartType: 시작 유형('Automatic', 'Disabled' 등)
#   $StatusResult: Get-TsStatus 결과, $Reachable: 조정 서버에 닿는지 확인하는 스크립트 블록(필요할 때만 부른다)
# 결과 Kind: healthy(정상) / stopped(직접 끊음) / stuck(멈춤) / network(진짜 네트워크 끊김) / ignore(재시작으로 풀 수 없음)
# Self.Online(조정 서버와 연결돼 있는지). 값이 없으면(이 값을 채우지 않는 Tailscale) 연결된 것으로 본다(관리 창과 같은 기준).
# 없는 값을 '오프라인'으로 보면 그런 버전에서 30분마다 서비스를 재시작하게 된다.
function Test-SelfOnline($Status) {
    if (-not $Status.Self -or -not $Status.Self.PSObject.Properties['Online']) { return $true }
    return [bool]$Status.Self.Online
}

function Get-WatchVerdict($ServiceStatus, $StartType, $StatusResult, [scriptblock]$Reachable) {
    if ($ServiceStatus -ne 'Running') {
        # 서비스를 일부러 '사용 안 함'으로 둔 경우는 건드리지 않는다.
        if (-not $ServiceStatus -or "$StartType" -eq 'Disabled') { return @{ Kind = 'ignore'; Reason = '서비스 없음 또는 사용 안 함' } }
        return @{ Kind = 'stuck'; Reason = "서비스 꺼짐($ServiceStatus)" }
    }
    if ($StatusResult.TimedOut) { return @{ Kind = 'stuck'; Reason = "tailscale status가 $($statusWait / 1000)초 동안 응답 없음" } }
    $st = $StatusResult.Status
    if (-not $st) { return @{ Kind = 'stuck'; Reason = 'tailscale status를 읽지 못함' } }

    $state = "$($st.BackendState)"
    if ($state -eq 'Stopped') { return @{ Kind = 'stopped'; Reason = '' } }
    if ($state -eq 'Running') {
        if (Test-SelfOnline $st) { return @{ Kind = 'healthy'; Reason = '' } }
        $why = '연결됨으로 보이지만 서버와 연결되지 않음'
    } elseif (@('Starting', 'NoState', '') -contains $state) {
        $why = "상태 '$state'에서 넘어가지 않음"
    } else {
        return @{ Kind = 'ignore'; Reason = "상태 '$state'" }   # NeedsLogin 등: 재시작으로 풀리지 않는다
    }
    $health = @($st.Health) | Where-Object { $_ } | Select-Object -First 1
    if ($health) { $why += " ($health)" }
    if (-not (& $Reachable)) { return @{ Kind = 'network'; Reason = $why } }
    return @{ Kind = 'stuck'; Reason = $why }
}

# 멈춘 상태가 이어진 시간에 따라 할 일을 정한다. 시각은 모두 유닉스 초.
#   $Record: 이전 기록(since, last) 또는 $null, $LastHealAt: 마지막 재시작 시각 또는 $null, $LastHealOk: 그 재시작으로 풀렸는지
# 결과: start(새로 세기 시작) / wait(아직 기다림) / cooldown(최근에 재시작해서 쉼) / heal(지금 재시작)
# 쉬는 시간: 지난 재시작이 실패했으면 30분(재시작으로 안 풀리는 원인), 성공했으면 10분.
#   성공 뒤에도 길게 쉬면 절전에 자주 들어가는 PC에서 다음 멈춤을 30분 동안 못 푼다. 0분이면 재시작이 효과 없이 3~4분마다 반복될 수 있다.
function Get-StuckAction($Record, [int64]$Now, [int]$HealAfter, $LastHealAt, [bool]$LastHealOk = $false) {
    if (-not $Record) { return 'start' }
    $gap = $Now - [int64]$Record.last
    # 절전·꺼짐으로 확인이 한동안 비었으면 깨어난 직후 바로 재시작하지 않도록 새로 센다.
    if ($gap -lt 0 -or $gap -gt $maxGapSeconds) { return 'start' }
    # 작업 실행 시각이 매번 조금씩 어긋나므로 30초 여유를 둔다(그렇지 않으면 판정이 한 주기 늦어진다).
    $elapsed = ($Now - [int64]$Record.since) / 60.0
    if ($elapsed + 0.5 -lt $HealAfter) { return 'wait' }
    if ($null -ne $LastHealAt) {
        $sinceHeal = $Now - [int64]$LastHealAt
        $cooldown = if ($LastHealOk) { $healOkCooldownMinutes } else { $healCooldownMinutes }
        if ($sinceHeal -ge 0 -and $sinceHeal -lt $cooldown * 60) { return 'cooldown' }
    }
    return 'heal'
}

function Get-Epoch { [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() }

function Read-JsonFile($path) {
    if (-not $path -or -not (Test-Path -LiteralPath $path)) { return $null }
    try { return (Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json) } catch { return $null }
}

function Write-JsonFile($path, $obj) {
    if (-not $path) { return }
    Set-Content -LiteralPath $path -Value ($obj | ConvertTo-Json -Compress) -Encoding UTF8
}

# 관리자 권한으로 기록을 남겨도 되는 폴더인지 본다: 링크가 아니고, 폴더와 기록 파일의 소유자가 Administrators나 SYSTEM.
# 일반 사용자도 C:\ProgramData에 폴더를 만들 수 있으므로, 설치되지 않은 PC에서 미리 만들어 둔 폴더·링크에 쓰지 않으려는 것이다.
function Get-TrustedDataDir([string]$Dir) {
    $trusted = @('S-1-5-32-544', 'S-1-5-18')
    try {
        foreach ($p in @($Dir) + @('connect.log', 'stuck-since.json', 'heal-last.json' | ForEach-Object { Join-Path $Dir $_ })) {
            if ($p -ne $Dir -and -not (Test-Path -LiteralPath $p)) { continue }
            $item = Get-Item -LiteralPath $p -Force -ErrorAction Stop
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { return $null }
            if ($trusted -notcontains (Get-Acl -LiteralPath $p).GetOwner([Security.Principal.SecurityIdentifier]).Value) { return $null }
        }
        return $Dir
    } catch { return $null }
}

# 서비스를 멈췄다가 다시 켠다. 멈춘 서비스는 정상 종료가 안 될 수 있으므로 30초 안에 멈추지 않으면 tailscaled를 끝낸다.
function Restart-TailscaleService {
    $svc = Get-Service -Name Tailscale -ErrorAction Stop
    if ($svc.Status -ne 'Stopped') {
        if ($svc.Status -ne 'StopPending') { $svc.Stop() }
        try {
            $svc.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
        } catch {
            Log '복구: 서비스가 30초 안에 멈추지 않아 tailscaled 프로세스를 끝냄'
            Get-Process -Name tailscaled -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
            # 프로세스가 끝나도 서비스 관리자가 Stopped로 바꾸기까지 시간이 걸린다. 그 전에 Start를 건너뛰면 서비스가 꺼진 채 남는다.
            try { $svc.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(20)) } catch { Log '복구: tailscaled를 끝냈지만 서비스가 20초 안에 Stopped가 되지 않음' }
        }
    }
    $svc.Refresh()
    if ($svc.Status -eq 'Stopped') { $svc.Start() }
    $svc.WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
}

# Tailscale 서비스를 다시 시작하고 최대 60초 동안 결과를 기다린다. 결과는 로그·이벤트 로그·heal-last.json에 남긴다.
# 결과: connected(연결됨) / stopped(직접 끊긴 상태로 돌아옴) / login(로그인 필요) / failed
function Invoke-Heal([string]$Why, [string]$Trigger, [int]$Minutes) {
    Log "복구: $Why -> Tailscale 서비스 다시 시작"
    $started = Get-Date
    $err = $null
    try { Restart-TailscaleService } catch { $err = $_.Exception.Message; Log "복구: 서비스 다시 시작 중 오류: $err" }

    $outcome = 'failed'
    $state = '응답 없음'
    $until = (Get-Date).AddSeconds(60)
    do {
        Start-Sleep -Seconds 3
        $st = (Get-TsStatus).Status
        if (-not $st) { continue }
        $state = "$($st.BackendState)"
        if ($state -eq 'Running' -and (Test-SelfOnline $st)) { $outcome = 'connected'; break }
        if ($state -eq 'Stopped') { $outcome = 'stopped'; break }
        if ($state -eq 'NeedsLogin' -or $state -eq 'NeedsMachineAuth') { $outcome = 'login'; break }
    } while ((Get-Date) -lt $until)
    $secs = [int]((Get-Date) - $started).TotalSeconds

    Write-JsonFile $healFile ([ordered]@{
        at = Get-Epoch; time = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss'); trigger = $Trigger
        outcome = $outcome; minutes = $Minutes; reason = $Why
    })
    switch ($outcome) {
        'connected' { Write-Result 1005 Information "복구: $Why -> 서비스를 다시 시작해 $secs 초 만에 연결됨" }
        'stopped'   { Write-Result 1005 Information "복구: $Why -> 서비스를 다시 시작함 (직접 끊긴 상태라 연결은 하지 않음)" }
        'login'     { Write-Result 1002 Error "복구: $Why -> 서비스를 다시 시작했지만 로그인이 필요함(키 만료 등)" }
        default     { Write-Result 1006 Error "복구 실패: $Why -> 서비스를 다시 시작했지만 $secs 초 안에 연결되지 않음 (상태 '$state')$(if ($err) { ", 오류: $err" })" }
    }
    return $outcome
}

# 시험(tests\watch-verdict.ps1)에서 함수만 불러갈 때는 여기서 멈춘다.
if ($MyInvocation.InvocationName -eq '.') { return }

# 로그가 너무 커지지 않게 최근 500줄만 유지
# 파이프라인으로 같은 파일을 읽으면서 쓰면 '사용 중' 오류가 나므로, 먼저 다 읽은 뒤 쓴다.
if ((Test-Path $logFile) -and ((Get-Item $logFile).Length -gt 200KB)) {
    try {
        $tail = Get-Content $logFile -Tail 500 -Encoding UTF8
        Set-Content -Path $logFile -Value $tail -Encoding UTF8
    } catch { }
}

$ts = Find-TailscaleExe

if ($Heal) {
    # 관리 창은 Program Files에 있는 이 스크립트를 실행하므로, 기록은 스크립트 폴더가 아니라 설치 폴더에 남긴다.
    $trustedDir = Get-TrustedDataDir 'C:\ProgramData\TailscaleAutoConnect'
    $logFile   = if ($trustedDir) { Join-Path $trustedDir 'connect.log' } else { $null }
    $stuckFile = if ($trustedDir) { Join-Path $trustedDir 'stuck-since.json' } else { $null }
    $healFile  = if ($trustedDir) { Join-Path $trustedDir 'heal-last.json' } else { $null }

    # 결과 파일은 사용자가 쓸 수 있는 %TEMP%에 있으므로 새로 만들 때만 쓴다.
    $writer = $null
    if ($ResultFile) {
        try {
            $fs = [IO.File]::Open($ResultFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
            $writer = New-Object IO.StreamWriter($fs, (New-Object Text.UTF8Encoding $true))
            $writer.AutoFlush = $true
        } catch { $writer = $null }
    }
    function Write-HealReport($m) { Write-Host $m; if ($writer) { $writer.WriteLine($m) } }

    $code = 1
    $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $isAdmin) {
        Write-HealReport '관리자 권한이 필요합니다. 관리자 PowerShell에서 실행하세요.'
    } elseif (-not $ts) {
        Write-HealReport 'tailscale.exe를 찾지 못했습니다. Tailscale이 설치되어 있는지 확인하세요.'
        $code = 3
    } else {
        if (-not $trustedDir) { Write-HealReport '설치 폴더를 확인할 수 없어 기록은 남기지 않습니다(다시 설치하면 해결됩니다).' }
        $outcome = Invoke-Heal -Why '직접 요청' -Trigger 'manual' -Minutes 0
        if ($stuckFile) { Remove-Item -LiteralPath $stuckFile -Force -ErrorAction SilentlyContinue }
        switch ($outcome) {
            'connected' { Write-HealReport 'Tailscale 서비스를 다시 시작했고, 서버에 다시 연결됐습니다.'; $code = 0 }
            'stopped'   { Write-HealReport 'Tailscale 서비스를 다시 시작했습니다. 직접 끊은 상태라 연결은 하지 않았습니다.'; $code = 0 }
            'login'     { Write-HealReport 'Tailscale 서비스를 다시 시작했지만 로그인이 필요합니다(키 만료 등).'; $code = 2 }
            default     { Write-HealReport 'Tailscale 서비스를 다시 시작했지만 60초 안에 연결되지 않았습니다. 인터넷 연결과 최근 기록을 확인하세요.' }
        }
    }
    if ($writer) { $writer.Dispose() }
    exit $code
}

if ($Watch) {
    # 주기 감시: 정상이면 아무것도 남기지 않고 끝낸다(로그가 불어나지 않도록).
    $sinceFile = Join-Path $PSScriptRoot 'stopped-since.txt'
    if (-not $ts) { exit 3 }
    $svc = Get-Service -Name Tailscale -ErrorAction SilentlyContinue
    $svcStatus = if ($svc) { "$($svc.Status)" } else { $null }
    $startType = if ($svc) { "$($svc.StartType)" } else { $null }
    $statusResult = if ($svcStatus -eq 'Running') { Get-TsStatus } else { $null }
    $verdict = Get-WatchVerdict -ServiceStatus $svcStatus -StartType $startType -StatusResult $statusResult -Reachable { Test-ControlReachable }

    # 'stopped'가 아니면 끊김 기록을, 'stuck'이 아니면 멈춤 기록을 지운다.
    # 로그인 필요 등 다른 상태는 'tailscale up'이나 재시작으로 풀 수 없으므로 건드리지 않는다.
    if ($verdict.Kind -ne 'stopped' -and (Test-Path -LiteralPath $sinceFile)) { Remove-Item -LiteralPath $sinceFile -Force -ErrorAction SilentlyContinue }
    if (($verdict.Kind -ne 'stuck' -or $HealAfterMinutes -le 0) -and (Test-Path -LiteralPath $stuckFile)) { Remove-Item -LiteralPath $stuckFile -Force -ErrorAction SilentlyContinue }

    if ($verdict.Kind -eq 'stuck' -and $HealAfterMinutes -gt 0) {
        $now = Get-Epoch
        $record = Read-JsonFile $stuckFile
        $lastHeal = Read-JsonFile $healFile
        $lastHealAt = if ($lastHeal -and $null -ne $lastHeal.at) { [int64]$lastHeal.at } else { $null }
        $lastHealOk = $lastHeal -and @('connected', 'stopped') -contains "$($lastHeal.outcome)"
        $action = Get-StuckAction -Record $record -Now $now -HealAfter $HealAfterMinutes -LastHealAt $lastHealAt -LastHealOk $lastHealOk
        # 부팅 직후 'Tailscale Auto Connect'가 아직 연결을 재시도하는 중이면 서비스를 재시작해 방해하지 않는다.
        if ($action -eq 'heal' -and (Get-ScheduledTask -TaskName 'Tailscale Auto Connect' -ErrorAction SilentlyContinue).State -eq 'Running') { $action = 'wait' }
        if ($action -eq 'start') {
            # 절전에서 깨어난 직후처럼 잠깐 연결이 늦는 경우가 흔하므로, 다음 확인에서도 멈춰 있을 때만 로그에 남긴다.
            Write-JsonFile $stuckFile ([ordered]@{ since = $now; last = $now; reason = $verdict.Reason; logged = $false })
            exit 0
        }
        if ($action -ne 'heal') {
            $logged = [bool]$record.logged
            if (-not $logged) {
                $mins = [int][Math]::Round(($now - [int64]$record.since) / 60.0)
                Log "감시: 멈춘 상태가 $mins 분째 이어짐 - $($verdict.Reason). $HealAfterMinutes 분이 되면 Tailscale 서비스를 다시 시작"
                $logged = $true
            }
            Write-JsonFile $stuckFile ([ordered]@{ since = [int64]$record.since; last = $now; reason = $verdict.Reason; logged = $logged })
            exit 0
        }
        $mins = [int][Math]::Round(($now - [int64]$record.since) / 60.0)
        $outcome = Invoke-Heal -Why "$mins 분 동안 멈춤: $($verdict.Reason)" -Trigger 'watch' -Minutes $mins
        if ($outcome -eq 'connected' -or $outcome -eq 'stopped') {
            Remove-Item -LiteralPath $stuckFile -Force -ErrorAction SilentlyContinue
            exit 0
        }
        # 풀리지 않았으면 기록을 남겨 두고, 쉬는 시간이 지난 뒤 다시 시도한다.
        Write-JsonFile $stuckFile ([ordered]@{ since = [int64]$record.since; last = Get-Epoch; reason = $verdict.Reason; logged = $true })
        exit 1
    }

    if ($verdict.Kind -ne 'stopped' -or $ReconnectAfterMinutes -le 0) { exit 0 }

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
