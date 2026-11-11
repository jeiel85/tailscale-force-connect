# Tailscale 부팅 시 강제 연결 - 설치
#   powershell -ExecutionPolicy Bypass -File .\install.ps1          # 설치 + 끊긴 상태에서 동작 검증
#   powershell -ExecutionPolicy Bypass -File .\install.ps1 -NoTest  # 설치만 (연결을 끊지 않음)
# 관리자 권한이 아니면 UAC로 스스로 다시 실행한다.
param([switch]$NoTest)

$ErrorActionPreference = 'Stop'
$dir         = 'C:\ProgramData\TailscaleAutoConnect'
$taskName    = 'Tailscale Auto Connect'
$eventSource = 'TailscaleForceConnect'
$logFile     = Join-Path $dir 'install.log'

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    $argList = "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    if ($NoTest) { $argList += ' -NoTest' }
    try {
        $p = Start-Process powershell.exe -Verb RunAs -ArgumentList $argList -PassThru -Wait
    } catch {
        Write-Host '관리자 권한을 받지 못해 설치하지 않았습니다. 다시 실행해서 UAC 창에서 [예]를 누르세요.' -ForegroundColor Yellow
        exit 1
    }
    if (Test-Path $logFile) { Get-Content $logFile -Encoding UTF8 }
    exit $p.ExitCode
}

# 주의: 'R' 같은 짧은 이름은 기본 별칭(r = Invoke-History)과 겹쳐 함수가 무시되므로 쓰지 않는다.
function Write-Report($m) {
    Write-Host $m
    Add-Content -Path $logFile -Value $m -Encoding UTF8
}
function Get-TsState {
    if (-not $ts) { return $null }
    (& $ts status --json 2>$null | Out-String | ConvertFrom-Json).BackendState
}
# connect.ps1의 같은 이름 함수와 동일하게 유지한다: 서비스와 같은 폴더의 tailscale.exe를 찾는다.
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

$exitCode = 0
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Set-Content -Path $logFile -Value "설치 시작 $(Get-Date -Format s)" -Encoding UTF8

$ts = Find-TailscaleExe
try {
    if (-not $ts) { throw 'Tailscale을 찾지 못했습니다. Tailscale을 설치하고 로그인한 뒤 다시 실행하세요.' }
    Write-Report "Tailscale: $ts"

    # 1) 스크립트 설치. SYSTEM이 실행하므로 일반 사용자는 수정하지 못하게 ACL을 제한한다.
    $acl = New-Object System.Security.AccessControl.DirectorySecurity
    $acl.SetAccessRuleProtection($true, $false)
    $inh = 'ContainerInherit,ObjectInherit'
    foreach ($id in 'NT AUTHORITY\SYSTEM', 'BUILTIN\Administrators') {
        $acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($id, 'FullControl', $inh, 'None', 'Allow')))
    }
    $acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule('BUILTIN\Users', 'ReadAndExecute', $inh, 'None', 'Allow')))
    Set-Acl -Path $dir -AclObject $acl
    Copy-Item -Path (Join-Path $PSScriptRoot 'connect.ps1') -Destination $dir -Force
    # 한 줄 설치로 받은 경우 내려받은 폴더가 지워지므로 제거 스크립트도 함께 둔다.
    Copy-Item -Path (Join-Path $PSScriptRoot 'uninstall.ps1') -Destination $dir -Force
    Write-Report "스크립트 설치: $dir\connect.ps1, uninstall.ps1"

    # 2) Unattended 모드: 로그인 전·로그아웃 후에도 연결 유지.
    #    이미 켜져 있어도 같은 값을 다시 쓰는 것뿐이라 항상 실행한다(불안정한 'tailscale debug prefs' 출력에 기대지 않음).
    $ErrorActionPreference = 'Continue'
    $out = & $ts set --unattended=true 2>&1 | Out-String
    $setCode = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($setCode -eq 0) { Write-Report 'Unattended 모드: 켜짐' }
    else { Write-Report "경고: Unattended 모드를 켜지 못함 (종료 코드 $setCode) $($out.Trim())" }

    # 3) 이벤트 로그 원본: connect.ps1이 부팅 결과를 '응용 프로그램' 로그에 남긴다.
    if (-not [Diagnostics.EventLog]::SourceExists($eventSource)) {
        New-EventLog -LogName Application -Source $eventSource
    }
    Write-Report "이벤트 로그 원본: Application / $eventSource"

    # 4) 예약 작업: 부팅 20초 뒤 SYSTEM으로 실행 (Windows 로그인 불필요). connect.ps1은 최대 10분 재시도한다.
    $action    = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$dir\connect.ps1`""
    $trigger   = New-ScheduledTaskTrigger -AtStartup
    $trigger.Delay = 'PT20S'
    $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
    $settings  = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Minutes 15) -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description 'Tailscale을 부팅 때마다 연결 상태로 만든다 (수동 Disconnect 상태로 재부팅해도).' -Force | Out-Null
    Write-Report "예약 작업 등록: $taskName (시작 시, SYSTEM)"

    # 빠른 시작이 켜져 있으면 '종료 → 전원 켜기'는 최대 절전 복귀라 시작 시 작업이 실행되지 않는다.
    $hiberboot = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Power' -Name HiberbootEnabled -ErrorAction SilentlyContinue).HiberbootEnabled
    if ($hiberboot -eq 1) {
        Write-Report '경고: Windows 빠른 시작이 켜져 있습니다. 재시작이나 정전 뒤에는 동작하지만, 일반 종료 후 전원을 켤 때는 동작하지 않습니다.'
        Write-Report '      항상 동작하게 하려면 제어판 > 전원 옵션 > 전원 단추 작동 설정에서 [빠른 시작 켜기]를 끄세요.'
    }

    # 5) 검증: 일부러 끊고 작업을 실행해 다시 연결되는지 확인한다.
    $ErrorActionPreference = 'Continue'
    if ($NoTest) {
        Write-Report '검증 건너뜀 (-NoTest)'
    } else {
        & $ts down
        Start-Sleep -Seconds 2
        Write-Report "검증: down 후 상태 = $(Get-TsState)"
        $testStart = Get-Date
        Start-ScheduledTask -TaskName $taskName
        $deadline = (Get-Date).AddMinutes(11)
        do { Start-Sleep -Seconds 3 } while ((Get-ScheduledTask -TaskName $taskName).State -eq 'Running' -and (Get-Date) -lt $deadline)
        $result = (Get-ScheduledTaskInfo -TaskName $taskName).LastTaskResult
        $state  = Get-TsState
        Write-Report "검증: 작업 결과 코드 = $result, 상태 = $state"
        $evt = Get-EventLog -LogName Application -Source $eventSource -After $testStart.AddSeconds(-1) -Newest 1 -ErrorAction SilentlyContinue
        Write-Report "검증: 이벤트 로그 = $(if ($evt) { "$($evt.EntryType) $($evt.EventID) $($evt.Message)" } else { '기록 없음' })"
        if ($result -ne 0 -or $state -ne 'Running') { $exitCode = 1; Write-Report '검증 실패' } else { Write-Report '검증 성공' }
        Write-Report '--- connect.log ---'
        Get-Content "$dir\connect.log" -Encoding UTF8 -Tail 20 | ForEach-Object { Write-Report $_ }
    }
} catch {
    $exitCode = 1
    Write-Report "설치 오류: $($_.Exception.Message)"
} finally {
    $ErrorActionPreference = 'Continue'
    if ($ts -and (Get-TsState) -ne 'Running') { & $ts up 2>&1 | Out-Null }   # 어떤 경우든 연결은 복구
    Write-Report "최종 상태: $(Get-TsState)"
    Write-Report "완료 (종료 코드 $exitCode)"
}
exit $exitCode
