# tailscale-force-connect

Windows PC에서 **재부팅할 때마다 Tailscale을 연결 상태로 되돌립니다.** 평소에는 직접 끊을 수 있고, 강제로 연결하는 건 부팅할 때뿐입니다.

> **English:** A tiny PowerShell + Task Scheduler setup that makes Tailscale on Windows reconnect on every boot,
> even if you clicked *Disconnect* before shutting down. It runs as SYSTEM before anyone logs in, keeps your
> existing `tailscale up` settings, and does **not** stop you from disconnecting manually afterwards.

🌐 소개 페이지: <https://jeiel85.github.io/tailscale-force-connect/>

## 왜 필요한가

Tailscale은 마지막 연결 상태를 기억합니다. 트레이에서 **Disconnect**를 누른 채로 재부팅하면(`WantRunning=false`)
끊긴 상태가 그대로 유지됩니다. 원격으로 접속해야 하는 PC가 정전이나 강제 재부팅 뒤에 끊긴 채로 켜지면
그 PC 앞으로 직접 가야 합니다.

이 도구는 부팅할 때마다 SYSTEM 권한으로 `tailscale up`을 실행해 그 상태를 덮어씁니다.

## 먼저 공식 기능을 검토하세요

Tailscale Windows 클라이언트 **1.84 이상**에는 비슷한 목적의 [시스템 정책](https://tailscale.com/docs/features/tailscale-system-policies)이 있습니다.
관리하는 기기가 많거나 MDM·GPO를 쓴다면 공식 정책이 더 맞습니다.

| 방법 | 직접 끊기 | 동작 |
|---|---|---|
| `AlwaysOn.Enabled` 정책 | ❌ 불가 | 연결을 끊거나 클라이언트를 종료할 수 없게 막습니다. `AlwaysOn.OverrideWithReason`을 켜면 사유를 입력하고 끊을 수 있습니다(사유는 관리 콘솔 로그에 남음). |
| `ReconnectAfter` 정책 | ⭕ 가능 | 끊긴 상태로 있을 수 있는 시간을 정합니다(예: `30m`). 재부팅과 상관없이 그 시간이 지나면 다시 연결됩니다. |
| **이 도구** | ⭕ 가능 | 다음 부팅까지 끊긴 상태가 유지되고, **부팅할 때만** 다시 연결합니다. |

공식 문서에는 이 정책들이 재부팅할 때 정확히 어떻게 동작하는지 나와 있지 않습니다. "평소엔 마음대로 끊고, 부팅하면 무조건 연결"이라는
동작이 꼭 필요할 때 이 도구를 쓰세요.

## 설치

필요한 것: Windows 10/11, Tailscale(기본 경로 `C:\Program Files\Tailscale\`)에 로그인한 상태, 관리자 권한.

```powershell
git clone https://github.com/jeiel85/tailscale-force-connect.git
cd tailscale-force-connect
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

- 관리자 권한이 아니면 스스로 UAC 창을 띄웁니다.
- 설치가 끝나면 **몇 초 동안 일부러 연결을 끊었다가** 예약 작업으로 다시 연결되는지 확인합니다.
  그 PC에 Tailscale로 원격 접속해 있다면 `-NoTest`를 붙여 이 확인을 건너뛰세요.
- 마지막 줄이 `완료 (종료 코드 0)`이고 그 앞에 `검증 성공`이 있으면 설치가 끝난 것입니다.
- Unattended 모드(로그인 전에도 연결 유지)가 꺼져 있으면 켭니다.

**권장:** [관리 콘솔](https://login.tailscale.com/admin/machines)에서 그 기기의 **Disable key expiry**를 켜세요.
키가 만료되면(기본 180일) 다시 로그인해야 하므로 자동 연결이 되지 않습니다. 이때는 `connect.log`에 `로그인이 필요함`이 남습니다.

## 구성

| 파일 | 역할 |
|---|---|
| `connect.ps1` | 부팅 때 실행되는 본체입니다. 서비스가 꺼져 있으면 켜고, 연결될 때까지 10초 간격으로 최대 30회(약 5분) `tailscale up`을 실행합니다. |
| `install.ps1` | `connect.ps1`을 `C:\ProgramData\TailscaleAutoConnect\`에 복사하고, Unattended 모드를 켜고, 예약 작업을 등록한 뒤 동작을 확인합니다. |
| `uninstall.ps1` | 예약 작업과 설치 폴더를 지웁니다. Tailscale 본체와 Unattended 설정은 건드리지 않습니다. |

설치되는 것:

- 예약 작업 **`Tailscale Auto Connect`**: 시작 시 20초 뒤에 SYSTEM 계정으로 실행되며, 실행 시간은 최대 10분입니다.
- 폴더 `C:\ProgramData\TailscaleAutoConnect\`: SYSTEM과 Administrators만 쓸 수 있고 Users는 읽기만 할 수 있습니다.
  SYSTEM이 실행하는 스크립트를 일반 사용자가 바꾸지 못하게 하려는 설정입니다.
  - `connect.ps1`
  - `connect.log`: 부팅할 때마다 남는 실행 기록입니다. 200KB를 넘으면 최근 500줄만 남깁니다.
  - `install.log`: 마지막 설치 결과입니다.

## 확인과 문제 해결

예약 작업은 SYSTEM 소유라 **관리자 PowerShell**에서만 조회됩니다. 로그 파일과 `tailscale status`는 일반 권한으로도 볼 수 있습니다.

```powershell
Get-ScheduledTask -TaskName 'Tailscale Auto Connect' | Get-ScheduledTaskInfo   # LastTaskResult 0 = 성공
Get-Content C:\ProgramData\TailscaleAutoConnect\connect.log -Tail 20
tailscale status
```

`connect.ps1` 종료 코드:

| 코드 | 의미 |
|---|---|
| `0` | 연결됨 |
| `1` | 5분 안에 연결하지 못함 |
| `2` | 로그인이 필요함(키 만료 등) |
| `3` | `tailscale.exe`가 없음 |

## 구현 메모

- **플래그 없는 `tailscale up`만 씁니다.** 플래그를 하나라도 붙이면(`--timeout` 포함) Tailscale이 "기본값이 아닌 설정을
  모두 다시 적으라"며 실행을 거부합니다. 플래그가 없으면 exit node나 accept-routes 같은 기존 설정은 그대로 두고 연결만 켭니다.
  응답이 없을 때를 대비해 한 번 호출할 때마다 30초 제한을 스크립트가 직접 겁니다.
- **`.ps1`은 UTF-8 BOM으로 저장합니다.** 예약 작업은 Windows PowerShell 5.1로 실행되는데, BOM이 없으면 한글을 CP949로 읽어 깨집니다.
- **함수 이름을 `R`처럼 짧게 짓지 마세요.** `r`은 `Invoke-History`의 기본 별칭이라 같은 이름의 함수보다 먼저 호출됩니다.
  초기 버전의 설치가 조용히 멈춘 원인이었습니다.
- `Start-Process -PassThru`로 받은 프로세스는 `WaitForExit(timeout)`을 부르기 전에 `$p.Handle`을 한 번 읽어 두어야
  `ExitCode`를 얻을 수 있습니다.

## 제거

```powershell
powershell -ExecutionPolicy Bypass -File .\uninstall.ps1
```

## 라이선스

[MIT](LICENSE)
