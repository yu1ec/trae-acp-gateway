import { useCallback, useEffect, useRef, useState } from "react";
import {
  getAvailableMonitors,
  getCursorPosition,
  getWindowRect,
  setWindowPosition,
  type ScreenMonitor,
  type ScreenPoint,
  type WindowRect,
} from "../api/tauri";

export type EdgeSide = "left" | "right";

const STORAGE_KEY = "trae-thoughts-edge-hide";
// Keep in sync with `.thoughts-edge-hotzone` width in thoughts.css.
const PEEK_LOGICAL = 10;
const SLIDE_MS = 180;
const POLL_MS = 160;
// Display hot-plug watchdog: cheap enough to run while the mode is on, and it
// is the only reliable signal the webview gets for monitor changes.
const MONITOR_POLL_MS = 1000;
// macOS rearranges windows over a few frames after a display change.
const MONITOR_SETTLE_MS = 350;
const LEAVE_MISSES = 2;
// The pointer may cross the transparent window padding without meaning to
// leave; a small margin keeps the panel from twitching away at the border.
const LEAVE_MARGIN = 6;
// Slack around the parked strip when checking the cursor from screen space.
const STRIP_MARGIN = 4;

function loadEnabled(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

function saveEnabled(enabled: boolean) {
  try {
    localStorage.setItem(STORAGE_KEY, String(enabled));
  } catch {
    // storage unavailable
  }
}

function clamp(value: number, min: number, max: number): number {
  return max < min ? min : Math.min(max, Math.max(min, value));
}

function workArea(rect: WindowRect): {
  position: ScreenPoint;
  size: TauriWindowSize;
} | null {
  if (!rect.monitor) return null;
  return rect.monitor.workArea ?? rect.monitor;
}

function monitorArea(monitor: ScreenMonitor): {
  position: ScreenPoint;
  size: TauriWindowSize;
} {
  return monitor.workArea ?? monitor;
}

/** Stable identity of a monitor's geometry, for spotting layout changes. */
function monitorKey(monitor: ScreenMonitor): string {
  const area = monitorArea(monitor);
  return [
    monitor.position.x,
    monitor.position.y,
    monitor.size.width,
    monitor.size.height,
    monitor.scaleFactor,
    area.position.x,
    area.position.y,
    area.size.width,
    area.size.height,
  ].join(":");
}

function sameMonitor(a: ScreenMonitor, b: ScreenMonitor): boolean {
  return monitorKey(a) === monitorKey(b);
}

/** Signature of the whole display layout; any hot-plug or rearrangement
 *  changes it. Sorted so monitor enumeration order does not matter. */
function layoutKey(monitors: ScreenMonitor[]): string {
  return monitors.map(monitorKey).sort().join("|");
}

function overlapArea(
  position: ScreenPoint,
  size: TauriWindowSize,
  monitor: ScreenMonitor,
): number {
  const area = monitorArea(monitor);
  const width =
    Math.min(position.x + size.width, area.position.x + area.size.width) -
    Math.max(position.x, area.position.x);
  const height =
    Math.min(position.y + size.height, area.position.y + area.size.height) -
    Math.max(position.y, area.position.y);
  return Math.max(0, width) * Math.max(0, height);
}

function distanceToMonitor(
  position: ScreenPoint,
  size: TauriWindowSize,
  monitor: ScreenMonitor,
): number {
  const area = monitorArea(monitor);
  const cx = position.x + size.width / 2;
  const cy = position.y + size.height / 2;
  const dx = Math.max(
    area.position.x - cx,
    0,
    cx - (area.position.x + area.size.width),
  );
  const dy = Math.max(
    area.position.y - cy,
    0,
    cy - (area.position.y + area.size.height),
  );
  return dx * dx + dy * dy;
}

/**
 * `currentMonitor` can lag behind a hot-plug, or still describe a display that
 * was just disconnected. Trust it only while it matches an attached monitor;
 * otherwise dock against the monitor the window overlaps most, falling back to
 * the nearest one when the window sits entirely off-screen.
 */
function resolveMonitor(
  rect: WindowRect,
  monitors: ScreenMonitor[] | null,
): WindowRect {
  if (!monitors) return rect;
  if (rect.monitor && monitors.some((m) => sameMonitor(m, rect.monitor!))) {
    return rect;
  }

  let best = monitors[0];
  let bestOverlap = 0;
  for (const monitor of monitors) {
    const overlap = overlapArea(rect.position, rect.size, monitor);
    if (overlap > bestOverlap) {
      best = monitor;
      bestOverlap = overlap;
    }
  }
  if (bestOverlap === 0) {
    let bestDistance = Number.POSITIVE_INFINITY;
    for (const monitor of monitors) {
      const distance = distanceToMonitor(rect.position, rect.size, monitor);
      if (distance < bestDistance) {
        best = monitor;
        bestDistance = distance;
      }
    }
  }
  return { ...rect, monitor: best };
}

/** Window geometry together with the monitor that is safe to dock against. */
async function getDockRect(): Promise<WindowRect | null> {
  const [rect, monitors] = await Promise.all([
    getWindowRect(),
    getAvailableMonitors(),
  ]);
  return rect ? resolveMonitor(rect, monitors) : null;
}

/** Which screen edge the window is closer to, judged by its centre. */
function nearestSide(rect: WindowRect): EdgeSide {
  const area = workArea(rect);
  if (!area) return "right";
  const windowCenter = rect.position.x + rect.size.width / 2;
  const areaCenter = area.position.x + area.size.width / 2;
  return windowCenter < areaCenter ? "left" : "right";
}

function clampY(
  rect: WindowRect,
  area: { position: ScreenPoint; size: TauriWindowSize },
) {
  return clamp(
    rect.position.y,
    area.position.y,
    area.position.y + area.size.height - rect.size.height,
  );
}

/** Showing: the window sits flush against its edge. */
function dockedTarget(rect: WindowRect, side: EdgeSide): ScreenPoint {
  const area = workArea(rect);
  if (!area) return { x: rect.position.x, y: rect.position.y };
  return {
    x:
      side === "right"
        ? area.position.x + area.size.width - rect.size.width
        : area.position.x,
    y: clampY(rect, area),
  };
}

/** Tucked away: only a thin strip stays on screen so it can be hovered back. */
function parkedTarget(rect: WindowRect, side: EdgeSide): ScreenPoint {
  const area = workArea(rect);
  if (!area) return { x: rect.position.x, y: rect.position.y };
  const peek = peekPixels(rect);
  return {
    x:
      side === "right"
        ? area.position.x + area.size.width - peek
        : area.position.x + peek - rect.size.width,
    y: clampY(rect, area),
  };
}

/** Window geometry is physical pixels while the CSS strip is logical pixels. */
function peekPixels(rect: WindowRect): number {
  return Math.max(
    3,
    Math.round(PEEK_LOGICAL * (rect.monitor?.scaleFactor ?? 1)),
  );
}

/** Screen-space rectangle where the parked window's visible strip lives. */
interface StripRect {
  x0: number;
  x1: number;
  y0: number;
  y1: number;
}

function stripRect(
  rect: WindowRect,
  side: EdgeSide,
  parked: ScreenPoint,
): StripRect | null {
  const area = workArea(rect);
  if (!area) return null;
  const peek = peekPixels(rect);
  const edge = area.position.x + area.size.width;
  return {
    x0:
      side === "right"
        ? edge - peek - STRIP_MARGIN
        : area.position.x - STRIP_MARGIN,
    x1:
      side === "right"
        ? edge + STRIP_MARGIN
        : area.position.x + peek + STRIP_MARGIN,
    y0: parked.y,
    y1: parked.y + rect.size.height,
  };
}

/**
 * Edge auto-hide for the floating thoughts panel.
 *
 * Enabling docks the window to whichever screen edge it is closest to. While
 * docked, the window follows the cursor: leaving the panel slides it mostly
 * off-screen, leaving a hoverable strip; hovering that strip slides it back.
 * Display hot-plugs are watched so the panel re-docks instead of staying at a
 * coordinate that no longer belongs to any screen.
 * Position maths run in physical pixels (Tauri's window API), while the strip
 * width is logical, so the scale factor bridges the two.
 */
export function useEdgeHide() {
  const [enabled, setEnabled] = useState(loadEnabled);
  const [hidden, setHidden] = useState(false);
  const [side, setSide] = useState<EdgeSide>("right");
  const enabledRef = useRef(enabled);
  const hiddenRef = useRef(false);
  const sideRef = useRef<EdgeSide>("right");
  // Incremented for every slide, so a superseded animation stops on its next
  // frame instead of fighting the newer target.
  const runRef = useRef(0);
  const missesRef = useRef(0);
  const stripRef = useRef<StripRect | null>(null);
  const hiddenAtRef = useRef(0);

  const slideTo = useCallback((target: ScreenPoint): Promise<void> => {
    const run = ++runRef.current;
    return new Promise((resolve) => {
      void getWindowRect().then((rect) => {
        if (!rect || run !== runRef.current) {
          resolve();
          return;
        }
        const from = rect.position;
        const started = performance.now();
        const step = () => {
          if (run !== runRef.current) {
            resolve();
            return;
          }
          const t = Math.min(1, (performance.now() - started) / SLIDE_MS);
          const eased = 1 - (1 - t) ** 3;
          void setWindowPosition(
            from.x + (target.x - from.x) * eased,
            from.y + (target.y - from.y) * eased,
          );
          if (t < 1) requestAnimationFrame(step);
          else resolve();
        };
        requestAnimationFrame(step);
      });
    });
  }, []);

  const dock = useCallback(
    async (rect: WindowRect) => {
      const nextSide = nearestSide(rect);
      sideRef.current = nextSide;
      setSide(nextSide);
      await slideTo(dockedTarget(rect, nextSide));
    },
    [slideTo],
  );

  const reveal = useCallback(async () => {
    if (!enabledRef.current || !hiddenRef.current) return;
    const rect = await getDockRect();
    if (!rect) return;
    hiddenRef.current = false;
    setHidden(false);
    missesRef.current = 0;
    await slideTo(dockedTarget(rect, sideRef.current));
  }, [slideTo]);

  const hide = useCallback(async () => {
    if (!enabledRef.current || hiddenRef.current) return;
    const rect = await getDockRect();
    if (!rect) return;
    const nextSide = nearestSide(rect);
    sideRef.current = nextSide;
    setSide(nextSide);
    hiddenRef.current = true;
    setHidden(true);
    const parked = parkedTarget(rect, nextSide);
    stripRef.current = stripRect(rect, nextSide, parked);
    hiddenAtRef.current = performance.now();
    await slideTo(parked);
  }, [slideTo]);

  const toggle = useCallback(async () => {
    if (enabledRef.current) {
      enabledRef.current = false;
      setEnabled(false);
      saveEnabled(false);
      // A hide may be mid-flight; bring the panel back so the feature can be
      // switched off from a fully visible window.
      if (hiddenRef.current) {
        hiddenRef.current = false;
        setHidden(false);
        const rect = await getDockRect();
        if (rect) await slideTo(dockedTarget(rect, sideRef.current));
      }
      return;
    }

    const rect = await getDockRect();
    enabledRef.current = true;
    setEnabled(true);
    saveEnabled(true);
    if (rect) await dock(rect);
  }, [dock, slideTo]);

  /**
   * Re-align after the display layout changed. Unplugging a display can leave
   * the window parked at a coordinate that no longer exists, which strands the
   * panel off-screen until the app restarts. Recompute against the new work
   * area while keeping the current visible/tucked state.
   */
  const resync = useCallback(async () => {
    if (!enabledRef.current) return;
    const rect = await getDockRect();
    if (!rect) return;
    const nextSide = nearestSide(rect);
    sideRef.current = nextSide;
    setSide(nextSide);
    if (hiddenRef.current) {
      const parked = parkedTarget(rect, nextSide);
      stripRef.current = stripRect(rect, nextSide, parked);
      hiddenAtRef.current = performance.now();
      // The old spot is off-screen, so there is nothing meaningful to animate.
      runRef.current += 1;
      await setWindowPosition(parked.x, parked.y);
      return;
    }
    await slideTo(dockedTarget(rect, nextSide));
  }, [slideTo]);

  // Display hot-plug watchdog. macOS does not notify the webview of monitor
  // changes, and the window is not guaranteed to move or resize, so polling the
  // attached monitors is the reliable signal that the layout needs re-docking.
  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    let known: string | null = null;
    let settleTimer = 0;

    const check = async () => {
      const monitors = await getAvailableMonitors();
      if (cancelled || !monitors) return;
      const next = layoutKey(monitors);
      if (known === null) {
        known = next;
        return;
      }
      if (next === known) return;
      known = next;
      // Let macOS finish rearranging windows before measuring again.
      window.clearTimeout(settleTimer);
      settleTimer = window.setTimeout(() => {
        if (!cancelled) void resync();
      }, MONITOR_SETTLE_MS);
    };

    void check();
    const id = window.setInterval(() => void check(), MONITOR_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
      window.clearTimeout(settleTimer);
    };
  }, [enabled, resync]);

  // Restore the remembered mode when the panel window opens: dock first, let
  // the pointer poll below tuck it away once the cursor is elsewhere.
  useEffect(() => {
    if (!enabledRef.current) return;
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void getDockRect().then((rect) => {
        if (cancelled || !rect || !enabledRef.current) return;
        void dock(rect);
      });
    }, 120);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [dock]);

  // While docked, watch the global cursor instead of relying on DOM leave
  // events: the window moves underneath the pointer, which makes hover events
  // unreliable exactly when they matter.
  useEffect(() => {
    if (!enabled || hidden) return;
    missesRef.current = 0;
    const id = window.setInterval(() => {
      if (!enabledRef.current || hiddenRef.current) return;
      void Promise.all([getCursorPosition(), getWindowRect()]).then(
        ([cursor, rect]) => {
          if (!cursor || !rect || hiddenRef.current) return;
          const inside =
            cursor.x >= rect.position.x - LEAVE_MARGIN &&
            cursor.x <= rect.position.x + rect.size.width + LEAVE_MARGIN &&
            cursor.y >= rect.position.y - LEAVE_MARGIN &&
            cursor.y <= rect.position.y + rect.size.height + LEAVE_MARGIN;
          if (inside) {
            missesRef.current = 0;
            return;
          }
          // One sample can land while the window is still sliding; require a
          // second consecutive miss before committing to hide.
          missesRef.current += 1;
          if (missesRef.current >= LEAVE_MISSES) {
            missesRef.current = 0;
            void hide();
          }
        },
      );
    }, POLL_MS);
    return () => window.clearInterval(id);
  }, [enabled, hidden, hide]);

  // The parked strip is a `pointerenter` target too, but WebKit can skip hover
  // events in an inactive app. Polling the cursor keeps the panel reachable
  // while it is tucked away; the wait after parking avoids catching the pointer
  // that is still on its way out.
  useEffect(() => {
    if (!enabled || !hidden) return;
    const id = window.setInterval(() => {
      if (!enabledRef.current || !hiddenRef.current) return;
      if (performance.now() - hiddenAtRef.current < SLIDE_MS + 120) return;
      const strip = stripRef.current;
      if (!strip) return;
      void getCursorPosition().then((cursor) => {
        if (!cursor || !hiddenRef.current) return;
        if (cursor.x < strip.x0 || cursor.x > strip.x1) return;
        if (cursor.y < strip.y0 || cursor.y > strip.y1) return;
        void reveal();
      });
    }, POLL_MS);
    return () => window.clearInterval(id);
  }, [enabled, hidden, reveal]);

  return { enabled, hidden, side, toggle, reveal };
}
