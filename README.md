# tailscale-force-connect

Windows PC에서 **재부팅할 때마다 Tailscale을 연결 상태로 되돌립니다.** 직접 끊을 수도 있지만, 끊은 상태는 **1시간**(바꿀 수 있음)까지만 유지되고 그 뒤에는 자동으로 다시 연결됩니다.

> **English:** A tiny PowerShell + Task Scheduler setup that makes Tailscale on Windows reconnect on every boot,
> even if you clicked *Disconnect* before shutting down. It runs as SYSTEM before anyone logs in, keeps your
> existing `tailscale up` settings, and still lets you disconnect manually — Tailscale reconnects by itself
> after about 1 hour (configurable): a SYSTEM task checks every minute and reconnects once it has been off that long.
> The same task restarts the Tailscale service when it looks connected but has actually lost its control-server
> session for 3 minutes while the internet works (seen after waking from Modern Standby).
> A small tray icon shows the connection state and whether that SYSTEM task is still running.

🌐 소개 페이지: <https://jeiel85.github.io/tailscale-force-connect/>

## 왜 필요한가

Tailscale은 마지막 연결 상태를 기억합니다. 트레이에서 **Disconnect**를 누른 채로 재부팅하면(`WantRunning=false`)
끊긴 상태가 그대로 유지됩니다. 원격으로 접속해야 하는 PC가 정전이나 강제 재부팅 뒤에 끊긴 채로 켜지면
그 PC 앞으로 직접 가야 합니다.

끊긴 것도 아닌데 접속이 안 될 때도 있습니다. 절전(모던 스탠바이)에서 깨어난 뒤 인터넷은 되는데 Tailscale이
"network is down" 상태에 머물러, **트레이 아이콘은 '연결됨'인데 다른 기기에서는 offline**으로 보인 적이 있습니다.
Tailscale 서비스를 다시 시작하자 바로 풀렸습니다.

이 도구는 세 가지를 합니다.

1. **부팅할 때:** SYSTEM 권한으로 `tailscale up`을 실행해 끊긴 상태를 덮어씁니다.
2. **평소에 직접 끊었을 때:** SYSTEM 예약 작업 `Tailscale Auto Reconnect`가 1분마다 상태를 확인해, 끊긴 상태가
   **1시간** 넘게 이어지면 다시 연결합니다. 트레이 메뉴로 끊었든 `tailscale down`으로 끊었든 똑같이 동작합니다.
3. **연결된 척 멈췄을 때(멈춤 감시):** 같은 작업이, 연결된 것처럼 보이지만 서버와 연결되지 않은 상태가 인터넷이 되는데도
   **3분** 넘게 이어지면 Tailscale 서비스를 다시 시작하고 Windows 알림으로 알립니다.

이 일들이 잘 되고 있는지는 작업 표시줄의 [트레이 아이콘](#트레이-아이콘) 색으로 늘 볼 수 있습니다.

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

설치하면 **Tailscale Force Connect** 관리 창이 함께 설치되고, 시작 메뉴나 [트레이 아이콘](#트레이-아이콘)으로 열 수 있습니다. 창은 열었을 때만 동작합니다.

- 지금 연결 상태와 Tailscale 기기 이름·IP. Tailscale 트레이는 '연결됨'인데 실제로는 서버와 연결되지 않았으면 **연결 안 됨**(주황)으로,
  `tailscale status`가 10초 안에 답하지 않으면 **응답 없음**(주황)으로 보여 줍니다.
- 부팅 시 자동 연결, 직접 끊은 뒤 자동 재연결, 멈춤 감시 설정. 끊긴 상태라면 몇 분 뒤 다시 연결되는지도 보여 줍니다.
- 감시 작업이 마지막으로 확인한 때(예: "감시 작업 정상: 20초 전에 확인했습니다"). 한동안 돌지 않았으면 빨간 글씨로 알리고, [적용]을 누르면 다시 등록합니다.
- 최근 기록(`connect.log`): 최근 40줄을 칸 안에서 스크롤해 보고(항상 가장 최근 줄부터), 실패·멈춤 기록은 주황색으로 표시합니다.
- 버튼: 재연결 시간 바꾸기, 동작 확인, **Tailscale 다시 시작**(서비스 재시작), 제거. 이 버튼을 누를 때만 UAC 창이 뜹니다.
- 새 버전 알림: 창을 열 때 GitHub 최신 릴리스를 확인하고, 새 버전이 있으면 알림 창을 띄워 **60초 뒤 자동으로 업데이트**합니다. [나중에]를 누르면 멈추고 위쪽에 [업데이트] 버튼이 있는 알림만 남습니다. 머리글의 버전 표시(예: `v1.5.0`)를 누르면 언제든 직접 확인할 수 있습니다(아래 "업데이트" 참고).

## 트레이 아이콘

작업 표시줄 알림 영역(시계 옆)에 상태 아이콘이 늘 떠 있습니다. 앱 아이콘과 같은 모양이고, 가운데 원의 색이 상태를 나타냅니다.

| 색 | 뜻 |
|---|---|
| 초록 | 연결됨, 감시 작업 정상 |
| 주황 | 직접 끊음(정한 시간 뒤 다시 연결), 연결된 것처럼 보이지만 서버와 연결 안 됨, 응답 없음 |
| 빨강 | **감시 작업이 돌지 않음**, 로그인 필요, Tailscale 없음 |
| 회색 | 확인하는 중 |

- 마우스를 올리면 지금 상태와 감시 작업이 마지막으로 확인한 때(예: `감시 정상 · 20초 전 확인`)가 보입니다. 오른쪽 클릭 메뉴 위쪽에도 같은 내용이 있습니다.
- 왼쪽 클릭이나 메뉴의 [관리 창 열기]로 관리 창을 엽니다.
- **감시 작업이 살아 있는지:** SYSTEM 감시 작업은 정상일 때 로그를 남기지 않고, 그 실행 기록은 일반 권한으로 볼 수 없습니다.
  그래서 감시 작업이 확인할 때마다 설치 폴더의 `watch-last.json`에 시각을 덮어쓰고, 트레이와 관리 창이 그 시각을 봅니다.
  1분 주기면 7분, 5분 주기면 17분이 지나도록 새 기록이 없으면 빨강으로 바뀝니다(서비스 재시작이 길어진 한 번은 기다립니다). 켜진 직후·설치(업데이트) 직후·절전에서 깨어난 직후에는 그만큼 기다렸다가 판단합니다.
  관리 창의 [적용]을 누르면 감시 작업을 다시 등록합니다.
- 트레이는 상태를 보여 주기만 하고 연결·재시작은 하지 않습니다. 그 일은 지금처럼 SYSTEM 예약 작업이 하므로, 트레이를 꺼도 자동 연결은 그대로 동작합니다.
  15초마다 `tailscale status`와 설치 폴더의 기록을 읽습니다.
- 1분마다 도는 알림 작업이 트레이가 떠 있지 않으면 띄웁니다. 로그인 직후, 업데이트 뒤, 오류로 꺼진 뒤에도 1분 안에 다시 뜹니다. 관리 창을 열 때도 바로 띄웁니다.
- **끄기:** 관리 창의 **트레이 아이콘** 선택을 끄거나 트레이 메뉴의 [트레이 아이콘 끄기]를 누릅니다. 사용자마다 따로 정하며 관리자 권한이 필요 없습니다
  (`%LOCALAPPDATA%\Tailscale Force Connect\tray-off.txt`가 생깁니다).
- Windows 11은 새 아이콘을 작업 표시줄의 ^ 안에 숨깁니다. 늘 보이게 하려면 ^를 눌러 아이콘을 작업 표시줄로 끌어 놓거나,
  설정 → 개인 설정 → 작업 표시줄 → 기타 시스템 트레이 아이콘에서 **Tailscale Force Connect**를 켜세요.

## Windows 알림

관리 창을 열지 않아도 Windows 알림으로 알려 줍니다. 알림은 로그인한 사용자 권한으로 도는 예약 작업이 짧게 실행됐다가 끝나며 띄웁니다(트레이 아이콘은 알림을 띄우지 않습니다).

| 알림 | 언제 | 누르면 |
|---|---|---|
| **Tailscale 연결이 끊겼습니다** — 약 N분 뒤 자동으로 다시 연결합니다 | 1분마다 상태를 확인하다가 끊긴 것을 처음 봤을 때(한 번만) | 관리 창이 열림 |
| **Tailscale에 다시 연결됐습니다** | 끊김 알림을 띄운 뒤 연결이 돌아왔을 때 | 관리 창이 열림 |
| **Tailscale을 다시 시작했습니다** — N분 동안 서버와 연결되지 않아 서비스를 다시 시작했고 연결이 돌아왔습니다 | 멈춤 감시가 서비스를 다시 시작했을 때(풀리지 않았으면 "다시 시작했지만 연결되지 않았습니다") | 관리 창이 열림 |
| **새 버전 vX.Y.Z** | 매일 12:30에 확인해 새 버전이 있을 때(버전마다 한 번). 로그인 2분 뒤 확인에서 찾으면 알림 대신 관리 창이 바로 열립니다(아래 "업데이트") | 관리 창이 열리고 업데이트 창이 뜸(60초 뒤 자동 업데이트) |

- 연결 상태는 **1분마다** 확인합니다. Tailscale이 끊김을 알려 주는 이벤트가 없어서, 상주 프로세스 없이 할 수 있는 가장 빠른 방법입니다.
- **알림 끄기:** 관리 창의 **Windows 알림** 선택을 끄면 위 알림을 모두 띄우지 않습니다. 사용자마다 따로 정하며 관리자 권한이 필요 없습니다
  (`%LOCALAPPDATA%\Tailscale Force Connect\notify-off.txt`가 생깁니다). 예약 작업은 그대로 돌지만 아무것도 하지 않고 바로 끝납니다.
- 알림이 이 앱 이름과 아이콘으로 보이도록 시작 메뉴 바로가기에 앱 ID(`TailscaleForceConnect.Manager`)를 기록합니다. 알림을 누르면 `tailscale-force-connect:` 링크로 관리 창을 엽니다.
- 알림이 보이지 않으면 Windows 설정 → 시스템 → 알림에서 **Tailscale Force Connect**가 켜져 있는지, 방해 금지 모드가 꺼져 있는지 확인하세요.
  아래 명령으로 시험 알림을 띄울 수 있습니다.

  ```powershell
  & "C:\Program Files\Tailscale Force Connect\TailscaleForceConnect.exe" --notify-test
  ```

- `tailscale-force-connect:` 링크로 열리면(알림, 웹 페이지) 다른 인자는 보지 않고 관리 창만 엽니다. 웹 페이지가 링크에 `--watch-notify` 같은 인자를 끼워 넣어도 창 없는 동작은 실행되지 않습니다.

창을 여는 것만으로는 관리자 권한이 필요 없습니다. 상태는 일반 권한으로 읽을 수 있는 정보(`tailscale status`, `settings.json`, `connect.log`)만 씁니다.
버튼은 `C:\Program Files\Tailscale Force Connect\`에 있는 스크립트를 관리자 권한으로 실행합니다. 이 폴더는 관리자만 쓸 수 있습니다.
Rust와 [Slint](https://slint.dev)로 만들었고(소프트웨어 렌더러), GPU가 없는 환경이나 원격 데스크톱에서도 그려집니다. 실행 파일은 약 8MB입니다.

> **원격으로 접속 중이라면** "동작 확인"을 누르지 마세요. 연결을 몇 초 끊었다가 다시 연결되는지 보는 기능이라, 그 사이 원격 연결이 끊깁니다.
> 관리 창은 원격 데스크톱 세션에서 열렸거나 최근 2분 안에 Tailscale로 트래픽이 오간 기기(`tailscale status`의 `Active`)가 있으면
> "원격으로 접속 중인 것 같습니다" 창을 띄워 한 번 더 묻습니다. AnyDesk 같은 다른 원격 도구는 Tailscale로 오가는 경우에만 알아챕니다.

## 업데이트

관리 창이 새 버전을 찾으면 알림 창이 뜨고 **60초 뒤 자동으로 업데이트를 시작**합니다(홈베이스·claude-usage와 같은 방식). 기다리지 않으려면 **[지금 업데이트]**, 멈추려면 **[나중에]**를 누릅니다.
업데이트가 시작되면 UAC 창이 한 번 뜨고, [예]를 누르면 관리 창이 닫혔다가 새 버전으로 다시 열립니다.

- **언제 뜨나:** 관리 창을 열 때 자동으로 확인합니다. 그리고 로그인 2분 뒤 예약 작업이 새 버전을 찾으면 **관리 창을 직접 열어** 같은 카운트다운을 시작합니다(로그인할 때마다, 같은 로그인에서는 한 번).
  매일 12:30 확인은 작업 중인 화면을 가로채지 않도록 Windows 알림만 띄우고, 알림을 누르면 관리 창이 열려 카운트다운이 시작됩니다.
- **되풀이 방지:** 카운트다운으로 시작한 업데이트가 끝나지 않았으면(UAC 거절·방치, 설치 실패) 그 버전은 다시 자동으로 시작하지 않고 버튼을 기다립니다. 알림 창에 그 이유가 표시됩니다.
- **직접 확인:** 머리글의 버전 표시를 누르면 직접 확인합니다("최신 버전입니다", "확인하지 못했습니다" 등이 옆에 표시됩니다). 이때 뜨는 알림 창은 카운트다운 없이 버튼을 기다립니다.
- **끄기:** 관리 창의 **Windows 알림**을 끄면 로그인 직후 관리 창을 여는 것도 하지 않습니다. 관리 창을 직접 열었을 때의 카운트다운은 [나중에]로 멈춥니다.

업데이트는 [`update.ps1`](update.ps1)이 관리자 권한으로 진행합니다.

1. GitHub 최신 릴리스가 설치된 버전(`settings.json`의 `version`)보다 새것인지 봅니다. 같으면 아무것도 하지 않고, **낮으면 설치하지 않습니다**(직접 실행할 때 이전 버전으로 내려가지 않도록. 일부러 내리거나 다시 설치하려면 `-Force`).
2. zip을 관리자만 쓸 수 있는 `C:\Program Files\Tailscale Force Connect\` 안의 임시 폴더에 받습니다.
3. GitHub이 릴리스 파일마다 기록하는 **SHA-256 값과 비교**합니다. 다르거나 기록이 없으면 중단합니다.
4. 지금 설치된 관리 앱 파일을 같은 임시 폴더에 백업합니다.
5. 지금의 자동 재연결·멈춤 감시 설정을 유지한 채 `install.ps1 -NoTest`로 설치합니다. **연결은 끊지 않습니다.**
6. 설치가 도중에 실패해 파일이 이미 바뀌었으면, 백업해 둔 **이전 버전의 `install.ps1`로 다시 설치해 되돌립니다**(새 관리 앱과 이전 예약 작업이 섞이지 않도록).
   아무것도 바뀌기 전에 실패했으면(로그인 필요 등) 되돌릴 것이 없으므로 그대로 둡니다. 되돌리기는 v1.7.0 이상에서 업데이트할 때부터 동작합니다.

설치할 때 `C:\Program Files\Tailscale Force Connect\`에서 알려진 파일(관리 창, `install.ps1`, `uninstall.ps1`, `connect.ps1`, `update.ps1`) 밖의 파일은 지웁니다. 이전 버전에만 있던 파일이 남지 않게 하려는 것입니다.

카운트다운이 끝나도 설치는 **UAC 창에서 [예]를 눌러야** 진행합니다. 사용자 승인 없이 SYSTEM 권한으로 몰래 업데이트하는 방식은 쓰지 않습니다. 그 방식이면 릴리스가 오염됐을 때 받은 코드가 모든 설치 PC에서 곧바로 SYSTEM 권한으로 실행되기 때문입니다([DECISIONS.md](DECISIONS.md)).
명령으로 업데이트하려면 한 줄 설치를 다시 실행하거나, 관리자 PowerShell에서 `& "C:\Program Files\Tailscale Force Connect\update.ps1"`를 실행합니다.

## 설치

필요한 것: Windows 10/11, Tailscale에 로그인한 상태(로그인되어 있지 않으면 설치가 멈추지 않고 바로 실패합니다), 관리자 권한. Tailscale 설치 위치는 서비스 등록 정보에서 찾으므로 기본 경로가 아니어도 됩니다.
세 가지 방법 모두 설치 중에 UAC 창이 한 번 뜹니다.

**방법 1. 한 줄로 설치** — cmd나 PowerShell에 붙여 넣습니다.

```bat
powershell -c "irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex"
```

[`get.ps1`](docs/get.ps1)이 하는 일:

1. GitHub API로 [최신 릴리스](https://github.com/jeiel85/tailscale-force-connect/releases/latest)와 GitHub이 그 zip에 기록한 **SHA-256**을 확인하고, 그 버전의 zip을 임시 폴더에 받아 값을 비교합니다. 다르거나 기록이 없으면 중단합니다.
2. UAC로 관리자 PowerShell을 엽니다. 이 창은 zip을 **관리자만 쓸 수 있는 새 폴더**(`C:\Windows\Temp\tailscale-force-connect-…`, 만들 때부터 Administrators·SYSTEM만 쓸 수 있는 권한)로 복사해 SHA-256을 다시 확인하고, 거기서 압축을 풀어 `install.ps1`을 실행합니다.
   UAC 승인을 기다리는 동안 같은 사용자의 다른 프로그램이 임시 폴더의 파일을 바꿔치기해도 관리자 권한으로 실행되지 않습니다(관리자 창이 실행하는 작은 단계 스크립트도 명령줄에 실린 SHA-256과 맞을 때만 실행).
3. `install.ps1`은 먼저 관리 창을 Program Files에 설치하고 나서 예약 작업을 등록합니다. 결과를 이 창에 보여 주고 관리 창을 연 뒤 임시 폴더를 지웁니다.

GitHub API는 같은 IP에서 시간당 60번까지만 익명으로 부를 수 있습니다. 한 사무실에서 여러 PC를 연달아 설치하다 "rate limit" 오류가 나면 잠시 뒤 다시 실행하세요.
인터넷에서 받은 스크립트를 바로 실행하는 방식이니, 먼저 내용을 읽어 보고 싶다면 방법 2를 쓰세요.

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
- 직접 끊은 뒤 자동으로 다시 연결될 때까지의 시간을 정합니다. 기본값은 `1h`입니다. 감시 작업이 1분마다 확인하므로 실제로는 설정 시간보다 1~2분 늦게 다시 연결됩니다(멈춤 감시를 끄고 10분 이상으로 정하면 5분마다 확인).
  - 바꾸려면: `install.cmd -ReconnectAfter 30m`, 한 줄 설치는 cmd에서 `set TFC_RECONNECT_AFTER=30m && powershell -c "..."`,
    PowerShell에서 `$env:TFC_RECONNECT_AFTER='30m'; irm ... | iex`
  - 끄려면 값을 `0`으로 줍니다.
  - 형식은 `h`·`m` 단위입니다(`30m`, `1h`, `1h30m`).
- 멈춤 감시 시간을 정합니다. 기본값은 `3m`입니다. 연결된 것처럼 보이지만 서버와 연결되지 않은 상태가 이만큼 이어지면 Tailscale 서비스를 다시 시작합니다.
  - 바꾸려면: `install.cmd -HealAfter 10m`, 한 줄 설치는 `set TFC_HEAL_AFTER=10m && ...` / `$env:TFC_HEAL_AFTER='10m'; ...`
  - 끄려면 값을 `0`으로 줍니다. 자동 재연결과 멈춤 감시를 둘 다 끄면 감시 작업을 지웁니다.
  - 업데이트해도 두 설정은 그대로 유지됩니다(1.5.0 이하에서 올리면 멈춤 감시가 기본값 `3m`으로 켜집니다).

> **Windows 빠른 시작을 확인하세요.** 빠른 시작이 켜져 있으면(Windows 기본값) **일반 종료 후 전원을 켤 때**는 완전 부팅이 아니라
> 최대 절전 복귀가 되어 이 작업이 실행되지 않습니다. 재시작과 정전 뒤 부팅에서는 동작합니다. 설치 스크립트가 빠른 시작이 켜진 것을 감지하면 경고를 출력합니다.
> 항상 동작하게 하려면 제어판 → 전원 옵션 → 전원 단추 작동 설정에서 **빠른 시작 켜기**를 끄세요.

**권장:** [관리 콘솔](https://login.tailscale.com/admin/machines)에서 그 기기의 **Disable key expiry**를 켜세요.
키가 만료되면(기본 180일) 다시 로그인해야 하므로 자동 연결이 되지 않습니다. 이때는 `connect.log`와 이벤트 로그에 `로그인이 필요함`이 남습니다.

## 구성

| 파일 | 역할 |
|---|---|
| `connect.ps1` | 부팅 때 실행되는 본체입니다. 서비스가 꺼져 있으면 켜고, 연결될 때까지 최대 10분 동안 10초 간격으로 `tailscale up`을 실행합니다. 결과는 로그 파일과 이벤트 로그에 남깁니다. `-Watch`는 감시 작업이, `-Heal`(서비스 바로 재시작)은 관리 창의 [Tailscale 다시 시작] 버튼이 실행합니다. |
| `tests/*.ps1` | 관리자 권한·실제 Tailscale 없이 돌리는 시험입니다. `watch-verdict.ps1`(감시 판정, 서비스 경로 해석), `update-helpers.ps1`(업데이트 버전 판단·되돌리기), `web-install.ps1`(한 줄 설치·제거의 무결성 확인, `-Network`면 실제 릴리스로 SHA-256 확인). 릴리스 zip에는 들어가지 않습니다. |
| `install.ps1` | `connect.ps1`을 `C:\ProgramData\TailscaleAutoConnect\`에 복사하고, Unattended 모드를 켜고, 이벤트 로그 원본과 예약 작업(부팅 시 연결, 자동 재연결·멈춤 감시, 알림)을 등록한 뒤 동작을 확인합니다. |
| `uninstall.ps1` | 예약 작업, 설치 폴더, 관리 앱, 이벤트 로그 원본, 사용자별 알림 기록을 지우고 결과를 보여 줍니다. Tailscale 본체와 Unattended 설정은 건드리지 않습니다. |
| `TailscaleForceConnect.exe --watch-notify` / `--notify-update` / `--notify-test` | 창 없이 끊김·재연결 알림 / 새 버전 알림(로그인 직후면 관리 창을 엶) / 시험 알림을 띄웁니다(예약 작업이 실행). `--watch-notify`는 트레이 아이콘이 떠 있지 않으면 띄웁니다. |
| `TailscaleForceConnect.exe --tray` | 트레이 아이콘입니다. 로그인 세션마다 하나만 뜹니다. |
| `update.ps1` | 최신 릴리스가 새 버전이면 받아 SHA-256을 확인한 뒤, 설정을 유지하고 연결을 끊지 않은 채 설치합니다. 실패하면 이전 버전으로 되돌립니다. 관리 창의 [업데이트] 버튼이 실행합니다. |
| `install.cmd`, `uninstall.cmd` | 더블클릭용 실행 파일입니다. 같은 폴더의 `.ps1`을 실행합니다. |
| `TailscaleForceConnect.exe` | 관리 창입니다(릴리스 zip에만 들어 있음). 소스는 `gui/`에 있습니다. |
| `gui/` | 관리 창 소스(Rust + Slint)입니다. |
| `scripts/build-release.ps1` | 관리 창을 빌드하고 릴리스 zip을 만듭니다. |
| `docs/get.ps1`, `docs/remove.ps1` | 한 줄 설치·제거용 스크립트입니다. GitHub Pages로 배포되며, 릴리스 zip에는 들어가지 않습니다. |

설치되는 것:

- 예약 작업 **`Tailscale Auto Connect`**: 시작 시 20초 뒤에 SYSTEM 계정으로 실행되며, 실행 시간은 최대 15분입니다.
- 예약 작업 **`Tailscale Force Connect Status Alert`**: 로그인한 사용자 권한으로 1분마다 `TailscaleForceConnect.exe --watch-notify`를 실행해 끊김·재연결 알림을 띄우고, 트레이 아이콘이 떠 있지 않으면 띄웁니다(시작 프로그램에는 따로 등록하지 않음).
- 예약 작업 **`Tailscale Force Connect Update Check`**: 로그인한 사용자 권한으로 로그인 2분 뒤와 매일 12:30에 `--notify-update`를 실행합니다. 새 버전이 있으면 로그인 직후에는 관리 창을 열고(60초 뒤 자동 업데이트), 그 밖에는 알림을 띄웁니다.
- 레지스트리 `HKLM\SOFTWARE\Classes\tailscale-force-connect`: 알림을 누르면 관리 창을 여는 링크입니다. 알림 표시 기록, 자동 업데이트 시도 기록, 알림·트레이 아이콘 끄기 표시는 사용자별로 `%LOCALAPPDATA%\Tailscale Force Connect\`에 남고, 제거할 때 함께 지웁니다.
- 예약 작업 **`Tailscale Auto Reconnect`**: SYSTEM 계정으로 1분마다 실행됩니다(`connect.ps1 -Watch`). 정상이면 로그는 남기지 않고, 확인한 시각과 판정만 `watch-last.json`에 덮어씁니다(트레이 아이콘이 감시가 도는지 보는 데 씀).
  - 자동 재연결: 끊긴 상태(`Stopped`)를 처음 본 시각을 설치 폴더의 `stopped-since.txt`에 적어 두고, 설정한 시간이 지나도 끊겨 있으면 `tailscale up`을 실행합니다.
  - 멈춤 감시: 아래 상태가 이어진 시간을 `stuck-since.json`에 적어 두고, 설정한 시간(기본 3분)이 지나면 Tailscale 서비스를 다시 시작합니다.
    - `Running`인데 서버와 연결되지 않음(`tailscale status --json`의 `Self.Online`이 `false`), 또는 `Starting`에서 넘어가지 않음.
      이때는 `controlplane.tailscale.com:443`에 TCP로 연결해 보고, **인터넷이 안 되면 진짜 네트워크 문제로 보고 건드리지 않습니다.**
    - `tailscale status`가 20초 동안 응답하지 않음, 서비스가 꺼져 있음(시작 유형이 '사용 안 함'이면 건드리지 않음).
    - 절전·꺼짐으로 확인이 7분 넘게 비면 깨어난 직후 바로 재시작하지 않고 처음부터 다시 셉니다. 1분 안에 풀리는 잠깐의 끊김은 로그에도 남기지 않습니다.
    - 재시작은 서비스를 멈춘 뒤(30초 안에 안 멈추면 `tailscaled`를 끝냄) 다시 켜고, 60초 동안 연결을 확인합니다. 결과는 `heal-last.json`에 남기고,
      풀리지 않았으면 30분 뒤에 다시 시도합니다(방화벽 등으로 계속 안 될 때 반복해서 재시작하지 않도록). 풀렸다가 다시 멈추면 10분 뒤부터 다시 재시작할 수 있습니다.
    - 부팅 직후 `Tailscale Auto Connect`가 아직 연결을 재시도하는 중이면 재시작하지 않고 기다립니다.
    - `tailscale status`에 `Self.Online` 값이 없으면(그 값을 채우지 않는 Tailscale) 연결된 것으로 봅니다.
  - 로그인이 필요한 상태처럼 `tailscale up`이나 재시작으로 풀 수 없는 상태는 건드리지 않습니다.
- 이벤트 로그 원본 **`TailscaleForceConnect`**(응용 프로그램 로그): 부팅할 때마다 결과를 남깁니다. ID `1000` 연결됨(정보), `1004` 직접 끊은 뒤 자동 재연결(정보),
  `1005` 멈춘 Tailscale을 서비스 재시작으로 복구(정보), `1001` 10분 안에 연결 실패, `1002` 로그인 필요, `1003` `tailscale.exe` 없음,
  `1006` 서비스를 다시 시작했지만 연결되지 않음(이상 오류).
- 폴더 `C:\Program Files\Tailscale Force Connect\`: 관리 창과 스크립트 원본입니다. 시작 메뉴 바로가기 **Tailscale Force Connect**도 만듭니다.
- 폴더 `C:\ProgramData\TailscaleAutoConnect\`: SYSTEM과 Administrators만 쓸 수 있고 Users는 읽기만 할 수 있습니다.
  SYSTEM이 실행하는 스크립트를 일반 사용자가 바꾸지 못하게 하려는 설정입니다.
  - `connect.ps1`
  - `uninstall.ps1`: 한 줄로 설치했을 때도 제거할 수 있도록 함께 복사해 둡니다.
  - `connect.log`: 부팅할 때마다 남는 실행 기록입니다. 200KB를 넘으면 최근 500줄만 남깁니다.
  - `install.log`: 마지막 설치 결과입니다.
  - `settings.json`: 버전, 자동 재연결 시간, 멈춤 감시 시간입니다. 관리 창이 관리자 권한 없이 읽습니다.
  - `stopped-since.txt`, `stuck-since.json`, `heal-last.json`, `watch-last.json`: 감시 작업의 기록입니다(끊긴 시각, 멈춘 시각, 마지막 재시작 결과, 마지막 확인 시각).
    SYSTEM은 사용자 화면에 알림을 띄울 수 없어서, 1분마다 도는 사용자 알림 작업이 `heal-last.json`을 읽어 재시작 알림을 띄웁니다.

## 확인과 문제 해결

예약 작업은 SYSTEM 소유라 **관리자 PowerShell**에서만 조회됩니다. 로그 파일, 감시 작업의 마지막 확인 기록, `tailscale status`는 일반 권한으로도 볼 수 있습니다.

```powershell
Get-ScheduledTask -TaskName 'Tailscale Auto Connect' | Get-ScheduledTaskInfo   # LastTaskResult 0 = 성공
Get-Content C:\ProgramData\TailscaleAutoConnect\connect.log -Tail 20
Get-Content C:\ProgramData\TailscaleAutoConnect\watch-last.json   # {"at":유닉스 초,"kind":"healthy"} - 1분마다 바뀌면 감시 작업이 도는 중
Get-EventLog -LogName Application -Source TailscaleForceConnect -Newest 5   # 부팅 결과 기록
tailscale status
```

이벤트 뷰어 → Windows 로그 → 응용 프로그램에서 원본 `TailscaleForceConnect`로 필터링해도 볼 수 있습니다.
오류 이벤트(1001~1003, 1006)에 작업을 연결하면 실패 알림을 받도록 설정할 수 있습니다.

**Tailscale 트레이 아이콘은 정상인데 다른 기기에서 접속이 안 되면** `tailscale status`의 맨 아래 `# Health check:`와 자기 기기 줄의 `offline`을 확인하세요.
멈춤 감시가 켜져 있으면 몇 분 안에 저절로 풀립니다. 바로 풀려면 관리 창의 **[Tailscale 다시 시작]**을 누르거나, 관리자 PowerShell에서
`& "C:\Program Files\Tailscale Force Connect\connect.ps1" -Heal`을 실행합니다.
감시 판정은 `powershell -ExecutionPolicy Bypass -File tests\watch-verdict.ps1`로 시험할 수 있습니다.

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

[`remove.ps1`](docs/remove.ps1)은 설치할 때 복사해 둔 `C:\ProgramData\TailscaleAutoConnect\uninstall.ps1`을 실행합니다(그 폴더와 파일이 관리자만 쓸 수 있을 때만).
그 파일이 없거나 믿을 수 없으면 한 줄 설치와 같은 방식으로 최신 릴리스 zip을 받아 SHA-256을 확인하고, 관리자 전용 폴더에서 그 안의 `uninstall.ps1`을 실행합니다.
예약 작업, 설치 폴더, 관리 창과 시작 메뉴 바로가기, 이벤트 로그 원본, 사용자별 알림 기록을 지우고 결과를 보여 줍니다. 그 밖의 Tailscale 설정은 그대로 남습니다.
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
한 줄 설치·제거·업데이트는 모두 asset의 SHA-256 기록(`digest`)이 없으면 멈추므로, 릴리스를 만든 뒤 `gh release view vX.Y.Z --json assets`로 `digest`가 있는지 확인합니다.

관리 창 화면은 개발 빌드(`cargo run`)에서 환경 변수로 확인할 수 있습니다: `TFC_DATA_DIR`(설치 폴더 대신 읽을 폴더), `TFC_FAKE_STATUS_FILE`(`tailscale status --json` 대신 읽을 파일, `timeout`이면 응답 없음), `TFC_FAKE_LATEST`(최신 버전), `TFC_FORCE_LIGHT`(밝은 테마).

## 라이선스

[MIT](LICENSE). 배포하는 관리 창 실행 파일은 [Slint](https://slint.dev)를 Royalty-free 2.0 라이선스로 사용합니다(창 바닥글의 "Made with Slint"). 자세한 내용은 [DECISIONS.md](DECISIONS.md)에 있습니다.
