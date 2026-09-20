import { useEffect, useRef, useState } from "react";
import { getLogs } from "../api/tauri";
import { parseLogLine, type LogLevel, type ParsedLogLine } from "../lib/logParse";

const POLL_MS = 300;

export interface UseLogsResult {
  lines: ParsedLogLine[];
  pending: number;
  clearPending: () => void;
}

export function useLogs(paused: boolean): UseLogsResult {
  const [lines, setLines] = useState<ParsedLogLine[]>([]);
  const [pending, setPending] = useState(0);
  const firstRef = useRef<string | null>(null);

  useEffect(() => {
    if (!paused) {
      setPending(0);
    }
  }, [paused]);

  useEffect(() => {
    let cancelled = false;

    const tick = async () => {
      try {
        const rawLines = await getLogs();
        if (cancelled) return;

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
              return [...prev.slice(k), ...added.map(parseLogLine)];
            }
            added = rawLines;
            fullReset = true;
          }

          firstRef.current = rawLines[0] ?? null;
          addedCount = added.length;

          if (fullReset) {
            return added.map(parseLogLine);
          }

          return [...prev, ...added.map(parseLogLine)];
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

  return { lines, pending, clearPending: () => setPending(0) };
}

export function filterLines(
  lines: ParsedLogLine[],
  levels: Set<LogLevel>,
  search: string,
): ParsedLogLine[] {
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
