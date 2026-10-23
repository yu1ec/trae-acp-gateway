import { useCallback, useEffect, useRef, useState } from "react";
import {
  getCursorPosition,
  getWindowRect,
  setWindowPosition,
  type ScreenPoint,
  type WindowRect,
} from "../api/tauri";

export type EdgeSide = "left" | "right";

const STORAGE_KEY = "trae-thoughts-edge-hide";
// Keep in sync with `.thoughts-edge-hotzone` width in thoughts.css.
const PEEK_LOGICAL = 10;
const SLIDE_MS = 180;
const POLL_MS = 160;
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
    const rect = await getWindowRect();
    if (!rect) return;
    hiddenRef.current = false;
    setHidden(false);
    missesRef.current = 0;
    await slideTo(dockedTarget(rect, sideRef.current));
  }, [slideTo]);

  const hide = useCallback(async () => {
    if (!enabledRef.current || hiddenRef.current) return;
    const rect = await getWindowRect();
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
        const rect = await getWindowRect();
        if (rect) await slideTo(dockedTarget(rect, sideRef.current));
      }
      return;
    }

    const rect = await getWindowRect();
    enabledRef.current = true;
    setEnabled(true);
    saveEnabled(true);
    if (rect) await dock(rect);
  }, [dock, slideTo]);

  // Restore the remembered mode when the panel window opens: dock first, let
  // the pointer poll below tuck it away once the cursor is elsewhere.
  useEffect(() => {
    if (!enabledRef.current) return;
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void getWindowRect().then((rect) => {
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
