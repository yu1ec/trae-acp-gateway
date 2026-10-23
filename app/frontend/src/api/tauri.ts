export type LogLevelSetting =
  | "error"
  | "warn"
  | "info"
  | "debug"
  | "trace";

export const LOG_LEVEL_OPTIONS: { value: LogLevelSetting; label: string }[] = [
  { value: "error", label: "Error" },
  { value: "warn", label: "Warn" },
  { value: "info", label: "Info" },
  { value: "debug", label: "Debug" },
  { value: "trace", label: "Trace" },
];

export interface AppConfig {
  port: number;
  workdir?: string;
  trae_cmd: string;
  trae_args: string[];
  sandbox: boolean;
  log_level: LogLevelSetting;
  autostart: boolean;
  auto_check_update: boolean;
  thought_panel_enabled: boolean;
}

export interface ThoughtSegment {
  session_id: string;
  /** Client conversation id, stable across turns of the same session. */
  client_session: string | null;
  workdir: string;
  started_at: number;
  ended_at: number | null;
  prompt: string;
  text: string;
}

export interface ThoughtSnapshot {
  segments: ThoughtSegment[];
  version: number;
  enabled: boolean;
}

export interface UpdateCheckResult {
  current_version: string;
  latest_version: string | null;
  update_available: boolean;
  release_url: string;
  installer_name: string | null;
  download_url: string | null;
}

export interface UpdateDownloadProgress {
  downloaded: number;
  total: number | null;
  bytes_per_sec: number;
}

export interface TraeCliStatus {
  installed: boolean;
  path: string | null;
}

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const tauri = window.__TAURI__;
  if (!tauri) {
    return Promise.reject(new Error("Tauri API not available"));
  }
  return tauri.core.invoke<T>(cmd, args);
}

export function getConfig(): Promise<AppConfig> {
  return invoke<AppConfig>("get_config");
}

export function saveConfig(cfg: AppConfig): Promise<void> {
  return invoke<void>("save_config", { cfg });
}

export function getLogs(): Promise<string[]> {
  return invoke<string[]>("get_logs");
}

export function clearLogs(): Promise<void> {
  return invoke<void>("clear_logs");
}

export function getThoughts(): Promise<ThoughtSnapshot> {
  return invoke<ThoughtSnapshot>("get_thoughts");
}

export function clearThoughts(): Promise<void> {
  return invoke<void>("clear_thoughts");
}

export function showSettingsWindow(): Promise<void> {
  return invoke<void>("show_settings_window");
}

export function showThoughtsWindow(): Promise<void> {
  return invoke<void>("show_thoughts_window");
}

export function getAppVersion(): Promise<string> {
  return invoke<string>("get_app_version");
}

export function getTraeCliStatus(): Promise<TraeCliStatus> {
  return invoke<TraeCliStatus>("get_trae_cli_status");
}

export function installTraeCli(): Promise<TraeCliStatus> {
  return invoke<TraeCliStatus>("install_trae_cli_command");
}

export function checkForUpdate(): Promise<UpdateCheckResult> {
  return invoke<UpdateCheckResult>("check_for_update");
}

export function downloadAndInstallUpdate(accelerate: boolean): Promise<void> {
  return invoke<void>("download_and_install_update", { accelerate });
}

export function listenUpdateProgress(
  handler: (progress: UpdateDownloadProgress) => void,
): Promise<() => void> {
  const tauri = window.__TAURI__;
  if (!tauri?.event) {
    return Promise.reject(new Error("Tauri event API not available"));
  }
  return tauri.event.listen<UpdateDownloadProgress>(
    "update-download-progress",
    (ev) => handler(ev.payload),
  );
}

export function openExternalUrl(url: string): Promise<void> {
  return invoke<void>("plugin:opener|open_url", { url });
}

function currentWindow(): TauriWindowHandle | null {
  return window.__TAURI__?.window?.getCurrentWindow() ?? null;
}

export async function getWindowSize(): Promise<TauriWindowSize | null> {
  try {
    return (await currentWindow()?.innerSize()) ?? null;
  } catch {
    return null;
  }
}

/** Resize the current window using logical pixels. */
export async function setWindowLogicalSize(
  width: number,
  height: number,
): Promise<void> {
  const win = currentWindow();
  const LogicalSize = window.__TAURI__?.window?.LogicalSize;
  if (!win || !LogicalSize) return;
  await win.setSize(new LogicalSize(width, height));
}

/** Restore a size previously reported by `getWindowSize` (physical pixels). */
export async function setWindowPhysicalSize(
  size: TauriWindowSize,
): Promise<void> {
  const win = currentWindow();
  const PhysicalSize = window.__TAURI__?.window?.PhysicalSize;
  if (!win || !PhysicalSize) return;
  await win.setSize(new PhysicalSize(size.width, size.height));
}

export async function getWindowScaleFactor(): Promise<number> {
  try {
    return (await currentWindow()?.scaleFactor()) ?? 1;
  } catch {
    return 1;
  }
}

export async function closeCurrentWindow(): Promise<void> {
  await currentWindow()?.close();
}

/** Pin the current window above every other app window. */
export async function setWindowAlwaysOnTop(onTop: boolean): Promise<void> {
  await currentWindow()?.setAlwaysOnTop(onTop);
}

export async function isWindowAlwaysOnTop(): Promise<boolean | null> {
  try {
    return (await currentWindow()?.isAlwaysOnTop()) ?? null;
  } catch {
    return null;
  }
}
