# update.ps1의 판단·되돌리기 함수를 관리자 권한 없이 시험한다(가짜 설치 폴더와 가짜 install.ps1 사용).
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\update-helpers.ps1
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\update.ps1')

$script:failures = 0
function Assert-Equal($name, $actual, $expected) {
    if ("$actual" -ceq "$expected") { Write-Host "ok    $name" }
    else { Write-Host "FAIL  $name : '$actual' (기대값 '$expected')" -ForegroundColor Red; $script:failures++ }
}

# --- Get-UpdateAction ---
Assert-Equal '새 버전은 설치'                 (Get-UpdateAction 'v1.8.0' '1.7.0' $false) 'install'
Assert-Equal '두 자리 버전 비교'              (Get-UpdateAction 'v1.10.0' '1.9.3' $false) 'install'
Assert-Equal '같은 버전은 설치하지 않음'      (Get-UpdateAction 'v1.7.0' '1.7.0' $false) 'same'
Assert-Equal '낮은 버전으로 내려가지 않음'    (Get-UpdateAction 'v1.6.0' '1.7.0' $false) 'older'
Assert-Equal '-Force면 낮아도 설치'           (Get-UpdateAction 'v1.6.0' '1.7.0' $true) 'install'
Assert-Equal '설치된 버전을 모르면 설치'      (Get-UpdateAction 'v1.7.0' '' $false) 'install'
Assert-Equal '최신 버전 형식이 다르면 멈춤'   (Get-UpdateAction 'v1.8.0-beta' '1.7.0' $false) 'unknown'

# --- 백업·변경 확인·되돌리기 (가짜 폴더) ---
$root = Join-Path ([IO.Path]::GetTempPath()) ('tfc-update-test-' + [guid]::NewGuid().ToString('N'))
$app = Join-Path $root 'app'; $data = Join-Path $root 'data'; $backup = Join-Path $root 'stage\previous'; $pkg = Join-Path $root 'stage\package'
New-Item -ItemType Directory -Path $app, $data, $pkg | Out-Null
try {
    # 이전 버전의 install.ps1 흉내: 다른 파일을 이전 내용으로 다시 쓰고 성공한다.
    $others = $appFiles | Where-Object { $_ -ne 'install.ps1' }
    $restore = (@($others | ForEach-Object { "Set-Content -LiteralPath '$(Join-Path $app $_)' -Value 'old $_' -Encoding ASCII" }) + "Write-Output 'restored'" + 'exit 0') -join "`r`n"
    function Reset-App {
        foreach ($f in $others) { Set-Content -LiteralPath (Join-Path $app $f) -Value "old $f" -Encoding ASCII }
        Set-Content -LiteralPath (Join-Path $app 'install.ps1') -Value $restore -Encoding ASCII
        Set-Content -LiteralPath (Join-Path $data 'connect.ps1') -Value 'old connect.ps1' -Encoding ASCII
    }
    Reset-App
    Assert-Equal '백업하면 되돌릴 수 있음' (Backup-AppFiles -From $app -Dest $backup -Data $data) $true
    Assert-Equal '바뀐 것 없음'            (Test-InstallChanged -App $app -Backup $backup -Data $data) $false
    Set-Content -LiteralPath (Join-Path $app $appName) -Value 'new exe' -Encoding ASCII
    Assert-Equal '실행 파일이 바뀜'        (Test-InstallChanged -App $app -Backup $backup -Data $data) $true
    Reset-App
    Set-Content -LiteralPath (Join-Path $data 'connect.ps1') -Value 'new connect.ps1' -Encoding ASCII
    Assert-Equal '예약 작업 스크립트가 바뀜' (Test-InstallChanged -App $app -Backup $backup -Data $data) $true
    Reset-App
    Assert-Equal '실행 파일이 없으면 되돌릴 수 없음' (Backup-AppFiles -From $data -Dest (Join-Path $root 'nobackup') -Data $data) $false
    # 설치 전부터 두 connect.ps1이 달랐던 경우(Program Files와 ProgramData)는 바뀐 것으로 보지 않는다.
    $backup2 = Join-Path $root 'stage\previous2'
    Set-Content -LiteralPath (Join-Path $data 'connect.ps1') -Value 'older data connect.ps1' -Encoding ASCII
    [void](Backup-AppFiles -From $app -Dest $backup2 -Data $data)
    Assert-Equal '설치 전부터 달랐던 connect.ps1은 변경 아님' (Test-InstallChanged -App $app -Backup $backup2 -Data $data) $false
    Reset-App

    $newInstaller = Join-Path $pkg 'install.ps1'

    # 1) 새 install.ps1이 파일을 바꾼 뒤 실패 → 이전 버전으로 되돌림
    Set-Content -LiteralPath $newInstaller -Value "Set-Content -LiteralPath '$(Join-Path $app $appName)' -Value 'new exe' -Encoding ASCII`r`nWrite-Output 'half installed'`r`nexit 1" -Encoding ASCII
    $msg = try { Install-WithRollback -Installer $newInstaller -InstallArgs @('-NoTest') -App $app -Data $data -Backup $backup -CanRollback $true; 'no error' } catch { $_.Exception.Message }
    Assert-Equal '도중 실패는 되돌림' ($msg -like '*이전 버전으로 되돌렸습니다*') $true
    Assert-Equal '되돌린 뒤 실행 파일은 이전 것' (Get-Content -LiteralPath (Join-Path $app $appName)) "old $appName"
    Assert-Equal '설치 출력이 보고에 남음' ($lines -contains '  half installed') $true

    # 1-1) 마지막 단계(settings.json 새 버전)까지 마치고 1로 끝남(Unattended 실패 등) → 되돌리지 않음
    Reset-App
    $settingsPath = Join-Path $data 'settings.json'
    Set-Content -LiteralPath $newInstaller -Value "Set-Content -LiteralPath '$(Join-Path $app $appName)' -Value 'new exe' -Encoding ASCII`r`nSet-Content -LiteralPath '$settingsPath' -Value '{ ""version"": ""9.9.9"" }' -Encoding ASCII`r`nexit 1" -Encoding ASCII
    $msg = try { Install-WithRollback -Installer $newInstaller -InstallArgs @('-NoTest') -App $app -Data $data -Backup $backup -CanRollback $true -NewVersion '9.9.9'; 'no error' } catch { $_.Exception.Message }
    Assert-Equal '설치를 마친 뒤 실패는 되돌리지 않음' ($msg -like '*새 버전은 그대로 둡니다*') $true
    Assert-Equal '새 실행 파일 유지' (Get-Content -LiteralPath (Join-Path $app $appName)) 'new exe'

    # 1-2) 같은 버전 다시 설치(-Force): settings.json에 처음부터 그 버전이 있어도, 이번 실행이 쓰지 않았으면 마친 것으로 보지 않음 → 되돌림
    Reset-App
    Set-Content -LiteralPath $newInstaller -Value "Set-Content -LiteralPath '$(Join-Path $app $appName)' -Value 'new exe' -Encoding ASCII`r`nexit 1" -Encoding ASCII
    $msg = try { Install-WithRollback -Installer $newInstaller -InstallArgs @('-NoTest') -App $app -Data $data -Backup $backup -CanRollback $true -NewVersion '9.9.9'; 'no error' } catch { $_.Exception.Message }
    Assert-Equal '같은 버전 재설치 도중 실패는 되돌림' ($msg -like '*이전 버전으로 되돌렸습니다*') $true
    Assert-Equal '같은 버전 재설치 실패 뒤 실행 파일은 이전 것' (Get-Content -LiteralPath (Join-Path $app $appName)) "old $appName"
    Remove-Item -LiteralPath $settingsPath -Force

    # 2) 아무것도 바꾸지 않고 실패(로그인 필요 등) → 되돌리지 않음
    Reset-App
    Set-Content -LiteralPath $newInstaller -Value "Write-Output 'needs login'`r`nexit 1" -Encoding ASCII
    $msg = try { Install-WithRollback -Installer $newInstaller -InstallArgs @('-NoTest') -App $app -Data $data -Backup $backup -CanRollback $true; 'no error' } catch { $_.Exception.Message }
    Assert-Equal '바뀐 것이 없으면 되돌리지 않음' ($msg -like '*바뀌지 않았습니다*') $true

    # 3) 백업이 없으면 되돌리지 못한다고 알림
    Set-Content -LiteralPath $newInstaller -Value "Set-Content -LiteralPath '$(Join-Path $app $appName)' -Value 'new exe' -Encoding ASCII`r`nexit 1" -Encoding ASCII
    $msg = try { Install-WithRollback -Installer $newInstaller -InstallArgs @('-NoTest') -App $app -Data $data -Backup $backup -CanRollback $false; 'no error' } catch { $_.Exception.Message }
    Assert-Equal '백업이 없으면 알림' ($msg -like '*되돌리지 못했습니다*') $true

    # 4) 성공하면 예외 없음, 인자도 그대로 전달
    Reset-App
    Set-Content -LiteralPath $newInstaller -Value "Write-Output (`$args -join ',')`r`nexit 0" -Encoding ASCII
    $msg = try { Install-WithRollback -Installer $newInstaller -InstallArgs @('-NoTest', '-ReconnectAfter', '1h') -App $app -Data $data -Backup $backup -CanRollback $true; 'no error' } catch { $_.Exception.Message }
    Assert-Equal '성공' $msg 'no error'
    Assert-Equal '인자 전달' ($lines -contains '  -NoTest,-ReconnectAfter,1h') $true
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($script:failures) { Write-Host "$($script:failures)개 실패" -ForegroundColor Red; exit 1 }
Write-Host '모두 통과'
exit 0
