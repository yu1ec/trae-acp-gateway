import { useCallback, useEffect, useRef, useState } from "react";
import {
  clearThoughts,
  getThoughts,
  type ThoughtSegment,
  type ThoughtSnapshot,
} from "../api/tauri";

const POLL_MS = 300;

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

    const tick = async () => {
      // The window is only hidden on close, not destroyed; skip IPC while it
      // is not visible.
      if (document.hidden) return;
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
      }
    };

    void tick();
    const id = window.setInterval(() => void tick(), POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, []);

  return { segments, enabled, clear };
}
