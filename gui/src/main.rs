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
        atomic::{AtomicBool, AtomicI64, Ordering},
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
    /// (시각 "MM-DD HH:MM", 내용)
    log: Vec<(String, String)>,
    fast_startup: bool,
    active_peers: Vec<String>,
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
        LaunchMode::Window => {}
    }

    let ui = AppWindow::new()?;
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    ui.set_notify_on(notifications_enabled());
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
                checking.store(false, Ordering::SeqCst);
                let _ = weak.upgrade_in_event_loop(move |ui| match latest {
                    Some(v) if is_newer(&v, env!("CARGO_PKG_VERSION")) => {
                        ui.set_update_status(format!("새 버전 v{v}").into());
                        ui.set_update_version(v.into());
                        ui.set_update_available(true);
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
    {
        let weak = ui.as_weak();
        ui.on_update(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(script) = sibling("update.ps1") else {
                show_result(&ui, false, "업데이트 스크립트를 찾지 못했습니다", "update.ps1이 이 실행 파일과 같은 폴더에 있어야 합니다. 한 줄 설치로 다시 설치하세요.");
                return;
            };
            ui.set_result_visible(false);
            ui.set_busy(true);
            ui.set_busy_text("관리자 권한 창에서 [예]를 누르세요. 업데이트가 시작되면 이 창은 닫히고, 끝나면 새 버전으로 다시 열립니다.".into());
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
        });
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
    let lines: Vec<LogLine> = s
        .log
        .iter()
        .map(|(time, text)| LogLine { time: time.into(), text: text.into(), tone: log_tone(text) })
        .collect();
    let current = ui.get_log_lines();
    if current.row_count() != lines.len() || current.iter().zip(&lines).any(|(a, b)| a != *b) {
        ui.set_log_lines(ModelRc::new(VecModel::from(lines)));
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

    Snapshot {
        installed,
        settings_found,
        reconnect_minutes,
        heal_found,
        heal_minutes,
        state,
        title,
        detail,
        log: read_log_tail(&data.join("connect.log")),
        fast_startup: fast_startup_enabled(),
        active_peers,
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

fn read_log_tail(path: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let lines: Vec<&str> = text.trim_start_matches('\u{feff}').lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(LOG_LINES);
    lines[start..].iter().map(|l| split_log_line(l.trim_end())).collect()
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
/// connect.ps1이 남기는 실패·오류·멈춤 문구(…실패, 오류:, …하지 못함, 로그인이 필요함, 시간 초과, 응답 없음, …되지 않음)를 경고로 본다.
/// "원인 -> 결과" 줄은 결과만 본다(원인에 '응답 없음'이 있어도 다시 시작해 연결됐으면 보통).
fn log_tone(text: &str) -> i32 {
    let outcome = text.rsplit("->").next().unwrap_or(text);
    if text.starts_with("---") {
        1
    } else if ["실패", "오류", "못함", "필요함", "초과", "응답 없음", "되지 않음"].iter().any(|w| outcome.contains(w)) {
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

/// 사용자별 알림 기록을 두는 폴더 (%LOCALAPPDATA%\Tailscale Force Connect)
fn user_state_dir() -> PathBuf {
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

/// 새 버전이 있고 그 버전을 아직 알리지 않았으면 Windows 알림을 띄운다. 같은 버전은 한 번만 알린다.
fn notify_if_update_available() {
    if !notifications_enabled() {
        return; // 관리 창에서 알림을 껐다(새 버전은 관리 창을 열면 보인다)
    }
    let Some(latest) = latest_release_version() else { return };
    if !is_newer(&latest, env!("CARGO_PKG_VERSION")) {
        return;
    }
    let state_dir = user_state_dir();
    let marker = state_dir.join("notified-version.txt");
    if std::fs::read_to_string(&marker).map(|s| s.trim() == latest).unwrap_or(false) {
        return;
    }
    if show_toast(
        &format!("새 버전 v{latest}"),
        "Tailscale Force Connect를 업데이트할 수 있습니다. 누르면 관리 창이 열리고 바로 업데이트할 수 있습니다.",
    ) {
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
        assert_eq!(log_tone("오류: 액세스가 거부되었습니다"), 2);
        assert_eq!(log_tone("복구: 3 분 동안 멈춤: tailscale status가 20 초 동안 응답 없음 -> 서비스를 다시 시작해 14 초 만에 연결됨"), 0);
        assert_eq!(log_tone("복구 실패: 직접 요청 -> 서비스를 다시 시작했지만 30 초 안에 연결되지 않음 (상태 'Stopped')"), 2);
        assert_eq!(log_tone("복구: 직접 요청 -> 서비스를 다시 시작했지만 로그인이 필요함(키 만료 등)"), 2);
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
