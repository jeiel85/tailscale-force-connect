# Tailscale 부팅 시 강제 연결 - 제거 (Tailscale 자체와 Unattended 설정은 건드리지 않는다)
# 관리자 권한이 아니면 UAC로 스스로 다시 실행하고, 관리자 창의 결과를 이 창에 보여 준다.
# -FromGui: 관리 창이 실행한 경우. 관리 창이 닫히기를 기다렸다가 지우고, 결과를 메시지 상자로 보여 준다.
param([string]$ResultFile, [switch]$FromGui)

$ErrorActionPreference = 'Stop'
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    # 관리자 창은 끝나자마자 닫히므로 결과를 새 임시 파일로 넘겨받는다.
    $result = Join-Path $env:TEMP ('tailscale-force-connect-uninstall-' + [guid]::NewGuid().ToString('N') + '.txt')
    try {
        $p = Start-Process powershell.exe -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`" -ResultFile `"$result`"" -PassThru -Wait
    } catch {
        Write-Host '관리자 권한을 받지 못해 제거하지 않았습니다. 다시 실행해서 UAC 창에서 [예]를 누르세요.' -ForegroundColor Yellow
        exit 1
    }
    if (Test-Path -LiteralPath $result) {
        Get-Content -LiteralPath $result -Encoding UTF8
        Remove-Item -LiteralPath $result -Force -ErrorAction SilentlyContinue
    } else {
        Write-Host '관리자 창이 결과를 남기지 못했습니다. 관리자 PowerShell에서 uninstall.ps1을 직접 실행해 확인하세요.' -ForegroundColor Yellow
    }
    exit $p.ExitCode
}

$taskName    = 'Tailscale Auto Connect'
$watchTask   = 'Tailscale Auto Reconnect'
$dir         = 'C:\ProgramData\TailscaleAutoConnect'
$eventSource = 'TailscaleForceConnect'
$appDir      = Join-Path $env:ProgramFiles 'Tailscale Force Connect'
$appName     = 'TailscaleForceConnect'
$shortcut    = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\Tailscale Force Connect.lnk'
$notifyTask  = 'Tailscale Force Connect Update Check'
$alertTask   = 'Tailscale Force Connect Status Alert'
$protoKey    = 'HKLM:\SOFTWARE\Classes\tailscale-force-connect'
$script:reportLines = New-Object System.Collections.Generic.List[string]

# 결과 파일은 사용자가 쓸 수 있는 %TEMP%에 있으므로, 이미 있는 파일(링크 포함)에는 쓰지 않고 새로 만들 때만 쓴다.
$resultWriter = $null
if ($ResultFile) {
    try {
        $fs = [IO.File]::Open($ResultFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        $resultWriter = New-Object IO.StreamWriter($fs, (New-Object Text.UTF8Encoding $true))
        $resultWriter.AutoFlush = $true
    } catch { $resultWriter = $null }
}
function Write-Report($m) {
    Write-Host $m
    $script:reportLines.Add($m)
    if ($resultWriter) { $resultWriter.WriteLine($m) }
}
function Test-Reparse($item) { [bool]($item.Attributes -band [IO.FileAttributes]::ReparsePoint) }

$exitCode = 0
try {
    if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
        # 부팅 직후라 connect.ps1이 아직 재시도 중일 수 있으므로 먼저 멈춘다.
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
        Write-Report "예약 작업 삭제: $taskName"
    } else {
        Write-Report "예약 작업 없음: $taskName"
    }
    if (Get-ScheduledTask -TaskName $alertTask -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $alertTask -Confirm:$false
        Write-Report "예약 작업 삭제: $alertTask"
    }
    # 상태 아이콘(트레이)은 스스로 닫히지 않으므로 알림 작업(트레이를 다시 띄움)을 지운 뒤 바로 닫는다. 모든 로그인 세션의 것을 닫는다.
    $trays = Get-CimInstance Win32_Process -Filter "Name='$appName.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($appDir, [StringComparison]::OrdinalIgnoreCase) -and "$($_.CommandLine)" -match '\s--tray(\s|$)' }
    if ($trays) {
        $trays | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
        Write-Report "트레이 아이콘 닫음: $(@($trays).Count)개"
    }
    if (Get-ScheduledTask -TaskName $notifyTask -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $notifyTask -Confirm:$false
        Write-Report "예약 작업 삭제: $notifyTask"
    }
    if (Test-Path -LiteralPath $protoKey) {
        Remove-Item -LiteralPath $protoKey -Recurse -Force
        Write-Report '알림 링크(tailscale-force-connect:) 등록 삭제'
    }
    if (Get-ScheduledTask -TaskName $watchTask -ErrorAction SilentlyContinue) {
        Stop-ScheduledTask -TaskName $watchTask -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $watchTask -Confirm:$false
        Write-Report "예약 작업 삭제: $watchTask"
    }

    if (Test-Path -LiteralPath $dir) {
        $root = Get-Item -LiteralPath $dir -Force
        if (Test-Reparse $root) {
            # 링크라면 링크만 지운다. 링크가 가리키는 대상은 건드리지 않는다.
            cmd.exe /c rmdir "$dir"
            Write-Report "링크 삭제(대상은 유지): $dir"
        } else {
            # 하위 폴더나 링크는 따라 들어가지 않는다(관리자 권한으로 엉뚱한 곳을 지우지 않도록).
            foreach ($child in Get-ChildItem -LiteralPath $dir -Force) {
                if ($child.PSIsContainer -or (Test-Reparse $child)) {
                    cmd.exe /c rmdir "$($child.FullName)"
                } else {
                    Remove-Item -LiteralPath $child.FullName -Force
                }
            }
            Remove-Item -LiteralPath $dir -Force
            Write-Report "폴더 삭제: $dir"
        }
    } else {
        Write-Report "폴더 없음: $dir"
    }

    # 관리 앱(Program Files)과 시작 메뉴 바로가기. 관리 창이 열려 있으면 실행 파일을 지울 수 없으므로 닫힐 때까지 기다린다.
    if (Test-Path -LiteralPath $appDir) {
        $deadline = (Get-Date).AddSeconds(15)
        while ((Get-Process -Name $appName -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($appDir, [StringComparison]::OrdinalIgnoreCase) }) -and (Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 500
        }
        Get-Process -Name $appName -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($appDir, [StringComparison]::OrdinalIgnoreCase) } | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500
        Remove-Item -LiteralPath $appDir -Recurse -Force
        Write-Report "관리 앱 삭제: $appDir"
    }
    if (Test-Path -LiteralPath $shortcut) {
        Remove-Item -LiteralPath $shortcut -Force
        Write-Report '시작 메뉴 바로가기 삭제'
    }

    # 사용자별 알림 기록(%LOCALAPPDATA%\Tailscale Force Connect). 관리 창과 알림 작업이 사용자 권한으로 만든 것이다.
    # 사용자가 바꿀 수 있는 위치라, 관리자 권한으로 엉뚱한 곳을 지우지 않도록 링크는 따라가지 않고 이 도구가 만드는 파일 이름만 지운다.
    $userFiles = @('notified-version.txt', 'disconnect-notified.txt', 'heal-notified.txt', 'notify-off.txt', 'auto-update-attempted.txt', 'logon-offered.txt', 'tray-off.txt')
    $cleaned = 0
    # 주의: $home·$profile은 PowerShell 자동 변수라 다른 이름을 쓴다.
    foreach ($entry in Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList' -ErrorAction SilentlyContinue) {
        try {
            $profileDir = [Environment]::ExpandEnvironmentVariables([string](Get-ItemProperty -LiteralPath $entry.PSPath -ErrorAction Stop).ProfileImagePath)
            if (-not $profileDir) { continue }
            $stateDir = Join-Path $profileDir 'AppData\Local\Tailscale Force Connect'
            if (-not (Test-Path -LiteralPath $stateDir)) { continue }
            # 프로필 폴더부터 AppData, Local, 기록 폴더까지 한 단계라도 링크면 건너뛴다.
            # 중간 폴더(사용자가 바꿀 수 있음)가 링크면 마지막 폴더만 보고는 프로필 밖을 지우는 것을 막지 못한다.
            $linked = $false
            foreach ($p in @($profileDir, (Join-Path $profileDir 'AppData'), (Join-Path $profileDir 'AppData\Local'), $stateDir)) {
                $item = Get-Item -LiteralPath $p -Force
                if (-not $item.PSIsContainer -or (Test-Reparse $item)) { $linked = $true; break }
            }
            if ($linked) { continue }
            foreach ($name in $userFiles) {
                $f = Join-Path $stateDir $name
                if (Test-Path -LiteralPath $f) {
                    $fi = Get-Item -LiteralPath $f -Force
                    if (-not $fi.PSIsContainer -and -not (Test-Reparse $fi)) { Remove-Item -LiteralPath $f -Force }
                }
            }
            if (-not (Get-ChildItem -LiteralPath $stateDir -Force)) { Remove-Item -LiteralPath $stateDir -Force }
            $cleaned++
        } catch { }
    }
    if ($cleaned) { Write-Report "사용자 알림 기록 삭제: $cleaned 명" }

    if ([Diagnostics.EventLog]::SourceExists($eventSource)) {
        Remove-EventLog -Source $eventSource
        Write-Report "이벤트 로그 원본 삭제: $eventSource"
    }
    Write-Report '제거 완료. Tailscale 설정은 그대로 남아 있습니다.'
} catch {
    $exitCode = 1
    Write-Report "제거 오류: $($_.Exception.Message)"
} finally {
    if ($resultWriter) { $resultWriter.Dispose() }
}
if ($FromGui) {
    # 관리 창은 이미 닫혔으므로 결과를 메시지 상자로 알린다.
    Add-Type -AssemblyName System.Windows.Forms
    $icon = if ($exitCode -eq 0) { 'Information' } else { 'Error' }
    [void][System.Windows.Forms.MessageBox]::Show(($script:reportLines -join "`r`n"), 'Tailscale Force Connect 제거', 'OK', $icon)
}
exit $exitCode
