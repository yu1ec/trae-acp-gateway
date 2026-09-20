export interface AppConfig {
  port: number;
  workdir: string;
  trae_cmd: string;
  trae_args: string[];
  sandbox: boolean;
  debug: boolean;
  autostart: boolean;
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
