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

interface TauriWindowPoint {
  x: number;
  y: number;
}

interface TauriMonitorArea {
  position: TauriWindowPoint;
  size: TauriWindowSize;
}

interface TauriMonitor {
  name: string | null;
  position: TauriWindowPoint;
  size: TauriWindowSize;
  scaleFactor: number;
  workArea?: TauriMonitorArea | null;
}

interface TauriWindowHandle {
  innerSize(): Promise<TauriWindowSize>;
  outerSize(): Promise<TauriWindowSize>;
  outerPosition(): Promise<TauriWindowPoint>;
  scaleFactor(): Promise<number>;
  setSize(size: unknown): Promise<void>;
  setPosition(position: unknown): Promise<void>;
  close(): Promise<void>;
  setAlwaysOnTop(onTop: boolean): Promise<void>;
  isAlwaysOnTop(): Promise<boolean>;
}

interface TauriSizeCtor {
  new (width: number, height: number): unknown;
}

interface TauriPositionCtor {
  new (x: number, y: number): unknown;
}

interface TauriWindowModule {
  getCurrentWindow(): TauriWindowHandle;
  LogicalSize: TauriSizeCtor;
  PhysicalSize: TauriSizeCtor;
  PhysicalPosition: TauriPositionCtor;
  cursorPosition(): Promise<TauriWindowPoint | null>;
  currentMonitor(): Promise<TauriMonitor | null>;
}

interface TauriGlobal {
  core: TauriCore;
  event?: TauriEvent;
  window?: TauriWindowModule;
}

interface Window {
  __TAURI__?: TauriGlobal;
}
