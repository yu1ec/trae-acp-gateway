use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter as _, Manager as _, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::{DialogExt as _, MessageDialogButtons};
use tauri_plugin_notification::NotificationExt as _;
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};
use trae_acp_gateway::{
    CURRENT_VERSION, Config as GatewayConfig, ThoughtSink, ThoughtUpdate, canonicalize_workdir,
    default_workdir_root, ensure_workdir, is_legacy_workdir, serve_with_thoughts, user_home_dir,
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
    /// Empty means "not configured": requests fall back to the gateway's cwd.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    workdir: String,
    /// Client-reported workdirs must stay under this root.
    #[serde(default = "default_workdir_root")]
    allowed_workdir_root: String,
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
    /// Open a floating panel that streams ACP reasoning chunks.
    #[serde(default)]
    thought_panel_enabled: bool,
}

impl AppConfig {
    fn defaults() -> Self {
        Self {
            port: 60111,
            workdir: String::new(),
            allowed_workdir_root: default_workdir_root(),
            trae_cmd: "traecli".into(),
            trae_args: vec!["acp".into(), "serve".into()],
            sandbox: true,
            log_level: default_log_level(),
            debug: false,
            autostart: false,
            auto_check_update: false,
            thought_panel_enabled: false,
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

/// Path that older versions wrote as the implicit default.
fn is_old_default_workdir(workdir: &str) -> bool {
    user_home_dir().is_some_and(|home| {
        std::path::Path::new(workdir.trim())
            == std::path::Path::new(&home).join("trae-acp-gateway")
    })
}

fn normalize_workdir(cfg: &mut AppConfig) {
    if is_legacy_workdir(&cfg.workdir) || is_old_default_workdir(&cfg.workdir) {
        cfg.workdir.clear();
        return;
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
        allowed_workdir_root: cfg.allowed_workdir_root.clone(),
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

    fn clear(&self) {
        self.partial.lock().unwrap().clear();
        self.lines.lock().unwrap().clear();
    }
}

/// Upper bound for the aggregated reasoning transcript. Large enough for a
/// long turn, small enough that polling the panel stays cheap.
const THOUGHT_CAP_BYTES: usize = 200_000;

/// Upper bound on retained segments, so many short turns cannot grow the
/// snapshot without limit.
const THOUGHT_MAX_SEGMENTS: usize = 40;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Reasoning produced by one prompt turn. The panel renders each segment as
/// its own block so turns from different requests stay distinguishable.
#[derive(Serialize, Clone)]
struct ThoughtSegment {
    session_id: String,
    workdir: String,
    started_at: u64,
    ended_at: Option<u64>,
    /// The user's question for this turn (may be truncated).
    prompt: String,
    text: String,
}

struct ThoughtInner {
    segments: VecDeque<ThoughtSegment>,
    version: u64,
}

/// Aggregated ACP reasoning stream for the desktop thought panel. The sink is
/// called from gateway worker threads; the panel polls snapshots.
struct ThoughtBuffer {
    inner: Mutex<ThoughtInner>,
    enabled: std::sync::atomic::AtomicBool,
}

impl ThoughtBuffer {
    fn new(enabled: bool) -> Self {
        Self {
            inner: Mutex::new(ThoughtInner {
                segments: VecDeque::new(),
                version: 0,
            }),
            enabled: std::sync::atomic::AtomicBool::new(enabled),
        }
    }

    fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    fn push(&self, update: ThoughtUpdate) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let now = now_ms();
        match update {
            ThoughtUpdate::Start {
                session_id,
                workdir,
                prompt,
            } => {
                let segment = Self::segment_mut(&mut inner, &session_id, &workdir, now);
                segment.prompt = prompt;
            }
            ThoughtUpdate::Delta { session_id, text } => {
                let segment = Self::segment_mut(&mut inner, &session_id, "", now);
                segment.text.push_str(&text);
            }
            ThoughtUpdate::End { session_id } => {
                if let Some(segment) = inner
                    .segments
                    .iter_mut()
                    .rev()
                    .find(|s| s.session_id == session_id && s.ended_at.is_none())
                {
                    segment.ended_at = Some(now);
                }
            }
        }
        Self::trim(&mut inner);
        inner.version = inner.version.wrapping_add(1);
    }

    /// Extend the open segment for `session_id`, or open a new one. A finished
    /// segment is never reopened: the same session id can only start again
    /// after a fresh `Start`.
    fn segment_mut<'a>(
        inner: &'a mut ThoughtInner,
        session_id: &str,
        workdir: &str,
        now: u64,
    ) -> &'a mut ThoughtSegment {
        if let Some(index) = inner
            .segments
            .iter()
            .rposition(|s| s.session_id == session_id && s.ended_at.is_none())
        {
            return &mut inner.segments[index];
        }
        inner.segments.push_back(ThoughtSegment {
            session_id: session_id.to_string(),
            workdir: workdir.to_string(),
            started_at: now,
            ended_at: None,
            prompt: String::new(),
            text: String::new(),
        });
        inner.segments.back_mut().unwrap()
    }

    /// Drop the oldest segments (or trim the oldest text) once the transcript
    /// exceeds its caps.
    fn trim(inner: &mut ThoughtInner) {
        while inner.segments.len() > THOUGHT_MAX_SEGMENTS {
            inner.segments.pop_front();
        }
        let mut total: usize = inner.segments.iter().map(|s| s.text.len()).sum();
        while total > THOUGHT_CAP_BYTES && inner.segments.len() > 1 {
            if let Some(removed) = inner.segments.pop_front() {
                total -= removed.text.len();
            }
        }
        if total > THOUGHT_CAP_BYTES {
            if let Some(oldest) = inner.segments.front_mut() {
                let mut cut = total - THOUGHT_CAP_BYTES;
                while cut < oldest.text.len() && !oldest.text.is_char_boundary(cut) {
                    cut += 1;
                }
                if cut < oldest.text.len() {
                    oldest.text.drain(..cut);
                }
            }
        }
    }

    fn snapshot(&self) -> ThoughtSnapshot {
        let inner = self.inner.lock().unwrap();
        ThoughtSnapshot {
            segments: inner.segments.iter().cloned().collect(),
            version: inner.version,
            enabled: self.enabled.load(Ordering::Relaxed),
        }
    }

    fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.segments.clear();
        inner.version = inner.version.wrapping_add(1);
    }
}

#[derive(Serialize)]
struct ThoughtSnapshot {
    segments: Vec<ThoughtSegment>,
    version: u64,
    enabled: bool,
}

struct AppState {
    cfg: Mutex<AppConfig>,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Bumped on every stop/start so a watcher only reports crashes for the
    /// gateway instance it was spawned for.
    generation: AtomicU64,
    logs: Arc<LogBuffer>,
    thoughts: Arc<ThoughtBuffer>,
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

fn is_running(state: &AppState) -> bool {
    state
        .handle
        .lock()
        .unwrap()
        .as_ref()
        .map(|h| !h.is_finished())
        .unwrap_or(false)
}

async fn stop(state: &AppState) -> bool {
    // Invalidate the watcher first so an intentional stop is not reported as
    // a crash.
    state.generation.fetch_add(1, Ordering::SeqCst);
    let handle = state.handle.lock().unwrap().take();
    match handle {
        Some(handle) => {
            handle.abort();
            // Wait until the task (and its listener) is actually dropped;
            // otherwise an immediate restart can fail to bind the same port.
            let _ = handle.await;
            true
        }
        None => false,
    }
}

fn watch_gateway(app: AppHandle, generation: u64) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let state = app.state::<AppState>();
            if state.generation.load(Ordering::SeqCst) != generation {
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
    let mut gcfg = to_gateway(&state.cfg.lock().unwrap());
    canonicalize_workdir(&mut gcfg).map_err(|e| e.to_string())?;
    let sink: ThoughtSink = {
        let thoughts = state.thoughts.clone();
        Arc::new(move |update| thoughts.push(update))
    };
    let (handle, _port) = serve_with_thoughts(Arc::new(gcfg), Some(sink))
        .await
        .map_err(|e| e.to_string())?;
    *state.handle.lock().unwrap() = Some(handle);
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    update_tray(app);
    watch_gateway(app.clone(), generation);
    Ok(())
}

fn update_tray(app: &AppHandle) {
    let state = app.state::<AppState>();
    let running = is_running(&state);
    let port = state.cfg.lock().unwrap().port;
    if let Some(item) = state.tray_status.lock().unwrap().as_ref() {
        let _ = item.set_text(if running {
            format!("Gateway: Running :{port}")
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
    let win = match app.get_webview_window(label) {
        Some(win) => win,
        None => match WebviewWindowBuilder::new(
            app,
            label,
            WebviewUrl::App(format!("index.html#/{label}").into()),
        )
        .title(title)
        .inner_size(w, h)
        .build()
        {
            Ok(win) => win,
            Err(e) => {
                tracing::error!("failed to open `{label}` window: {e}");
                return;
            }
        },
    };
    // On macOS the app is an accessory (LSUIElement), so a freshly built window
    // is only ordered front without activating the app. Without the explicit
    // set_focus the first tray click looks like it did nothing.
    let _ = win.unminimize();
    let _ = win.show();
    let _ = win.set_focus();
}

/// Floating, always-on-top reasoning panel. The window itself is transparent so
/// the in-page opacity slider can control how much of the desktop shows
/// through.
fn open_thoughts_window(app: &AppHandle) {
    let win = match app.get_webview_window("thoughts") {
        Some(win) => win,
        None => {
            let builder = WebviewWindowBuilder::new(
                app,
                "thoughts",
                WebviewUrl::App("index.html#/thoughts".into()),
            )
            .title("Thinking")
            .inner_size(520.0, 640.0)
            // The collapsed "rolled up" state is a single pill; the minimum
            // height has to allow the window to shrink to it.
            .min_inner_size(320.0, 60.0)
            .always_on_top(true)
            .transparent(true)
            // Fully custom chrome: the page draws its own close/appearance/
            // roll-up controls at the top-left corner.
            .decorations(false)
            // The system window shadow draws a dark rim around the transparent
            // window; the page provides its own look.
            .shadow(false)
            .maximizable(false);
            match builder.build() {
                Ok(win) => win,
                Err(e) => {
                    tracing::error!("failed to open thoughts window: {e}");
                    return;
                }
            }
        }
    };
    let _ = win.unminimize();
    let _ = win.show();
    let _ = win.set_focus();
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
    state.thoughts.set_enabled(cfg.thought_panel_enabled);
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

    let was_running = stop(&state).await;
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
    stop(&state).await;
    update_tray(&app);
    Ok(())
}

#[derive(Serialize)]
struct Status {
    running: bool,
    port: u16,
}

#[derive(Serialize)]
struct TraeCliStatus {
    installed: bool,
    path: Option<String>,
}

fn trae_cli_status(cmd: &str) -> TraeCliStatus {
    match trae_acp_gateway::trae_cli::find_installed(cmd) {
        Some(path) => TraeCliStatus {
            installed: true,
            path: Some(path.display().to_string()),
        },
        None => TraeCliStatus {
            installed: false,
            path: None,
        },
    }
}

#[tauri::command]
fn get_trae_cli_status(state: State<'_, AppState>) -> TraeCliStatus {
    trae_cli_status(&state.cfg.lock().unwrap().trae_cmd)
}

#[tauri::command]
async fn install_trae_cli_command(state: State<'_, AppState>) -> Result<TraeCliStatus, String> {
    let cmd = state.cfg.lock().unwrap().trae_cmd.clone();
    if !trae_acp_gateway::trae_cli::is_default_traecli(&cmd) {
        return Err(format!(
            "当前 Trae command 为 `{cmd}`，请改为 `traecli` 后再使用自动安装"
        ));
    }
    tauri::async_runtime::spawn_blocking(trae_acp_gateway::trae_cli::install_trae_cli)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let status = trae_cli_status(&cmd);
    if !status.installed {
        return Err(format!(
            "安装脚本已执行，但仍未找到 `{cmd}`；请确认安装目录已加入 PATH，或手动指定 Trae command"
        ));
    }
    Ok(status)
}

#[tauri::command]
fn get_status(state: State<'_, AppState>) -> Status {
    let running = is_running(&state);
    let port = state.cfg.lock().unwrap().port;
    Status { running, port }
}

#[tauri::command]
fn get_logs(state: State<'_, AppState>) -> Vec<String> {
    state.logs.snapshot()
}

#[tauri::command]
fn clear_logs(state: State<'_, AppState>) {
    state.logs.clear();
}

#[tauri::command]
fn get_thoughts(state: State<'_, AppState>) -> ThoughtSnapshot {
    state.thoughts.snapshot()
}

#[tauri::command]
fn clear_thoughts(state: State<'_, AppState>) {
    state.thoughts.clear();
}

#[tauri::command]
fn show_settings_window(app: AppHandle) {
    open_window(&app, "settings", "Settings", 520.0, 540.0);
}

#[tauri::command]
fn show_thoughts_window(app: AppHandle) {
    open_thoughts_window(&app);
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
    let result =
        update::download_and_install(accelerate, move |downloaded, total, bytes_per_sec| {
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

    result.map_err(|e| format!("{e}. Manual download: {github_url}"))
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

/// Check for the ACP agent before auto-starting. When it is missing, ask the
/// user whether to run the official installer, then start the gateway anyway
/// (the tray toggle stays usable and failures are logged clearly).
async fn start_after_trae_cli_check(app: AppHandle) {
    let cmd = app.state::<AppState>().cfg.lock().unwrap().trae_cmd.clone();

    if trae_acp_gateway::trae_cli::find_installed(&cmd).is_none() {
        tracing::warn!(cmd = %cmd, "TRAE CLI not found");

        if trae_acp_gateway::trae_cli::is_default_traecli(&cmd) {
            let dialog_app = app.clone();
            let dialog_cmd = cmd.clone();
            let wants_install = tauri::async_runtime::spawn_blocking(move || {
                dialog_app
                    .dialog()
                    .message(format!(
                        "未检测到 TRAE CLI（`{dialog_cmd}`），Gateway 无法处理请求。\n\n是否现在自动安装？\n\n{}",
                        trae_acp_gateway::trae_cli::install_hint()
                    ))
                    .title("TRAE CLI 未安装")
                    .buttons(MessageDialogButtons::OkCancelCustom(
                        "自动安装".into(),
                        "稍后".into(),
                    ))
                    .blocking_show()
            })
            .await
            .unwrap_or(false);

            if wants_install {
                let install = tauri::async_runtime::spawn_blocking(
                    trae_acp_gateway::trae_cli::install_trae_cli,
                )
                .await;
                match install {
                    Ok(Ok(())) if trae_acp_gateway::trae_cli::find_installed(&cmd).is_some() => {
                        tracing::info!("TRAE CLI installed");
                        let _ = app
                            .notification()
                            .builder()
                            .title("TRAE CLI 安装完成")
                            .body("Gateway 即将启动")
                            .show();
                    }
                    Ok(Ok(())) => {
                        tracing::warn!(cmd = %cmd, "installer finished but command still missing");
                        notify_error(
                            &app,
                            "TRAE CLI 未找到",
                            &format!(
                                "安装脚本已执行，但仍未找到 `{cmd}`。\n请确认安装目录已加入 PATH，或在设置中手动指定 Trae command。"
                            ),
                        );
                    }
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "TRAE CLI install failed");
                        notify_error(
                            &app,
                            "TRAE CLI 安装失败",
                            &format!(
                                "{e}\n\n可在设置页重试，或手动安装：\n{}",
                                trae_acp_gateway::trae_cli::install_hint()
                            ),
                        );
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "TRAE CLI install task failed");
                        notify_error(&app, "TRAE CLI 安装失败", &e.to_string());
                    }
                }
            }
        } else {
            notify_error(
                &app,
                "TRAE CLI 未找到",
                &format!(
                    "配置的 agent 命令 `{cmd}` 不存在。请在设置中检查 Trae command，或安装 TRAE CLI：\n{}",
                    trae_acp_gateway::trae_cli::install_hint()
                ),
            );
        }
    }

    let state = app.state::<AppState>();
    if let Err(e) = start(&app, &state).await {
        tracing::error!("auto-start failed: {e}");
        notify_error(&app, "Gateway 启动失败", &e);
    }
}

fn main() {
    let logs = Arc::new(LogBuffer::new());
    let (filter_layer, log_reload) = reload::Layer::new(env_filter_for_level("info"));
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
            clear_logs,
            get_thoughts,
            clear_thoughts,
            show_settings_window,
            show_thoughts_window,
            pick_workdir,
            get_app_version,
            get_trae_cli_status,
            install_trae_cli_command,
            check_for_update,
            download_and_install_update,
        ])
        .setup(move |app| {
            let data_dir = app.path().app_data_dir()?;
            let config_path = data_dir.join("config.json");
            let mut cfg = load_config_from_file(&config_path);
            apply_log_level(&log_reload, &cfg.log_level)?;
            let thoughts = Arc::new(ThoughtBuffer::new(cfg.thought_panel_enabled));
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
            let thoughts_item =
                MenuItem::with_id(app, "thoughts", "Open Thoughts", true, None::<&str>)?;
            let logs_item = MenuItem::with_id(app, "logs", "Open Logs", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &status,
                    &toggle,
                    &PredefinedMenuItem::separator(app)?,
                    &settings,
                    &thoughts_item,
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
                                stop(&state).await;
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
                    "thoughts" => open_thoughts_window(app),
                    "logs" => open_window(app, "logs", "Logs", 720.0, 560.0),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            app.manage(AppState {
                cfg: Mutex::new(cfg.clone()),
                handle: Mutex::new(None),
                generation: AtomicU64::new(0),
                logs: logs.clone(),
                thoughts: thoughts.clone(),
                log_reload: log_reload.clone(),
                tray_status: Mutex::new(Some(status)),
                tray_toggle: Mutex::new(Some(toggle)),
            });

            let _ = app.notification().request_permission();

            #[cfg(target_os = "macos")]
            let _ = app.set_dock_visibility(false);

            // Tray-manager contract: the gateway comes up with the app.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(start_after_trae_cli_check(app_handle));

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

#[cfg(test)]
mod thought_tests {
    use super::*;

    #[test]
    fn segments_are_grouped_by_session_and_end_is_tracked() {
        let buffer = ThoughtBuffer::new(true);
        buffer.push(ThoughtUpdate::Start {
            session_id: "session-a".into(),
            workdir: "/work/a".into(),
            prompt: "first question".into(),
        });
        buffer.push(ThoughtUpdate::Delta {
            session_id: "session-a".into(),
            text: "first".into(),
        });
        buffer.push(ThoughtUpdate::Start {
            session_id: "session-b".into(),
            workdir: "/work/b".into(),
            prompt: "second question".into(),
        });
        buffer.push(ThoughtUpdate::Delta {
            session_id: "session-b".into(),
            text: "second".into(),
        });
        buffer.push(ThoughtUpdate::End {
            session_id: "session-a".into(),
        });

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.segments.len(), 2);
        assert_eq!(snapshot.segments[0].text, "first");
        assert_eq!(snapshot.segments[0].workdir, "/work/a");
        assert_eq!(snapshot.segments[0].prompt, "first question");
        assert!(snapshot.segments[0].ended_at.is_some());
        assert_eq!(snapshot.segments[1].text, "second");
        assert_eq!(snapshot.segments[1].prompt, "second question");
        assert!(snapshot.segments[1].ended_at.is_none());
    }
}
