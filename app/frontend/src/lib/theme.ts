export type ThemeSetting = "system" | "light" | "dark";

type ResolvedTheme = "light" | "dark";

const THEME_STORAGE_KEY = "trae-theme";
const THEME_EVENT = "theme-changed";

export const THEME_OPTIONS: { value: ThemeSetting; label: string }[] = [
  { value: "system", label: "跟随系统" },
  { value: "light", label: "亮色" },
  { value: "dark", label: "暗黑" },
];

let current: ThemeSetting = "system";
const listeners = new Set<(theme: ThemeSetting) => void>();

function isThemeSetting(value: unknown): value is ThemeSetting {
  return value === "system" || value === "light" || value === "dark";
}

function prefersDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function loadTheme(): ThemeSetting {
  try {
    const raw = localStorage.getItem(THEME_STORAGE_KEY);
    if (isThemeSetting(raw)) return raw;
  } catch {
    // storage unavailable
  }
  return "system";
}

function resolve(theme: ThemeSetting): ResolvedTheme {
  if (theme === "system") return prefersDark() ? "dark" : "light";
  return theme;
}

/** Paint the resolved theme onto the document without persisting it. */
function paint(theme: ThemeSetting) {
  current = theme;
  const resolved = resolve(theme);
  const root = document.documentElement;
  root.dataset.theme = resolved;
  root.style.colorScheme = resolved;

  // Keep the native titlebar/scrollbars in step with the page. `null` means
  // "follow the OS", which also avoids forcing an appearance in system mode.
  try {
    const win = window.__TAURI__?.window?.getCurrentWindow();
    void win?.setTheme?.(theme === "system" ? null : resolved).catch(() => {});
  } catch {
    // window API unavailable
  }
}

function notify() {
  for (const listener of listeners) listener(current);
}

function remember(theme: ThemeSetting) {
  try {
    localStorage.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // storage unavailable
  }
}

/** Change the theme everywhere: persist it and tell the other windows. */
export function applyTheme(theme: ThemeSetting) {
  paint(theme);
  remember(theme);
  void window.__TAURI__?.event?.emit(THEME_EVENT, theme).catch(() => {});
  notify();
}

/** Subscribe to theme changes made by this or another window. */
export function subscribeTheme(
  listener: (theme: ThemeSetting) => void,
): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/**
 * Apply the saved theme before the first render. Every window shares the same
 * origin, so localStorage already holds the choice; the Tauri event keeps
 * already-open windows in step while the app is running.
 */
export function initTheme() {
  if (window.location.hash.includes("/thoughts")) {
    document.documentElement.dataset.window = "thoughts";
  }
  paint(loadTheme());

  window
    .matchMedia("(prefers-color-scheme: dark)")
    .addEventListener("change", () => {
      if (current !== "system") return;
      paint(current);
      notify();
    });

  window.addEventListener("storage", (event) => {
    if (event.key !== THEME_STORAGE_KEY) return;
    if (!isThemeSetting(event.newValue)) return;
    paint(event.newValue);
    notify();
  });

  const events = window.__TAURI__?.event;
  if (events) {
    void events
      .listen<ThemeSetting>(THEME_EVENT, (event) => {
        if (!isThemeSetting(event.payload)) return;
        remember(event.payload);
        paint(event.payload);
        notify();
      })
      .catch(() => {});
  }
}
