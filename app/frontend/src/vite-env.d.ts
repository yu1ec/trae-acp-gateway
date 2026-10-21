/// <reference types="vite/client" />

interface TauriCore {
  invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

interface TauriEventPayload<T> {
  payload: T;
}

interface TauriEvent {
  listen<T>(
    event: string,
    handler: (event: TauriEventPayload<T>) => void,
  ): Promise<() => void>;
}

interface TauriGlobal {
  core: TauriCore;
  event?: TauriEvent;
}

interface Window {
  __TAURI__?: TauriGlobal;
}
