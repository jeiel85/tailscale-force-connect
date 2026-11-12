# 결정 기록

되돌리기 어려운 결정(주요 의존성, 설치 위치, 배포 형식)만 남깁니다.

## 2026-11-12 관리 창(GUI)을 Rust + Slint로 만든다 (v1.3.0)

**요구:** 예약 작업만 있어서 상태를 볼 방법이 없다. 트레이 상주는 번잡하고 무거우니 필요할 때만 여는 가벼운 창을 만들고, 보기 좋아야 한다.

**검토한 선택지**

| 선택지 | 장점 | 단점 |
|---|---|---|
| PowerShell WinForms | 빌드 불필요, 배포 그대로 | 모양이 낡았고 다듬기 어렵다 |
| Rust + egui(eframe) | MIT/Apache, 작다 | 기본 렌더러가 OpenGL이라 원격 데스크톱·가상 GPU에서 그려지지 않을 수 있다 |
| Rust + Win32 직접 | 가장 작다 | 보기 좋게 만드는 데 드는 작업량이 크다 |
| **Rust + Slint (소프트웨어 렌더러, Fluent 스타일)** | Windows 11과 비슷한 컨트롤, GPU 없이 그려진다(원격 데스크톱 포함), 단일 실행 파일 | 라이선스 조건이 있다(아래) |

**결정:** Slint 1.18, `renderer-software` + `backend-winit`, Fluent 스타일. 이 기기에 Tailscale로 원격 접속해서 창을 여는 경우가 실제로 있으므로 GPU 의존이 없는 쪽을 골랐다.

**라이선스:** Slint는 `GPL-3.0-only OR Slint Royalty-free 2.0 OR 상용`이다. 이 저장소의 코드는 MIT로 두고, 배포하는 실행 파일은 Royalty-free 2.0 조건으로 Slint를 쓴다. 조건인 출처 표시는 관리 창 바닥글의 "Made with Slint" 링크와 소개 페이지 표기로 한다. 조건이 바뀌면 이 항목을 다시 검토한다.

## 2026-11-12 설치 위치와 순서

- 관리 앱과 스크립트 원본: `C:\Program Files\Tailscale Force Connect\` (관리자만 쓸 수 있다). 관리 창의 설치·제거 버튼은 **이 폴더의** 스크립트를 UAC로 실행하므로, 일반 사용자가 바꿔치기할 수 있는 위치의 스크립트를 관리자 권한으로 실행하지 않는다.
- 실행 중 데이터: 기존대로 `C:\ProgramData\TailscaleAutoConnect\` (connect.ps1, 로그, `settings.json`). 관리 창은 이 폴더를 관리자 권한 없이 읽기만 한다.
- 순서: 한 줄 설치 → `install.ps1`이 먼저 관리 앱을 Program Files에 설치하고 시작 메뉴 바로가기를 만든 뒤 예약 작업을 등록 → 끝나면 관리 창을 연다.

**알고 있는 위험:** 서명하지 않은 실행 파일이다. 브라우저로 받은 zip에서 실행하면 SmartScreen 경고가 뜰 수 있고, 회사 보안 소프트웨어가 막을 수 있다. 한 줄 설치(PowerShell로 내려받음)는 웹에서 받은 파일 표시가 붙지 않아 SmartScreen 경고가 뜨지 않는다. 코드 서명은 비용이 들어 지금은 하지 않는다.

## 2026-11-11 공식 `ReconnectAfter` 정책 대신 감시 작업 (v1.2.0)

Tailscale 1.104.5에서 정책이 적용된 상태(`tailscale syspolicy list`에 표시)였지만 `tailscale down` 뒤 95~150초가 지나도 다시 연결되지 않았다. 그래서 SYSTEM 예약 작업이 상태를 직접 확인하는 방식을 택했다. 정책이 동작하는 버전이 확인되면 다시 검토한다(#5).
