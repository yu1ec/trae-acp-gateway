import { useCallback, useEffect, useRef, useState } from "react";
import {
  getAvailableMonitors,
  getCursorPosition,
  getWindowPosition,
  getWindowRect,
  setWindowPosition,
  type ScreenMonitor,
  type ScreenPoint,
  type WindowRect,
} from "../api/tauri";

export type EdgeSide = "left" | "right";

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
// How close a dragged panel must be to a screen edge before it attaches. The
// window has 8px of transparent padding, so this is forgiving enough that
// "flush against the edge" is easy to hit without feeling like a magnet.
const SNAP_LOGICAL = 24;
// The window is moved natively while dragging, so page timers do not run until
// the drag ends. Poll the window position and wait for it to stand still
// before deciding whether the drop landed on an edge.
const DRAG_POLL_MS = 140;
const DRAG_SETTLE_MS = 220;
// Ignore sub-pixel/rounding noise when comparing against the position this
// hook last moved the window to.
const DRAG_EPSILON = 2;
// setPosition is async; give the final frame time to land before treating a
// position difference as a user drag.
const PROGRAMMATIC_GRACE_MS = 120;

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

/**
 * Monitor used for snap decisions after a drag. Unlike `resolveMonitor` this
 * always prefers the display the window actually sits on: during a drag across
 * displays `currentMonitor` can briefly describe the one the drag started on.
 */
function resolveMonitorByOverlap(
  rect: WindowRect,
  monitors: ScreenMonitor[] | null,
): WindowRect {
  if (!monitors || monitors.length === 0) return rect;
  let best: ScreenMonitor | null = null;
  let bestOverlap = 0;
  for (const monitor of monitors) {
    const overlap = overlapArea(rect.position, rect.size, monitor);
    if (overlap > bestOverlap) {
      best = monitor;
      bestOverlap = overlap;
    }
  }
  if (best) return { ...rect, monitor: best };
  return resolveMonitor(rect, monitors);
}

/** Window geometry together with the monitor that is safe to dock against. */
async function getDockRect(): Promise<WindowRect | null> {
  const [rect, monitors] = await Promise.all([
    getWindowRect(),
    getAvailableMonitors(),
  ]);
  return rect ? resolveMonitor(rect, monitors) : null;
}

/** Like `getDockRect`, but prefers the display the panel is already docked on. */
async function getDockedRect(
  preferred: ScreenMonitor | null,
): Promise<WindowRect | null> {
  const [rect, monitors] = await Promise.all([
    getWindowRect(),
    getAvailableMonitors(),
  ]);
  if (!rect) return null;
  if (!monitors) return rect;
  if (preferred) {
    const still = monitors.find((m) => sameMonitor(m, preferred));
    if (still) return { ...rect, monitor: still };
  }
  return resolveMonitor(rect, monitors);
}

/** Like `getDockRect`, but picks the display by overlap for drag drops. */
async function getSnapRect(): Promise<WindowRect | null> {
  const [rect, monitors] = await Promise.all([
    getWindowRect(),
    getAvailableMonitors(),
  ]);
  return rect ? resolveMonitorByOverlap(rect, monitors) : null;
}

/** Which screen edge the window is closer to, judged by its centre. */
function nearestSide(rect: WindowRect): EdgeSide {
  const area = workArea(rect);
  if (!area) return "right";
  const windowCenter = rect.position.x + rect.size.width / 2;
  const areaCenter = area.position.x + area.size.width / 2;
  return windowCenter < areaCenter ? "left" : "right";
}

/**
 * Edge the window was dropped against, or null when it landed away from both.
 * Detection uses the monitor's full frame, not its work area: a Dock on the
 * side shrinks the work area, but the user still drags towards the physical
 * edge. Docking then uses the work area so the card never sits under the Dock.
 */
function snapSide(rect: WindowRect): EdgeSide | null {
  const monitor = rect.monitor;
  if (!monitor) return null;
  const threshold = Math.max(
    8,
    Math.round(SNAP_LOGICAL * monitor.scaleFactor),
  );
  const leftGap = rect.position.x - monitor.position.x;
  const rightGap =
    monitor.position.x +
    monitor.size.width -
    (rect.position.x + rect.size.width);
  if (leftGap <= threshold && leftGap <= rightGap) return "left";
  if (rightGap <= threshold) return "right";
  return null;
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
 * The panel is not switched on and off from a button: dropping it against the
 * left or right edge of any attached display attaches it, and dragging it away
 * from the edge detaches it. While attached, the window follows the cursor:
 * leaving the panel slides it mostly off-screen, leaving a hoverable strip;
 * hovering that strip slides it back. Display hot-plugs are watched so the
 * panel re-docks instead of staying at a coordinate that no longer belongs to
 * any screen. Position maths run in physical pixels (Tauri's window API),
 * while the strip width is logical, so the scale factor bridges the two.
 */
export function useEdgeHide() {
  const [snapped, setSnapped] = useState(false);
  const [hidden, setHidden] = useState(false);
  const [side, setSide] = useState<EdgeSide>("right");
  const snappedRef = useRef(false);
  const hiddenRef = useRef(false);
  const sideRef = useRef<EdgeSide>("right");
  // Incremented for every slide, so a superseded animation stops on its next
  // frame instead of fighting the newer target.
  const runRef = useRef(0);
  const missesRef = useRef(0);
  const stripRef = useRef<StripRect | null>(null);
  const hiddenAtRef = useRef(0);
  // Set when the panel was revealed by an activation that came from outside
  // the page (tray menu, window switcher). While held, the pointer is allowed
  // to be elsewhere without tucking the panel away again: the user can read
  // with the keyboard. Hovering the panel, or focusing another app, ends it.
  const holdRef = useRef(false);
  // The only writer of the window position besides the user is this hook, so a
  // position that no longer matches `targetRef` means the panel was dragged.
  const targetRef = useRef<ScreenPoint | null>(null);
  const animatingRef = useRef(false);
  const programmaticAtRef = useRef(0);
  const draggingRef = useRef(false);
  const lastMovedAtRef = useRef(0);
  // Display the panel is attached to. A parked window sits mostly off-screen,
  // so re-deriving its monitor from `currentMonitor` can land on a neighbour;
  // remember the display chosen when it was docked instead.
  const dockMonitorRef = useRef<ScreenMonitor | null>(null);

  const slideTo = useCallback((target: ScreenPoint): Promise<void> => {
    const run = ++runRef.current;
    animatingRef.current = true;
    return new Promise((resolve) => {
      void getWindowRect().then((rect) => {
        const finish = () => {
          // A newer slide (or an unsnap) owns the flag now.
          if (run === runRef.current) {
            targetRef.current = target;
            animatingRef.current = false;
            programmaticAtRef.current = performance.now();
          }
          resolve();
        };
        if (!rect || run !== runRef.current) {
          finish();
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
          else finish();
        };
        requestAnimationFrame(step);
      });
    });
  }, []);

  /** Attach the panel to `side`, flush against that display's work area. */
  const attach = useCallback(
    async (rect: WindowRect, nextSide: EdgeSide) => {
      snappedRef.current = true;
      hiddenRef.current = false;
      holdRef.current = false;
      missesRef.current = 0;
      stripRef.current = null;
      sideRef.current = nextSide;
      dockMonitorRef.current = rect.monitor;
      draggingRef.current = false;
      setSnapped(true);
      setHidden(false);
      setSide(nextSide);
      await slideTo(dockedTarget(rect, nextSide));
    },
    [slideTo],
  );

  /** Detach from the edge so the window can be dragged freely. */
  const unsnap = useCallback(() => {
    if (!snappedRef.current) return;
    snappedRef.current = false;
    hiddenRef.current = false;
    holdRef.current = false;
    missesRef.current = 0;
    stripRef.current = null;
    dockMonitorRef.current = null;
    setSnapped(false);
    setHidden(false);
    // Stop any slide that is still running; the drag is the new authority.
    runRef.current += 1;
    animatingRef.current = false;
  }, []);

  /**
   * Slide the panel back on screen. Returns true when it was actually parked,
   * so callers can tell a real reveal from a no-op. `hold` keeps it open after
   * an external activation until the pointer reaches it or focus moves away.
   */
  const reveal = useCallback(
    async (hold = false): Promise<boolean> => {
      if (!snappedRef.current || !hiddenRef.current) return false;
      const rect = await getDockedRect(dockMonitorRef.current);
      // A drag may have detached the panel while the geometry was loading.
      if (!rect || !snappedRef.current || !hiddenRef.current) return false;
      dockMonitorRef.current = rect.monitor;
      hiddenRef.current = false;
      setHidden(false);
      missesRef.current = 0;
      if (hold) holdRef.current = true;
      await slideTo(dockedTarget(rect, sideRef.current));
      return true;
    },
    [slideTo],
  );

  const hide = useCallback(async () => {
    if (!snappedRef.current || hiddenRef.current) return;
    holdRef.current = false;
    const rect = await getDockRect();
    // The panel may have been detached (or already tucked) while loading.
    if (!rect || !snappedRef.current || hiddenRef.current) return;
    dockMonitorRef.current = rect.monitor;
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

  /**
   * Keep a detached panel reachable. Unplugging the display it was floating on
   * leaves it at coordinates that no longer belong to any screen; nudge it
   * into the nearest work area instead of stranding it off-screen.
   */
  const keepOnScreen = useCallback(async () => {
    const [rect, monitors] = await Promise.all([
      getWindowRect(),
      getAvailableMonitors(),
    ]);
    if (!rect || !monitors) return;
    const reachable = monitors.some(
      (monitor) => overlapArea(rect.position, rect.size, monitor) > 0,
    );
    if (reachable) return;
    const area = workArea(resolveMonitor(rect, monitors));
    if (!area) return;
    const x = clamp(
      rect.position.x,
      area.position.x,
      area.position.x + area.size.width - rect.size.width,
    );
    const y = clampY(rect, area);
    if (x === rect.position.x && y === rect.position.y) return;
    runRef.current += 1;
    animatingRef.current = false;
    targetRef.current = { x, y };
    programmaticAtRef.current = performance.now();
    await setWindowPosition(x, y);
  }, []);

  /**
   * Re-align after the display layout changed. Unplugging a display can leave
   * the window parked at a coordinate that no longer exists, which strands the
   * panel off-screen until the app restarts. Recompute against the new work
   * area while keeping the current visible/tucked state. A detached panel is
   * left where the user put it unless that spot is gone entirely.
   */
  const resync = useCallback(async () => {
    if (!snappedRef.current) {
      await keepOnScreen();
      return;
    }
    const rect = await getDockRect();
    if (!rect || !snappedRef.current) return;
    dockMonitorRef.current = rect.monitor;
    const nextSide = nearestSide(rect);
    sideRef.current = nextSide;
    setSide(nextSide);
    if (hiddenRef.current) {
      const parked = parkedTarget(rect, nextSide);
      stripRef.current = stripRect(rect, nextSide, parked);
      hiddenAtRef.current = performance.now();
      // The old spot is off-screen, so there is nothing meaningful to animate.
      runRef.current += 1;
      animatingRef.current = false;
      targetRef.current = parked;
      programmaticAtRef.current = performance.now();
      await setWindowPosition(parked.x, parked.y);
      return;
    }
    await slideTo(dockedTarget(rect, nextSide));
  }, [keepOnScreen, slideTo]);

  // Display hot-plug watchdog. macOS does not notify the webview of monitor
  // changes, and the window is not guaranteed to move or resize, so polling the
  // attached monitors is the reliable signal that the layout needs re-docking
  // (or, for a detached panel, that its display went away).
  useEffect(() => {
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
  }, [resync]);

  // On open, attach immediately when the window already sits against an edge;
  // otherwise leave it floating where the OS placed it.
  useEffect(() => {
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void getSnapRect().then((rect) => {
        if (cancelled || !rect) return;
        const nextSide = snapSide(rect);
        if (nextSide) {
          void attach(rect, nextSide);
        } else {
          targetRef.current = rect.position;
        }
      });
    }, 120);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [attach]);

  // Drag watchdog. A native window drag does not report its live position to
  // the page, so compare the window against the position this hook last moved
  // it to. Any other position is the user: detach at once, then, once the
  // window stands still, attach again if it was dropped against an edge.
  useEffect(() => {
    if (hidden) return;
    let cancelled = false;

    const tick = () => {
      void getWindowPosition().then((position) => {
        if (cancelled || !position || hiddenRef.current) return;
        if (
          animatingRef.current ||
          performance.now() - programmaticAtRef.current <
            PROGRAMMATIC_GRACE_MS
        ) {
          return;
        }
        const target = targetRef.current;
        if (!target) {
          targetRef.current = position;
          return;
        }
        const moved =
          Math.abs(position.x - target.x) > DRAG_EPSILON ||
          Math.abs(position.y - target.y) > DRAG_EPSILON;
        if (moved) {
          // Track the live position so the next sample compares like with like.
          targetRef.current = position;
          draggingRef.current = true;
          lastMovedAtRef.current = performance.now();
          unsnap();
          return;
        }
        if (!draggingRef.current) return;
        if (performance.now() - lastMovedAtRef.current < DRAG_SETTLE_MS) return;
        draggingRef.current = false;
        void getSnapRect().then((snapRect) => {
          if (cancelled || !snapRect || hiddenRef.current) return;
          const nextSide = snapSide(snapRect);
          if (nextSide) {
            void attach(snapRect, nextSide);
          } else {
            targetRef.current = snapRect.position;
          }
        });
      });
    };

    const id = window.setInterval(tick, DRAG_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [hidden, attach, unsnap]);

  // While attached, watch the global cursor instead of relying on DOM leave
  // events: the window moves underneath the pointer, which makes hover events
  // unreliable exactly when they matter.
  useEffect(() => {
    if (!snapped || hidden) return;
    missesRef.current = 0;
    const id = window.setInterval(() => {
      if (!snappedRef.current || hiddenRef.current) return;
      void Promise.all([getCursorPosition(), getWindowRect()]).then(
        ([cursor, rect]) => {
          if (!cursor || !rect || hiddenRef.current) return;
          const inside =
            cursor.x >= rect.position.x - LEAVE_MARGIN &&
            cursor.x <= rect.position.x + rect.size.width + LEAVE_MARGIN &&
            cursor.y >= rect.position.y - LEAVE_MARGIN &&
            cursor.y <= rect.position.y + rect.size.height + LEAVE_MARGIN;
          if (inside) {
            // The pointer reached the panel; from here the usual rules apply.
            holdRef.current = false;
            missesRef.current = 0;
            return;
          }
          // An externally revealed panel stays put until the pointer visits it
          // (or the window loses focus), even if the cursor is far away.
          if (holdRef.current) return;
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
  }, [snapped, hidden, hide]);

  // Focus moving elsewhere is an explicit "I am done here": release the hold
  // so the ordinary pointer poll can tuck the panel away again. Without this,
  // a keyboard-only reveal would keep the panel pinned indefinitely.
  useEffect(() => {
    const release = () => {
      holdRef.current = false;
    };
    window.addEventListener("blur", release);
    return () => window.removeEventListener("blur", release);
  }, []);

  // The parked strip is a `pointerenter` target too, but WebKit can skip hover
  // events in an inactive app. Polling the cursor keeps the panel reachable
  // while it is tucked away; the wait after parking avoids catching the pointer
  // that is still on its way out.
  useEffect(() => {
    if (!snapped || !hidden) return;
    const id = window.setInterval(() => {
      if (!snappedRef.current || !hiddenRef.current) return;
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
  }, [snapped, hidden, reveal]);

  return { snapped, hidden, side, reveal };
}
