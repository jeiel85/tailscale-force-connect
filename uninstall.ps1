# Tailscale 부팅 시 강제 연결 - 제거 (Tailscale 자체와 Unattended 설정은 건드리지 않는다)
$ErrorActionPreference = 'Stop'
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    $p = Start-Process powershell.exe -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`"" -PassThru -Wait
    exit $p.ExitCode
}

$taskName = 'Tailscale Auto Connect'
$dir      = 'C:\ProgramData\TailscaleAutoConnect'

if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
    Write-Host "예약 작업 삭제: $taskName"
} else {
    Write-Host "예약 작업 없음: $taskName"
}
if (Test-Path $dir) {
    Remove-Item -Recurse -Force $dir
    Write-Host "폴더 삭제: $dir"
}
