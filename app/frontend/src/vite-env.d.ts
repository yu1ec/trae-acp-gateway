/// <reference types="vite/client" />

interface TauriCore {
  invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

interface TauriGlobal {
  core: TauriCore;
}

interface Window {
  __TAURI__?: TauriGlobal;
}
