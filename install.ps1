# Tailscale 부팅 시 강제 연결 - 설치
#   powershell -ExecutionPolicy Bypass -File .\install.ps1          # 설치 + 끊긴 상태에서 동작 검증
#   powershell -ExecutionPolicy Bypass -File .\install.ps1 -NoTest  # 설치만 (연결을 끊지 않음)
#   ... -ReconnectAfter 30m   # 직접 끊은 뒤 자동 재연결까지의 시간 (기본 1h, 0이면 끔. 단위는 h, m)
# 관리자 권한이 아니면 UAC로 스스로 다시 실행하고, 관리자 창의 결과를 이 창에 보여 준다.
param([switch]$NoTest, [string]$ReconnectAfter = '1h', [string]$ResultFile)

$ErrorActionPreference = 'Stop'
$dir         = 'C:\ProgramData\TailscaleAutoConnect'
$taskName    = 'Tailscale Auto Connect'
$eventSource = 'TailscaleForceConnect'
$logFile     = Join-Path $dir 'install.log'
$watchTask   = 'Tailscale Auto Reconnect'
$version     = '1.5.0'
$appDir      = Join-Path $env:ProgramFiles 'Tailscale Force Connect'   # 관리 앱(GUI)과 스크립트 원본
$appExe      = 'TailscaleForceConnect.exe'
$shortcut    = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\Tailscale Force Connect.lnk'
$appId       = 'TailscaleForceConnect.Manager'          # Windows 알림의 앱 ID (시작 메뉴 바로가기에 기록)
$protocol    = 'tailscale-force-connect'                # 알림을 누르면 관리 창을 여는 링크
$notifyTask  = 'Tailscale Force Connect Update Check'   # 로그인할 때·매일 새 버전을 확인해 알림 (사용자 권한)
$alertTask   = 'Tailscale Force Connect Status Alert'   # 1분마다 연결 상태를 보고 끊김·재연결을 알림 (사용자 권한)

# 직접 끊은 뒤 다시 연결할 때까지의 시간. 30m, 1h, 1h30m처럼 h·m 단위로 받는다. 0이면 감시 작업을 만들지 않는다.
$ReconnectAfter = $ReconnectAfter.Trim().ToLower()
$reconnectOff = @('0', 'off', '') -contains $ReconnectAfter
$reconnectMinutes = 0
if (-not $reconnectOff) {
    if ($ReconnectAfter -match '^(?:(\d+)h)?(?:(\d+)m)?$' -and $ReconnectAfter -ne '') {
        $reconnectMinutes = [int]$Matches[1] * 60 + [int]$Matches[2]
    }
    if ($reconnectMinutes -lt 1) {
        Write-Host "-ReconnectAfter 값 '$ReconnectAfter'이(가) 올바르지 않습니다. 30m, 1h, 1h30m처럼 쓰거나 0으로 끄세요." -ForegroundColor Yellow
        exit 1
    }
}

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    # 관리자 창은 끝나자마자 닫히므로 결과를 새 임시 파일로 넘겨받는다(이전 install.log를 잘못 보여 주지 않도록).
    $result  = Join-Path $env:TEMP ('tailscale-force-connect-install-' + [guid]::NewGuid().ToString('N') + '.txt')
    $argList = "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`" -ResultFile `"$result`" -ReconnectAfter `"$ReconnectAfter`""
    if ($NoTest) { $argList += ' -NoTest' }
    try {
        $p = Start-Process powershell.exe -Verb RunAs -ArgumentList $argList -PassThru -Wait
    } catch {
        Write-Host '관리자 권한을 받지 못해 설치하지 않았습니다. 다시 실행해서 UAC 창에서 [예]를 누르세요.' -ForegroundColor Yellow
        exit 1
    }
    if (Test-Path -LiteralPath $result) {
        Get-Content -LiteralPath $result -Encoding UTF8
        Remove-Item -LiteralPath $result -Force -ErrorAction SilentlyContinue
    } else {
        Write-Host '관리자 창이 결과를 남기지 못했습니다. 관리자 PowerShell에서 install.ps1을 직접 실행해 오류를 확인하세요.' -ForegroundColor Yellow
    }
    exit $p.ExitCode
}

# 결과 파일은 사용자가 쓸 수 있는 %TEMP%에 있으므로, 이미 있는 파일(링크 포함)에는 쓰지 않고 새로 만들 때만 쓴다.
$script:resultWriter = $null
if ($ResultFile) {
    try {
        $fs = [IO.File]::Open($ResultFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        $script:resultWriter = New-Object IO.StreamWriter($fs, (New-Object Text.UTF8Encoding $true))
        $script:resultWriter.AutoFlush = $true
    } catch { $script:resultWriter = $null }
}
$script:logReady = $false

# 주의: 'R' 같은 짧은 이름은 기본 별칭(r = Invoke-History)과 겹쳐 함수가 무시되므로 쓰지 않는다.
function Write-Report($m) {
    Write-Host $m
    if ($script:resultWriter) { $script:resultWriter.WriteLine($m) }
    if ($script:logReady) { Add-Content -LiteralPath $logFile -Value $m -Encoding UTF8 }
}
function Get-TsState {
    if (-not $ts) { return $null }
    try { (& $ts status --json 2>$null | Out-String | ConvertFrom-Json).BackendState } catch { $null }
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

# SYSTEM이 실행할 스크립트를 두는 폴더를 만든다. 일반 사용자는 C:\ProgramData에 폴더를 만들 수 있으므로,
# 미리 만들어 둔 폴더(소유자가 일반 사용자)나 링크를 그대로 쓰면 스크립트를 바꿔치기당할 수 있다.
function Initialize-InstallDir {
    $sidAdmins = 'S-1-5-32-544'; $sidSystem = 'S-1-5-18'; $sidUsers = 'S-1-5-32-545'
    $trusted   = @($sidAdmins, $sidSystem)
    $sid = { param($s) New-Object Security.Principal.SecurityIdentifier($s) }

    $acl = New-Object System.Security.AccessControl.DirectorySecurity
    $acl.SetOwner((& $sid $sidAdmins))
    $acl.SetAccessRuleProtection($true, $false)
    $inh = [Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit'
    $none = [Security.AccessControl.PropagationFlags]::None
    foreach ($s in $sidSystem, $sidAdmins) {
        $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule((& $sid $s), 'FullControl', $inh, $none, 'Allow')))
    }
    $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule((& $sid $sidUsers), 'ReadAndExecute', $inh, $none, 'Allow')))

    if (Test-Path -LiteralPath $dir) {
        $item = Get-Item -LiteralPath $dir -Force
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "$dir 이(가) 링크(reparse point)입니다. 직접 확인해 지운 뒤 다시 설치하세요."
        }
        $owner = (Get-Acl -LiteralPath $dir).GetOwner([Security.Principal.SecurityIdentifier]).Value
        if ($trusted -notcontains $owner) {
            throw "$dir 의 소유자($owner)가 관리자나 SYSTEM이 아닙니다. 누가 만들었는지 확인해 지운 뒤 다시 설치하세요."
        }
        # 이 도구는 하위 폴더를 만들지 않는다. 폴더·링크가 있으면 따라 들어가지 않고 바로 의심 항목으로 본다.
        $suspicious = Get-ChildItem -LiteralPath $dir -Force | Where-Object {
            $_.PSIsContainer -or
            ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
            ($trusted -notcontains (Get-Acl -LiteralPath $_.FullName).GetOwner([Security.Principal.SecurityIdentifier]).Value)
        }
        if ($suspicious) {
            throw "$dir 안에 관리자가 만들지 않은 항목이 있습니다: $(($suspicious | ForEach-Object Name) -join ', '). 확인해 지운 뒤 다시 설치하세요."
        }
        Set-Acl -LiteralPath $dir -AclObject $acl
    } else {
        # 처음부터 제한된 ACL로 만들어, 만든 직후 일반 사용자가 파일을 끼워 넣을 틈을 없앤다.
        [void][IO.Directory]::CreateDirectory($dir, $acl)
    }
}

# 시작 메뉴 바로가기에 System.AppUserModel.ID를 기록한다. 데스크톱 앱이 Windows 알림을 띄우려면 필요하다.
function Set-ShortcutAppId([string]$Path, [string]$AppId) {
    if (-not ('TfcShortcutAppId' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
[ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface ITfcPropertyStore {
    [PreserveSig] int GetCount(out uint count);
    [PreserveSig] int GetAt(uint index, out TfcPropertyKey key);
    [PreserveSig] int GetValue(ref TfcPropertyKey key, IntPtr value);
    [PreserveSig] int SetValue(ref TfcPropertyKey key, ref TfcPropVariant value);
    [PreserveSig] int Commit();
}
[StructLayout(LayoutKind.Sequential, Pack = 4)]
struct TfcPropertyKey { public Guid fmtid; public uint pid; }
[StructLayout(LayoutKind.Explicit, Size = 24)]
struct TfcPropVariant { [FieldOffset(0)] public ushort vt; [FieldOffset(8)] public IntPtr p; }
public static class TfcShortcutAppId {
    [DllImport("shell32.dll", CharSet = CharSet.Unicode, PreserveSig = false)]
    static extern void SHGetPropertyStoreFromParsingName(string path, IntPtr bindCtx, int flags, ref Guid iid, [MarshalAs(UnmanagedType.Interface)] out ITfcPropertyStore store);
    public static void Set(string path, string appId) {
        Guid iid = typeof(ITfcPropertyStore).GUID;
        ITfcPropertyStore store;
        SHGetPropertyStoreFromParsingName(path, IntPtr.Zero, 2 /* GPS_READWRITE */, ref iid, out store);
        TfcPropertyKey key = new TfcPropertyKey { fmtid = new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"), pid = 5 };
        TfcPropVariant value = new TfcPropVariant { vt = 31 /* VT_LPWSTR */, p = Marshal.StringToCoTaskMemUni(appId) };
        try {
            Marshal.ThrowExceptionForHR(store.SetValue(ref key, ref value));
            Marshal.ThrowExceptionForHR(store.Commit());
        } finally {
            Marshal.FreeCoTaskMem(value.p);
            Marshal.ReleaseComObject(store);
        }
    }
}
'@
    }
    [TfcShortcutAppId]::Set($Path, $AppId)
}

$exitCode = 0
$didDown  = $false
$ts = Find-TailscaleExe
try {
    # 0) 설치 폴더 준비. install.log도 이 폴더에 쓰므로 가장 먼저 한다.
    Initialize-InstallDir
    Set-Content -LiteralPath $logFile -Value "설치 시작 $(Get-Date -Format s)" -Encoding UTF8
    $script:logReady = $true
    Write-Host "설치 시작 $(Get-Date -Format s)"
    if ($script:resultWriter) { $script:resultWriter.WriteLine("설치 시작 $(Get-Date -Format s)") }

    if (-not $ts) { throw 'Tailscale을 찾지 못했습니다. Tailscale을 설치하고 로그인한 뒤 다시 실행하세요.' }
    Write-Report "Tailscale: $ts"

    # 로그인되지 않은 상태에서 'tailscale up'은 인증을 무기한 기다리므로 미리 확인한다. Stopped(끊김)는 정상이다.
    $state0 = Get-TsState
    if (@('Running', 'Stopped', 'Starting') -notcontains $state0) {
        throw "Tailscale 상태가 '$state0'입니다. Tailscale에 로그인한 뒤 다시 설치하세요."
    }

    # 0-1) 관리 앱 설치: GUI와 스크립트 원본을 Program Files(관리자만 쓸 수 있는 곳)에 두고 시작 메뉴 바로가기를 만든다.
    #      GUI의 설치·제거 버튼은 여기 있는 스크립트를 관리자 권한으로 실행한다. GUI가 여기서 실행한 경우에는 복사하지 않는다.
    $srcExe  = Join-Path $PSScriptRoot $appExe
    $fromApp = ([IO.Path]::GetFullPath($PSScriptRoot).TrimEnd('\') -ieq [IO.Path]::GetFullPath($appDir).TrimEnd('\'))
    if ($fromApp) {
        Write-Report "관리 앱: $appDir"
    } elseif (Test-Path -LiteralPath $srcExe) {
        # 업데이트할 때 관리 창이 열려 있으면 실행 파일을 덮어쓸 수 없으므로 닫는다.
        $running = Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($appExe)) -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -and ($_.Path -ieq (Join-Path $appDir $appExe)) }
        if ($running) { $running | Stop-Process -Force; Start-Sleep -Seconds 1; Write-Report '열려 있던 관리 창을 닫음 (업데이트)' }
        New-Item -ItemType Directory -Force -Path $appDir | Out-Null
        foreach ($f in $appExe, 'install.ps1', 'uninstall.ps1', 'connect.ps1', 'update.ps1') {
            Copy-Item -LiteralPath (Join-Path $PSScriptRoot $f) -Destination $appDir -Force
        }
        $wsh = New-Object -ComObject WScript.Shell
        $lnk = $wsh.CreateShortcut($shortcut)
        $lnk.TargetPath = Join-Path $appDir $appExe
        $lnk.WorkingDirectory = $appDir
        $lnk.Description = 'Tailscale 자동 연결 상태 보기·설정'
        $lnk.Save()
        Write-Report "관리 앱 설치: $appDir (시작 메뉴: Tailscale Force Connect)"
    } else {
        Write-Report '관리 앱: 패키지에 없어 건너뜀 (스크립트만 설치)'
    }

    # 0-2) 새 버전 Windows 알림. 관리 앱이 설치된 경우에만 등록한다.
    #      - 시작 메뉴 바로가기에 앱 ID를 넣어야 알림이 이 앱 이름·아이콘으로 표시된다.
    #      - 알림을 누르면 tailscale-force-connect: 링크로 관리 창을 연다.
    #      - 확인은 로그인한 사용자 권한으로 도는 작업이 한다(상주 프로세스 없음).
    $installedExe = Join-Path $appDir $appExe
    if (Test-Path -LiteralPath $installedExe) {
        if (-not (Test-Path -LiteralPath $shortcut)) {
            $wsh = New-Object -ComObject WScript.Shell
            $lnk = $wsh.CreateShortcut($shortcut)
            $lnk.TargetPath = $installedExe
            $lnk.WorkingDirectory = $appDir
            $lnk.Description = 'Tailscale 자동 연결 상태 보기·설정'
            $lnk.Save()
        }
        Set-ShortcutAppId -Path $shortcut -AppId $appId

        $protoKey = "HKLM:\SOFTWARE\Classes\$protocol"
        New-Item -Path "$protoKey\shell\open\command" -Force | Out-Null   # 이 도구 전용 키라 새로 써도 된다
        Set-Item -Path $protoKey -Value 'URL:Tailscale Force Connect'
        New-ItemProperty -Path $protoKey -Name 'URL Protocol' -Value '' -PropertyType String -Force | Out-Null
        New-Item -Path "$protoKey\DefaultIcon" -Force | Out-Null
        Set-Item -Path "$protoKey\DefaultIcon" -Value "`"$installedExe`",0"
        Set-Item -Path "$protoKey\shell\open\command" -Value "`"$installedExe`" `"%1`""

        $nAction    = New-ScheduledTaskAction -Execute $installedExe -Argument '--notify-update'
        $nLogon     = New-ScheduledTaskTrigger -AtLogOn
        $nLogon.Delay = 'PT2M'
        $nDaily     = New-ScheduledTaskTrigger -Daily -At '12:30'
        $nPrincipal = New-ScheduledTaskPrincipal -GroupId 'S-1-5-32-545' -RunLevel Limited   # Users: 로그인한 사용자로 실행
        $nSettings  = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Minutes 5) -MultipleInstances IgnoreNew
        Register-ScheduledTask -TaskName $notifyTask -Action $nAction -Trigger @($nLogon, $nDaily) -Principal $nPrincipal -Settings $nSettings -Description '새 버전이 있으면 Windows 알림으로 알린다. 업데이트는 알림을 눌러 관리 창에서 한다.' -Force | Out-Null
        Write-Report "새 버전 알림: 로그인할 때와 매일 12:30에 확인 (예약 작업 $notifyTask)"

        # 연결 끊김·재연결 알림: 로그인한 사용자 권한으로 1분마다 상태만 확인한다(창 없음, 상주 프로세스 없음).
        # 시간 기준 반복 트리거는 재부팅 뒤에도 이어지고, 로그인해 있는 동안에만 실행된다.
        $aAction  = New-ScheduledTaskAction -Execute $installedExe -Argument '--watch-notify'
        $aTrigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Minutes 1)
        $aSettings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew
        Register-ScheduledTask -TaskName $alertTask -Action $aAction -Trigger $aTrigger -Principal $nPrincipal -Settings $aSettings -Description '1분마다 Tailscale 연결 상태를 확인해, 끊기거나 다시 연결되면 Windows 알림으로 알린다.' -Force | Out-Null
        Write-Report "연결 알림: 1분마다 확인해 끊김·재연결을 알림 (예약 작업 $alertTask)"
    }

    # 1) 스크립트 복사 (새 파일은 폴더의 제한된 ACL을 물려받는다)
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'connect.ps1') -Destination $dir -Force
    # 한 줄 설치로 받은 경우 내려받은 폴더가 지워지므로 제거 스크립트도 함께 둔다.
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'uninstall.ps1') -Destination $dir -Force
    Write-Report "스크립트 설치: $dir\connect.ps1, uninstall.ps1"

    # 2) Unattended 모드: 로그인 전·로그아웃 후에도 연결 유지.
    #    이미 켜져 있어도 같은 값을 다시 쓰는 것뿐이라 항상 실행한다(불안정한 'tailscale debug prefs' 출력에 기대지 않음).
    $ErrorActionPreference = 'Continue'
    $out = & $ts set --unattended=true 2>&1 | Out-String
    $setCode = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($setCode -eq 0) { Write-Report 'Unattended 모드: 켜짐' }
    else {
        $exitCode = 1   # 로그인 전 부팅에서 연결되지 않을 수 있으므로 설치 실패로 본다
        Write-Report "오류: Unattended 모드를 켜지 못함 (종료 코드 $setCode) $($out.Trim())"
    }

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

    # 4-1) 직접 끊은 뒤 자동 재연결: SYSTEM 작업이 주기적으로 상태를 보고, 끊긴 지 N분이 지나면 다시 연결한다.
    if ($reconnectOff) {
        if (Get-ScheduledTask -TaskName $watchTask -ErrorAction SilentlyContinue) {
            Unregister-ScheduledTask -TaskName $watchTask -Confirm:$false
        }
        Remove-Item -LiteralPath (Join-Path $dir 'stopped-since.txt') -Force -ErrorAction SilentlyContinue
        Write-Report '자동 재연결: 끔 (-ReconnectAfter 0)'
    } else {
        $interval  = if ($reconnectMinutes -lt 10) { 1 } else { 5 }   # 확인 주기(분). 실제 재연결은 설정 시간 + 최대 한 주기
        $wAction   = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$dir\connect.ps1`" -Watch -ReconnectAfterMinutes $reconnectMinutes"
        $wTrigger  = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Minutes $interval)
        $wSettings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Minutes 2) -MultipleInstances IgnoreNew
        Register-ScheduledTask -TaskName $watchTask -Action $wAction -Trigger $wTrigger -Principal $principal -Settings $wSettings -Description "직접 끊은 Tailscale을 $reconnectMinutes 분 뒤 다시 연결한다." -Force | Out-Null
        Remove-Item -LiteralPath (Join-Path $dir 'stopped-since.txt') -Force -ErrorAction SilentlyContinue
        Write-Report "자동 재연결: 직접 끊으면 약 $reconnectMinutes 분 뒤 다시 연결 (예약 작업 $watchTask, $interval 분마다 확인)"
    }

    # 4-2) 관리 창이 관리자 권한 없이 읽을 수 있는 설정 요약 (폴더 ACL상 Users는 읽기만 가능)
    $settings = [ordered]@{ version = $version; reconnectMinutes = $reconnectMinutes; installedAt = (Get-Date).ToString('s') }
    Set-Content -LiteralPath (Join-Path $dir 'settings.json') -Value ($settings | ConvertTo-Json) -Encoding UTF8
    Write-Report "설정 기록: $dir\settings.json (v$version)"

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
        $didDown = $true
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
    # 검증 때문에 직접 끊었는데 아직 연결되지 않았으면 복구한다. 응답이 없을 때를 대비해 30초만 기다린다.
    if ($didDown -and (Get-TsState) -ne 'Running') {
        $p = Start-Process -FilePath $ts -ArgumentList 'up' -NoNewWindow -PassThru
        if (-not $p.WaitForExit(30000)) { $p.Kill() }
    }
    Write-Report "최종 상태: $(Get-TsState)"
    Write-Report "완료 (종료 코드 $exitCode)"
    if ($script:resultWriter) { $script:resultWriter.Dispose() }
}
exit $exitCode
