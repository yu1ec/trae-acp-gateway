import { useCallback, useEffect, useRef, useState } from "react";
import {
  clearThoughts,
  getThoughts,
  listenThoughts,
  type ThoughtSegment,
  type ThoughtSnapshot,
} from "../api/tauri";

// The backend pushes an event as soon as the transcript changes, so this poll
// is only a safety net (missed events, event API unavailable). It is far slower
// than the old 300ms cadence because it is no longer the streaming source.
const POLL_MS = 1500;

export interface UseThoughtsResult {
  segments: ThoughtSegment[];
  enabled: boolean;
  clear: () => Promise<void>;
}

export function useThoughts(): UseThoughtsResult {
  const [segments, setSegments] = useState<ThoughtSegment[]>([]);
  const [enabled, setEnabled] = useState(false);
  const versionRef = useRef(-1);
  const clearGenRef = useRef(0);

  const clear = useCallback(async () => {
    clearGenRef.current += 1;
    versionRef.current = -1;
    setSegments([]);
    try {
      await clearThoughts();
    } catch {
      // window closing
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    let frame = 0;
    let inFlight = false;
    let queued = false;

    const fetchSnapshot = async () => {
      frame = 0;
      if (cancelled) return;
      // A fetch already contains everything up to its start; remember that
      // more arrived and re-run once it lands instead of stacking IPC calls.
      if (inFlight) {
        queued = true;
        return;
      }
      inFlight = true;
      const genAtStart = clearGenRef.current;
      try {
        const snap: ThoughtSnapshot = await getThoughts();
        if (cancelled || genAtStart !== clearGenRef.current) return;
        setEnabled(snap.enabled);
        if (snap.version !== versionRef.current) {
          versionRef.current = snap.version;
          setSegments(snap.segments);
        }
      } catch {
        // window closing
      } finally {
        inFlight = false;
        if (queued && !cancelled) {
          queued = false;
          schedule();
        }
      }
    };

    // Coalesce event bursts into one snapshot per frame: the backend can emit
    // dozens of updates while a long chunk is parsed, but the eye needs ~60
    // commits per second at most.
    function schedule() {
      if (cancelled || document.hidden || frame) return;
      frame = requestAnimationFrame(() => void fetchSnapshot());
    }

    void fetchSnapshot();
    const id = window.setInterval(() => void schedule(), POLL_MS);

    let unlisten: (() => void) | null = null;
    void listenThoughts(schedule)
      .then((off) => {
        if (cancelled) off();
        else unlisten = off;
      })
      .catch(() => {
        // Event API unavailable; the safety poll keeps the panel live.
      });

    // The window is only hidden on close, not destroyed; catch up when it
    // becomes visible again instead of relying on the next poll.
    const onVisibility = () => {
      if (!document.hidden) void schedule();
    };
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      cancelled = true;
      window.clearInterval(id);
      if (frame) cancelAnimationFrame(frame);
      unlisten?.();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);

  return { segments, enabled, clear };
}
