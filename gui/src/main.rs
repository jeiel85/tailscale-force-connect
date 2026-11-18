// tailscale-force-connect 관리 창.
// 상태 표시는 일반 권한으로 읽을 수 있는 것만 쓴다(tailscale status, settings.json, connect.log).
// 설치·설정·제거는 실행 파일과 같은 폴더의 PowerShell 스크립트를 UAC로 관리자 권한으로 실행한다.
#![windows_subsystem = "windows"]

use std::{
    io::Read,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

slint::include_modules!();

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DATA_DIR: &str = r"C:\ProgramData\TailscaleAutoConnect";
/// 'tailscale status'를 기다리는 최대 시간. 서비스가 멈추면 응답하지 않아 화면이 마지막 상태에 머물렀다.
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
/// 새 버전 확인(PowerShell, 내부 HTTP 제한 15초)을 기다리는 최대 시간
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(45);
/// 알림 한 번(PowerShell)을 기다리는 최대 시간. 알림 작업은 1분 제한이라
/// 상태 조회(10초) + 알림 두 번(멈춤 복구, 끊김)이 그 안에 끝나도록 짧게 둔다.
const TOAST_TIMEOUT: Duration = Duration::from_secs(20);
/// 최근 기록 줄 수. 칸 안에서 스크롤되므로 창 높이와 상관없다. 전체 기록은 [로그 폴더 열기]로 본다.
const LOG_LINES: usize = 40;
/// 새 버전 알림 창이 저절로 떴을 때 자동으로 업데이트하기까지 기다리는 시간(초).
/// 내용을 읽거나 [나중에]를 누를 시간은 주고, 아무도 없으면 그대로 진행되게 한다(홈베이스·claude-usage와 같은 값).
const AUTO_UPDATE_SECONDS: i32 = 60;
/// 로그인한 지 이 시간 안에 새 버전 확인(로그인 2분 뒤 트리거)이 새 버전을 찾으면 알림 대신 관리 창을 연다.
/// 매일 12:30 확인처럼 작업 중에 도는 확인은 화면을 가로채지 않고 알림만 띄운다.
const LOGON_OFFER_WINDOW: Duration = Duration::from_secs(10 * 60);

/// 콤보 상자 항목 순서와 같다: (install.ps1 -ReconnectAfter 값, 분)
const RECONNECT: [(&str, i64); 5] = [("0", 0), ("30m", 30), ("1h", 60), ("2h", 120), ("4h", 240)];
const RECONNECT_LABELS: [&str; 5] = ["끄기", "30분", "1시간", "2시간", "4시간"];

/// 화면 갱신 사이에 유지하는 값
struct UiState {
    /// 콤보 상자를 현재 설정으로 한 번 맞췄는지
    index_initialized: AtomicBool,
    /// 설치된 자동 재연결 시간(분). 모르면 -1
    installed_minutes: AtomicI64,
    /// 콤보 상자 끝에 붙인 '현재 설정' 항목의 분. 없으면 -1
    custom_option: AtomicI64,
    /// 설치된 멈춤 감시 시간(분). 모르면(1.5.0 이하로 설치) -1
    installed_heal: AtomicI64,
    /// 최근 Tailscale로 트래픽이 오간 기기 이름(마지막 상태 조회 기준). 동작 확인 전에 경고한다.
    active_peers: Mutex<Vec<String>>,
    /// 원격 접속 경고에서 [계속]을 누르면 실행할 일
    pending_test: Mutex<Option<PendingTest>>,
    /// 마지막으로 읽은 감시 작업 확인 시각(유닉스 초, 0이면 모름). 감시 작업이 파일을 쓰는 순간 읽지 못해도 이 값을 쓴다.
    watch_at: AtomicU64,
}

/// 연결을 몇 초 끊는 작업. 원격 접속 중이면 한 번 더 확인받고 실행한다.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PendingTest {
    /// [동작 확인]
    Verify,
    /// [설치하기] + '설치 후 동작 확인'
    InstallWithTest,
}

/// 실행 인자로 정하는 동작
#[derive(Debug, PartialEq)]
enum LaunchMode {
    Window,
    NotifyUpdate,
    WatchNotify,
    NotifyTest,
    /// 작업 표시줄 알림 영역의 상태 아이콘(창 없이 상주)
    Tray,
}

/// tailscale-force-connect: 링크(알림, 웹 페이지)로 열리면 다른 인자는 보지 않고 창만 연다.
/// 링크 문자열은 웹 페이지가 정할 수 있으므로, 링크로 창 없는 동작이 실행되지 않게 한다.
/// (등록된 명령은 "exe" "%1"이라 첫 인자는 항상 링크 이름으로 시작한다.)
fn launch_mode(args: &[String]) -> LaunchMode {
    if args.iter().any(|a| a.trim_matches('"').to_ascii_lowercase().starts_with("tailscale-force-connect:")) {
        return LaunchMode::Window;
    }
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if has("--notify-update") {
        LaunchMode::NotifyUpdate
    } else if has("--watch-notify") {
        LaunchMode::WatchNotify
    } else if has("--notify-test") {
        LaunchMode::NotifyTest
    } else if has("--tray") {
        LaunchMode::Tray
    } else {
        LaunchMode::Window
    }
}

/// install.ps1의 -ReconnectAfter / -HealAfter 값
fn minutes_arg(minutes: i64) -> String {
    if minutes <= 0 { "0".into() } else { format!("{minutes}m") }
}

struct Snapshot {
    installed: bool,
    settings_found: bool,
    reconnect_minutes: i64,
    /// settings.json에 healMinutes가 있는지(1.6.0 이상으로 설치됨)
    heal_found: bool,
    heal_minutes: i64,
    state: &'static str,
    title: String,
    detail: String,
    /// (시각 "MM-DD HH:MM", 내용). None이면 이번에는 읽지 못했으니 화면의 기록을 그대로 둔다.
    log: Option<Vec<(String, String)>>,
    fast_startup: bool,
    active_peers: Vec<String>,
    /// 감시 작업의 확인 주기(분). 감시 작업을 쓰지 않으면(설치 안 됨, 자동 재연결·멈춤 감시 둘 다 끔) None
    watch_interval: Option<i64>,
    /// 감시 작업이 마지막으로 확인한 시각(유닉스 초, watch-last.json). 기록이 없으면 None
    watch_at: Option<u64>,
    /// 직접 끊은 상태를 감시 작업이 처음 본 뒤 지난 분
    stopped_for: Option<i64>,
    /// 켜진 직후·설치 직후·깨어난 직후라 감시 작업이 아직 돌지 않았을 수 있음
    watch_settling: bool,
    /// 이 사용자가 트레이 아이콘을 켜 두었는지
    tray_on: bool,
}

/// Windows 알림의 앱 ID. install.ps1이 시작 메뉴 바로가기에 같은 값을 기록한다.
const APP_ID: &str = "TailscaleForceConnect.Manager";

fn main() -> Result<(), slint::PlatformError> {
    // 창 없이 동작하는 모드. 예약 작업 'Tailscale Force Connect Update Check'가 --notify-update로 실행한다.
    // (알림을 눌러 tailscale-force-connect: 링크로 열리면 인자가 붙지만, 그때는 평소처럼 창을 연다.)
    let args: Vec<String> = std::env::args().skip(1).collect();
    match launch_mode(&args) {
        LaunchMode::NotifyUpdate => {
            notify_if_update_available();
            return Ok(());
        }
        LaunchMode::WatchNotify => {
            watch_and_notify();
            return Ok(());
        }
        LaunchMode::NotifyTest => {
            show_toast(
                "Tailscale Force Connect 알림 시험",
                "새 버전이 나오면 이렇게 알려 드립니다. 누르면 관리 창이 열립니다.",
            );
            return Ok(());
        }
        LaunchMode::Tray => return run_tray(),
        LaunchMode::Window => {}
    }

    let ui = AppWindow::new()?;
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    ui.set_notify_on(notifications_enabled());
    ui.set_tray_on(tray_enabled());
    // 업데이트·설치 직후 다시 열린 관리 창이면 트레이도 바로 띄운다(그 밖에는 1분 알림 작업이 띄운다).
    if tray_should_run() {
        ensure_tray_running();
    }
    #[cfg(debug_assertions)]
    if std::env::var_os("TFC_FORCE_LIGHT").is_some() {
        ui.set_force_light(true); // 개발용: 어두운 테마 PC에서 밝은 테마 화면 확인
    }

    let refreshing = Arc::new(AtomicBool::new(false));
    let state = Arc::new(UiState {
        index_initialized: AtomicBool::new(false),
        installed_minutes: AtomicI64::new(-1),
        custom_option: AtomicI64::new(-1),
        installed_heal: AtomicI64::new(-1),
        active_peers: Mutex::new(Vec::new()),
        pending_test: Mutex::new(None),
        watch_at: AtomicU64::new(0),
    });

    let refresh = {
        let weak = ui.as_weak();
        let refreshing = refreshing.clone();
        let state = state.clone();
        move || {
            if refreshing.swap(true, Ordering::SeqCst) {
                return; // 이전 조회가 아직 끝나지 않았다
            }
            let weak = weak.clone();
            let refreshing = refreshing.clone();
            let state = state.clone();
            std::thread::spawn(move || {
                let snap = collect_snapshot();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    apply_snapshot(&ui, &snap, &state);
                });
                refreshing.store(false, Ordering::SeqCst);
            });
        }
    };
    let refresh = Arc::new(refresh);

    refresh();
    let timer = Timer::default();
    {
        let refresh = refresh.clone();
        timer.start(TimerMode::Repeated, Duration::from_secs(3), move || refresh());
    }

    // 설치 / 적용 / 동작 확인은 모두 install.ps1을 다른 인자로 실행한다.
    // keep_installed: 동작 확인처럼 설정을 바꾸면 안 되는 경우. 콤보 상자 값 대신 설치된 값을 넘긴다.
    let run_install = {
        let weak = ui.as_weak();
        let refresh = refresh.clone();
        let state = state.clone();
        Arc::new(move |test: bool, keep_installed: bool, busy_text: &str, title_ok: &str| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(script) = sibling("install.ps1") else {
                show_result(&ui, false, "설치 스크립트를 찾지 못했습니다", "install.ps1이 이 실행 파일과 같은 폴더에 있어야 합니다. 받은 zip을 모두 푼 뒤 실행하세요.");
                return;
            };
            let installed = state.installed_minutes.load(Ordering::SeqCst);
            let custom = state.custom_option.load(Ordering::SeqCst);
            let idx = ui.get_reconnect_index().max(0) as usize;
            let value = if keep_installed && installed >= 0 {
                minutes_arg(installed)
            } else if idx < RECONNECT.len() {
                RECONNECT[idx].0.to_string()
            } else if custom >= 0 {
                minutes_arg(custom) // 콤보 상자 끝의 '현재 설정' 항목
            } else {
                RECONNECT[2].0.to_string()
            };
            let mut args = format!("-ReconnectAfter {value}");
            // 멈춤 감시 설정은 관리 창에서 바꾸지 않으므로 설치된 값을 그대로 넘긴다(모르면 install.ps1 기본값).
            let heal = state.installed_heal.load(Ordering::SeqCst);
            if heal >= 0 {
                args.push_str(&format!(" -HealAfter {}", minutes_arg(heal)));
            }
            if !test {
                args.push_str(" -NoTest");
            }
            let installed_from_program_files = running_from_program_files();
            ui.set_result_visible(false);
            ui.set_busy(true);
            ui.set_busy_text("관리자 권한 창에서 [예]를 누르세요.".into());
            let weak = weak.clone();
            let refresh = refresh.clone();
            let busy_text = busy_text.to_string();
            let title_ok = title_ok.to_string();
            std::thread::spawn(move || {
                let w = weak.clone();
                let on_started = move || {
                    let bt = busy_text.clone();
                    let _ = w.upgrade_in_event_loop(move |ui| ui.set_busy_text(bt.into()));
                };
                let outcome = run_elevated(&script, &args, true, on_started);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_busy(false);
                    match outcome {
                        Ok((0, text)) => {
                            let mut body = summarize(&text);
                            if !installed_from_program_files {
                                body.push_str("\n시작 메뉴의 'Tailscale Force Connect'로 언제든 이 창을 열 수 있습니다.");
                            }
                            show_result(&ui, true, &title_ok, &body);
                        }
                        Ok((code, text)) => show_result(&ui, false, &format!("실패했습니다 (종료 코드 {code})"), &summarize(&text)),
                        Err(msg) => show_result(&ui, false, "실행하지 못했습니다", &msg),
                    }
                });
                refresh();
            });
        })
    };

    // 연결을 몇 초 끊는 작업(동작 확인, 설치 후 동작 확인)
    let run_test = {
        let run = run_install.clone();
        Arc::new(move |kind: PendingTest| match kind {
            PendingTest::Verify => run(true, true, "연결을 몇 초 끊었다가 다시 연결되는지 확인하는 중입니다. 설정은 바꾸지 않습니다.", "동작을 확인했습니다"),
            PendingTest::InstallWithTest => run(true, false, "설치하고 동작을 확인하는 중입니다. 연결이 몇 초 끊깁니다.", "설치했습니다"),
        })
    };
    // 원격 데스크톱이나 Tailscale로 접속 중인 것 같으면 경고를 띄우고 [계속]을 기다린다. 아니면 바로 실행한다.
    let request_test = {
        let weak = ui.as_weak();
        let state = state.clone();
        let run_test = run_test.clone();
        Arc::new(move |kind: PendingTest| {
            let peers = state.active_peers.lock().map(|p| p.clone()).unwrap_or_default();
            match remote_risk(&peers) {
                Some(text) => {
                    let Some(ui) = weak.upgrade() else { return };
                    if let Ok(mut pending) = state.pending_test.lock() {
                        *pending = Some(kind);
                    }
                    ui.set_confirm_test_text(text.into());
                    ui.set_confirm_test_visible(true);
                }
                None => run_test(kind),
            }
        })
    };
    {
        let run = run_install.clone();
        let request_test = request_test.clone();
        let weak = ui.as_weak();
        ui.on_install(move || {
            let test = weak.upgrade().map(|ui| ui.get_run_test()).unwrap_or(false);
            if test {
                request_test(PendingTest::InstallWithTest);
            } else {
                run(false, false, "설치하는 중입니다.", "설치했습니다");
            }
        });
    }
    {
        let run = run_install.clone();
        ui.on_apply_reconnect(move || run(false, false, "자동 재연결 설정을 바꾸는 중입니다.", "설정을 바꿨습니다"));
    }
    {
        let request_test = request_test.clone();
        ui.on_verify(move || request_test(PendingTest::Verify));
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        let run_test = run_test.clone();
        ui.on_confirm_test(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_confirm_test_visible(false);
            }
            let kind = state.pending_test.lock().ok().and_then(|mut pending| pending.take());
            if let Some(kind) = kind {
                run_test(kind);
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_notify_toggled(move |on| {
            if let Err(e) = set_notifications_enabled(on) {
                if let Some(ui) = weak.upgrade() {
                    ui.set_notify_on(!on);
                    show_result(&ui, false, "알림 설정을 바꾸지 못했습니다", &e.to_string());
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_tray_toggled(move |on| {
            if let Err(e) = set_tray_enabled(on) {
                if let Some(ui) = weak.upgrade() {
                    ui.set_tray_on(!on);
                    show_result(&ui, false, "트레이 아이콘 설정을 바꾸지 못했습니다", &e.to_string());
                }
                return;
            }
            if on {
                // 받은 zip에서 연 창이면 설치된 알림 작업이 1분 안에 띄운다(설치되지 않은 실행 파일로 띄우지 않는다).
                if tray_should_run() {
                    ensure_tray_running();
                }
            } else {
                wake_tray(); // 떠 있는 트레이가 바로 꺼진 것을 알아채고 끝나게 한다
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_uninstall(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(script) = sibling("uninstall.ps1") else {
                show_result(&ui, false, "제거 스크립트를 찾지 못했습니다", "uninstall.ps1이 이 실행 파일과 같은 폴더에 있어야 합니다.");
                return;
            };
            ui.set_result_visible(false);
            ui.set_busy(true);
            ui.set_busy_text("관리자 권한 창에서 [예]를 누르세요. 제거가 시작되면 이 창은 닫히고, 끝나면 결과 창이 뜹니다.".into());
            let weak = weak.clone();
            std::thread::spawn(move || {
                // 제거 스크립트가 이 실행 파일을 지워야 하므로 기다리지 않고 창을 닫는다. 결과는 스크립트가 메시지 상자로 알린다.
                let outcome = run_elevated(&script, "-FromGui", false, || {});
                let _ = weak.upgrade_in_event_loop(move |ui| match outcome {
                    Ok(_) => {
                        let _ = slint::quit_event_loop();
                    }
                    Err(msg) => {
                        ui.set_busy(false);
                        show_result(&ui, false, "제거하지 않았습니다", &msg);
                    }
                });
            });
        });
    }

    // 새 버전 확인. 설치된 관리 창(Program Files)은 열 때 자동으로 확인하고(실패하면 조용히 넘어감),
    // 머리글의 버전 표시를 누르면 어디서든 수동으로 확인한다. 새 버전이 있으면 알림 창을 띄운다.
    // 열 때 확인해서 뜬 알림 창은 AUTO_UPDATE_SECONDS 뒤 자동으로 업데이트한다(update_offer 참고).
    let checking = Arc::new(AtomicBool::new(false));
    let check_update = {
        let weak = ui.as_weak();
        let checking = checking.clone();
        Arc::new(move |manual: bool| {
            if checking.swap(true, Ordering::SeqCst) {
                return; // 이미 확인하는 중
            }
            if manual {
                if let Some(ui) = weak.upgrade() {
                    ui.set_update_status("확인하는 중…".into());
                }
            }
            let weak = weak.clone();
            let checking = checking.clone();
            std::thread::spawn(move || {
                let latest = latest_release_version();
                let attempted = auto_attempted_version();
                checking.store(false, Ordering::SeqCst);
                let _ = weak.upgrade_in_event_loop(move |ui| match latest {
                    Some(v) if is_newer(&v, env!("CARGO_PKG_VERSION")) => {
                        let (countdown, note) = update_offer(manual, &v, attempted.as_deref());
                        ui.set_update_status(format!("새 버전 v{v}").into());
                        ui.set_update_version(v.into());
                        ui.set_update_available(true);
                        ui.set_update_countdown_total(countdown.max(1));
                        ui.set_update_countdown(countdown);
                        ui.set_update_note(note.into());
                        ui.set_show_update_prompt(true);
                    }
                    Some(_) => {
                        if manual {
                            ui.set_update_status("최신 버전입니다".into());
                        }
                    }
                    None => {
                        if manual {
                            ui.set_update_status("확인하지 못했습니다. 인터넷 연결을 확인하세요.".into());
                        }
                    }
                });
            });
        })
    };
    if (running_from_program_files() || cfg!(debug_assertions)) && sibling("update.ps1").is_some() {
        check_update(false);
    }
    {
        let check = check_update.clone();
        ui.on_check_update(move || check(true));
    }
    // [업데이트]·[지금 업데이트]를 누르거나(auto=false), 알림 창의 카운트다운이 끝나면(auto=true) update.ps1을 UAC로 실행한다.
    let start_update = {
        let weak = ui.as_weak();
        Arc::new(move |auto: bool| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(script) = sibling("update.ps1") else {
                show_result(&ui, false, "업데이트 스크립트를 찾지 못했습니다", "update.ps1이 이 실행 파일과 같은 폴더에 있어야 합니다. 한 줄 설치로 다시 설치하세요.");
                return;
            };
            if auto {
                let version = ui.get_update_version().to_string();
                // 창을 열 때 읽은 기록은 낡았을 수 있다(관리 창 두 개가 각각 세던 중 다른 창이 먼저 시작함). 지금 다시 읽어,
                // 이미 이 버전을 자동으로 시작했으면 또 승인 창을 띄우지 않고 버튼을 기다린다.
                let (countdown, note) = update_offer(false, &version, auto_attempted_version().as_deref());
                if countdown == 0 {
                    ui.set_update_countdown(0);
                    ui.set_update_note(note.into());
                    ui.set_show_update_prompt(true);
                    return;
                }
                // 승인 창에서 멈추거나 실패해도 같은 버전을 다음에 또 자동으로 시작하지 않도록, 시작하기 전에 남긴다.
                record_auto_attempt(&version);
            }
            ui.set_result_visible(false);
            ui.set_busy(true);
            ui.set_busy_text(
                if auto {
                    "자동으로 업데이트를 시작합니다. 관리자 권한 창에서 [예]를 누르세요. 업데이트가 시작되면 이 창은 닫히고, 끝나면 새 버전으로 다시 열립니다."
                } else {
                    "관리자 권한 창에서 [예]를 누르세요. 업데이트가 시작되면 이 창은 닫히고, 끝나면 새 버전으로 다시 열립니다."
                }
                .into(),
            );
            let weak = weak.clone();
            std::thread::spawn(move || {
                // 업데이트가 이 실행 파일을 바꿔야 하므로 기다리지 않고 창을 닫는다. 실패하면 스크립트가 메시지 상자로 알린다.
                let outcome = run_elevated(&script, "-Relaunch", false, || {});
                let _ = weak.upgrade_in_event_loop(move |ui| match outcome {
                    Ok(_) => {
                        let _ = slint::quit_event_loop();
                    }
                    Err(msg) => {
                        ui.set_busy(false);
                        show_result(&ui, false, "업데이트하지 않았습니다", &msg);
                    }
                });
            });
        })
    };
    {
        let start = start_update.clone();
        ui.on_update(move || start(false));
    }
    {
        let start = start_update.clone();
        ui.on_auto_update(move || start(true));
    }

    // [Tailscale 다시 시작]: 이 실행 파일과 같은 폴더(설치된 경우 Program Files)의 connect.ps1 -Heal을 관리자 권한으로 실행한다.
    // 트레이 아이콘은 정상인데 다른 기기에서 접속이 안 될 때(서비스가 멈춘 경우) 쓴다.
    {
        let weak = ui.as_weak();
        let refresh = refresh.clone();
        ui.on_restart_tailscale(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(script) = sibling("connect.ps1") else {
                show_result(&ui, false, "스크립트를 찾지 못했습니다", "connect.ps1이 이 실행 파일과 같은 폴더에 있어야 합니다. 한 줄 설치로 다시 설치하세요.");
                return;
            };
            ui.set_result_visible(false);
            ui.set_busy(true);
            ui.set_busy_text("관리자 권한 창에서 [예]를 누르세요.".into());
            let weak = weak.clone();
            let refresh = refresh.clone();
            std::thread::spawn(move || {
                let w = weak.clone();
                let on_started = move || {
                    let _ = w.upgrade_in_event_loop(|ui| {
                        ui.set_busy_text("Tailscale 서비스를 다시 시작하고 연결을 확인하는 중입니다(최대 1분). 그동안 연결이 잠깐 끊깁니다.".into())
                    });
                };
                let outcome = run_elevated(&script, "-Heal", true, on_started);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_busy(false);
                    match outcome {
                        Ok((0, text)) => show_result(&ui, true, "Tailscale을 다시 시작했습니다", &summarize(&text)),
                        Ok((2, text)) => show_result(&ui, false, "로그인이 필요합니다", &summarize(&text)),
                        Ok((code, text)) => show_result(&ui, false, &format!("연결되지 않았습니다 (종료 코드 {code})"), &summarize(&text)),
                        Err(msg) => show_result(&ui, false, "실행하지 못했습니다", &msg),
                    }
                });
                refresh();
            });
        });
    }

    ui.on_open_logs(|| {
        let target = if data_dir().exists() { data_dir() } else { exe_dir() };
        let _ = Command::new("explorer.exe").arg(target).spawn();
    });
    ui.on_open_url(|url| {
        if url.starts_with("https://") {
            let _ = Command::new("explorer.exe").arg(url.as_str()).spawn();
        }
    });
    {
        let weak = ui.as_weak();
        ui.on_dismiss_result(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_result_visible(false);
            }
        });
    }

    ui.run()
}

fn show_result(ui: &AppWindow, ok: bool, title: &str, body: &str) {
    ui.set_result_ok(ok);
    ui.set_result_title(title.into());
    ui.set_result_text(body.trim().into());
    ui.set_result_visible(true);
}

/// 스크립트 출력에서 connect.log를 옮겨 적은 부분은 빼고, 사람이 볼 줄만 남긴다.
fn summarize(text: &str) -> String {
    let mut out = Vec::new();
    let mut in_log = false;
    for line in text.lines() {
        let line = line.trim_end();
        if line.starts_with("--- connect.log") {
            in_log = true;
            continue;
        }
        if in_log && line.starts_with("20") {
            continue;
        }
        in_log = false;
        if line.trim().is_empty() || line.starts_with("설치 시작") {
            continue;
        }
        out.push(line.to_string());
    }
    if out.is_empty() {
        return "출력이 없습니다. 로그 폴더의 install.log를 확인하세요.".into();
    }
    out.join("\n")
}

fn apply_snapshot(ui: &AppWindow, s: &Snapshot, state: &UiState) {
    ui.set_installed(s.installed);
    ui.set_state(s.state.into());
    ui.set_state_title(s.title.clone().into());
    ui.set_state_detail(s.detail.clone().into());
    ui.set_fast_startup(s.fast_startup);
    ui.set_boot_text(if s.installed { "자동으로 연결" } else { "설치 안 됨" }.into());
    let (reconnect_text, reconnect_on) = if !s.installed {
        ("설치 안 됨".to_string(), false)
    } else if !s.settings_found {
        ("다시 설치하면 표시".to_string(), false)
    } else if s.reconnect_minutes <= 0 {
        ("그대로 둠".to_string(), false)
    } else {
        (format!("{} 뒤 재연결", minutes_label(s.reconnect_minutes)), true)
    };
    ui.set_reconnect_text(reconnect_text.into());
    ui.set_reconnect_on(reconnect_on);
    let (heal_text, heal_on) = if !s.installed {
        ("설치 안 됨".to_string(), false)
    } else if !s.settings_found {
        ("다시 설치하면 표시".to_string(), false)
    } else if !s.heal_found {
        ("다시 설치하면 켜짐".to_string(), false) // 1.5.0 이하로 설치됨
    } else if s.heal_minutes <= 0 {
        ("그대로 둠".to_string(), false)
    } else {
        (format!("{} 뒤 재시작", minutes_label(s.heal_minutes)), true)
    };
    ui.set_heal_text(heal_text.into());
    ui.set_heal_on(heal_on);
    if let Some(at) = s.watch_at {
        state.watch_at.store(at, Ordering::SeqCst);
    }
    let watch_at = Some(state.watch_at.load(Ordering::SeqCst)).filter(|at| *at > 0);
    let health = watch_health(s.watch_interval, watch_at, unix_now(), s.watch_settling);
    let (watch_text, watch_tone) = watch_window_text(&health, s.watch_interval);
    ui.set_watch_text(watch_text.into());
    ui.set_watch_tone(watch_tone);
    // 트레이 메뉴에서 끈 것도 반영한다(그대로 두면 다시 켜려고 두 번 눌러야 한다).
    ui.set_tray_on(s.tray_on);
    if let Ok(mut peers) = state.active_peers.lock() {
        peers.clone_from(&s.active_peers);
    }

    let known = s.installed && s.settings_found;
    state.installed_minutes.store(if known { s.reconnect_minutes } else { -1 }, Ordering::SeqCst);
    state.installed_heal.store(if known && s.heal_found { s.heal_minutes } else { -1 }, Ordering::SeqCst);

    // 프리셋에 없는 값(예: 한 줄 설치로 넣은 1h30m)은 콤보 상자 끝에 '현재 설정' 항목으로 보여 준다.
    let preset = RECONNECT.iter().position(|(_, m)| *m == s.reconnect_minutes);
    let custom = if known && preset.is_none() { s.reconnect_minutes } else { -1 };
    if state.custom_option.swap(custom, Ordering::SeqCst) != custom {
        let mut labels: Vec<SharedString> = RECONNECT_LABELS.iter().map(|l| SharedString::from(*l)).collect();
        if custom >= 0 {
            labels.push(format!("{} (현재 설정)", minutes_label(custom)).into());
        }
        ui.set_reconnect_options(ModelRc::new(VecModel::from(labels)));
    }

    // 콤보 상자는 처음 한 번만 현재 설정으로 맞춘다(사용자가 고르는 중인 값을 덮어쓰지 않도록).
    if known && !state.index_initialized.swap(true, Ordering::SeqCst) {
        let i = preset.unwrap_or(RECONNECT.len());
        ui.set_reconnect_index(i as i32);
    }

    // 같은 기록을 다시 넣으면 화면이 맨 아래로 돌아가므로(위로 올려 읽는 중일 수 있다), 바뀐 때만 넣는다.
    if let Some(log) = &s.log {
        let lines: Vec<LogLine> =
            log.iter().map(|(time, text)| LogLine { time: time.into(), text: text.into(), tone: log_tone(text) }).collect();
        let current = ui.get_log_lines();
        if current.row_count() != lines.len() || current.iter().zip(&lines).any(|(a, b)| a != *b) {
            ui.set_log_lines(ModelRc::new(VecModel::from(lines)));
        }
    }
}

fn minutes_label(m: i64) -> String {
    match (m / 60, m % 60) {
        (0, mm) => format!("{mm}분"),
        (h, 0) => format!("{h}시간"),
        (h, mm) => format!("{h}시간 {mm}분"),
    }
}

fn collect_snapshot() -> Snapshot {
    collect_snapshot_for(true)
}

/// window가 false면(트레이) 관리 창에만 보이는 최근 기록과 빠른 시작 설정은 읽지 않는다(15초마다 읽으므로).
fn collect_snapshot_for(window: bool) -> Snapshot {
    let data = data_dir();
    let installed = data.join("connect.ps1").exists();

    let settings = read_settings();
    let setting = |key: &str| settings.as_ref().and_then(|v| v.get(key)).and_then(|m| m.as_i64());
    let (settings_found, reconnect_minutes) = setting("reconnectMinutes").map(|m| (true, m)).unwrap_or((false, 0));
    let (heal_found, heal_minutes) = setting("healMinutes").map(|m| (true, m)).unwrap_or((false, 0));
    let heal_active = if installed && heal_found { heal_minutes } else { 0 };

    let mut active_peers = Vec::new();
    let (state, title, detail) = match find_tailscale() {
        None => ("missing", "Tailscale 없음".to_string(), "Tailscale을 먼저 설치하고 로그인하세요.".to_string()),
        Some(ts) => match tailscale_status(&ts) {
            Err(StatusError::TimedOut) => {
                // 감시 작업도 이 상태를 멈춤으로 보고 서비스를 다시 시작한다.
                let detail = if heal_active > 0 {
                    format!("Tailscale이 {}초 동안 응답하지 않습니다. 이어지면 자동으로 다시 시작합니다.", STATUS_TIMEOUT.as_secs())
                } else {
                    format!("Tailscale이 {}초 동안 응답하지 않습니다. 이어지면 [Tailscale 다시 시작]을 누르세요.", STATUS_TIMEOUT.as_secs())
                };
                ("stuck", "응답 없음".to_string(), detail)
            }
            Err(StatusError::Failed) => ("unknown", "상태를 알 수 없음".to_string(), "Tailscale 서비스가 응답하지 않습니다.".to_string()),
            Ok(v) => {
                active_peers = active_peer_names(&v);
                describe_state(&v, installed, reconnect_minutes, heal_active)
            }
        },
    };

    let watch_interval = if installed && settings_found { watch_interval_minutes(reconnect_minutes, heal_active) } else { None };
    Snapshot {
        installed,
        settings_found,
        reconnect_minutes,
        heal_found,
        heal_minutes,
        state,
        title,
        detail,
        log: if window { read_log_tail(&data.join("connect.log")) } else { None },
        fast_startup: window && fast_startup_enabled(),
        active_peers,
        watch_interval,
        watch_at: read_data_json("watch-last.json").and_then(|v| v.get("at")?.as_u64()),
        stopped_for: stopped_for_minutes(),
        watch_settling: just_started(watch_interval),
        tray_on: tray_enabled(),
    }
}

/// 최근 트래픽이 오간 피어(Peer.Active)의 이름. Tailscale은 대략 '최근 2분 안에 패킷이 오감'으로 정의한다.
fn active_peer_names(v: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = v
        .get("Peer")
        .and_then(|p| p.as_object())
        .map(|peers| {
            peers
                .values()
                .filter(|p| p.get("Active").and_then(|a| a.as_bool()) == Some(true))
                .map(|p| {
                    p.get("DNSName")
                        .and_then(|d| d.as_str())
                        .and_then(|d| d.split('.').next())
                        .filter(|s| !s.is_empty())
                        .or_else(|| p.get("HostName").and_then(|h| h.as_str()))
                        .unwrap_or("이름 없는 기기")
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// 동작 확인(연결을 몇 초 끊음) 전에 원격 접속 중인 것 같은지 본다. 그렇다면 경고 문구.
fn remote_risk(active_peers: &[String]) -> Option<String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_REMOTESESSION};
    let rdp = unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0;
    remote_risk_text(rdp, active_peers)
}

fn remote_risk_text(rdp: bool, active_peers: &[String]) -> Option<String> {
    let mut reasons = Vec::new();
    if rdp {
        reasons.push("이 창은 원격 데스크톱 세션에서 열려 있습니다.".to_string());
    }
    if !active_peers.is_empty() {
        let shown = active_peers.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        let more = if active_peers.len() > 3 { format!(" 외 {}대", active_peers.len() - 3) } else { String::new() };
        reasons.push(format!("지금 Tailscale로 {shown}{more}와(과) 연결이 오가고 있습니다."));
    }
    if reasons.is_empty() {
        return None;
    }
    Some(format!(
        "{} 동작 확인은 Tailscale 연결을 몇 초 끊습니다. Tailscale로 원격 접속 중이라면 그동안 화면이 끊기고, 다시 연결되지 않으면 원격에서는 되돌릴 수 없습니다.",
        reasons.join(" ")
    ))
}

/// heal_minutes: 동작 중인 멈춤 감시 시간(분). 꺼져 있거나 설치되지 않았으면 0.
fn describe_state(v: &serde_json::Value, installed: bool, reconnect_minutes: i64, heal_minutes: i64) -> (&'static str, String, String) {
    let backend = v.get("BackendState").and_then(|s| s.as_str()).unwrap_or("");
    let me = v.get("Self");
    // Tailscale에서 쓰는 이름(DNSName의 첫 부분)을 우선한다. 없으면 Windows 컴퓨터 이름.
    let dns_label = me
        .and_then(|s| s.get("DNSName"))
        .and_then(|s| s.as_str())
        .and_then(|d| d.split('.').next())
        .filter(|s| !s.is_empty());
    let host = dns_label.or_else(|| me.and_then(|s| s.get("HostName")).and_then(|s| s.as_str())).unwrap_or("");
    let ip = me
        .and_then(|s| s.get("TailscaleIPs"))
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|s| s.as_str())
        .unwrap_or("");
    match backend {
        "Running" => {
            // Self.Online은 조정 서버와 연결돼 있는지를 나타낸다. 꺼져 있으면 트레이는 '연결됨'이어도 다른 기기에서는 offline으로 보인다.
            // 값이 없으면(예전 Tailscale) 연결된 것으로 본다.
            let online = me.and_then(|s| s.get("Online")).and_then(|o| o.as_bool()).unwrap_or(true);
            if !online {
                let lead = "서버와 연결되지 않았습니다(트레이는 정상으로 보일 수 있음).";
                let detail = if heal_minutes > 0 {
                    format!("{lead} {} 넘게 이어지면 자동으로 다시 시작합니다.", minutes_label(heal_minutes))
                } else if installed {
                    format!("{lead} 이어지면 [Tailscale 다시 시작]을 누르세요.")
                } else {
                    lead.to_string()
                };
                return ("stuck", "연결 안 됨".into(), detail);
            }
            let detail = [host, ip].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
            ("running", "연결됨".into(), detail)
        }
        "Stopped" => {
            let detail = if installed && reconnect_minutes > 0 {
                match stopped_for_minutes() {
                    Some(m) if m < reconnect_minutes => format!("직접 끊은 상태입니다. 약 {}분 뒤 자동으로 다시 연결합니다.", reconnect_minutes - m),
                    Some(_) => "곧 자동으로 다시 연결합니다.".into(),
                    None => format!("직접 끊은 상태입니다. {} 뒤 자동으로 다시 연결합니다.", minutes_label(reconnect_minutes)),
                }
            } else if installed {
                "직접 끊은 상태입니다. 다음 부팅 때 다시 연결합니다.".into()
            } else {
                "Tailscale이 끊겨 있습니다.".into()
            };
            ("stopped", "끊김".into(), detail)
        }
        "NeedsLogin" | "NeedsMachineAuth" => (
            "login",
            "로그인 필요".into(),
            "Tailscale에 로그인해야 자동 연결이 동작합니다.".into(),
        ),
        "Starting" => ("unknown", "연결하는 중…".into(), String::new()),
        other => ("unknown", "상태를 알 수 없음".into(), other.to_string()),
    }
}

/// 감시 작업이 끊긴 상태를 처음 본 때 만든 stopped-since.txt의 수정 시각으로 경과 시간을 잰다.
fn stopped_for_minutes() -> Option<i64> {
    let modified = std::fs::metadata(data_dir().join("stopped-since.txt")).ok()?.modified().ok()?;
    let elapsed = SystemTime::now().duration_since(modified).ok()?;
    Some((elapsed.as_secs() / 60) as i64)
}

/// connect.log의 마지막 LOG_LINES줄. 파일이 없으면 빈 목록, 읽지 못하면(connect.ps1이 정리하며 다시 쓰는 중 등) None.
fn read_log_tail(path: &Path) -> Option<Vec<(String, String)>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    // 쓰는 도중의 마지막 줄처럼 UTF-8이 깨진 부분이 있어도 나머지는 보여 준다.
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.trim_start_matches('\u{feff}').lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(LOG_LINES);
    Some(lines[start..].iter().map(|l| split_log_line(l.trim_end())).collect())
}

/// "2026-11-11 23:50:28  내용" → ("11-11 23:50", "내용"). 형식이 다르면 시각 없이 그대로.
fn split_log_line(l: &str) -> (String, String) {
    if l.len() > 19 && l.as_bytes()[4] == b'-' && l.is_char_boundary(5) && l.is_char_boundary(16) && l.is_char_boundary(19) {
        (l[5..16].to_string(), l[19..].trim_start().to_string())
    } else {
        (String::new(), l.to_string())
    }
}

/// 최근 기록 줄의 색: 0 보통, 1 흐리게(실행 시작 표시), 2 경고.
/// connect.ps1이 남기는 실패·오류·멈춤 문구(…실패, 오류:, …하지 못함, 로그인이 필요함, 시간 초과, 종료 코드, 멈춘 상태, 응답 없음, …되지 않음)를 경고로 본다.
/// "원인 -> 결과" 줄은 결과만 본다(원인에 '응답 없음'이 있어도 다시 시작해 연결됐으면 보통).
fn log_tone(text: &str) -> i32 {
    let outcome = text.rsplit("->").next().unwrap_or(text);
    if text.starts_with("---") {
        1
    } else if ["실패", "오류", "못함", "필요함", "초과", "종료 코드", "멈춘 상태", "응답 없음", "되지 않음"].iter().any(|w| outcome.contains(w)) {
        2
    } else {
        0
    }
}

fn fast_startup_enabled() -> bool {
    use winreg::{RegKey, enums::HKEY_LOCAL_MACHINE};
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SYSTEM\CurrentControlSet\Control\Session Manager\Power")
        .and_then(|k| k.get_value::<u32, _>("HiberbootEnabled"))
        .map(|v| v == 1)
        .unwrap_or(false)
}

/// connect.ps1과 같은 방식: 서비스(tailscaled.exe)와 같은 폴더의 tailscale.exe, 없으면 기본 경로.
fn find_tailscale() -> Option<PathBuf> {
    use winreg::{RegKey, enums::HKEY_LOCAL_MACHINE};
    if let Ok(img) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SYSTEM\CurrentControlSet\Services\Tailscale")
        .and_then(|k| k.get_value::<String, _>("ImagePath"))
    {
        let daemon = daemon_path(&expand_env(img.trim()));
        if let Some(dir) = Path::new(&daemon).parent() {
            let candidate = dir.join("tailscale.exe");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let candidate = PathBuf::from(pf).join(r"Tailscale\tailscale.exe");
    candidate.exists().then_some(candidate)
}

/// 서비스 ImagePath에서 실행 파일 경로만 꺼낸다(connect.ps1의 Get-DaemonPath와 같은 규칙).
/// 따옴표가 없으면 Windows처럼 '.exe'로 끝나는 곳까지를 경로로 본다(공백이 든 경로를 따옴표 없이 등록한 경우).
fn daemon_path(image_path: &str) -> String {
    let img = image_path.trim();
    if let Some(rest) = img.strip_prefix('"') {
        return rest.split('"').next().unwrap_or("").to_string();
    }
    // to_ascii_lowercase는 바이트 길이를 바꾸지 않으므로 같은 위치로 자를 수 있다.
    let lower = img.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(".exe") {
        let end = from + i + 4;
        if end == img.len() || img[end..].starts_with(char::is_whitespace) {
            return img[..end].to_string();
        }
        from = end;
    }
    img.split_whitespace().next().unwrap_or("").to_string()
}

fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                out.push_str(&std::env::var(name).unwrap_or_else(|_| format!("%{name}%")));
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[derive(Debug, PartialEq)]
enum StatusError {
    /// STATUS_TIMEOUT 안에 끝나지 않음(서비스가 멈춤)
    TimedOut,
    /// 실행하지 못했거나 읽을 수 없는 출력(서비스 꺼짐 등)
    Failed,
}

fn tailscale_status(ts: &Path) -> Result<serde_json::Value, StatusError> {
    #[cfg(debug_assertions)]
    if let Ok(path) = std::env::var("TFC_FAKE_STATUS_FILE") {
        // 개발용: 'tailscale status --json' 대신 이 파일을 읽는다(멈춤 상태 화면 확인). 'timeout'이면 응답 없음 흉내.
        if path == "timeout" {
            return Err(StatusError::TimedOut);
        }
        let text = std::fs::read_to_string(path).map_err(|_| StatusError::Failed)?;
        return serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| StatusError::Failed);
    }
    let (_, out) = run_with_timeout(Command::new(ts).args(["status", "--json"]), STATUS_TIMEOUT).map_err(|e| match e {
        RunError::TimedOut => StatusError::TimedOut,
        RunError::Spawn => StatusError::Failed,
    })?;
    serde_json::from_slice(&out).map_err(|_| StatusError::Failed)
}

#[derive(Debug)]
enum RunError {
    Spawn,
    TimedOut,
}

/// 창 없이 실행하고 timeout까지 기다린다. 넘으면 프로세스를 끝낸다. (종료 상태, 표준 출력)을 돌려준다.
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<(ExitStatus, Vec<u8>), RunError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|_| RunError::Spawn)?;
    // 출력은 다른 스레드에서 읽어야 파이프가 가득 차서 멈추지 않는다.
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok((status, reader.join().unwrap_or_default())),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::TimedOut);
            }
        }
    }
}

/// 설치 폴더(connect.ps1, settings.json, 기록). 개발 빌드에서는 TFC_DATA_DIR로 바꿔 화면을 확인한다.
fn data_dir() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("TFC_DATA_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(DATA_DIR)
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn sibling(name: &str) -> Option<PathBuf> {
    let p = exe_dir().join(name);
    p.exists().then_some(p)
}

fn running_from_program_files() -> bool {
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let app = PathBuf::from(pf).join("Tailscale Force Connect");
    exe_dir().to_string_lossy().eq_ignore_ascii_case(&app.to_string_lossy())
}

/// GitHub 최신 릴리스의 버전("v1.4.0" → "1.4.0"). Windows PowerShell로 조회한다(별도 HTTP 라이브러리를 넣지 않으려고).
fn latest_release_version() -> Option<String> {
    #[cfg(debug_assertions)]
    if let Ok(fake) = std::env::var("TFC_FAKE_LATEST") {
        return Some(fake); // 개발용: 업데이트 알림 화면 확인
    }
    let script = "try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072 } catch { };         (Invoke-RestMethod -UseBasicParsing -TimeoutSec 15 -Uri 'https://api.github.com/repos/jeiel85/tailscale-force-connect/releases/latest'         -Headers @{ 'User-Agent' = 'tailscale-force-connect-updater'; 'Accept' = 'application/vnd.github+json' }).tag_name";
    let (_, out) = run_with_timeout(
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            // PowerShell 7에서 상속된 모듈 경로가 Windows PowerShell 5.1에 섞이지 않게 한다.
            .env_remove("PSModulePath"),
        UPDATE_CHECK_TIMEOUT,
    )
    .ok()?;
    let tag = String::from_utf8_lossy(&out).trim().to_string();
    let v = tag.trim_start_matches('v').to_string();
    parse_version(&v).map(|_| v)
}

/// 사용자별 알림 기록을 두는 폴더 (%LOCALAPPDATA%\Tailscale Force Connect). 개발 빌드에서는 TFC_STATE_DIR로 바꾼다.
fn user_state_dir() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("TFC_STATE_DIR") {
        return PathBuf::from(dir);
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Tailscale Force Connect")
}

/// 알림을 끈 사용자의 표시 파일. 관리 창의 'Windows 알림' 선택으로 만들고 지운다(관리자 권한 불필요).
/// 예약 작업은 그대로 1분마다 돌지만, 이 파일이 있으면 아무것도 띄우지 않고 바로 끝난다.
fn notify_off_marker() -> PathBuf {
    user_state_dir().join("notify-off.txt")
}

fn notifications_enabled() -> bool {
    !notify_off_marker().exists()
}

fn set_notifications_enabled(on: bool) -> std::io::Result<()> {
    let marker = notify_off_marker();
    if on {
        match std::fs::remove_file(&marker) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::create_dir_all(user_state_dir())?;
        std::fs::write(&marker, "Windows 알림을 끈 표시입니다. 관리 창에서 다시 켜면 지워집니다.")
    }
}

/// 카운트다운이 끝나 자동으로 업데이트를 시작한 버전. 그 업데이트가 끝나지 않았으면(승인 창 거절·방치, 설치 실패)
/// 같은 버전은 다시 자동으로 시작하지 않고 버튼을 기다린다(승인 창이 되풀이해 뜨지 않도록).
/// 업데이트가 되면 현재 버전이 이 값과 같아져 더 이상 쓰이지 않는다.
fn auto_attempt_marker() -> PathBuf {
    user_state_dir().join("auto-update-attempted.txt")
}

fn auto_attempted_version() -> Option<String> {
    std::fs::read_to_string(auto_attempt_marker()).ok().map(|s| s.trim().to_string())
}

fn record_auto_attempt(version: &str) {
    let _ = std::fs::create_dir_all(user_state_dir());
    let _ = std::fs::write(auto_attempt_marker(), version);
}

/// 새 버전 알림 창의 (자동 업데이트까지 남은 초, 안내 문구). 초가 0이면 버튼을 기다린다.
/// - 머리글의 버전 표시를 눌러 직접 확인한 경우는 이미 화면 앞에 있으므로 세지 않는다.
/// - 같은 버전을 이미 한 번 자동으로 시작했는데 아직 그 버전이 아니면, 끝나지 않은 것이므로 세지 않고 그 사실을 알린다.
fn update_offer(manual: bool, latest: &str, attempted: Option<&str>) -> (i32, String) {
    if manual {
        return (0, String::new());
    }
    if attempted.is_some_and(|a| a.trim_start_matches('v') == latest.trim_start_matches('v')) {
        return (
            0,
            "이 버전은 한 번 자동으로 업데이트를 시작했지만 끝나지 않았습니다(관리자 권한 창을 거절했거나 설치 실패). [지금 업데이트]를 눌러 주세요.".into(),
        );
    }
    (AUTO_UPDATE_SECONDS, String::new())
}

/// 지금 로그인 세션이 시작된 뒤 지난 시간. 알 수 없으면 None(그때는 알림만 띄운다).
fn session_logon_age() -> Option<(i64, Duration)> {
    use windows_sys::Win32::System::RemoteDesktop::{
        WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTSFreeMemory, WTSINFOW, WTSQuerySessionInformationW, WTSSessionInfo,
    };
    let mut buf: windows_sys::core::PWSTR = std::ptr::null_mut();
    let mut len: u32 = 0;
    let ok = unsafe { WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTSSessionInfo, &mut buf, &mut len) };
    if ok == 0 || buf.is_null() {
        return None;
    }
    // LogonTime·CurrentTime은 같은 기준(100ns 단위 FILETIME)이라 빼기만 하면 된다.
    let times = (len as usize >= std::mem::size_of::<WTSINFOW>()).then(|| {
        let info = unsafe { std::ptr::read_unaligned(buf as *const WTSINFOW) };
        (info.LogonTime, info.CurrentTime)
    });
    unsafe { WTSFreeMemory(buf.cast()) };
    let (logon, current) = times?;
    logon_age(logon, current).map(|age| (logon, age))
}

fn logon_age(logon: i64, current: i64) -> Option<Duration> {
    if logon <= 0 || current < logon {
        return None;
    }
    Some(Duration::from_millis(((current - logon) / 10_000) as u64))
}

/// 로그인 직후의 새 버전 확인이면 알림 대신 관리 창을 열지 정한다.
/// 이 버전을 한 번 자동으로 시도했으면(끝나지 않음) 열지 않는다.
fn should_offer_window(logon_age: Option<Duration>, attempted_same: bool) -> bool {
    matches!(logon_age, Some(age) if age <= LOGON_OFFER_WINDOW) && !attempted_same
}

/// logon-offered.txt에 남기는 값: 같은 로그인에서 같은 버전으로 관리 창을 이미 열었는지 알아보는 데 쓴다.
fn logon_offer_key(logon_id: i64, version: &str) -> String {
    format!("{logon_id} {version}")
}

/// "1분", "90초"처럼 사람이 읽는 시간
fn seconds_label(secs: i32) -> String {
    if secs > 0 && secs % 60 == 0 { format!("{}분", secs / 60) } else { format!("{secs}초") }
}

/// 열려 있는 관리 창(창 제목과 winit 창 클래스로 찾는다). 없으면 null.
fn manager_window() -> windows_sys::Win32::Foundation::HWND {
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
    let class = wide("Window Class");
    let title = wide("Tailscale Force Connect");
    unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) }
}

/// 관리 창이 이미 열려 있는지
fn manager_window_open() -> bool {
    !manager_window().is_null()
}

/// 로그인 직후 새 버전을 찾았으면 관리 창을 연다. 창은 열면서 새 버전을 다시 확인하고, AUTO_UPDATE_SECONDS 뒤 자동으로 업데이트한다.
/// 창을 열었으면(또는 이미 열려 있거나, 이 로그인에서 이 버전으로 이미 열었으면) true. 그때는 알림을 띄우지 않는다.
/// 이 실행은 예약 작업(5분 제한)이라 창은 explorer.exe를 거쳐 따로 띄운다.
fn offer_window_at_logon(latest: &str) -> bool {
    let logon = session_logon_age();
    let marker = user_state_dir().join("logon-offered.txt");
    // 로그인 직후 연 창에서 [나중에]를 눌렀는데 곧이어 매일 12:30 확인이 돌면 같은 내용의 알림이 겹치므로 건너뛴다.
    // 버전을 함께 남기므로, 로그아웃 없이 며칠 쓰는 동안 나온 다음 버전은 평소처럼 알린다.
    if let Some((id, _)) = logon {
        if std::fs::read_to_string(&marker).is_ok_and(|s| s.trim() == logon_offer_key(id, latest)) {
            return true;
        }
    }
    let attempted_same = update_offer(false, latest, auto_attempted_version().as_deref()).0 == 0;
    if !should_offer_window(logon.map(|(_, age)| age), attempted_same) {
        return false;
    }
    if manager_window_open() {
        return true; // 열 때 이미 확인했다
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    if Command::new("explorer.exe").arg(exe).spawn().is_err() {
        return false;
    }
    if let Some((id, _)) = logon {
        let _ = std::fs::create_dir_all(user_state_dir());
        let _ = std::fs::write(&marker, logon_offer_key(id, latest));
    }
    true
}

/// 설치 폴더의 JSON 파일(settings.json, heal-last.json). PowerShell 5.1이 붙이는 BOM은 떼고 읽는다.
fn read_data_json(name: &str) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(data_dir().join(name)).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

fn read_settings() -> Option<serde_json::Value> {
    read_data_json("settings.json")
}

/// settings.json에 기록된 자동 재연결 시간(분). 설치되지 않았거나 기록이 없으면 None.
fn installed_reconnect_minutes() -> Option<i64> {
    read_settings()?.get("reconnectMinutes")?.as_i64()
}

fn current_backend_state() -> Option<String> {
    #[cfg(debug_assertions)]
    if let Ok(fake) = std::env::var("TFC_FAKE_STATE") {
        return Some(fake); // 개발용: 끊김 알림 확인
    }
    let v = tailscale_status(&find_tailscale()?).ok()?;
    v.get("BackendState")?.as_str().map(str::to_string)
}

/// 예약 작업 'Tailscale Force Connect Status Alert'가 1분마다 --watch-notify로 실행한다.
/// 끊긴 것을 처음 보면 "끊겼습니다, 약 N분 뒤 다시 연결" 알림을 한 번 띄우고,
/// 그 뒤 다시 연결되면 "다시 연결됐습니다" 알림으로 마무리한다. 연결돼 있는 동안은 아무것도 하지 않는다.
fn watch_and_notify() {
    // 트레이 아이콘이 떠 있지 않으면(로그인 직후, 업데이트로 닫힘, 오류로 끝남) 다시 띄운다. 알림을 꺼도 트레이는 따로 정한다.
    // 이 작업이 끝나거나 1분 제한으로 끝나도 여기서 띄운 프로세스는 남는다(2026-11-18 이 PC에서 확인).
    if tray_should_run() {
        ensure_tray_running();
    }
    if !notifications_enabled() {
        return; // 관리 창에서 알림을 껐다
    }
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // 재시작 직후에는 서비스가 아직 응답하지 않을 수 있으므로 상태를 읽기 전에 알린다.
    notify_heal_if_new(now);
    let Some(state) = current_backend_state() else { return };
    let marker = user_state_dir().join("disconnect-notified.txt");
    match state.as_str() {
        "Stopped" => {
            if marker.exists() {
                return; // 이번 끊김은 이미 알렸다
            }
            let body = match installed_reconnect_minutes() {
                Some(m) if m > 0 => {
                    let elapsed = stopped_for_minutes().unwrap_or(0).max(0);
                    let remaining = (m - elapsed).max(1);
                    format!(
                        "약 {} 뒤 자동으로 다시 연결합니다. 1분마다 상태를 확인하고 있습니다. 누르면 관리 창이 열립니다.",
                        minutes_label(remaining)
                    )
                }
                _ => "자동 재연결이 꺼져 있어 다음 부팅 때 다시 연결합니다. 지금 연결하려면 Tailscale에서 Connect를 누르세요.".to_string(),
            };
            if show_toast("Tailscale 연결이 끊겼습니다", &body) {
                let _ = std::fs::create_dir_all(user_state_dir());
                let _ = std::fs::write(&marker, now.to_string());
            }
        }
        "Running" => {
            let Ok(text) = std::fs::read_to_string(&marker) else { return };
            let _ = std::fs::remove_file(&marker);
            let since = text.trim().parse::<u64>().unwrap_or(now);
            let mins = now.saturating_sub(since) / 60;
            let body = if mins == 0 {
                "연결이 돌아왔습니다.".to_string()
            } else {
                format!("끊긴 것을 알린 지 {} 만에 연결이 돌아왔습니다.", minutes_label(mins as i64))
            };
            show_toast("Tailscale에 다시 연결됐습니다", &body);
        }
        _ => {} // 로그인 필요 등은 관리 창에서 보여 준다
    }
}

/// 감시 작업(SYSTEM)이 멈춘 Tailscale을 다시 시작했으면 한 번 알린다. SYSTEM은 사용자 화면에 알림을 띄울 수 없어서
/// 결과를 heal-last.json에 남기고, 1분마다 도는 이 사용자 작업이 읽어 알린다.
/// 관리 창에서 직접 누른 재시작(manual)은 결과를 이미 봤으므로 알리지 않는다.
fn notify_heal_if_new(now: u64) {
    let Some(v) = read_data_json("heal-last.json") else { return };
    let at = v.get("at").and_then(|a| a.as_u64()).unwrap_or(0);
    if at == 0 || v.get("trigger").and_then(|t| t.as_str()) != Some("watch") {
        return;
    }
    // 오래된 기록(이 사용자가 로그인하기 전, 업데이트 직후 등)은 알리지 않는다.
    if now.saturating_sub(at) > 15 * 60 {
        return;
    }
    let marker = user_state_dir().join("heal-notified.txt");
    let seen = std::fs::read_to_string(&marker).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    if at <= seen {
        return;
    }
    let outcome = v.get("outcome").and_then(|o| o.as_str()).unwrap_or("");
    let minutes = v.get("minutes").and_then(|m| m.as_i64()).unwrap_or(0);
    let (title, body) = heal_toast_text(outcome, minutes);
    if show_toast(&title, &body) {
        let _ = std::fs::create_dir_all(user_state_dir());
        let _ = std::fs::write(&marker, at.to_string());
    }
}

fn heal_toast_text(outcome: &str, minutes: i64) -> (String, String) {
    let lasted = if minutes > 0 { format!("{} 동안 ", minutes_label(minutes)) } else { String::new() };
    match outcome {
        "connected" => (
            "Tailscale을 다시 시작했습니다".into(),
            format!("연결된 것처럼 보였지만 {lasted}서버와 연결되지 않아 서비스를 다시 시작했고, 연결이 돌아왔습니다. 누르면 관리 창이 열립니다."),
        ),
        "stopped" => (
            "Tailscale을 다시 시작했습니다".into(),
            "멈춰 있던 서비스를 다시 시작했습니다. 직접 끊은 상태라 연결은 하지 않았습니다.".into(),
        ),
        "login" => (
            "Tailscale 로그인이 필요합니다".into(),
            "멈춘 Tailscale을 다시 시작했지만 로그인이 필요합니다(키 만료 등). Tailscale에서 다시 로그인하세요.".into(),
        ),
        _ => (
            "Tailscale을 다시 시작했지만 연결되지 않았습니다".into(),
            format!("{lasted}서버와 연결되지 않아 서비스를 다시 시작했지만 풀리지 않았습니다. 30분 뒤 다시 시도합니다. 누르면 관리 창이 열립니다."),
        ),
    }
}

/// 예약 작업 'Tailscale Force Connect Update Check'(로그인 2분 뒤, 매일 12:30)가 --notify-update로 실행한다.
/// 새 버전이 있으면, 로그인 직후에는 관리 창을 열어 1분 뒤 자동으로 업데이트하고(로그인마다),
/// 그 밖에는 Windows 알림을 띄운다(같은 버전은 한 번만).
fn notify_if_update_available() {
    if !notifications_enabled() {
        return; // 관리 창에서 알림을 껐다(새 버전은 관리 창을 열면 보인다)
    }
    let Some(latest) = latest_release_version() else { return };
    if !is_newer(&latest, env!("CARGO_PKG_VERSION")) {
        return;
    }
    if offer_window_at_logon(&latest) {
        return;
    }
    let state_dir = user_state_dir();
    let marker = state_dir.join("notified-version.txt");
    if std::fs::read_to_string(&marker).map(|s| s.trim() == latest).unwrap_or(false) {
        return;
    }
    let (countdown, _) = update_offer(false, &latest, auto_attempted_version().as_deref());
    let body = if countdown > 0 {
        format!(
            "Tailscale Force Connect를 업데이트할 수 있습니다. 누르면 관리 창이 열리고, {} 뒤 자동으로 업데이트합니다.",
            seconds_label(countdown)
        )
    } else {
        "Tailscale Force Connect를 업데이트할 수 있습니다. 누르면 관리 창이 열리고 바로 업데이트할 수 있습니다.".to_string()
    };
    if show_toast(&format!("새 버전 v{latest}"), &body) {
        let _ = std::fs::create_dir_all(&state_dir);
        let _ = std::fs::write(&marker, &latest);
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Windows 알림(토스트)을 띄운다. 누르면 tailscale-force-connect: 링크로 관리 창이 열린다.
/// WinRT 바인딩을 넣지 않으려고 Windows PowerShell의 WinRT 지원을 쓴다. 띄웠으면 true.
fn show_toast(title: &str, body: &str) -> bool {
    let xml = format!(
        "<toast activationType=\"protocol\" launch=\"tailscale-force-connect:update\">\
<visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>\
<actions><action content=\"관리 창 열기\" activationType=\"protocol\" arguments=\"tailscale-force-connect:update\"/></actions></toast>",
        xml_escape(title),
        xml_escape(body)
    );
    let script = format!(
        "$ErrorActionPreference = 'Stop'\n\
[void][Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]\n\
[void][Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]\n\
$xml = New-Object Windows.Data.Xml.Dom.XmlDocument\n\
$xml.LoadXml(@'\n{xml}\n'@)\n\
$toast = New-Object Windows.UI.Notifications.ToastNotification $xml\n\
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{APP_ID}').Show($toast)\n"
    );
    run_with_timeout(
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encode_ps_command(&script)])
            .env_remove("PSModulePath"),
        TOAST_TIMEOUT,
    )
    .map(|(s, _)| s.success())
    .unwrap_or(false)
}

/// PowerShell -EncodedCommand 형식: UTF-16LE를 Base64로. 따옴표·한글이 명령줄에서 깨지지 않게 한다.
fn encode_ps_command(script: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    let major = parts.next()??;
    let minor = parts.next()??;
    let patch = parts.next()??;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// 감시 작업이 살아 있는지
// 감시 작업(SYSTEM)은 정상일 때 로그를 남기지 않고, 그 실행 기록은 일반 권한으로 조회할 수 없다.
// 그래서 connect.ps1 -Watch가 실행할 때마다 덮어쓰는 watch-last.json의 시각으로 판단한다.
// ---------------------------------------------------------------------------

/// install.ps1과 같은 규칙의 감시 작업 확인 주기(분): 멈춤 감시가 켜져 있거나 자동 재연결이 10분 미만이면 1분, 그 밖에는 5분.
/// 둘 다 꺼져 있으면 감시 작업을 등록하지 않으므로 None.
fn watch_interval_minutes(reconnect: i64, heal: i64) -> Option<i64> {
    if reconnect <= 0 && heal <= 0 {
        return None;
    }
    Some(if heal > 0 || reconnect < 10 { 1 } else { 5 })
}

/// 감시 작업 한 번의 실행 제한(분). install.ps1의 ExecutionTimeLimit과 같다.
const WATCH_RUN_LIMIT_MINUTES: i64 = 4;

/// 마지막 확인 뒤 이만큼(초) 지나면 감시 작업이 멈춘 것으로 본다: 세 번 거른 시간과
/// '실행 제한까지 걸린 한 번(그동안의 실행은 건너뜀) + 다음 주기' 중 긴 쪽에 2분을 더한다. 1분 주기는 7분, 5분 주기는 17분.
fn watch_stale_after(interval: i64) -> u64 {
    let interval = interval.max(1);
    ((interval * 3).max(WATCH_RUN_LIMIT_MINUTES + interval) + 2) as u64 * 60
}

#[derive(Clone, Debug, PartialEq)]
enum WatchHealth {
    /// 감시 작업을 쓰지 않음(설치 안 됨, 자동 재연결·멈춤 감시 둘 다 끔)
    Off,
    /// 정상: 마지막 확인이 이 초 전
    Alive(u64),
    /// 켜진 직후·설치 직후·깨어난 직후라 아직 판단하지 않음
    Pending,
    /// 마지막 확인 뒤 이 분이 지나도록 돌지 않음. 기록이 아예 없으면 None
    Stale(Option<u64>),
}

/// settling: 켜진 직후·설치 직후·절전에서 깨어난 직후라 감시 작업이 아직 돌 기회가 없었을 수 있는 동안
fn watch_health(interval: Option<i64>, last_at: Option<u64>, now: u64, settling: bool) -> WatchHealth {
    let Some(interval) = interval else { return WatchHealth::Off };
    if let Some(at) = last_at {
        let age = now.saturating_sub(at); // 시계가 뒤로 가 미래 시각이면 방금 확인한 것으로 본다
        if age <= watch_stale_after(interval) {
            return WatchHealth::Alive(age);
        }
    }
    if settling {
        return WatchHealth::Pending;
    }
    WatchHealth::Stale(last_at.map(|at| now.saturating_sub(at) / 60))
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 켜진 직후나 설치·업데이트 직후, 절전에서 깨어난 직후에는 감시 작업이 아직 돌지 않았을 수 있다
/// (작업은 부팅 뒤, 설치하고 1분 뒤부터 돈다). 설치 시각은 install.ps1이 매번 새로 쓰는 settings.json의 수정 시각으로 본다.
fn just_started(interval: Option<i64>) -> bool {
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    let Some(interval) = interval else { return false };
    let wait = Duration::from_secs(watch_stale_after(interval));
    let uptime = Duration::from_millis(unsafe { GetTickCount64() });
    let since_install = std::fs::metadata(data_dir().join("settings.json"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok());
    uptime < wait || since_install.is_some_and(|d| d < wait) || since_last_wake().is_some_and(|d| d < wait)
}

/// 절전·최대 절전에서 마지막으로 깨어난 뒤 지난 시간. 깨어난 적이 없으면 None.
fn since_last_wake() -> Option<Duration> {
    use windows_sys::Win32::System::{
        Power::{CallNtPowerInformation, LastWakeTime},
        SystemInformation::GetTickCount64,
    };
    let mut wake: u64 = 0; // 깨어난 때의 인터럽트 시간(100ns 단위, 절전 시간 포함). GetTickCount64와 같은 기준이다
    let status = unsafe {
        CallNtPowerInformation(LastWakeTime, std::ptr::null(), 0, (&raw mut wake).cast(), size_of::<u64>() as u32)
    };
    if status != 0 || wake == 0 {
        return None;
    }
    let now = unsafe { GetTickCount64() }.saturating_mul(10_000);
    Some(Duration::from_nanos(now.saturating_sub(wake).saturating_mul(100)))
}

/// "45초", "12분", "3시간", "2일"
fn ago_label(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}초"),
        s if s < 3600 => format!("{}분", s / 60),
        s if s < 48 * 3600 => format!("{}시간", s / 3600),
        s => format!("{}일", s / 86400),
    }
}

/// 관리 창의 감시 작업 줄: (문구, 0 정상 / 1 확인 전 / 2 멈춘 것 같음). 문구가 비면 숨긴다.
fn watch_window_text(h: &WatchHealth, interval: Option<i64>) -> (String, i32) {
    match h {
        WatchHealth::Off => (String::new(), 1),
        WatchHealth::Alive(age) => (
            format!("감시 작업 정상: {} 전에 확인했습니다({}분마다 확인).", ago_label(*age), interval.unwrap_or(1)),
            0,
        ),
        WatchHealth::Pending => ("감시 작업이 곧 첫 확인을 합니다(켜지거나 설치된 직후).".into(), 1),
        WatchHealth::Stale(Some(m)) => (
            format!("감시 작업이 {} 동안 돌지 않았습니다. [적용]을 누르면 다시 등록합니다.", ago_label(m * 60)),
            2,
        ),
        WatchHealth::Stale(None) => ("감시 작업의 확인 기록이 없습니다. [적용]을 누르면 다시 등록합니다.".into(), 2),
    }
}

// ---------------------------------------------------------------------------
// 작업 표시줄 상태 아이콘 (--tray)
// 트레이가 떠 있지 않으면 1분마다 도는 알림 작업(--watch-notify)과 관리 창이 띄운다. 트레이는 알림을 띄우지 않고 상태만 보여 준다
// (알림은 지금처럼 알림 작업이 맡는다. 두 곳에서 띄우면 겹친다).
// ---------------------------------------------------------------------------

/// 트레이가 상태를 다시 읽는 간격. 관리 창이 깨우면(트레이 끄기) 바로 다시 본다.
const TRAY_POLL: Duration = Duration::from_secs(15);
/// 같은 로그인 세션에 트레이가 하나만 뜨게 하는 이름(Local\은 세션마다 따로). 개발 빌드는 설치된 트레이와 겹치지 않게 따로 둔다.
const TRAY_MUTEX: &str = if cfg!(debug_assertions) { r"Local\TailscaleForceConnect.Tray.Dev" } else { r"Local\TailscaleForceConnect.Tray" };
/// 관리 창이 떠 있는 트레이를 바로 깨우는 이벤트
const TRAY_WAKE: &str = if cfg!(debug_assertions) { r"Local\TailscaleForceConnect.TrayWake.Dev" } else { r"Local\TailscaleForceConnect.TrayWake" };

/// 트레이 아이콘을 끈 사용자의 표시 파일. 관리 창의 '트레이 아이콘' 선택이나 트레이 메뉴로 만들고 지운다.
fn tray_off_marker() -> PathBuf {
    user_state_dir().join("tray-off.txt")
}

fn tray_enabled() -> bool {
    !tray_off_marker().exists()
}

fn set_tray_enabled(on: bool) -> std::io::Result<()> {
    let marker = tray_off_marker();
    if on {
        match std::fs::remove_file(&marker) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::create_dir_all(user_state_dir())?;
        std::fs::write(&marker, "트레이 아이콘을 끈 표시입니다. 관리 창에서 다시 켜면 지워집니다.")
    }
}

/// 설치된 관리 앱(Program Files)이고 이 PC에 설치돼 있을 때만 트레이를 띄운다(받은 zip에서 연 창은 띄우지 않음).
fn tray_should_run() -> bool {
    running_from_program_files() && data_dir().join("connect.ps1").exists()
}

/// 이 프로세스가 끝나면 닫히는 핸들
struct OwnedHandle(windows_sys::Win32::Foundation::HANDLE);
// 커널 개체 핸들은 어느 스레드에서 써도 된다.
unsafe impl Send for OwnedHandle {}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// 이 세션에 트레이가 이미 떠 있으면 None. 돌려받은 핸들을 쥐고 있는 동안 다른 트레이는 뜨지 않는다.
fn acquire_tray_instance() -> Option<Option<OwnedHandle>> {
    use windows_sys::Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
        System::Threading::CreateMutexW,
    };
    let name = wide(TRAY_MUTEX);
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if h.is_null() {
        return Some(None); // 만들지 못했으면 겹칠 수 있더라도 띄운다(아이콘이 없는 것보다 낫다)
    }
    let handle = OwnedHandle(h);
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return None;
    }
    Some(Some(handle))
}

fn tray_running() -> bool {
    use windows_sys::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    let name = wide(TRAY_MUTEX);
    let h = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if h.is_null() {
        return false;
    }
    drop(OwnedHandle(h));
    true
}

/// UAC로 권한을 올려 실행 중인지(같은 사용자의 일반 권한 토큰이 따로 있음).
/// UAC를 끈 PC나 기본 Administrator 계정은 모든 프로세스가 관리자 권한이라 여기에 해당하지 않는다.
fn elevated_by_uac() -> bool {
    use windows_sys::Win32::{
        Security::{GetTokenInformation, TOKEN_ELEVATION_TYPE, TOKEN_QUERY, TokenElevationType, TokenElevationTypeFull},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let token = OwnedHandle(token);
    let mut kind: TOKEN_ELEVATION_TYPE = 0;
    let mut len = 0u32;
    let ok = unsafe {
        GetTokenInformation(token.0, TokenElevationType, (&raw mut kind).cast(), size_of::<TOKEN_ELEVATION_TYPE>() as u32, &mut len)
    };
    ok != 0 && kind == TokenElevationTypeFull
}

/// 트레이를 켜 두었는데 떠 있지 않으면 띄운다. 겹쳐 띄워도 나중 것은 바로 끝난다.
/// UAC로 권한을 올려 연 관리 창에서는 띄우지 않는다(트레이가 관리자 권한으로 상주하고, 일반 트레이도 막는다).
fn ensure_tray_running() {
    if !tray_enabled() || tray_running() || elevated_by_uac() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = Command::new(exe).arg("--tray").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// 떠 있는 트레이가 바로 상태를 다시 읽게 한다(트레이를 끄면 바로 사라진다).
fn wake_tray() {
    use windows_sys::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
    let name = wide(TRAY_WAKE);
    let h = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
    if !h.is_null() {
        let h = OwnedHandle(h);
        unsafe { SetEvent(h.0) };
    }
}

/// 작업 표시줄 창. 탐색기가 다시 시작되면 바뀐다(없으면 0).
fn taskbar_window() -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
    let class = wide("Shell_TrayWnd");
    unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) as isize }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    /// 연결됨, 감시 정상
    Ok,
    /// 직접 끊음, 서버와 연결 안 됨, 응답 없음
    Warn,
    /// 감시 작업 멈춤, 로그인 필요, Tailscale 없음, 설치 안 됨
    Bad,
    /// 확인하는 중, 알 수 없음
    Idle,
}

#[derive(Clone, Debug, PartialEq)]
struct TrayView {
    tone: Tone,
    state_line: String,
    watch_line: String,
    tooltip: String,
}

/// 알림 영역 툴팁은 끝의 null을 포함해 128자(UTF-16)까지다.
const TOOLTIP_MAX: usize = 127;

fn truncate_utf16(s: &str, max: usize) -> String {
    if s.encode_utf16().count() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        if used + c.len_utf16() > max - 1 {
            break;
        }
        used += c.len_utf16();
        out.push(c);
    }
    out.push('…');
    out
}

fn tray_view(s: &Snapshot, watch: &WatchHealth) -> TrayView {
    let tone = if !s.installed || matches!(watch, WatchHealth::Stale(_)) {
        Tone::Bad
    } else {
        match s.state {
            "running" => Tone::Ok,
            "stopped" | "stuck" => Tone::Warn,
            "login" | "missing" => Tone::Bad,
            _ => Tone::Idle,
        }
    };
    let state_line = match s.state {
        "running" if !s.detail.is_empty() => format!("{} · {}", s.title, s.detail),
        "stopped" => {
            let next = if !s.installed {
                None
            } else if s.reconnect_minutes > 0 {
                Some(match s.stopped_for {
                    Some(m) if m < s.reconnect_minutes => format!("약 {} 뒤 다시 연결", minutes_label(s.reconnect_minutes - m)),
                    Some(_) => "곧 다시 연결".to_string(),
                    None => format!("{} 뒤 다시 연결", minutes_label(s.reconnect_minutes)),
                })
            } else {
                Some("다음 부팅 때 연결".to_string())
            };
            match next {
                Some(n) => format!("{} · {n}", s.title),
                None => s.title.clone(),
            }
        }
        "stuck" if s.title == "응답 없음" => "응답 없음 · Tailscale이 답하지 않음".to_string(),
        "stuck" => format!("{} · 다른 기기에서는 offline", s.title),
        _ => s.title.clone(),
    };
    let watch_line = if !s.installed {
        "설치되지 않음 · 관리 창에서 설치".to_string()
    } else {
        match watch {
            WatchHealth::Off => "자동 재연결·멈춤 감시 꺼짐".to_string(),
            WatchHealth::Alive(age) => format!("감시 정상 · {} 전 확인", ago_label(*age)),
            WatchHealth::Pending => "감시 작업 첫 확인 대기".to_string(),
            WatchHealth::Stale(Some(m)) => format!("감시 작업이 {} 동안 돌지 않음", ago_label(m * 60)),
            WatchHealth::Stale(None) => "감시 작업 확인 기록 없음".to_string(),
        }
    };
    let tooltip = truncate_utf16(&format!("Tailscale Force Connect\n{state_line}\n{watch_line}"), TOOLTIP_MAX);
    TrayView { tone, state_line, watch_line, tooltip }
}

/// 관리 창의 어두운 테마 색과 같다. 빨강만 16px에서도 주황과 구별되도록 더 진하게 했다.
fn tone_rgb(t: Tone) -> [u8; 3] {
    match t {
        Tone::Ok => [0x3C, 0xC2, 0x93],
        Tone::Warn => [0xF0, 0xA6, 0x3A],
        Tone::Bad => [0xEB, 0x4B, 0x3F],
        Tone::Idle => [0x8A, 0x98, 0xA3],
    }
}

fn in_rounded_square(x: f32, y: f32, size: f32, radius: f32) -> bool {
    let cx = x.clamp(radius, size - radius);
    let cy = y.clamp(radius, size - radius);
    (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
}

/// 앱 아이콘과 같은 모양(어두운 둥근 사각형, 밝은 고리, 가운데 원)을 상태 색으로 그린다.
/// 알림 영역 아이콘은 16~32px이라 앱 아이콘보다 고리와 가운데 원을 조금 키웠다. 가장자리는 한 픽셀을 4x4로 나눠 부드럽게 칠한다.
fn render_status_icon(size: u32, tone: Tone) -> slint::SharedPixelBuffer<slint::Rgba8Pixel> {
    const SS: u32 = 4;
    let s = size as f32;
    let c = s / 2.0;
    let bg = [0x14u8, 0x20, 0x2A];
    let ring = [0xCFu8, 0xE3, 0xD8];
    let dot = tone_rgb(tone);
    let glow: [u8; 3] = std::array::from_fn(|i| (bg[i] as f32 * 0.7 + dot[i] as f32 * 0.3) as u8);
    let (corner, r_dot, r_in, r_out) = (s * 0.22, s * 0.27, s * 0.345, s * 0.43);
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(size, size);
    let pixels = buf.make_mut_slice();
    for y in 0..size {
        for x in 0..size {
            let mut sum = [0u32; 3];
            let mut covered = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    if !in_rounded_square(fx, fy, s, corner) {
                        continue;
                    }
                    let d = ((fx - c).powi(2) + (fy - c).powi(2)).sqrt();
                    let col = if d <= r_dot {
                        dot
                    } else if d < r_in {
                        glow
                    } else if d <= r_out {
                        ring
                    } else {
                        bg
                    };
                    for i in 0..3 {
                        sum[i] += col[i] as u32;
                    }
                    covered += 1;
                }
            }
            pixels[(y * size + x) as usize] = if covered == 0 {
                slint::Rgba8Pixel { r: 0, g: 0, b: 0, a: 0 }
            } else {
                slint::Rgba8Pixel {
                    r: (sum[0] / covered) as u8,
                    g: (sum[1] / covered) as u8,
                    b: (sum[2] / covered) as u8,
                    a: (covered * 255 / (SS * SS)) as u8,
                }
            };
        }
    }
    buf
}

/// 알림 영역 아이콘 크기(100%에서 16px). 이 크기로 그려야 흐리지 않다.
fn tray_icon_size() -> u32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
    (unsafe { GetSystemMetrics(SM_CXSMICON) }).clamp(16, 64) as u32
}

fn status_icon(tone: Tone) -> slint::Image {
    slint::Image::from_rgba8(render_status_icon(tray_icon_size(), tone))
}

/// 트레이를 띄운 스레드(이벤트 루프)에서만 쓴다.
struct TrayState {
    ui: StatusTray,
    tone: Tone,
    /// 아이콘을 만들 때의 작업 표시줄 창
    taskbar: isize,
}

thread_local! {
    static TRAY: std::cell::RefCell<Option<TrayState>> = const { std::cell::RefCell::new(None) };
    /// 관리 창을 띄운 시각. 빠르게 두 번 눌러 창이 두 개 뜨지 않게 한다.
    static WINDOW_SPAWNED: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// 관리 창이 열려 있으면 앞으로 가져오고, 없으면 연다.
fn open_manager_window() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow};
    let hwnd = manager_window();
    if !hwnd.is_null() {
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
        }
        return;
    }
    if WINDOW_SPAWNED.get().is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
        return; // 방금 띄운 창이 아직 뜨는 중
    }
    if let Ok(exe) = std::env::current_exe() {
        if Command::new(exe).spawn().is_ok() {
            WINDOW_SPAWNED.set(Some(Instant::now()));
        }
    }
}

fn new_status_tray(view: &TrayView) -> Result<StatusTray, slint::PlatformError> {
    let tray = StatusTray::new()?;
    tray.set_status_icon(status_icon(view.tone));
    tray.set_tip(view.tooltip.clone().into());
    tray.set_state_line(view.state_line.clone().into());
    tray.set_watch_line(view.watch_line.clone().into());
    tray.on_open_window(open_manager_window);
    tray.on_turn_off(|| {
        // 끈 표시를 남기지 못했으면 알림 작업이 1분 안에 다시 띄우므로, 끄지 않고 남는다(관리 창에서 끄면 이유를 보여 준다).
        if set_tray_enabled(false).is_ok() {
            let _ = slint::quit_event_loop();
        }
    });
    tray.show()?;
    Ok(tray)
}

fn apply_tray_view(view: TrayView, taskbar: isize) {
    TRAY.with(|cell| {
        let mut slot = cell.borrow_mut();
        let Some(st) = slot.as_mut() else { return };
        // 탐색기가 다시 시작되면 알림 영역이 새로 만들어지면서 아이콘이 사라진다. Slint 트레이는 그때 오는 알림(TaskbarCreated)을
        // 받지 못하는 메시지 전용 창을 쓰므로, 작업 표시줄 창이 바뀐 것을 보고 아이콘을 새로 만든다(로그인 직후 작업 표시줄이 늦게 뜬 경우도).
        // 새 작업 표시줄이 아직 아이콘을 받을 준비가 안 돼 실패했으면 다음 확인 때 다시 만든다.
        if taskbar != 0 && taskbar != st.taskbar {
            if let Ok(ui) = new_status_tray(&view) {
                st.ui = ui; // 새 아이콘을 띄운 뒤 이전 것을 버린다(이벤트 루프가 끝나지 않도록)
                st.tone = view.tone;
                st.taskbar = taskbar;
            }
            return;
        }
        if st.tone != view.tone {
            st.ui.set_status_icon(status_icon(view.tone));
            st.tone = view.tone;
        }
        st.ui.set_tip(view.tooltip.into());
        st.ui.set_state_line(view.state_line.into());
        st.ui.set_watch_line(view.watch_line.into());
    });
}

/// --tray: 알림 영역에 상태 아이콘을 띄우고 15초마다 갱신한다. 트레이를 끄면(관리 창, 트레이 메뉴) 끝난다.
fn run_tray() -> Result<(), slint::PlatformError> {
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    if !tray_enabled() {
        return Ok(());
    }
    let Some(_instance) = acquire_tray_instance() else {
        return Ok(()); // 이 세션에 이미 떠 있다
    };
    let wake_name = wide(TRAY_WAKE);
    let wake = OwnedHandle(unsafe { CreateEventW(std::ptr::null(), 0, 0, wake_name.as_ptr()) });

    let first = TrayView {
        tone: Tone::Idle,
        state_line: "확인하는 중…".into(),
        watch_line: " ".into(),
        tooltip: "Tailscale Force Connect\n확인하는 중…".into(),
    };
    let taskbar = taskbar_window();
    let ui = new_status_tray(&first)?;
    TRAY.with(|cell| *cell.borrow_mut() = Some(TrayState { ui, tone: first.tone, taskbar }));

    std::thread::spawn(move || {
        let wake = wake;
        let mut last_at: Option<u64> = None;
        let mut woke: Option<Instant> = None;
        loop {
            if !tray_enabled() {
                let _ = slint::invoke_from_event_loop(|| {
                    let _ = slint::quit_event_loop();
                });
                return;
            }
            let snap = collect_snapshot_for(false);
            // 감시 작업이 파일을 쓰는 순간이라 읽지 못했으면 지난 값을 쓴다.
            if snap.watch_at.is_some() {
                last_at = snap.watch_at;
            }
            // 깨어난 시각은 Windows에서 읽지만(watch_settling), 읽지 못하는 경우(모던 대기 등)에 대비해 이 스레드가 직접 본 것도 쓴다.
            let settle = snap.watch_interval.map(|i| Duration::from_secs(watch_stale_after(i)));
            let settling = snap.watch_settling || woke.zip(settle).is_some_and(|(w, s)| w.elapsed() < s);
            let view = tray_view(&snap, &watch_health(snap.watch_interval, last_at, unix_now(), settling));
            let taskbar = taskbar_window();
            if slint::invoke_from_event_loop(move || apply_tray_view(view, taskbar)).is_err() {
                return; // 이벤트 루프가 끝났다
            }
            let before = SystemTime::now();
            if wake.0.is_null() {
                std::thread::sleep(TRAY_POLL);
            } else {
                unsafe { WaitForSingleObject(wake.0, TRAY_POLL.as_millis() as u32) };
            }
            // 기다린 시간이 훨씬 길면 절전에서 깨어난 것이다. 감시 작업도 곧 다시 도니 잠시 판단을 미룬다.
            if SystemTime::now().duration_since(before).is_ok_and(|d| d > TRAY_POLL + Duration::from_secs(60)) {
                woke = Some(Instant::now());
            }
        }
    });

    slint::run_event_loop_until_quit()?;
    TRAY.with(|cell| cell.borrow_mut().take());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("1.4.0", "1.3.0"));
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("1.3.0", "1.3.0"));
        assert!(!is_newer("1.2.9", "1.3.0"));
        assert!(!is_newer("1.4.0-beta", "1.3.0"));
        assert!(!is_newer("garbage", "1.3.0"));
        assert!(!is_newer("1.4", "1.3.0"));
    }

    #[test]
    fn auto_update_offer() {
        // 열 때 확인해서 뜬 알림 창은 세고, 직접 확인은 세지 않는다
        assert_eq!(update_offer(false, "1.9.0", None), (AUTO_UPDATE_SECONDS, String::new()));
        assert_eq!(update_offer(true, "1.9.0", None), (0, String::new()));
        // 다른 버전을 시도했던 기록은 상관없다
        assert_eq!(update_offer(false, "1.9.0", Some("1.8.1")).0, AUTO_UPDATE_SECONDS);
        // 같은 버전을 이미 자동으로 시작했는데 아직이면 버튼을 기다리고 이유를 보여 준다
        let (secs, note) = update_offer(false, "1.9.0", Some("1.9.0"));
        assert_eq!(secs, 0);
        assert!(note.contains("끝나지 않았습니다"), "{note}");
        assert_eq!(update_offer(false, "1.9.0", Some("v1.9.0")).0, 0);
        assert_eq!(update_offer(true, "1.9.0", Some("1.9.0")), (0, String::new()));
    }

    #[test]
    fn logon_offer_window() {
        let min = |m: u64| Some(Duration::from_secs(m * 60));
        assert!(should_offer_window(min(2), false));
        assert!(should_offer_window(min(10), false));
        assert!(!should_offer_window(min(11), false), "매일 확인처럼 로그인한 지 오래면 알림만");
        assert!(!should_offer_window(None, false), "로그인 시각을 모르면 알림만");
        assert!(!should_offer_window(min(2), true), "이미 자동으로 시도한 버전");
        // 같은 로그인·같은 버전일 때만 같은 값(다음 버전은 다시 알림)
        assert_eq!(logon_offer_key(134, "1.9.1"), "134 1.9.1");
        assert_ne!(logon_offer_key(134, "1.9.1"), logon_offer_key(134, "1.9.2"));
        assert_eq!(seconds_label(60), "1분");
        assert_eq!(seconds_label(90), "90초");
        assert_eq!(seconds_label(30), "30초");
        // FILETIME(100ns) 차이 → 시간
        assert_eq!(logon_age(1_000_000_000, 1_000_000_000 + 1_200_000_000), Some(Duration::from_secs(120)));
        assert_eq!(logon_age(0, 5), None);
        assert_eq!(logon_age(10, 5), None);
        // 이 PC의 실제 세션: 읽을 수 있으면 로그인한 지 지난 시간이 나온다
        let session = session_logon_age();
        eprintln!("session logon: {session:?}");
        if let Some((id, age)) = session {
            assert!(id > 0 && age < Duration::from_secs(400 * 24 * 3600), "{id} {age:?}");
        }
    }

    #[test]
    fn encoded_command() {
        // [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('ab'))
        assert_eq!(encode_ps_command("ab"), "YQBiAA==");
        // [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('한')) == 'XNU=' (U+D55C → 5C D5)
        assert_eq!(encode_ps_command("한"), "XNU=");
        assert_eq!(xml_escape("<a&\"b\">"), "&lt;a&amp;&quot;b&quot;&gt;");
    }

    #[test]
    fn minutes_args() {
        assert_eq!(minutes_arg(0), "0");
        assert_eq!(minutes_arg(-1), "0");
        assert_eq!(minutes_arg(90), "90m");
        assert_eq!(minutes_arg(3), "3m");
    }

    fn status(backend: &str, online: Option<bool>) -> serde_json::Value {
        let mut me = serde_json::json!({ "DNSName": "pc.tail1234.ts.net.", "TailscaleIPs": ["100.64.0.1"] });
        if let Some(o) = online {
            me["Online"] = serde_json::json!(o);
        }
        serde_json::json!({ "BackendState": backend, "Self": me })
    }

    #[test]
    fn running_but_offline_is_stuck() {
        // 실제로 겪은 상태: BackendState는 Running인데 Self.Online이 false
        let (state, title, detail) = describe_state(&status("Running", Some(false)), true, 60, 3);
        assert_eq!(state, "stuck");
        assert_eq!(title, "연결 안 됨");
        assert!(detail.contains("3분"), "{detail}");
        let (_, _, detail) = describe_state(&status("Running", Some(false)), true, 60, 0);
        assert!(detail.contains("[Tailscale 다시 시작]"), "{detail}");

        let (state, _, detail) = describe_state(&status("Running", Some(true)), true, 60, 3);
        assert_eq!(state, "running");
        assert_eq!(detail, "pc · 100.64.0.1");
        // Online 값이 없는 예전 Tailscale은 연결된 것으로 본다
        assert_eq!(describe_state(&status("Running", None), true, 60, 3).0, "running");
    }

    #[test]
    fn heal_toasts() {
        let (title, body) = heal_toast_text("connected", 4);
        assert_eq!(title, "Tailscale을 다시 시작했습니다");
        assert!(body.contains("4분 동안"), "{body}");
        assert!(heal_toast_text("failed", 0).1.starts_with("서버와"));
        assert_eq!(heal_toast_text("login", 3).0, "Tailscale 로그인이 필요합니다");
    }

    #[test]
    fn daemon_paths() {
        let p = r"C:\Program Files\Tailscale\tailscaled.exe";
        assert_eq!(daemon_path(&format!("\"{p}\"")), p);
        assert_eq!(daemon_path(&format!("\"{p}\" --port 41641")), p);
        // 따옴표 없이 등록된 공백 경로(이전에는 C:\Program 에서 잘렸다)
        assert_eq!(daemon_path(p), p);
        assert_eq!(daemon_path(&format!("{p} --port 41641")), p);
        assert_eq!(daemon_path(r"D:\Apps\TAILSCALED.EXE"), r"D:\Apps\TAILSCALED.EXE");
        assert_eq!(daemon_path(r"  D:\Tailscale\tailscaled.exe"), r"D:\Tailscale\tailscaled.exe");
        assert_eq!(daemon_path(r"D:\Tailscale\tailscaled --x"), r"D:\Tailscale\tailscaled");
        assert_eq!(daemon_path(r"D:\a.exes\tailscaled.exe"), r"D:\a.exes\tailscaled.exe");
    }

    #[test]
    fn link_launch_opens_window_only() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(launch_mode(&a(&[])), LaunchMode::Window);
        assert_eq!(launch_mode(&a(&["--watch-notify"])), LaunchMode::WatchNotify);
        assert_eq!(launch_mode(&a(&["--notify-update"])), LaunchMode::NotifyUpdate);
        assert_eq!(launch_mode(&a(&["--notify-test"])), LaunchMode::NotifyTest);
        assert_eq!(launch_mode(&a(&["tailscale-force-connect:update"])), LaunchMode::Window);
        // 웹 페이지가 링크에 인자를 끼워 넣어도 창만 연다
        assert_eq!(launch_mode(&a(&["tailscale-force-connect:x", "--notify-update"])), LaunchMode::Window);
        assert_eq!(launch_mode(&a(&["TAILSCALE-FORCE-CONNECT:--watch-notify"])), LaunchMode::Window);
        assert_eq!(launch_mode(&a(&["\"tailscale-force-connect:x\"", "--watch-notify"])), LaunchMode::Window);
        assert_eq!(launch_mode(&a(&["--tray"])), LaunchMode::Tray);
        assert_eq!(launch_mode(&a(&["tailscale-force-connect:x", "--tray"])), LaunchMode::Window);
    }

    #[test]
    fn watch_interval_and_health() {
        // install.ps1과 같은 주기
        assert_eq!(watch_interval_minutes(60, 3), Some(1));
        assert_eq!(watch_interval_minutes(60, 0), Some(5));
        assert_eq!(watch_interval_minutes(5, 0), Some(1));
        assert_eq!(watch_interval_minutes(0, 3), Some(1));
        assert_eq!(watch_interval_minutes(0, 0), None);
        // 1분 주기: 실행 제한(4분)까지 걸린 한 번 뒤 다음 실행까지 기다린다
        assert_eq!(watch_stale_after(1), 420);
        assert_eq!(watch_stale_after(5), 1020);

        let now = 1_000_000;
        assert_eq!(watch_health(None, Some(now), now, false), WatchHealth::Off);
        assert_eq!(watch_health(Some(1), Some(now - 20), now, false), WatchHealth::Alive(20));
        assert_eq!(watch_health(Some(1), Some(now - 330), now, false), WatchHealth::Alive(330), "서비스 재시작이 길어진 실행 뒤");
        assert_eq!(watch_health(Some(1), Some(now - 420), now, false), WatchHealth::Alive(420));
        assert_eq!(watch_health(Some(1), Some(now - 421), now, false), WatchHealth::Stale(Some(7)));
        assert_eq!(watch_health(Some(5), Some(now - 900), now, false), WatchHealth::Alive(900), "5분 주기는 17분까지 정상");
        assert_eq!(watch_health(Some(1), None, now, false), WatchHealth::Stale(None));
        // 켜진 직후·깨어난 직후에는 낡은 기록이어도 기다린다
        assert_eq!(watch_health(Some(1), Some(now - 3600), now, true), WatchHealth::Pending);
        assert_eq!(watch_health(Some(1), None, now, true), WatchHealth::Pending);
        // 시계가 뒤로 갔으면 방금 확인한 것으로 본다
        assert_eq!(watch_health(Some(1), Some(now + 30), now, false), WatchHealth::Alive(0));
    }

    #[test]
    fn watch_texts() {
        assert_eq!(ago_label(45), "45초");
        assert_eq!(ago_label(12 * 60 + 5), "12분");
        assert_eq!(ago_label(3 * 3600), "3시간");
        assert_eq!(ago_label(3 * 86400), "3일");
        assert_eq!(watch_window_text(&WatchHealth::Off, None), (String::new(), 1));
        let (t, tone) = watch_window_text(&WatchHealth::Alive(20), Some(1));
        assert_eq!(tone, 0);
        assert_eq!(t, "감시 작업 정상: 20초 전에 확인했습니다(1분마다 확인).");
        let (t, tone) = watch_window_text(&WatchHealth::Stale(Some(12)), Some(1));
        assert_eq!(tone, 2);
        assert!(t.contains("12분 동안 돌지 않았습니다") && t.contains("[적용]"), "{t}");
        assert_eq!(watch_window_text(&WatchHealth::Stale(None), Some(1)).1, 2);
        assert_eq!(watch_window_text(&WatchHealth::Pending, Some(1)).1, 1);
    }

    fn snap(state: &'static str, title: &str, detail: &str) -> Snapshot {
        Snapshot {
            installed: true,
            settings_found: true,
            reconnect_minutes: 60,
            heal_found: true,
            heal_minutes: 3,
            state,
            title: title.into(),
            detail: detail.into(),
            log: None,
            fast_startup: false,
            active_peers: Vec::new(),
            watch_interval: Some(1),
            watch_at: None,
            stopped_for: None,
            watch_settling: false,
            tray_on: true,
        }
    }

    #[test]
    fn tray_views() {
        let ok = WatchHealth::Alive(20);
        let v = tray_view(&snap("running", "연결됨", "my-pc · 100.101.102.103"), &ok);
        assert_eq!(v.tone, Tone::Ok);
        assert_eq!(v.state_line, "연결됨 · my-pc · 100.101.102.103");
        assert_eq!(v.watch_line, "감시 정상 · 20초 전 확인");
        assert_eq!(v.tooltip, "Tailscale Force Connect\n연결됨 · my-pc · 100.101.102.103\n감시 정상 · 20초 전 확인");

        // 연결돼 있어도 감시 작업이 멈췄으면 빨강: 이 아이콘이 알려야 할 가장 중요한 상태
        let v = tray_view(&snap("running", "연결됨", "my-pc"), &WatchHealth::Stale(Some(12)));
        assert_eq!(v.tone, Tone::Bad);
        assert_eq!(v.watch_line, "감시 작업이 12분 동안 돌지 않음");
        assert_eq!(tray_view(&snap("running", "연결됨", "my-pc"), &WatchHealth::Stale(None)).tone, Tone::Bad);
        // 켜진 직후·감시를 끈 경우는 연결 상태대로
        assert_eq!(tray_view(&snap("running", "연결됨", "my-pc"), &WatchHealth::Pending).tone, Tone::Ok);
        let v = tray_view(&snap("running", "연결됨", "my-pc"), &WatchHealth::Off);
        assert_eq!((v.tone, v.watch_line.as_str()), (Tone::Ok, "자동 재연결·멈춤 감시 꺼짐"));

        // 직접 끊음: 다시 연결까지 남은 시간
        let mut s = snap("stopped", "끊김", "직접 끊은 상태입니다.");
        s.stopped_for = Some(20);
        let v = tray_view(&s, &ok);
        assert_eq!((v.tone, v.state_line.as_str()), (Tone::Warn, "끊김 · 약 40분 뒤 다시 연결"));
        s.stopped_for = Some(61);
        assert_eq!(tray_view(&s, &ok).state_line, "끊김 · 곧 다시 연결");
        s.stopped_for = None;
        assert_eq!(tray_view(&s, &ok).state_line, "끊김 · 1시간 뒤 다시 연결");
        s.reconnect_minutes = 0;
        assert_eq!(tray_view(&s, &ok).state_line, "끊김 · 다음 부팅 때 연결");

        let v = tray_view(&snap("stuck", "연결 안 됨", "서버와 연결되지 않았습니다…"), &ok);
        assert_eq!((v.tone, v.state_line.as_str()), (Tone::Warn, "연결 안 됨 · 다른 기기에서는 offline"));
        assert_eq!(tray_view(&snap("stuck", "응답 없음", "…"), &ok).state_line, "응답 없음 · Tailscale이 답하지 않음");
        assert_eq!(tray_view(&snap("login", "로그인 필요", "…"), &ok).tone, Tone::Bad);
        assert_eq!(tray_view(&snap("missing", "Tailscale 없음", "…"), &ok).tone, Tone::Bad);
        let v = tray_view(&snap("unknown", "연결하는 중…", ""), &ok);
        assert_eq!((v.tone, v.state_line.as_str()), (Tone::Idle, "연결하는 중…"));

        let mut s = snap("running", "연결됨", "my-pc");
        s.installed = false;
        let v = tray_view(&s, &WatchHealth::Off);
        assert_eq!((v.tone, v.watch_line.as_str()), (Tone::Bad, "설치되지 않음 · 관리 창에서 설치"));
    }

    #[test]
    fn tooltip_limit() {
        assert_eq!(truncate_utf16("짧은 글", 127), "짧은 글");
        let long = "가".repeat(200);
        let t = truncate_utf16(&long, 127);
        assert_eq!(t.encode_utf16().count(), 127);
        assert!(t.ends_with('…'));
        // 서로게이트 쌍(UTF-16 두 칸)을 반으로 자르지 않는다
        let t = truncate_utf16(&"😀".repeat(100), 9);
        assert_eq!(t, "😀😀😀😀…");
    }

    #[test]
    fn status_icon_pixels() {
        for size in [16u32, 20, 24, 32] {
            for tone in [Tone::Ok, Tone::Warn, Tone::Bad, Tone::Idle] {
                let buf = render_status_icon(size, tone);
                let px = buf.as_slice();
                let at = |x: u32, y: u32| px[(y * size + x) as usize];
                assert_eq!(at(0, 0).a, 0, "{size}px 모서리는 투명");
                let center = at(size / 2, size / 2);
                assert_eq!([center.r, center.g, center.b, center.a], [tone_rgb(tone)[0], tone_rgb(tone)[1], tone_rgb(tone)[2], 255]);
                let edge = at(size / 2, 0);
                assert_eq!([edge.r, edge.g, edge.b, edge.a], [0x14, 0x20, 0x2A, 255], "{size}px 위쪽 가운데는 어두운 바탕");
            }
        }
        // 32px에서 가운데로부터 12.5px 떨어진 곳은 밝은 고리
        let buf = render_status_icon(32, Tone::Ok);
        let ring = buf.as_slice()[(16 * 32 + 28) as usize];
        assert_eq!([ring.r, ring.g, ring.b], [0xCF, 0xE3, 0xD8]);
    }

    #[test]
    fn remote_warning() {
        assert_eq!(remote_risk_text(false, &[]), None);
        let rdp = remote_risk_text(true, &[]).unwrap();
        assert!(rdp.contains("원격 데스크톱"), "{rdp}");
        let peers: Vec<String> = ["a", "b", "c", "d", "e"].iter().map(|s| s.to_string()).collect();
        let t = remote_risk_text(false, &peers).unwrap();
        assert!(t.contains("a, b, c 외 2대"), "{t}");
        assert!(!t.contains("원격 데스크톱 세션"), "{t}");
    }

    #[test]
    fn active_peers_from_status() {
        let v = serde_json::json!({
            "BackendState": "Running",
            "Peer": {
                "k1": { "DNSName": "laptop.tail1234.ts.net.", "HostName": "LAPTOP-1", "Active": true },
                "k2": { "DNSName": "", "HostName": "phone", "Active": true },
                "k3": { "DNSName": "server.tail1234.ts.net.", "Active": false },
                "k4": { "HostName": "old" }
            }
        });
        assert_eq!(active_peer_names(&v), vec!["laptop".to_string(), "phone".to_string()]);
        assert!(active_peer_names(&serde_json::json!({ "BackendState": "Stopped" })).is_empty());
    }

    #[test]
    fn timeout_kills_hung_process() {
        let started = Instant::now();
        let r = run_with_timeout(Command::new("cmd.exe").args(["/c", "ping -n 6 127.0.0.1 >nul"]), Duration::from_millis(300));
        assert!(matches!(r, Err(RunError::TimedOut)), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
        let (status, out) = run_with_timeout(Command::new("cmd.exe").args(["/c", "echo hello"]), Duration::from_secs(10)).unwrap();
        assert!(status.success());
        assert_eq!(String::from_utf8_lossy(&out).trim(), "hello");
        assert!(matches!(run_with_timeout(&mut Command::new("no-such-program-xyz.exe"), Duration::from_secs(1)), Err(RunError::Spawn)));
    }

    #[test]
    fn log_line_split_and_tone() {
        assert_eq!(split_log_line("2026-11-16 17:18:05  연결됨 (시도 1)"), ("11-16 17:18".into(), "연결됨 (시도 1)".into()));
        assert_eq!(split_log_line("형식이 다른 줄"), (String::new(), "형식이 다른 줄".into()));
        assert_eq!(log_tone("--- 시작 ---"), 1);
        assert_eq!(log_tone("연결됨 (시도 1)"), 0);
        assert_eq!(log_tone("복구: 직접 요청 -> 서비스를 다시 시작해 12 초 만에 연결됨"), 0);
        assert_eq!(log_tone("직접 끊은 지 60 분이 지나 다시 연결함"), 0);
        assert_eq!(log_tone("복구: 직접 요청 -> 서비스를 다시 시작함 (직접 끊긴 상태라 연결은 하지 않음)"), 0);
        assert_eq!(log_tone("감시: 끊긴 상태 확인 - 60 분 뒤에도 끊겨 있으면 다시 연결"), 0);
        assert_eq!(log_tone("감시: 멈춘 상태가 2 분째 이어짐 - tailscale status가 20 초 동안 응답 없음. 3 분이 되면 Tailscale 서비스를 다시 시작"), 2);
        assert_eq!(log_tone("실패: 10분 동안 5 회 시도했지만 연결되지 않음 (마지막 상태 'Stopped')"), 2);
        assert_eq!(log_tone("로그인이 필요함(키 만료 등) - 자동 연결 불가"), 2);
        assert_eq!(log_tone("tailscale up 시간 초과"), 2);
        assert_eq!(log_tone("tailscale up 종료 코드 1"), 2);
        assert_eq!(log_tone("감시: 멈춘 상태가 2 분째 이어짐 - 서비스 꺼짐(Stopped). 3 분이 되면 Tailscale 서비스를 다시 시작"), 2);
        assert_eq!(log_tone("감시: 멈춘 상태가 2 분째 이어짐 - 상태 'Starting'에서 넘어가지 않음. 3 분이 되면 Tailscale 서비스를 다시 시작"), 2);
        assert_eq!(log_tone("오류: 액세스가 거부되었습니다"), 2);
        assert_eq!(log_tone("복구: 3 분 동안 멈춤: tailscale status가 20 초 동안 응답 없음 -> 서비스를 다시 시작해 14 초 만에 연결됨"), 0);
        assert_eq!(log_tone("복구 실패: 직접 요청 -> 서비스를 다시 시작했지만 30 초 안에 연결되지 않음 (상태 'Stopped')"), 2);
        assert_eq!(log_tone("복구: 직접 요청 -> 서비스를 다시 시작했지만 로그인이 필요함(키 만료 등)"), 2);
    }

    #[test]
    fn log_tail_missing_and_partial() {
        let dir = std::env::temp_dir().join(format!("tfc-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_log_tail(&dir.join("none.log")), Some(Vec::new()));
        let p = dir.join("connect.log");
        let mut bytes = "\u{feff}2026-11-16 08:58:41  --- 시작 ---\r\n2026-11-16 08:58:48  연결됨 (시도 1)\r\n".as_bytes().to_vec();
        bytes.extend_from_slice(&[0xEC, 0x97]); // 쓰는 도중 잘린 글자
        std::fs::write(&p, &bytes).unwrap();
        let log = read_log_tail(&p).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(log.len(), 3);
        assert_eq!(log[0], ("11-16 08:58".into(), "--- 시작 ---".into()));
        assert_eq!(log[1], ("11-16 08:58".into(), "연결됨 (시도 1)".into()));
    }

    #[test]
    fn summarize_drops_copied_log() {
        let text = "설치 시작 2026
Tailscale: x
--- connect.log ---
2026-11-11 00:00:00  a
최종 상태: Running
완료 (종료 코드 0)";
        assert_eq!(summarize(text), "Tailscale: x
최종 상태: Running
완료 (종료 코드 0)");
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// PowerShell 스크립트를 UAC로 관리자 권한으로 실행한다. 창은 숨긴다.
/// wait이면 끝날 때까지 기다려 (종료 코드, -ResultFile로 받은 출력)을 돌려준다.
/// on_started는 UAC 승인 뒤 스크립트가 시작됐을 때 한 번 불린다.
fn run_elevated(script: &Path, extra_args: &str, wait: bool, on_started: impl FnOnce()) -> Result<(u32, String), String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError},
        System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
        UI::{
            Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::SW_HIDE,
        },
    };

    // 결과 파일은 관리자 스크립트가 '새로 만들 때만' 쓴다. 이름이 겹치지 않게 만든다.
    let result_file = wait.then(|| {
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        std::env::temp_dir().join(format!("tailscale-force-connect-gui-{}-{nanos}.txt", std::process::id()))
    });
    let mut params = format!(
        "-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\" {}",
        script.display(),
        extra_args
    );
    if let Some(rf) = &result_file {
        params.push_str(&format!(" -ResultFile \"{}\"", rf.display()));
    }

    let verb = wide("runas");
    let file = wide("powershell.exe");
    let params_w = wide(&params);
    let dir_w = wide(&script.parent().map(|p| p.display().to_string()).unwrap_or_default());

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params_w.as_ptr();
    info.lpDirectory = dir_w.as_ptr();
    info.nShow = SW_HIDE;

    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(if err == ERROR_CANCELLED {
            "관리자 권한 요청(UAC)이 거절되었습니다. 다시 눌러 [예]를 선택하세요.".into()
        } else {
            format!("PowerShell을 관리자 권한으로 실행하지 못했습니다 (오류 {err}).")
        });
    }
    on_started();

    let handle = info.hProcess;
    if !wait {
        if !handle.is_null() {
            unsafe { CloseHandle(handle) };
        }
        return Ok((0, String::new()));
    }
    let mut code: u32 = 1;
    if !handle.is_null() {
        unsafe {
            WaitForSingleObject(handle, INFINITE);
            GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
        }
    }
    let text = result_file
        .as_ref()
        .and_then(|rf| {
            let t = std::fs::read_to_string(rf).ok();
            let _ = std::fs::remove_file(rf);
            t
        })
        .map(|t| t.trim_start_matches('\u{feff}').to_string())
        .unwrap_or_default();
    Ok((code, text))
}
