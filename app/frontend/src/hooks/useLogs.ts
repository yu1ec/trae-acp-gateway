import { useCallback, useEffect, useRef, useState } from "react";
import { clearLogs, getLogs } from "../api/tauri";
import { parseLogLine, type LogLevel, type ParsedLogLine } from "../lib/logParse";

const POLL_MS = 300;

export interface StoredLogLine extends ParsedLogLine {
  id: number;
}

export interface UseLogsResult {
  lines: StoredLogLine[];
  pending: number;
  clearPending: () => void;
  clear: () => Promise<void>;
}

function withIds(rawLines: string[], nextId: { current: number }): StoredLogLine[] {
  return rawLines.map((raw) => ({
    ...parseLogLine(raw),
    id: nextId.current++,
  }));
}

export function useLogs(paused: boolean): UseLogsResult {
  const [lines, setLines] = useState<StoredLogLine[]>([]);
  const [pending, setPending] = useState(0);
  const firstRef = useRef<string | null>(null);
  const nextIdRef = useRef(1);
  const ingestGenRef = useRef(0);

  useEffect(() => {
    if (!paused) {
      setPending(0);
    }
  }, [paused]);

  const clear = useCallback(async () => {
    ingestGenRef.current += 1;
    firstRef.current = null;
    setLines([]);
    setPending(0);
    try {
      await clearLogs();
    } catch {
      // window closing
    }
  }, []);

  useEffect(() => {
    let cancelled = false;

    const tick = async () => {
      const genAtStart = ingestGenRef.current;
      try {
        const rawLines = await getLogs();
        if (cancelled || genAtStart !== ingestGenRef.current) return;

        let addedCount = 0;

        setLines((prev) => {
          const domCount = prev.length;
          const first = firstRef.current;
          let added: string[] = [];
          let fullReset = false;

          if (!rawLines.length) {
            if (domCount) {
              firstRef.current = null;
              return [];
            }
            return prev;
          }

          if (!domCount) {
            added = rawLines;
            fullReset = true;
          } else if (rawLines[0] === first && rawLines.length >= domCount) {
            added = rawLines.slice(domCount);
          } else {
            const k = rawLines.indexOf(first ?? "");
            if (k > 0) {
              added = rawLines.slice(domCount - k);
              firstRef.current = rawLines[0] ?? null;
              addedCount = added.length;
              const rotated = prev.slice(k);
              const appended = withIds(added, nextIdRef);
              return [...rotated, ...appended];
            }
            added = rawLines;
            fullReset = true;
          }

          firstRef.current = rawLines[0] ?? null;
          addedCount = added.length;

          if (fullReset) {
            return withIds(added, nextIdRef);
          }

          const appended = withIds(added, nextIdRef);
          return [...prev, ...appended];
        });

        if (paused && addedCount > 0) {
          setPending((p) => p + addedCount);
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
  }, [paused]);

  return {
    lines,
    pending,
    clearPending: () => setPending(0),
    clear,
  };
}

export function filterLines(
  lines: StoredLogLine[],
  levels: Set<LogLevel>,
  search: string,
): StoredLogLine[] {
  const q = search.trim().toLowerCase();

  return lines.filter((line) => {
    if (line.level && !levels.has(line.level)) {
      return false;
    }
    if (q && !line.raw.toLowerCase().includes(q)) {
      return false;
    }
    return true;
  });
}
