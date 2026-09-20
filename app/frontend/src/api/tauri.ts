export interface AppConfig {
  port: number;
  workdir: string;
  trae_cmd: string;
  trae_args: string[];
  sandbox: boolean;
  debug: boolean;
  autostart: boolean;
  auto_check_update: boolean;
}

export interface UpdateCheckResult {
  current_version: string;
  latest_version: string | null;
  update_available: boolean;
  release_url: string;
  installer_name: string | null;
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

export function pickWorkdir(): Promise<string | null> {
  return invoke<string | null>("pick_workdir");
}

export function getLogs(): Promise<string[]> {
  return invoke<string[]>("get_logs");
}

export function getAppVersion(): Promise<string> {
  return invoke<string>("get_app_version");
}

export function checkForUpdate(): Promise<UpdateCheckResult> {
  return invoke<UpdateCheckResult>("check_for_update");
}

export function downloadAndInstallUpdate(): Promise<void> {
  return invoke<void>("download_and_install_update");
}
