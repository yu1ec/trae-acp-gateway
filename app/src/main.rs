use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter as _, Manager as _, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::{DialogExt as _, MessageDialogButtons};
use tauri_plugin_notification::NotificationExt as _;
use tauri_plugin_updater::UpdaterExt as _;
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};
use trae_acp_gateway::{
    ActivitySink, CURRENT_VERSION, Config as GatewayConfig, ThoughtSink, ThoughtUpdate,
    canonicalize_workdir, default_workdir_root, ensure_workdir, is_legacy_workdir,
    serve_with_thoughts, user_home_dir,
};

mod permissions;
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

/// The desktop app always launches the agent with these fixed values; the
/// settings page only reports whether `traecli` can be found.
const TRAE_CMD: &str = "traecli";
const TRAE_ARGS: &[&str] = &["acp", "serve"];

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
    /// Set after the one-time macOS permission prompt so it is not shown again.
    #[serde(default)]
    permissions_prompted: bool,
}

impl AppConfig {
    fn defaults() -> Self {
        Self {
            port: 60111,
            workdir: String::new(),
            allowed_workdir_root: default_workdir_root(),
            log_level: default_log_level(),
            debug: false,
            autostart: false,
            auto_check_update: false,
            thought_panel_enabled: false,
            permissions_prompted: false,
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
        std::path::Path::new(workdir.trim()) == std::path::Path::new(&home).join("trae-acp-gateway")
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
        trae_cmd: TRAE_CMD.into(),
        trae_args: TRAE_ARGS.iter().map(|arg| (*arg).to_string()).collect(),
        // The app never approves sandbox-escape requests. traecli's own
        // permission mode decides what the agent may do.
        sandbox: true,
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

/// Coalescing window for thought events. ACP delivers many small chunks per
/// second; collapsing them keeps the webview repainting at roughly 25 fps
/// instead of re-serialising a full snapshot on every chunk.
const THOUGHT_EVENT_COALESCE: Duration = Duration::from_millis(40);

/// Event name the panel listens on. The payload is empty: the listener fetches
/// the snapshot once per frame, so no per-chunk text crosses the IPC boundary.
const THOUGHT_EVENT: &str = "thoughts-updated";

/// The OS can bring the thoughts window to the front (tray menu, window
/// switcher) without the page knowing edge-hide parked it off-screen. This
/// event asks the page to slide it back into view.
const THOUGHT_REVEAL_EVENT: &str = "thoughts-reveal";

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
    /// ACP session id; unique per turn, used only to match deltas.
    session_id: String,
    /// The client's conversation id, carried across turns of the same
    /// conversation so the panel can attribute interleaved sessions.
    client_session: Option<String>,
    workdir: String,
    /// Whether `workdir` was reported by the request rather than inherited
    /// from the gateway's configured fallback. The panel uses this to keep
    /// auxiliary requests (title/summary, no cwd) from naming a conversation
    /// after the fallback directory.
    workdir_reported: bool,
    started_at: u64,
    ended_at: Option<u64>,
    /// Last time this turn produced output. The panel focuses the live turn
    /// that wrote most recently, so concurrent requests never compete for the
    /// same screen position.
    updated_at: u64,
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
    /// Signals the emitter thread that the transcript changed. `None` until the
    /// app attaches its event channel.
    notify: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl ThoughtBuffer {
    fn new(enabled: bool) -> Self {
        Self {
            inner: Mutex::new(ThoughtInner {
                segments: VecDeque::new(),
                version: 0,
            }),
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            notify: Mutex::new(None),
        }
    }

    fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Forward transcript changes to the webview. Pushes only signal this
    /// thread; it coalesces bursts and emits one empty event, so a streaming
    /// turn costs one repaint per window rather than one IPC call per chunk.
    fn attach_events(&self, app: AppHandle) {
        let (tx, rx) = std::sync::mpsc::channel();
        *self.notify.lock().unwrap() = Some(tx);
        std::thread::spawn(move || {
            while rx.recv().is_ok() {
                std::thread::sleep(THOUGHT_EVENT_COALESCE);
                while rx.try_recv().is_ok() {}
                if let Err(e) = app.emit(THOUGHT_EVENT, ()) {
                    tracing::debug!("failed to emit thought event: {e}");
                }
            }
        });
    }

    /// Wake the emitter without blocking the ACP turn loop.
    fn wake(&self) {
        if let Some(tx) = self.notify.lock().unwrap().as_ref() {
            let _ = tx.send(());
        }
    }

    fn push(&self, update: ThoughtUpdate) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        {
            let mut inner = self.inner.lock().unwrap();
            let now = now_ms();
            match update {
                ThoughtUpdate::Start {
                    session_id,
                    client_session,
                    workdir,
                    workdir_reported,
                    prompt,
                } => {
                    let segment =
                        Self::segment_mut(&mut inner, &session_id, &workdir, workdir_reported, now);
                    segment.client_session = client_session;
                    segment.prompt = prompt;
                }
                ThoughtUpdate::Delta { session_id, text } => {
                    let segment = Self::segment_mut(&mut inner, &session_id, "", false, now);
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
                        segment.updated_at = now;
                    }
                }
            }
            Self::trim(&mut inner);
            inner.version = inner.version.wrapping_add(1);
        }
        self.wake();
    }

    /// Extend the open segment for `session_id`, or open a new one. A finished
    /// segment is never reopened: the same session id can only start again
    /// after a fresh `Start`.
    fn segment_mut<'a>(
        inner: &'a mut ThoughtInner,
        session_id: &str,
        workdir: &str,
        workdir_reported: bool,
        now: u64,
    ) -> &'a mut ThoughtSegment {
        if let Some(index) = inner
            .segments
            .iter()
            .rposition(|s| s.session_id == session_id && s.ended_at.is_none())
        {
            let segment = &mut inner.segments[index];
            segment.updated_at = now;
            return segment;
        }
        inner.segments.push_back(ThoughtSegment {
            session_id: session_id.to_string(),
            client_session: None,
            workdir: workdir.to_string(),
            workdir_reported,
            started_at: now,
            ended_at: None,
            updated_at: now,
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
        {
            let mut inner = self.inner.lock().unwrap();
            inner.segments.clear();
            inner.version = inner.version.wrapping_add(1);
        }
        self.wake();
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
    /// ACP turns currently in flight; drives the tray animation.
    activity: Arc<AtomicUsize>,
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
    // Turn guards are dropped when the gateway task is aborted; reset here as
    // well so a straggling update cannot leave the tray animation running.
    state.activity.store(0, Ordering::Relaxed);
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
                state.activity.store(0, Ordering::Relaxed);
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
    state.activity.store(0, Ordering::Relaxed);
    let mut gcfg = to_gateway(&state.cfg.lock().unwrap());
    canonicalize_workdir(&mut gcfg).map_err(|e| e.to_string())?;
    let sink: ThoughtSink = {
        let thoughts = state.thoughts.clone();
        Arc::new(move |update| thoughts.push(update))
    };
    let activity_sink: ActivitySink = {
        let activity = state.activity.clone();
        Arc::new(move |count| activity.store(count, Ordering::Relaxed))
    };
    let (handle, _port) = serve_with_thoughts(Arc::new(gcfg), Some(sink), Some(activity_sink))
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

/// Thinking-dots frames shown while the gateway is talking to the agent.
const TRAY_BUSY_FRAMES: &[&[u8]] = &[
    include_bytes!("../icons/tray-busy-1.png"),
    include_bytes!("../icons/tray-busy-2.png"),
    include_bytes!("../icons/tray-busy-3.png"),
    include_bytes!("../icons/tray-busy-4.png"),
    include_bytes!("../icons/tray-busy-5.png"),
    include_bytes!("../icons/tray-busy-6.png"),
    include_bytes!("../icons/tray-busy-7.png"),
    include_bytes!("../icons/tray-busy-8.png"),
];

const TRAY_FRAME_INTERVAL: Duration = Duration::from_millis(120);

/// Tray icon colour for `tauri dev` builds: amber instead of the release green,
/// so a locally running instance is obvious in the menu bar.
const TRAY_DEV_COLOR: [u8; 3] = [0xFF, 0x9F, 0x0A];

/// Decode a tray PNG, recolouring it in debug builds while keeping the alpha
/// channel (and therefore the antialiased edges) intact.
fn tray_image(bytes: &[u8]) -> tauri::image::Image<'static> {
    let image = tauri::image::Image::from_bytes(bytes).expect("tray icon is valid PNG");
    if !cfg!(debug_assertions) {
        return image.to_owned();
    }
    let mut rgba = image.rgba().to_vec();
    for px in rgba.chunks_exact_mut(4) {
        if px[3] != 0 {
            px[..3].copy_from_slice(&TRAY_DEV_COLOR);
        }
    }
    tauri::image::Image::new_owned(rgba, image.width(), image.height())
}

/// Swap the tray icon for a thinking-dots variant while any ACP turn is in
/// flight, and restore the static icon once the gateway goes quiet.
fn spawn_tray_animation(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let idle = tray_image(include_bytes!("../icons/tray-icon.png"));
        let frames: Vec<_> = TRAY_BUSY_FRAMES
            .iter()
            .map(|bytes| tray_image(bytes))
            .collect();
        let mut next_frame = 0;
        let mut was_busy = false;
        let activity = app.state::<AppState>().activity.clone();
        loop {
            tokio::time::sleep(TRAY_FRAME_INTERVAL).await;
            let busy = activity.load(Ordering::Relaxed) > 0;
            let Some(tray) = app.tray_by_id("main") else {
                continue;
            };
            if busy {
                let _ = tray.set_icon(Some(frames[next_frame].clone()));
                next_frame = (next_frame + 1) % frames.len();
            } else if was_busy {
                let _ = tray.set_icon(Some(idle.clone()));
                next_frame = 0;
            }
            was_busy = busy;
        }
    });
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
    // A parked panel is still "shown" at an off-screen coordinate, so a tray
    // click that only focuses it would look like nothing happened.
    let _ = win.emit(THOUGHT_REVEAL_EVENT, ());
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
    // The settings page does not read this flag back; keep the persisted value
    // so saving unrelated settings does not re-arm the one-time prompt.
    cfg.permissions_prompted = state.cfg.lock().unwrap().permissions_prompted;
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
fn get_trae_cli_status() -> TraeCliStatus {
    trae_cli_status(TRAE_CMD)
}

#[tauri::command]
async fn install_trae_cli_command() -> Result<TraeCliStatus, String> {
    tauri::async_runtime::spawn_blocking(trae_acp_gateway::trae_cli::install_trae_cli)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let status = trae_cli_status(TRAE_CMD);
    if !status.installed {
        return Err(format!(
            "安装脚本已执行，但仍未找到 `{TRAE_CMD}`；请查看官网说明手动安装，或确认安装目录已加入 PATH"
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
    /// True when the release ships a Tauri updater artifact, so the app can
    /// install and restart in place. Older releases only have a DMG/MSI and
    /// therefore fall back to a manual download link.
    auto_install: bool,
}

#[derive(Clone, Serialize)]
struct UpdateDownloadProgress {
    downloaded: u64,
    total: Option<u64>,
    bytes_per_sec: u64,
}

fn artifact_name(url: &str) -> Option<String> {
    url.split(['?', '#'])
        .next()
        .and_then(|path| path.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// Check the Tauri updater manifest first. Releases published before updater
/// artifacts existed have no `latest.json`, so fall back to the GitHub API and
/// report that the update must be downloaded manually.
async fn check_update_availability(app: &AppHandle) -> Result<UpdateCheckResponse, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => {
            let download_url = update.download_url.to_string();
            Ok(UpdateCheckResponse {
                current_version: update.current_version.clone(),
                latest_version: Some(update.version.clone()),
                update_available: true,
                release_url: format!(
                    "{}/releases/tag/v{}",
                    trae_acp_gateway::GITHUB_REPO_URL,
                    update.version
                ),
                installer_name: artifact_name(&download_url),
                download_url: Some(download_url),
                auto_install: true,
            })
        }
        Ok(None) => Ok(UpdateCheckResponse {
            current_version: CURRENT_VERSION.to_string(),
            latest_version: None,
            update_available: false,
            release_url: trae_acp_gateway::GITHUB_REPO_URL.to_string(),
            installer_name: None,
            download_url: None,
            auto_install: false,
        }),
        Err(updater_err) => {
            tracing::warn!(
                "updater manifest check failed: {updater_err}; falling back to GitHub API"
            );
            let info = update::check_app_update()
                .await
                .map_err(|e| format!("{updater_err}; fallback check failed: {e}"))?;
            Ok(UpdateCheckResponse {
                current_version: info.current_version,
                latest_version: info.latest_version,
                update_available: info.update_available,
                release_url: info.release_url,
                installer_name: info.asset_name,
                download_url: info.download_url,
                auto_install: false,
            })
        }
    }
}

#[tauri::command]
fn get_app_version() -> String {
    CURRENT_VERSION.to_string()
}

#[tauri::command]
async fn check_for_update(app: AppHandle) -> Result<UpdateCheckResponse, String> {
    let info = check_update_availability(&app).await?;
    if info.update_available {
        if let Some(latest) = &info.latest_version {
            notify_update_available(&app, latest);
        }
    }
    Ok(info)
}

/// Public key from `tauri.conf.json` (`plugins.updater.pubkey`), used to verify
/// artifacts downloaded through mirrors.
fn updater_pubkey(app: &AppHandle) -> Result<String, String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|cfg| cfg.get("pubkey"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| "plugins.updater.pubkey is not configured".to_string())
}

#[tauri::command]
async fn download_and_install_update(
    app: AppHandle,
    state: State<'_, AppState>,
    accelerate: bool,
) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("already up to date (v{CURRENT_VERSION})"))?;

    let permission = permissions::status();
    if permission.translocated {
        return Err(
            "当前应用从 macOS 隔离路径运行，请先把 Trae ACP Gateway 拖到「应用程序」后再更新。"
                .into(),
        );
    }

    let url = update.download_url.to_string();
    let filename = artifact_name(&url).unwrap_or_else(|| "update.bin".into());
    let app_emit = app.clone();
    let downloaded = update::download_update_artifact(
        &url,
        &filename,
        accelerate,
        move |downloaded, total, bytes_per_sec| {
            let _ = app_emit.emit(
                "update-download-progress",
                UpdateDownloadProgress {
                    downloaded,
                    total,
                    bytes_per_sec,
                },
            );
        },
    )
    .await
    .map_err(|e| format!("{e}. Manual download: {url}"))?;

    // Read and verify before touching the running gateway, so a tampered or
    // truncated download leaves everything as-is.
    let bytes = tauri::async_runtime::spawn_blocking(move || std::fs::read(downloaded))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("reading downloaded update: {e}"))?;

    let pubkey = updater_pubkey(&app)?;
    update::verify_update_signature(&bytes, &update.signature, &pubkey, &update.version)
        .map_err(|e| format!("{e}. Manual download: {url}"))?;

    // Stop the gateway before swapping the app bundle so no ACP turn is cut
    // mid-flight.
    stop(&state).await;

    let install_result = tauri::async_runtime::spawn_blocking(move || update.install(bytes))
        .await
        .map_err(|e| e.to_string())?;

    if let Err(e) = install_result {
        // The gateway was stopped above; bring it back so a failed install (for
        // example a cancelled admin prompt) does not leave the app unusable.
        let _ = start(&app, state.inner()).await;
        return Err(format!("installing update: {e}"));
    }

    #[cfg(target_os = "windows")]
    {
        // The updater launches the installer and exits the process already.
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Reply to the settings page first, then relaunch into the new bundle.
        let restart_app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            restart_app.restart();
        });
        Ok(())
    }
}

#[tauri::command]
fn get_permission_status() -> permissions::PermissionStatus {
    permissions::status()
}

#[tauri::command]
fn open_permission_settings(pane: String) -> Result<(), String> {
    permissions::open_settings(&pane).map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal_app_in_finder() -> Result<(), String> {
    permissions::reveal_in_finder().map_err(|e| e.to_string())
}

#[tauri::command]
fn reset_media_library_permission() -> Result<(), String> {
    permissions::reset_media_library().map_err(|e| e.to_string())
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
    let cmd = TRAE_CMD;

    if trae_acp_gateway::trae_cli::find_installed(cmd).is_none() {
        tracing::warn!(cmd = %cmd, "TRAE CLI not found");

        let dialog_app = app.clone();
        let wants_install = tauri::async_runtime::spawn_blocking(move || {
            dialog_app
                .dialog()
                .message(format!(
                    "未检测到 TRAE CLI（`{cmd}`），Gateway 无法处理请求。\n\n是否现在自动安装？\n\n{}",
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
            let install =
                tauri::async_runtime::spawn_blocking(trae_acp_gateway::trae_cli::install_trae_cli)
                    .await;
            match install {
                Ok(Ok(())) if trae_acp_gateway::trae_cli::find_installed(cmd).is_some() => {
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
                            "安装脚本已执行，但仍未找到 `{cmd}`。\n请确认安装目录已加入 PATH，或在设置页查看官网安装说明。"
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
    }

    let state = app.state::<AppState>();
    if let Err(e) = start(&app, &state).await {
        tracing::error!("auto-start failed: {e}");
        notify_error(&app, "Gateway 启动失败", &e);
    }
}

/// One-time macOS permission hint.
///
/// Full Disk Access is optional: only protected locations (Desktop, Documents,
/// Downloads, removable volumes) need it. The prompt is deliberately dismissible
/// and the settings page keeps a permanent entry point.
fn maybe_prompt_permissions(app: AppHandle) {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
    }

    #[cfg(target_os = "macos")]
    {
        if app
            .state::<AppState>()
            .cfg
            .lock()
            .unwrap()
            .permissions_prompted
        {
            return;
        }

        tauri::async_runtime::spawn(async move {
            let already_granted = permissions::status().full_disk_access == Some(true);
            if !already_granted {
                let dialog_app = app.clone();
                let open_settings = tauri::async_runtime::spawn_blocking(move || {
                    dialog_app
                        .dialog()
                        .message(
                            "如果 Gateway 需要读取「桌面」「文稿」「下载」或其它受保护目录中的项目，\
                             请在「系统设置 → 隐私与安全性 → 完全磁盘访问权限」中勾选 Trae ACP Gateway。\n\n\
                             普通项目目录（例如 ~/codes）不需要该权限，可以稍后在设置页授权。",
                        )
                        .title("系统权限（可选）")
                        .buttons(MessageDialogButtons::OkCancelCustom(
                            "打开设置".into(),
                            "稍后".into(),
                        ))
                        .blocking_show()
                })
                .await
                .unwrap_or(false);

                if open_settings {
                    if let Err(e) = permissions::open_settings("full-disk-access") {
                        tracing::warn!("failed to open Full Disk Access settings: {e}");
                    }
                }
            }

            let state = app.state::<AppState>();
            {
                let mut cfg = state.cfg.lock().unwrap();
                if cfg.permissions_prompted {
                    return;
                }
                cfg.permissions_prompted = true;
            }
            let cfg = state.cfg.lock().unwrap().clone();
            if let Err(e) = persist_config(&app, &cfg) {
                tracing::warn!("failed to persist permission prompt flag: {e}");
            }
        });
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .on_window_event(|window, event| {
            // Closing a window just hides it; the app lives in the tray.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
                return;
            }
            // Bringing the panel forward from outside the page (window
            // switcher, dock) does not touch the page's edge-hide state, so the
            // window would stay parked off-screen. Treat focus as an explicit
            // request to show it.
            if window.label() == "thoughts" {
                if let tauri::WindowEvent::Focused(true) = event {
                    let _ = window.emit(THOUGHT_REVEAL_EVENT, ());
                }
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
            get_permission_status,
            open_permission_settings,
            reveal_app_in_finder,
            reset_media_library_permission,
        ])
        .setup(move |app| {
            let data_dir = app.path().app_data_dir()?;
            let config_path = data_dir.join("config.json");
            let mut cfg = load_config_from_file(&config_path);
            apply_log_level(&log_reload, &cfg.log_level)?;
            let thoughts = Arc::new(ThoughtBuffer::new(cfg.thought_panel_enabled));
            thoughts.attach_events(app.handle().clone());
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

            let icon = tray_image(include_bytes!("../icons/tray-icon.png"));
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
                activity: Arc::new(AtomicUsize::new(0)),
                log_reload: log_reload.clone(),
                tray_status: Mutex::new(Some(status)),
                tray_toggle: Mutex::new(Some(toggle)),
            });

            spawn_tray_animation(app.handle().clone());

            let _ = app.notification().request_permission();

            #[cfg(target_os = "macos")]
            let _ = app.set_dock_visibility(false);

            // Tray-manager contract: the gateway comes up with the app.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(start_after_trae_cli_check(app_handle));

            // Give the gateway a moment to settle before showing a permission
            // dialog, so it never competes with the TRAE CLI install prompt.
            let permission_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                maybe_prompt_permissions(permission_app);
            });

            let auto_check = cfg.auto_check_update;
            if auto_check {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    match check_update_availability(&app_handle).await {
                        Ok(info) if info.update_available => {
                            if let Some(latest) = info.latest_version {
                                notify_update_available(&app_handle, &latest);
                            }
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!("update check failed: {e}"),
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
            client_session: Some("client-a".into()),
            workdir: "/work/a".into(),
            workdir_reported: true,
            prompt: "first question".into(),
        });
        buffer.push(ThoughtUpdate::Delta {
            session_id: "session-a".into(),
            text: "first".into(),
        });
        buffer.push(ThoughtUpdate::Start {
            session_id: "session-b".into(),
            client_session: Some("client-b".into()),
            workdir: "/work/b".into(),
            workdir_reported: true,
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
        assert_eq!(
            snapshot.segments[0].client_session.as_deref(),
            Some("client-a")
        );
        assert!(snapshot.segments[0].ended_at.is_some());
        assert_eq!(snapshot.segments[1].text, "second");
        assert_eq!(snapshot.segments[1].prompt, "second question");
        assert_eq!(
            snapshot.segments[1].client_session.as_deref(),
            Some("client-b")
        );
        assert!(snapshot.segments[1].ended_at.is_none());
    }
}
