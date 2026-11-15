use crate::supervisor::{Paths, RuntimeStatus, Supervisor};
use crate::ui_health::{UiAction, UiHealth};
use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};
use tauri::{
    AppHandle, Manager, RunEvent, WindowEvent,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

struct AppState {
    runtime: Mutex<Supervisor>,
    native: crate::native_control::NativeControl,
    collectors: Mutex<crate::collectors::Collectors>,
    snapshot: Mutex<RuntimeStatus>,
    ui_health: Mutex<UiHealth>,
    busy: AtomicBool,
    quitting: AtomicBool,
    allow_exit: AtomicBool,
}
impl AppState {
    fn worker_lifecycle(&self) -> (bool, bool) {
        (
            self.quitting.load(Ordering::SeqCst),
            self.allow_exit.load(Ordering::SeqCst),
        )
    }
}
fn show_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
fn publish(app: &AppHandle, status: RuntimeStatus) {
    if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
        *snapshot = status;
    }
}
fn publish_error(app: &AppHandle, error: String) {
    if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
        snapshot.error = Some(error);
    }
    show_settings(app);
}
#[tauri::command]
async fn collector_status(
    app: AppHandle,
) -> Result<std::collections::BTreeMap<String, crate::collectors::Status>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let transport = state
            .runtime
            .try_lock()
            .ok()
            .and_then(|runtime| runtime.collector_transport().ok());
        let mut collectors = state.collectors.lock().map_err(|_| "采集状态已锁定")?;
        if let Some(transport) = transport {
            collectors.refresh(&transport)?;
        }
        collectors.statuses()
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn collector_action(
    app: AppHandle,
    provider: String,
    action: String,
    username: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let transport = state
            .runtime
            .lock()
            .map_err(|_| "运行状态已锁定")?
            .collector_transport()
            .ok();
        let mut collectors = state.collectors.lock().map_err(|_| "采集状态已锁定")?;
        collectors.action(transport.as_ref(), &provider, &action, username.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn chatlog_action(
    app: AppHandle,
    action: String,
    source: Option<String>,
    key: Option<String>,
) -> Result<RuntimeStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut runtime = state.runtime.lock().map_err(|_| "运行状态已锁定")?;
        let result = runtime.chatlog_action(
            &action,
            source.as_deref().map(std::path::Path::new),
            key.as_deref(),
        );
        publish(&app, runtime.status());
        result
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn native_status(
    app: AppHandle,
) -> Result<std::collections::BTreeMap<String, crate::native_control::Status>, String> {
    app.state::<AppState>().native.status()
}
#[tauri::command]
async fn native_action(app: AppHandle, provider: String, action: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let transport = {
            let runtime = state.runtime.lock().map_err(|_| "运行状态已锁定")?;
            if action == "disable" {
                runtime.collector_transport()?
            } else {
                runtime.native_transport()?
            }
        };
        state.native.action(&transport, &provider, &action)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn runtime_status(app: AppHandle) -> Result<RuntimeStatus, String> {
    if let Ok(mut health) = app.state::<AppState>().ui_health.lock() {
        health.heartbeat(Instant::now());
    }
    app.state::<AppState>()
        .snapshot
        .lock()
        .map(|s| s.clone())
        .map_err(|_| "状态暂不可用".into())
}
#[tauri::command]
async fn runtime_action(app: AppHandle, action: String) -> Result<RuntimeStatus, String> {
    if !["start", "pause", "resume", "stop"].contains(&action.as_str()) {
        return Err("未知操作".into());
    }
    if app.state::<AppState>().busy.swap(true, Ordering::SeqCst) {
        return Err("上一项操作仍在进行，请稍候".into());
    }
    if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
        if action == "start" {
            snapshot.phase = "starting".into();
        } else if action == "stop" {
            snapshot.phase = "stopping".into();
        }
    }
    let copy = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = copy.state::<AppState>();
        let mut runtime = state.runtime.lock().map_err(|_| "运行状态已锁定")?;
        if ["pause", "stop"].contains(&action.as_str()) {
            state
                .collectors
                .lock()
                .map_err(|_| "采集状态已锁定")?
                .stop()?;
        }
        let result = match action.as_str() {
            "start" => runtime.start(),
            "pause" => runtime.pause(),
            "resume" => runtime.resume(),
            "stop" => runtime.stop(),
            _ => unreachable!(),
        };
        publish(&copy, runtime.status());
        result
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|value| value);
    app.state::<AppState>().busy.store(false, Ordering::SeqCst);
    if let Err(error) = &result {
        publish_error(&app, error.clone());
    }
    result
}
#[tauri::command]
async fn local_account(app: AppHandle) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppState>()
            .runtime
            .lock()
            .map_err(|_| "运行状态已锁定")?
            .account()
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn reset_account(
    app: AppHandle,
    email: String,
    password: String,
    reset_totp: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppState>()
            .runtime
            .lock()
            .map_err(|_| "运行状态已锁定")?
            .reset_account(&email, &password, reset_totp)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn create_account(app: AppHandle, email: String, password: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppState>()
            .runtime
            .lock()
            .map_err(|_| "运行状态已锁定")?
            .bootstrap(&email, &password)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn open_monica(app: AppHandle) -> Result<(), String> {
    let url = app
        .state::<AppState>()
        .snapshot
        .lock()
        .map_err(|_| "状态暂不可用")?
        .url
        .clone()
        .ok_or("服务尚未就绪")?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn open_logs(app: AppHandle) -> Result<(), String> {
    let path = app
        .state::<AppState>()
        .runtime
        .lock()
        .map_err(|_| "状态暂不可用")?
        .logs()
        .to_path_buf();
    app.opener()
        .open_path(path.display().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn autostart_enabled(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}
#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    if enabled {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    }
    .map_err(|e| e.to_string())
}
fn begin_quit(app: &AppHandle) {
    if app
        .state::<AppState>()
        .quitting
        .swap(true, Ordering::SeqCst)
    {
        return;
    }
    if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
        snapshot.phase = "stopping".into();
    }
    let app = app.clone();
    thread::spawn(move || {
        let state = app.state::<AppState>();
        let result = (|| {
            state
                .collectors
                .lock()
                .map_err(|_| "采集状态已锁定".to_string())?
                .stop()?;
            state
                .runtime
                .lock()
                .map_err(|_| "运行状态已锁定".to_string())?
                .stop()
        })();
        match result {
            Ok(status) => {
                publish(&app, status);
                app.state::<AppState>()
                    .allow_exit
                    .store(true, Ordering::SeqCst);
                app.exit(0);
            }
            Err(error) => {
                // A failed quit leaves the application alive. Publish its real
                // state so the form does not remain disabled as "stopping".
                if let Ok(runtime) = state.runtime.lock() {
                    publish(&app, runtime.status());
                } else if let Ok(mut snapshot) = state.snapshot.lock() {
                    snapshot.phase = "failed".into();
                }
                state.quitting.store(false, Ordering::SeqCst);
                publish_error(&app, error);
            }
        }
    });
}
#[tauri::command]
fn quit_app(app: AppHandle) {
    begin_quit(&app);
}
fn install_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let open = MenuItem::with_id(app, "open", "打开 Monica Rust", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置与运行状态…", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "暂停后台任务", true, None::<&str>)?;
    let resume = MenuItem::with_id(app, "resume", "恢复后台任务", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Monica Rust", true, Some("Cmd+Q"))?;
    let menu = Menu::with_items(app, &[&open, &settings, &pause, &resume, &quit])?;
    let pixels: Vec<u8> = (0..22 * 22)
        .flat_map(|index| {
            let x = index % 22;
            let y = index / 22;
            let ink = (4..18).contains(&y)
                && ((4..7).contains(&x)
                    || (15..18).contains(&x)
                    || ((7..15).contains(&x) && y >= x.min(21 - x) && y <= x.min(21 - x) + 3));
            [0, 0, 0, if ink { 255 } else { 0 }]
        })
        .collect();
    TrayIconBuilder::with_id("monica-rust")
        .icon(tauri::image::Image::new_owned(pixels, 22, 22))
        .icon_as_template(true)
        .tooltip("Monica Rust")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => {
                if let Err(error) = open_monica(app.clone()) {
                    publish_error(app, error);
                }
            }
            "settings" => show_settings(app),
            "quit" => begin_quit(app),
            action @ ("pause" | "resume") => {
                let app = app.clone();
                let action = action.to_string();
                tauri::async_runtime::spawn(async move {
                    let _ = runtime_action(app, action).await;
                });
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}
// This function runs as one native main-thread task. Native getters execute
// inline there, and a heartbeat/focus event accepted before the task is reflected
// in its final health check; no background Reload decision is carried across IPC.
fn check_ui(app: &AppHandle) -> UiAction {
    let Some(state) = app.try_state::<AppState>() else {
        return UiAction::Stop;
    };
    let quitting = state.quitting.load(Ordering::SeqCst);
    let exit_allowed = state.allow_exit.load(Ordering::SeqCst);
    let window = app.get_webview_window("settings");
    let active = !quitting
        && window.as_ref().is_some_and(|window| {
            window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false)
        });
    // Release the health lock before making a native call. The final decision
    // and reload still execute in the same event-loop turn.
    let action = state
        .ui_health
        .lock()
        .map(|mut health| {
            health.check(
                Instant::now(),
                SystemTime::now(),
                active,
                quitting,
                exit_allowed,
            )
        })
        .unwrap_or(if exit_allowed {
            UiAction::Stop
        } else {
            UiAction::Wait
        });
    if action == UiAction::Reload
        && !state.quitting.load(Ordering::SeqCst)
        && !state.allow_exit.load(Ordering::SeqCst)
        && let Some(window) = window
    {
        // Native reload works even when WebKit's content process has died.
        if let Err(error) = window.reload() {
            eprintln!("无法恢复 Monica Rust 设置界面：{error}");
        }
    }
    action
}

fn monitor_ui(app: AppHandle) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(2));
            if app
                .try_state::<AppState>()
                .is_none_or(|state| state.allow_exit.load(Ordering::SeqCst))
            {
                return;
            }
            let task_app = app.clone();
            let (done, result) = mpsc::channel();
            if app
                .run_on_main_thread(move || {
                    let _ = done.send(check_ui(&task_app));
                })
                .is_err()
            {
                continue;
            }
            // Only one check may be queued. Wait for that same task if the native
            // loop is busy instead of accumulating more tasks every two seconds.
            loop {
                match result.recv_timeout(Duration::from_secs(2)) {
                    Ok(UiAction::Stop) => return,
                    Ok(_) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {
                        if app
                            .try_state::<AppState>()
                            .is_none_or(|state| state.allow_exit.load(Ordering::SeqCst))
                        {
                            return;
                        }
                    }
                }
            }
        }
    });
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_settings(app)
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .macos_launcher(tauri_plugin_autostart::MacosLauncher::LaunchAgent)
                .arg("--autostart")
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            collector_status,
            collector_action,
            chatlog_action,
            native_status,
            native_action,
            runtime_status,
            runtime_action,
            create_account,
            local_account,
            reset_account,
            open_monica,
            open_logs,
            autostart_enabled,
            set_autostart,
            quit_app
        ])
        .on_window_event(|window, event| {
            if matches!(event, WindowEvent::Focused(_))
                && window.label() == "settings"
                && let Some(state) = window.try_state::<AppState>()
                && let Ok(mut health) = state.ui_health.lock()
            {
                health.focus_changed(Instant::now());
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let home = app.path().home_dir()?;
            let bundle = if cfg!(debug_assertions) {
                match std::env::var_os("MONICA_RUST_RUNTIME") {
                    Some(path) => PathBuf::from(path),
                    None => app.path().resource_dir()?.join("runtime"),
                }
            } else {
                app.path().resource_dir()?.join("runtime")
            };
            let data = if cfg!(debug_assertions) {
                std::env::var_os("MONICA_RUST_TEST_DATA_DIR")
                    .map(PathBuf::from)
                    .unwrap_or(home.join("Library/Application Support/Monica Rust"))
            } else {
                home.join("Library/Application Support/Monica Rust")
            };
            let logs = if cfg!(debug_assertions)
                && std::env::var_os("MONICA_RUST_TEST_DATA_DIR").is_some()
            {
                data.join("logs")
            } else {
                home.join("Library/Logs/Monica Rust")
            };
            let runtime = Supervisor::new(Paths {
                data: data.clone(),
                logs,
                postgres_bin: bundle.join("postgres/bin"),
                server: bundle.join("monica-server"),
                web: bundle.join("web"),
            })
            .map_err(std::io::Error::other)?;
            let mut snapshot = runtime.status();
            snapshot.phase = "starting".into();
            app.manage(AppState {
                runtime: Mutex::new(runtime),
                collectors: Mutex::new(
                    crate::collectors::Collectors::new(
                        data.join("collector-data"),
                        bundle.join("collectors/run-collector"),
                    )
                    .map_err(std::io::Error::other)?,
                ),
                native: crate::native_control::NativeControl::new(data)
                    .map_err(std::io::Error::other)?,
                snapshot: Mutex::new(snapshot),
                ui_health: Mutex::new(UiHealth::new(Instant::now(), SystemTime::now())),
                busy: AtomicBool::new(false),
                quitting: AtomicBool::new(false),
                allow_exit: AtomicBool::new(false),
            });
            install_tray(app.handle())?;
            monitor_ui(app.handle().clone());
            for provider in crate::native_control::PROVIDERS {
                let handle = app.handle().clone();
                thread::spawn(move || {
                    crate::background::run(
                        Duration::from_secs(15),
                        || handle.state::<AppState>().worker_lifecycle(),
                        || {
                            let state = handle.state::<AppState>();
                            if !state.native.enabled(provider) {
                                return;
                            }
                            let transport = state
                                .runtime
                                .try_lock()
                                .ok()
                                .and_then(|r| r.native_transport().ok());
                            if let Some(transport) = transport {
                                let _ = state.native.sync(&transport, provider, false);
                            }
                        },
                    );
                });
            }

            #[cfg(unix)]
            {
                let mut signals = signal_hook::iterator::Signals::new([
                    signal_hook::consts::SIGINT,
                    signal_hook::consts::SIGTERM,
                ])?;
                let signal_app = app.handle().clone();
                thread::spawn(move || {
                    for _ in signals.forever() {
                        begin_quit(&signal_app);
                    }
                });
            }
            if std::env::args().any(|arg| arg == "--autostart")
                && let Some(window) = app.get_webview_window("settings")
            {
                let _ = window.hide();
            }
            let handle = app.handle().clone();
            thread::spawn(move || {
                {
                    let state = handle.state::<AppState>();
                    state.busy.store(true, Ordering::SeqCst);
                    if let Ok(mut runtime) = state.runtime.lock() {
                        let _ = runtime.start();
                        publish(&handle, runtime.status());
                    }
                    state.busy.store(false, Ordering::SeqCst);
                }
                crate::background::run(
                    Duration::from_secs(2),
                    || handle.state::<AppState>().worker_lifecycle(),
                    || {
                        let state = handle.state::<AppState>();
                        if let Ok(mut runtime) = state.runtime.try_lock() {
                            runtime.tick();
                            publish(&handle, runtime.status());
                        }
                    },
                );
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("无法启动 Monica Rust 设置界面");
    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = &event {
            show_settings(app);
        }
        if let RunEvent::ExitRequested { api, .. } = event
            && !app.state::<AppState>().allow_exit.load(Ordering::SeqCst)
        {
            api.prevent_exit();
            begin_quit(app);
        }
    });
}
