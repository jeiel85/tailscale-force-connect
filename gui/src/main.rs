// tailscale-force-connect 관리 창.
// 상태 표시는 일반 권한으로 읽을 수 있는 것만 쓴다(tailscale status, settings.json, connect.log).
// 설치·설정·제거는 실행 파일과 같은 폴더의 PowerShell 스크립트를 UAC로 관리자 권한으로 실행한다.
#![windows_subsystem = "windows"]

use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

slint::include_modules!();

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DATA_DIR: &str = r"C:\ProgramData\TailscaleAutoConnect";
const LOG_LINES: usize = 9;

/// 콤보 상자 항목 순서와 같다: (install.ps1 -ReconnectAfter 값, 분)
const RECONNECT: [(&str, i64); 5] = [("0", 0), ("30m", 30), ("1h", 60), ("2h", 120), ("4h", 240)];

struct Snapshot {
    installed: bool,
    settings_found: bool,
    reconnect_minutes: i64,
    state: &'static str,
    title: String,
    detail: String,
    log: Vec<String>,
    fast_startup: bool,
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    ui.set_version(env!("CARGO_PKG_VERSION").into());

    let refreshing = Arc::new(AtomicBool::new(false));
    let index_initialized = Arc::new(AtomicBool::new(false));

    let refresh = {
        let weak = ui.as_weak();
        let refreshing = refreshing.clone();
        let index_initialized = index_initialized.clone();
        move || {
            if refreshing.swap(true, Ordering::SeqCst) {
                return; // 이전 조회가 아직 끝나지 않았다
            }
            let weak = weak.clone();
            let refreshing = refreshing.clone();
            let index_initialized = index_initialized.clone();
            std::thread::spawn(move || {
                let snap = collect_snapshot();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    apply_snapshot(&ui, &snap, &index_initialized);
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
    let run_install = {
        let weak = ui.as_weak();
        let refresh = refresh.clone();
        Arc::new(move |test: bool, busy_text: &str, title_ok: &str| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(script) = sibling("install.ps1") else {
                show_result(&ui, false, "설치 스크립트를 찾지 못했습니다", "install.ps1이 이 실행 파일과 같은 폴더에 있어야 합니다. 받은 zip을 모두 푼 뒤 실행하세요.");
                return;
            };
            let idx = (ui.get_reconnect_index().max(0) as usize).min(RECONNECT.len() - 1);
            let mut args = format!("-ReconnectAfter {}", RECONNECT[idx].0);
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

    {
        let run = run_install.clone();
        let weak = ui.as_weak();
        ui.on_install(move || {
            let test = weak.upgrade().map(|ui| ui.get_run_test()).unwrap_or(true);
            let text = if test { "설치하고 동작을 확인하는 중입니다. 연결이 몇 초 끊깁니다." } else { "설치하는 중입니다." };
            run(test, text, "설치했습니다");
        });
    }
    {
        let run = run_install.clone();
        ui.on_apply_reconnect(move || run(false, "자동 재연결 설정을 바꾸는 중입니다.", "설정을 바꿨습니다"));
    }
    {
        let run = run_install.clone();
        ui.on_verify(move || run(true, "연결을 몇 초 끊었다가 다시 연결되는지 확인하는 중입니다.", "동작을 확인했습니다"));
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

    ui.on_open_logs(|| {
        let target = if Path::new(DATA_DIR).exists() { PathBuf::from(DATA_DIR) } else { exe_dir() };
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

fn apply_snapshot(ui: &AppWindow, s: &Snapshot, index_initialized: &AtomicBool) {
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
        (format!("{} 뒤 다시 연결", minutes_label(s.reconnect_minutes)), true)
    };
    ui.set_reconnect_text(reconnect_text.into());
    ui.set_reconnect_on(reconnect_on);

    // 콤보 상자는 처음 한 번만 현재 설정으로 맞춘다(사용자가 고르는 중인 값을 덮어쓰지 않도록).
    if s.installed && s.settings_found && !index_initialized.swap(true, Ordering::SeqCst) {
        if let Some(i) = RECONNECT.iter().position(|(_, m)| *m == s.reconnect_minutes) {
            ui.set_reconnect_index(i as i32);
        }
    }

    let lines: Vec<SharedString> = s.log.iter().map(|l| SharedString::from(l.as_str())).collect();
    ui.set_log_lines(ModelRc::new(VecModel::from(lines)));
}

fn minutes_label(m: i64) -> String {
    match (m / 60, m % 60) {
        (0, mm) => format!("{mm}분"),
        (h, 0) => format!("{h}시간"),
        (h, mm) => format!("{h}시간 {mm}분"),
    }
}

fn collect_snapshot() -> Snapshot {
    let data = Path::new(DATA_DIR);
    let installed = data.join("connect.ps1").exists();

    let (settings_found, reconnect_minutes) = std::fs::read_to_string(data.join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t.trim_start_matches('\u{feff}')).ok())
        .and_then(|v| v.get("reconnectMinutes").and_then(|m| m.as_i64()))
        .map(|m| (true, m))
        .unwrap_or((false, 0));

    let (state, title, detail) = match find_tailscale() {
        None => ("missing", "Tailscale 없음".to_string(), "Tailscale을 먼저 설치하고 로그인하세요.".to_string()),
        Some(ts) => match tailscale_status(&ts) {
            None => ("unknown", "상태를 알 수 없음".to_string(), "Tailscale 서비스가 응답하지 않습니다.".to_string()),
            Some(v) => describe_state(&v, installed, reconnect_minutes),
        },
    };

    Snapshot {
        installed,
        settings_found,
        reconnect_minutes,
        state,
        title,
        detail,
        log: read_log_tail(&data.join("connect.log")),
        fast_startup: fast_startup_enabled(),
    }
}

fn describe_state(v: &serde_json::Value, installed: bool, reconnect_minutes: i64) -> (&'static str, String, String) {
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
    let modified = std::fs::metadata(Path::new(DATA_DIR).join("stopped-since.txt")).ok()?.modified().ok()?;
    let elapsed = SystemTime::now().duration_since(modified).ok()?;
    Some((elapsed.as_secs() / 60) as i64)
}

fn read_log_tail(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let lines: Vec<&str> = text.trim_start_matches('\u{feff}').lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(LOG_LINES);
    lines[start..]
        .iter()
        .map(|l| {
            // "2026-11-11 23:50:28  내용" → "11-11 23:50  내용"
            let l = l.trim_end();
            if l.len() > 19 && l.as_bytes()[4] == b'-' && l.is_char_boundary(5) && l.is_char_boundary(16) && l.is_char_boundary(19) {
                format!("{}{}", &l[5..16], &l[19..])
            } else {
                l.to_string()
            }
        })
        .collect()
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
        let img = expand_env(img.trim());
        let daemon = match img.strip_prefix('"') {
            Some(rest) => rest.split('"').next().unwrap_or("").to_string(),
            None => img.split_whitespace().next().unwrap_or("").to_string(),
        };
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

fn tailscale_status(ts: &Path) -> Option<serde_json::Value> {
    let out = Command::new(ts)
        .args(["status", "--json"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    serde_json::from_slice(&out.stdout).ok()
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
