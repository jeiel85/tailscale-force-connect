# tailscale-force-connect

Windows PC에서 **재부팅할 때마다 Tailscale을 연결 상태로 되돌립니다.** 직접 끊을 수도 있지만, 끊은 상태는 **1시간**(바꿀 수 있음)까지만 유지되고 그 뒤에는 자동으로 다시 연결됩니다.

> **English:** A tiny PowerShell + Task Scheduler setup that makes Tailscale on Windows reconnect on every boot,
> even if you clicked *Disconnect* before shutting down. It runs as SYSTEM before anyone logs in, keeps your
> existing `tailscale up` settings, and still lets you disconnect manually — Tailscale reconnects by itself
> after about 1 hour (configurable): a SYSTEM task checks every 5 minutes and reconnects once it has been off that long.

🌐 소개 페이지: <https://jeiel85.github.io/tailscale-force-connect/>

## 왜 필요한가

Tailscale은 마지막 연결 상태를 기억합니다. 트레이에서 **Disconnect**를 누른 채로 재부팅하면(`WantRunning=false`)
끊긴 상태가 그대로 유지됩니다. 원격으로 접속해야 하는 PC가 정전이나 강제 재부팅 뒤에 끊긴 채로 켜지면
그 PC 앞으로 직접 가야 합니다.

이 도구는 두 가지를 합니다.

1. **부팅할 때:** SYSTEM 권한으로 `tailscale up`을 실행해 끊긴 상태를 덮어씁니다.
2. **평소에 직접 끊었을 때:** SYSTEM 예약 작업 `Tailscale Auto Reconnect`가 5분마다 상태를 확인해, 끊긴 상태가
   **1시간** 넘게 이어지면 다시 연결합니다. 트레이 메뉴로 끊었든 `tailscale down`으로 끊었든 똑같이 동작합니다.

## 먼저 공식 기능을 검토하세요

Tailscale Windows 클라이언트 **1.84 이상**에는 비슷한 목적의 [시스템 정책](https://tailscale.com/docs/features/tailscale-system-policies)이 있습니다.
관리하는 기기가 많거나 MDM·GPO를 쓴다면 공식 정책이 더 맞습니다.

| 방법 | 직접 끊기 | 동작 |
|---|---|---|
| `AlwaysOn.Enabled` 정책 | ❌ 불가 | 연결을 끊거나 클라이언트를 종료할 수 없게 막습니다. `AlwaysOn.OverrideWithReason`을 켜면 사유를 입력하고 끊을 수 있습니다(사유는 관리 콘솔 로그에 남음). |
| `ReconnectAfter` 정책 | ⭕ 가능 | 끊긴 상태로 있을 수 있는 시간을 정합니다(예: `30m`). 단, 이 PC(1.104.5)에서 `tailscale down` 뒤에는 시간이 지나도 다시 연결되지 않았습니다(아래 참고). |
| **이 도구** | ⭕ 가능 | 직접 끊으면 약 1시간(바꿀 수 있음) 뒤 다시 연결하고, **부팅할 때는 기다리지 않고 바로** 다시 연결합니다. |

공식 `ReconnectAfter` 정책도 시험해 봤습니다. 정책은 적용됐지만(`tailscale syspolicy list`에 표시), `tailscale down` 뒤 1~2분이 지나도
다시 연결되지 않았습니다. 그래서 이 도구는 정책에 기대지 않고 상태를 직접 확인합니다. 정책이 잘 동작하는 환경이라면 정책을 쓰는 편이 더 단순합니다.

## 관리 창

설치하면 **Tailscale Force Connect** 관리 창이 함께 설치되고, 시작 메뉴에서 열 수 있습니다. 트레이에 상주하지 않고 열었을 때만 동작합니다.

- 지금 연결 상태와 Tailscale 기기 이름·IP
- 부팅 시 자동 연결, 직접 끊은 뒤 자동 재연결 설정. 끊긴 상태라면 몇 분 뒤 다시 연결되는지도 보여 줍니다.
- 최근 기록(`connect.log`)
- 버튼: 재연결 시간 바꾸기, 동작 확인, 제거. 이 버튼을 누를 때만 UAC 창이 뜹니다.
- 새 버전 알림: 창을 열 때 GitHub 최신 릴리스를 확인하고, 새 버전이 있으면 위쪽에 [업데이트] 버튼을 보여 줍니다(아래 "업데이트" 참고).

창을 여는 것만으로는 관리자 권한이 필요 없습니다. 상태는 일반 권한으로 읽을 수 있는 정보(`tailscale status`, `settings.json`, `connect.log`)만 씁니다.
버튼은 `C:\Program Files\Tailscale Force Connect\`에 있는 스크립트를 관리자 권한으로 실행합니다. 이 폴더는 관리자만 쓸 수 있습니다.
Rust와 [Slint](https://slint.dev)로 만들었고(소프트웨어 렌더러), GPU가 없는 환경이나 원격 데스크톱에서도 그려집니다. 실행 파일은 약 8MB입니다.

> **원격으로 접속 중이라면** "동작 확인"을 누르지 마세요. 연결을 몇 초 끊었다가 다시 연결되는지 보는 기능이라, 그 사이 원격 연결이 끊깁니다.

## 업데이트

관리 창에 "새 버전" 알림이 뜨면 **[업데이트]** 를 누르세요. UAC 창이 한 번 뜨고, 관리 창이 닫혔다가 새 버전으로 다시 열립니다.

업데이트는 [`update.ps1`](update.ps1)이 관리자 권한으로 진행합니다.

1. GitHub 최신 릴리스의 zip을 관리자만 쓸 수 있는 `C:\Program Files\Tailscale Force Connect\` 안의 임시 폴더에 받습니다.
2. GitHub이 릴리스 파일마다 기록하는 **SHA-256 값과 비교**합니다. 다르거나 기록이 없으면 중단합니다.
3. 지금의 자동 재연결 설정을 유지한 채 `install.ps1 -NoTest`로 설치합니다. **연결은 끊지 않습니다.**

새 버전은 자동으로 확인하지만, 설치는 사용자가 눌러야 진행합니다. 사용자 승인 없이 SYSTEM 권한으로 몰래 업데이트하는 방식은 쓰지 않습니다. 그 방식이면 릴리스가 오염됐을 때 받은 코드가 모든 설치 PC에서 곧바로 SYSTEM 권한으로 실행되기 때문입니다([DECISIONS.md](DECISIONS.md)).
명령으로 업데이트하려면 한 줄 설치를 다시 실행하거나, 관리자 PowerShell에서 `& "C:\Program Files\Tailscale Force Connect\update.ps1"`를 실행합니다.

## 설치

필요한 것: Windows 10/11, Tailscale에 로그인한 상태(로그인되어 있지 않으면 설치가 멈추지 않고 바로 실패합니다), 관리자 권한. Tailscale 설치 위치는 서비스 등록 정보에서 찾으므로 기본 경로가 아니어도 됩니다.
세 가지 방법 모두 설치 중에 UAC 창이 한 번 뜹니다.

**방법 1. 한 줄로 설치** — cmd나 PowerShell에 붙여 넣습니다.

```bat
powershell -c "irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex"
```

[`get.ps1`](docs/get.ps1)이 [최신 릴리스](https://github.com/jeiel85/tailscale-force-connect/releases/latest) zip을 임시 폴더에 받아 `install.ps1`을 실행합니다.
`install.ps1`은 먼저 관리 창을 Program Files에 설치하고 나서 예약 작업을 등록합니다. 다 끝나면 관리 창을 열고 임시 폴더를 지웁니다. 인터넷에서 받은 스크립트를 바로 실행하는 방식이니, 먼저 내용을 읽어 보고 싶다면 방법 2를 쓰세요.

**방법 2. zip 받아서 더블클릭**

1. [`tailscale-force-connect.zip`](https://github.com/jeiel85/tailscale-force-connect/releases/latest/download/tailscale-force-connect.zip)을 받아 압축을 풉니다.
2. `install.cmd`를 더블클릭합니다. `TailscaleForceConnect.exe`를 바로 열어 "설치하기"를 눌러도 됩니다.

"Windows의 PC 보호" 창이 뜨면 **추가 정보 → 실행**을 누르세요. 서명하지 않은 스크립트라서 뜨는 경고입니다.

**방법 3. git으로 받기**

```powershell
git clone https://github.com/jeiel85/tailscale-force-connect.git
cd tailscale-force-connect
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

설치 결과:

- 설치가 끝나면 **몇 초 동안 일부러 연결을 끊었다가** 예약 작업으로 다시 연결되는지 확인합니다.
  그 PC에 Tailscale로 원격 접속해 있다면 이 확인을 건너뛰세요.
  - 방법 1 (cmd): `set TFC_NOTEST=1 && powershell -c "irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex"`
  - 방법 1 (PowerShell): `$env:TFC_NOTEST=1; irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex`
    (PowerShell에서는 `set`이 환경 변수를 만들지 않으므로 cmd 줄을 그대로 붙여 넣지 마세요. 값은 그 창을 닫을 때까지 남습니다.)
  - 방법 2: `install.cmd -NoTest`
  - 방법 3: `install.ps1 -NoTest`
- 마지막 줄이 `완료 (종료 코드 0)`이고 그 앞에 `검증 성공`이 있으면 설치가 끝난 것입니다. 한 줄 설치는 실패하면 오류를 출력하고 0이 아닌 종료 코드로 끝납니다.
- Unattended 모드(로그인 전에도 연결 유지)를 켭니다.
- 직접 끊은 뒤 자동으로 다시 연결될 때까지의 시간을 정합니다. 기본값은 `1h`입니다. 감시 작업이 5분마다 확인하므로 실제로는 설정 시간보다 최대 10분쯤 늦게(끊긴 것을 처음 확인하는 데 한 주기, 시간이 다 된 것을 확인하는 데 한 주기) 다시 연결됩니다.
  - 바꾸려면: `install.cmd -ReconnectAfter 30m`, 한 줄 설치는 cmd에서 `set TFC_RECONNECT_AFTER=30m && powershell -c "..."`,
    PowerShell에서 `$env:TFC_RECONNECT_AFTER='30m'; irm ... | iex`
  - 끄려면 값을 `0`으로 줍니다. 이미 등록된 감시 작업이 있으면 지웁니다.
  - 형식은 `h`·`m` 단위입니다(`30m`, `1h`, `1h30m`). 10분 미만으로 정하면 1분마다 확인합니다.

> **Windows 빠른 시작을 확인하세요.** 빠른 시작이 켜져 있으면(Windows 기본값) **일반 종료 후 전원을 켤 때**는 완전 부팅이 아니라
> 최대 절전 복귀가 되어 이 작업이 실행되지 않습니다. 재시작과 정전 뒤 부팅에서는 동작합니다. 설치 스크립트가 빠른 시작이 켜진 것을 감지하면 경고를 출력합니다.
> 항상 동작하게 하려면 제어판 → 전원 옵션 → 전원 단추 작동 설정에서 **빠른 시작 켜기**를 끄세요.

**권장:** [관리 콘솔](https://login.tailscale.com/admin/machines)에서 그 기기의 **Disable key expiry**를 켜세요.
키가 만료되면(기본 180일) 다시 로그인해야 하므로 자동 연결이 되지 않습니다. 이때는 `connect.log`와 이벤트 로그에 `로그인이 필요함`이 남습니다.

## 구성

| 파일 | 역할 |
|---|---|
| `connect.ps1` | 부팅 때 실행되는 본체입니다. 서비스가 꺼져 있으면 켜고, 연결될 때까지 최대 10분 동안 10초 간격으로 `tailscale up`을 실행합니다. 결과는 로그 파일과 이벤트 로그에 남깁니다. |
| `install.ps1` | `connect.ps1`을 `C:\ProgramData\TailscaleAutoConnect\`에 복사하고, Unattended 모드를 켜고, 이벤트 로그 원본과 예약 작업 2개(부팅 시 연결, 자동 재연결 감시)를 등록한 뒤 동작을 확인합니다. |
| `uninstall.ps1` | 예약 작업 2개, 설치 폴더, 이벤트 로그 원본을 지우고 결과를 보여 줍니다. Tailscale 본체와 Unattended 설정은 건드리지 않습니다. |
| `update.ps1` | 최신 릴리스를 받아 SHA-256을 확인한 뒤, 설정을 유지하고 연결을 끊지 않은 채 설치합니다. 관리 창의 [업데이트] 버튼이 실행합니다. |
| `install.cmd`, `uninstall.cmd` | 더블클릭용 실행 파일입니다. 같은 폴더의 `.ps1`을 실행합니다. |
| `TailscaleForceConnect.exe` | 관리 창입니다(릴리스 zip에만 들어 있음). 소스는 `gui/`에 있습니다. |
| `gui/` | 관리 창 소스(Rust + Slint)입니다. |
| `scripts/build-release.ps1` | 관리 창을 빌드하고 릴리스 zip을 만듭니다. |
| `docs/get.ps1`, `docs/remove.ps1` | 한 줄 설치·제거용 스크립트입니다. GitHub Pages로 배포되며, 릴리스 zip에는 들어가지 않습니다. |

설치되는 것:

- 예약 작업 **`Tailscale Auto Connect`**: 시작 시 20초 뒤에 SYSTEM 계정으로 실행되며, 실행 시간은 최대 15분입니다.
- 예약 작업 **`Tailscale Auto Reconnect`**: SYSTEM 계정으로 5분마다 실행됩니다(`connect.ps1 -Watch`). 끊긴 상태(`Stopped`)를 처음 본 시각을
  설치 폴더의 `stopped-since.txt`에 적어 두고, 설정한 시간이 지나도 끊겨 있으면 `tailscale up`을 실행합니다. 연결돼 있으면 아무 기록도 남기지 않습니다.
  로그인이 필요한 상태처럼 `tailscale up`으로 풀 수 없는 상태는 건드리지 않습니다.
- 이벤트 로그 원본 **`TailscaleForceConnect`**(응용 프로그램 로그): 부팅할 때마다 결과를 남깁니다. ID `1000` 연결됨(정보), `1004` 직접 끊은 뒤 자동 재연결(정보),
  `1001` 10분 안에 연결 실패, `1002` 로그인 필요, `1003` `tailscale.exe` 없음(이상 오류).
- 폴더 `C:\Program Files\Tailscale Force Connect\`: 관리 창과 스크립트 원본입니다. 시작 메뉴 바로가기 **Tailscale Force Connect**도 만듭니다.
- 폴더 `C:\ProgramData\TailscaleAutoConnect\`: SYSTEM과 Administrators만 쓸 수 있고 Users는 읽기만 할 수 있습니다.
  SYSTEM이 실행하는 스크립트를 일반 사용자가 바꾸지 못하게 하려는 설정입니다.
  - `connect.ps1`
  - `uninstall.ps1`: 한 줄로 설치했을 때도 제거할 수 있도록 함께 복사해 둡니다.
  - `connect.log`: 부팅할 때마다 남는 실행 기록입니다. 200KB를 넘으면 최근 500줄만 남깁니다.
  - `install.log`: 마지막 설치 결과입니다.
  - `settings.json`: 버전과 자동 재연결 시간입니다. 관리 창이 관리자 권한 없이 읽습니다.

## 확인과 문제 해결

예약 작업은 SYSTEM 소유라 **관리자 PowerShell**에서만 조회됩니다. 로그 파일과 `tailscale status`는 일반 권한으로도 볼 수 있습니다.

```powershell
Get-ScheduledTask -TaskName 'Tailscale Auto Connect' | Get-ScheduledTaskInfo   # LastTaskResult 0 = 성공
Get-Content C:\ProgramData\TailscaleAutoConnect\connect.log -Tail 20
Get-EventLog -LogName Application -Source TailscaleForceConnect -Newest 5   # 부팅 결과 기록
tailscale status
```

이벤트 뷰어 → Windows 로그 → 응용 프로그램에서 원본 `TailscaleForceConnect`로 필터링해도 볼 수 있습니다.
오류 이벤트(1001~1003)에 작업을 연결하면 실패 알림을 받도록 설정할 수 있습니다.

`connect.ps1` 종료 코드:

| 코드 | 의미 |
|---|---|
| `0` | 연결됨 |
| `1` | 10분 안에 연결하지 못함 |
| `2` | 로그인이 필요함(키 만료 등) |
| `3` | `tailscale.exe`가 없음 |

## 구현 메모

- **플래그 없는 `tailscale up`만 씁니다.** 플래그를 하나라도 붙이면(`--timeout` 포함) Tailscale이 "기본값이 아닌 설정을
  모두 다시 적으라"며 실행을 거부합니다. 플래그가 없으면 exit node나 accept-routes 같은 기존 설정은 그대로 두고 연결만 켭니다.
  응답이 없을 때를 대비해 한 번 호출할 때마다 30초 제한을 스크립트가 직접 겁니다.
- **`.ps1`은 UTF-8 BOM으로 저장합니다.** 예약 작업은 Windows PowerShell 5.1로 실행되는데, BOM이 없으면 한글을 CP949로 읽어 깨집니다.
- **함수 이름을 `R`처럼 짧게 짓지 마세요.** `r`은 `Invoke-History`의 기본 별칭이라 같은 이름의 함수보다 먼저 호출됩니다.
  초기 버전의 설치가 조용히 멈춘 원인이었습니다.
- **설치 폴더를 믿기 전에 확인합니다.** 일반 사용자도 `C:\ProgramData`에 폴더를 만들 수 있으므로, 폴더가 이미 있으면
  링크가 아닌지, 폴더와 그 안 파일의 소유자가 Administrators나 SYSTEM인지 확인하고 아니면 설치를 중단합니다.
  새로 만들 때는 처음부터 제한된 ACL로 만듭니다. 제거할 때는 하위 폴더나 링크를 따라 들어가지 않습니다.
- **Unattended 상태를 `tailscale debug prefs`로 읽지 않습니다.** debug 출력은 안정된 API가 아니므로, 이미 켜져 있어도
  `tailscale set --unattended=true`를 다시 실행합니다(같은 값을 다시 쓸 뿐입니다).
- `Start-Process -PassThru`로 받은 프로세스는 `WaitForExit(timeout)`을 부르기 전에 `$p.Handle`을 한 번 읽어 두어야
  `ExitCode`를 얻을 수 있습니다.

## 제거

어느 방법으로 설치했든 아래 한 줄을 cmd나 PowerShell에 붙여 넣으면 됩니다. UAC 창이 한 번 뜹니다.

```bat
powershell -c "irm https://jeiel85.github.io/tailscale-force-connect/remove.ps1 | iex"
```

[`remove.ps1`](docs/remove.ps1)은 설치할 때 복사해 둔 `C:\ProgramData\TailscaleAutoConnect\uninstall.ps1`을 실행합니다.
그 파일이 없으면 최신 릴리스 zip을 받아 그 안의 `uninstall.ps1`을 실행합니다.
예약 작업 2개, 설치 폴더, 관리 창과 시작 메뉴 바로가기, 이벤트 로그 원본을 지우고 결과를 보여 줍니다. 그 밖의 Tailscale 설정은 그대로 남습니다.
관리 창의 "제거" 버튼을 두 번 눌러도 됩니다(창이 닫히고 결과 창이 뜹니다).

인터넷을 쓰지 않으려면 zip이나 git으로 받은 폴더에서 `uninstall.cmd`를 더블클릭하거나 아래 명령을 실행하세요.

```bat
powershell -ExecutionPolicy Bypass -File C:\ProgramData\TailscaleAutoConnect\uninstall.ps1
```

## 릴리스 만들기

Rust(MSVC)와 PowerShell 7이 필요합니다. 버전은 `gui/Cargo.toml`, `gui/app.rc`, `gui/app.manifest`, `gui/ui/app.slint`의 `version`, `install.ps1`의 `$version`을 함께 올립니다.
관리 창은 태그(`vX.Y.Z`)와 `Cargo.toml` 버전을 비교해 새 버전을 알리므로, 태그와 버전이 일치해야 합니다. 업데이트는 릴리스 asset의 SHA-256 기록을 확인하므로, asset은 `gh release create`나 웹 화면으로 올립니다.

```bash
git tag vX.Y.Z && git push origin vX.Y.Z
pwsh -File scripts/build-release.ps1 -Ref vX.Y.Z   # dist/tailscale-force-connect.zip
gh release create vX.Y.Z dist/tailscale-force-connect.zip --title vX.Y.Z --notes "..."
```

`build-release.ps1`은 관리 창을 릴리스 빌드하고, `git archive`로 설치에 필요한 파일만 꺼낸 뒤(무엇을 뺄지는 `.gitattributes`의 `export-ignore`가 정함) 실행 파일을 넣어 zip을 만듭니다.

한 줄 설치와 다운로드 링크는 `releases/latest`를 가리키므로 asset 이름은 항상 `tailscale-force-connect.zip`으로 둡니다.

## 라이선스

[MIT](LICENSE). 배포하는 관리 창 실행 파일은 [Slint](https://slint.dev)를 Royalty-free 2.0 라이선스로 사용합니다(창 바닥글의 "Made with Slint"). 자세한 내용은 [DECISIONS.md](DECISIONS.md)에 있습니다.
