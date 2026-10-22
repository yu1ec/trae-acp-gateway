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
