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

interface TauriWindowSize {
  width: number;
  height: number;
}

interface TauriWindowHandle {
  innerSize(): Promise<TauriWindowSize>;
  scaleFactor(): Promise<number>;
  setSize(size: unknown): Promise<void>;
  close(): Promise<void>;
  setAlwaysOnTop(onTop: boolean): Promise<void>;
  isAlwaysOnTop(): Promise<boolean>;
}

interface TauriSizeCtor {
  new (width: number, height: number): unknown;
}

interface TauriWindowModule {
  getCurrentWindow(): TauriWindowHandle;
  LogicalSize: TauriSizeCtor;
  PhysicalSize: TauriSizeCtor;
}

interface TauriGlobal {
  core: TauriCore;
  event?: TauriEvent;
  window?: TauriWindowModule;
}

interface Window {
  __TAURI__?: TauriGlobal;
}
