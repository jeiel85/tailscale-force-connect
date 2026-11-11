# Tailscale 부팅 시 강제 연결 - 제거 (Tailscale 자체와 Unattended 설정은 건드리지 않는다)
# 관리자 권한이 아니면 UAC로 스스로 다시 실행하고, 관리자 창의 결과를 이 창에 보여 준다.
param([string]$ResultFile)

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
exit $exitCode
