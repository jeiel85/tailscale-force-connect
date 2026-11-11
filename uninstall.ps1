# Tailscale 부팅 시 강제 연결 - 제거 (Tailscale 자체와 Unattended 설정은 건드리지 않는다)
# 관리자 권한이 아니면 UAC로 스스로 다시 실행하고, 관리자 창의 결과를 이 창에 보여 준다.
param([string]$ResultFile)

$ErrorActionPreference = 'Stop'
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    # 관리자 창은 끝나자마자 닫히므로 결과를 임시 파일로 넘겨받는다.
    $result = Join-Path $env:TEMP ('tailscale-force-connect-uninstall-' + [guid]::NewGuid().ToString('N') + '.txt')
    try {
        $p = Start-Process powershell.exe -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`" -ResultFile `"$result`"" -PassThru -Wait
    } catch {
        Write-Host '관리자 권한을 받지 못해 제거하지 않았습니다. 다시 실행해서 UAC 창에서 [예]를 누르세요.' -ForegroundColor Yellow
        exit 1
    }
    if (Test-Path $result) {
        Get-Content $result -Encoding UTF8
        Remove-Item $result -Force -ErrorAction SilentlyContinue
    }
    exit $p.ExitCode
}

$taskName    = 'Tailscale Auto Connect'
$dir         = 'C:\ProgramData\TailscaleAutoConnect'
$eventSource = 'TailscaleForceConnect'

function Write-Report($m) {
    Write-Host $m
    if ($ResultFile) { Add-Content -Path $ResultFile -Value $m -Encoding UTF8 }
}

$exitCode = 0
try {
    if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
        Write-Report "예약 작업 삭제: $taskName"
    } else {
        Write-Report "예약 작업 없음: $taskName"
    }
    if (Test-Path $dir) {
        Remove-Item -Recurse -Force $dir
        Write-Report "폴더 삭제: $dir"
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
}
exit $exitCode
