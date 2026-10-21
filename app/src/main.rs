use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter as _, Manager as _, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::DialogExt as _;
use tauri_plugin_notification::NotificationExt as _;
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};
use trae_acp_gateway::{
    CURRENT_VERSION, Config as GatewayConfig, canonicalize_workdir, default_workdir,
    ensure_workdir, is_legacy_workdir, serve,
};

mod update;

const LOG_LEVELS: &[&str] = &["error", "warn", "info", "debug", "trace"];

fn default_log_level() -> String {
    "info".into()
}

fn normalize_log_level(level: &str) -> String {
    let l = level.trim().to_lowercase();
    if LOG_LEVELS.contains(&l.as_str()) {
        l
    } else {
        "info".into()
    }
}

fn env_filter_for_level(level: &str) -> EnvFilter {
    let level = normalize_log_level(level);
    EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(format!(
            "trae_acp_gateway={level},trae_acp_gateway_app={level}"
        ))
    })
}

type LogFilterReload = reload::Handle<EnvFilter, tracing_subscriber::Registry>;

fn apply_log_level(reload: &LogFilterReload, level: &str) -> Result<(), String> {
    reload
        .reload(env_filter_for_level(level))
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct AppConfig {
    port: u16,
    workdir: String,
    trae_cmd: String,
    trae_args: Vec<String>,
    sandbox: bool,
    #[serde(default = "default_log_level")]
    log_level: String,
    /// Legacy field; read from old configs only.
    #[serde(default, skip_serializing)]
    #[allow(dead_code)]
    debug: bool,
    autostart: bool,
    auto_check_update: bool,
}

impl AppConfig {
    fn defaults() -> Self {
        Self {
            port: 60111,
            workdir: default_workdir(),
            trae_cmd: "traecli".into(),
            trae_args: vec!["acp".into(), "serve".into()],
            sandbox: true,
            log_level: default_log_level(),
            debug: false,
            autostart: false,
            auto_check_update: false,
        }
    }

    fn normalize_log_level(&mut self) {
        self.log_level = normalize_log_level(&self.log_level);
    }
}

fn load_config_from_file(path: &std::path::Path) -> AppConfig {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return AppConfig::defaults(),
    };
    let mut cfg: AppConfig = serde_json::from_str(&raw).unwrap_or_else(|_| AppConfig::defaults());
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
        if v.get("log_level").is_none() && v.get("debug").and_then(|d| d.as_bool()) == Some(true) {
            cfg.log_level = "debug".into();
        }
    }
    cfg.normalize_log_level();
    cfg
}

fn normalize_workdir(cfg: &mut AppConfig) {
    if is_legacy_workdir(&cfg.workdir) {
        cfg.workdir = default_workdir();
    }
    if let Err(e) = ensure_workdir(&cfg.workdir) {
        tracing::warn!("failed to create workdir `{}`: {e}", cfg.workdir);
    }
}

fn persist_config(app: &AppHandle, cfg: &AppConfig) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("config.json"), json).map_err(|e| e.to_string())
}

fn to_gateway(cfg: &AppConfig) -> GatewayConfig {
    let verbose = matches!(cfg.log_level.as_str(), "debug" | "trace");
    GatewayConfig {
        port: cfg.port,
        workdir: cfg.workdir.clone(),
        trae_cmd: cfg.trae_cmd.clone(),
        trae_args: cfg.trae_args.clone(),
        sandbox: cfg.sandbox,
        debug: verbose,
        auto_check_update: false,
        no_auto_check_update: false,
    }
}

const LOG_CAP: usize = 500;

struct LogBuffer {
    partial: Mutex<String>,
    lines: Mutex<VecDeque<String>>,
}

impl LogBuffer {
    fn new() -> Self {
        Self {
            partial: Mutex::new(String::new()),
            lines: Mutex::new(VecDeque::new()),
        }
    }

    fn push(&self, buf: &[u8]) {
        let mut partial = self.partial.lock().unwrap();
        partial.push_str(&String::from_utf8_lossy(buf));
        let mut lines = self.lines.lock().unwrap();
        while let Some(pos) = partial.find('\n') {
            let line: String = partial.drain(..=pos).collect();
            let line = line.trim_end().to_string();
            if !line.is_empty() {
                if lines.len() == LOG_CAP {
                    lines.pop_front();
                }
                lines.push_back(line);
            }
        }
    }

    fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().iter().cloned().collect()
    }
}

struct AppState {
    cfg: Mutex<AppConfig>,
    /// Actual TCP port when gateway is running (`None` when stopped).
    listen_port: Mutex<Option<u16>>,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    stop_requested: Mutex<bool>,
    logs: Arc<LogBuffer>,
    log_reload: LogFilterReload,
    tray_status: Mutex<Option<MenuItem<tauri::Wry>>>,
    tray_toggle: Mutex<Option<MenuItem<tauri::Wry>>>,
}

fn notify_error(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}

fn notify_update_available(app: &AppHandle, latest: &str) {
    let _ = app
        .notification()
        .builder()
        .title("发现新版本")
        .body(format!("v{latest} 可用，请在设置中下载安装"))
        .show();
}

fn notify_port_fallback(app: &AppHandle, preferred: u16, actual: u16) {
    let _ = app
        .notification()
        .builder()
        .title("Gateway 端口已切换")
        .body(format!(
            "端口 {preferred} 被占用，Gateway 已在 localhost:{actual} 运行"
        ))
        .show();
}

fn is_running(state: &AppState) -> bool {
    state
        .handle
        .lock()
        .unwrap()
        .as_ref()
        .map(|h| !h.is_finished())
        .unwrap_or(false)
}

fn stop(state: &AppState) -> bool {
    *state.stop_requested.lock().unwrap() = true;
    *state.listen_port.lock().unwrap() = None;
    state
        .handle
        .lock()
        .unwrap()
        .take()
        .map(|h| {
            h.abort();
            true
        })
        .unwrap_or(false)
}

fn watch_gateway(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let state = app.state::<AppState>();
            if *state.stop_requested.lock().unwrap() {
                break;
            }
            let crashed = state
                .handle
                .lock()
                .unwrap()
                .as_ref()
                .map(|h| h.is_finished())
                .unwrap_or(false);
            if crashed {
                *state.handle.lock().unwrap() = None;
                *state.listen_port.lock().unwrap() = None;
                update_tray(&app);
                notify_error(&app, "Gateway 已停止", "服务意外退出，请查看日志");
                tracing::error!("gateway exited unexpectedly");
                break;
            }
        }
    });
}

async fn start(app: &AppHandle, state: &AppState) -> Result<(), String> {
    if is_running(state) {
        return Ok(());
    }
    *state.stop_requested.lock().unwrap() = false;
    let preferred = state.cfg.lock().unwrap().port;
    let mut gcfg = to_gateway(&state.cfg.lock().unwrap());
    canonicalize_workdir(&mut gcfg).map_err(|e| e.to_string())?;
    let (handle, actual_port) = serve(Arc::new(gcfg)).await.map_err(|e| e.to_string())?;
    *state.listen_port.lock().unwrap() = Some(actual_port);
    *state.handle.lock().unwrap() = Some(handle);
    update_tray(app);
    if actual_port != preferred {
        notify_port_fallback(app, preferred, actual_port);
    }
    watch_gateway(app.clone());
    Ok(())
}

fn update_tray(app: &AppHandle) {
    let state = app.state::<AppState>();
    let running = is_running(&state);
    let display_port = if running {
        state
            .listen_port
            .lock()
            .unwrap()
            .unwrap_or_else(|| state.cfg.lock().unwrap().port)
    } else {
        0
    };
    if let Some(item) = state.tray_status.lock().unwrap().as_ref() {
        let _ = item.set_text(if running {
            format!("Gateway: Running :{display_port}")
        } else {
            "Gateway: Stopped".into()
        });
    }
    if let Some(item) = state.tray_toggle.lock().unwrap().as_ref() {
        let _ = item.set_text(if running {
            "Stop Gateway"
        } else {
            "Start Gateway"
        });
    }
}

fn open_window(app: &AppHandle, label: &str, title: &str, w: f64, h: f64) {
    if let Some(win) = app.get_webview_window(label) {
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::App(format!("index.html#/{label}").into()),
    )
    .title(title)
    .inner_size(w, h)
    .build();
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> AppConfig {
    state.cfg.lock().unwrap().clone()
}

#[tauri::command]
fn get_config_path(app: AppHandle) -> Result<String, String> {
    app.path()
        .app_data_dir()
        .map(|d| d.join("config.json").display().to_string())
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn save_config(
    app: AppHandle,
    state: State<'_, AppState>,
    mut cfg: AppConfig,
) -> Result<(), String> {
    normalize_workdir(&mut cfg);
    cfg.normalize_log_level();
    apply_log_level(&state.log_reload, &cfg.log_level)?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("config.json"), json).map_err(|e| e.to_string())?;

    let autolaunch = app.autolaunch();
    if cfg.autostart {
        autolaunch.enable().map_err(|e| e.to_string())?;
    } else {
        autolaunch.disable().map_err(|e| e.to_string())?;
    }

    let was_running = stop(&state);
    *state.cfg.lock().unwrap() = cfg;
    if was_running {
        start(&app, &state).await?;
    }
    update_tray(&app);
    Ok(())
}

#[tauri::command]
async fn start_gateway(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    start(&app, &state).await
}

#[tauri::command]
async fn stop_gateway(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    stop(&state);
    update_tray(&app);
    Ok(())
}

#[derive(Serialize)]
struct Status {
    running: bool,
    port: u16,
}

#[tauri::command]
fn get_status(state: State<'_, AppState>) -> Status {
    let running = is_running(&state);
    let port = if running {
        state
            .listen_port
            .lock()
            .unwrap()
            .unwrap_or_else(|| state.cfg.lock().unwrap().port)
    } else {
        state.cfg.lock().unwrap().port
    };
    Status { running, port }
}

#[tauri::command]
fn get_logs(state: State<'_, AppState>) -> Vec<String> {
    state.logs.snapshot()
}

#[tauri::command]
async fn pick_workdir(app: AppHandle) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Option<String>, String> {
        Ok(app
            .dialog()
            .file()
            .blocking_pick_folder()
            .and_then(|f| f.into_path().ok())
            .map(|p| p.display().to_string()))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
struct UpdateCheckResponse {
    current_version: String,
    latest_version: Option<String>,
    update_available: bool,
    release_url: String,
    installer_name: Option<String>,
    download_url: Option<String>,
}

#[derive(Clone, Serialize)]
struct UpdateDownloadProgress {
    downloaded: u64,
    total: Option<u64>,
    bytes_per_sec: u64,
}

#[tauri::command]
fn get_app_version() -> String {
    CURRENT_VERSION.to_string()
}

#[tauri::command]
async fn check_for_update(app: AppHandle) -> Result<UpdateCheckResponse, String> {
    let info = update::check_app_update()
        .await
        .map_err(|e| e.to_string())?;
    if info.update_available {
        if let Some(latest) = &info.latest_version {
            notify_update_available(&app, latest);
        }
    }
    Ok(UpdateCheckResponse {
        current_version: info.current_version,
        latest_version: info.latest_version,
        update_available: info.update_available,
        release_url: info.release_url,
        installer_name: info.asset_name,
        download_url: info.download_url,
    })
}

#[tauri::command]
async fn download_and_install_update(app: AppHandle, accelerate: bool) -> Result<(), String> {
    let info = update::check_app_update()
        .await
        .map_err(|e| e.to_string())?;
    if !info.update_available {
        return Err(format!("already up to date (v{CURRENT_VERSION})"));
    }
    let github_url = info.download_url.clone().ok_or_else(|| {
        format!(
            "release has no installer asset `{}`",
            info.asset_name.unwrap_or_default()
        )
    })?;

    let app_emit = app.clone();
    let result = update::download_and_install(accelerate, move |downloaded, total, bytes_per_sec| {
        let _ = app_emit.emit(
            "update-download-progress",
            UpdateDownloadProgress {
                downloaded,
                total,
                bytes_per_sec,
            },
        );
    })
    .await;

    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.show();
        let _ = win.set_focus();
    }

    result.map_err(|e| {
        format!("{e}. Manual download: {github_url}")
    })
}

struct LogWriter(Arc<LogBuffer>);

impl Clone for LogWriter {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.push(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogWriter {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn main() {
    let logs = Arc::new(LogBuffer::new());
    let (filter_layer, log_reload) =
        reload::Layer::new(env_filter_for_level("info"));
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_writer(LogWriter(logs.clone())),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .on_window_event(|window, event| {
            // Closing a window just hides it; the app lives in the tray.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            get_config_path,
            save_config,
            start_gateway,
            stop_gateway,
            get_status,
            get_logs,
            pick_workdir,
            get_app_version,
            check_for_update,
            download_and_install_update,
        ])
        .setup(move |app| {
            let data_dir = app.path().app_data_dir()?;
            let config_path = data_dir.join("config.json");
            let mut cfg = load_config_from_file(&config_path);
            apply_log_level(&log_reload, &cfg.log_level)?;
            let before = cfg.workdir.clone();
            normalize_workdir(&mut cfg);
            if cfg.workdir != before {
                let app_handle = app.handle().clone();
                if let Err(e) = persist_config(&app_handle, &cfg) {
                    tracing::warn!("failed to migrate config workdir: {e}");
                }
            }

            let status = MenuItem::with_id(app, "status", "Gateway: Stopped", false, None::<&str>)?;
            let toggle = MenuItem::with_id(app, "toggle", "Start Gateway", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "Open Settings", true, None::<&str>)?;
            let logs_item = MenuItem::with_id(app, "logs", "Open Logs", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &status,
                    &toggle,
                    &PredefinedMenuItem::separator(app)?,
                    &settings,
                    &logs_item,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
                ],
            )?;

            let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png"))
                .expect("tray icon is valid PNG")
                .to_owned();
            TrayIconBuilder::with_id("main")
                .icon(icon)
                .icon_as_template(false)
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "toggle" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = app.state::<AppState>();
                            let result = if is_running(&state) {
                                stop(&state);
                                update_tray(&app);
                                Ok(())
                            } else {
                                start(&app, &state).await
                            };
                            if let Err(e) = result {
                                tracing::error!("gateway toggle failed: {e}");
                                notify_error(&app, "Gateway 操作失败", &e);
                            }
                        });
                    }
                    "settings" => open_window(app, "settings", "Settings", 520.0, 540.0),
                    "logs" => open_window(app, "logs", "Logs", 720.0, 560.0),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            app.manage(AppState {
                cfg: Mutex::new(cfg.clone()),
                listen_port: Mutex::new(None),
                handle: Mutex::new(None),
                stop_requested: Mutex::new(false),
                logs: logs.clone(),
                log_reload: log_reload.clone(),
                tray_status: Mutex::new(Some(status)),
                tray_toggle: Mutex::new(Some(toggle)),
            });

            let _ = app.notification().request_permission();

            #[cfg(target_os = "macos")]
            let _ = app.set_dock_visibility(false);

            // Tray-manager contract: the gateway comes up with the app.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<AppState>();
                if let Err(e) = start(&app_handle, &state).await {
                    tracing::error!("auto-start failed: {e}");
                    notify_error(&app_handle, "Gateway 启动失败", &e);
                }
            });

            let auto_check = cfg.auto_check_update;
            if auto_check {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Ok(info) = update::check_app_update().await {
                        if info.update_available {
                            if let Some(latest) = info.latest_version {
                                notify_update_available(&app_handle, &latest);
                            }
                        }
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
