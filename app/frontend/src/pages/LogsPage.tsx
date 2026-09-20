import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import LogToolbar from "../components/logs/LogToolbar";
import LogViewer from "../components/logs/LogViewer";
import { filterLines, useLogs } from "../hooks/useLogs";
import { LOG_LEVELS, type LogLevel } from "../lib/logParse";
import "../styles/logs.css";

const defaultLevels = () => new Set<LogLevel>(LOG_LEVELS);

export default function LogsPage() {
  const [paused, setPaused] = useState(false);
  const [search, setSearch] = useState("");
  const [levels, setLevels] = useState<Set<LogLevel>>(defaultLevels);
  const { lines, pending, clearPending } = useLogs(paused);
  const viewerRef = useRef<HTMLDivElement>(null);

  const visibleLines = useMemo(
    () => filterLines(lines, levels, search),
    [lines, levels, search],
  );

  const goBottom = useCallback(() => {
    const el = viewerRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, []);

  const atBottom = useCallback(() => {
    const el = viewerRef.current;
    if (!el) return true;
    return el.scrollTop + el.clientHeight >= el.scrollHeight - 4;
  }, []);

  useEffect(() => {
    if (!paused) {
      requestAnimationFrame(goBottom);
    }
  }, [lines.length, paused, goBottom]);

  const handleTogglePause = () => {
    setPaused((p) => {
      const next = !p;
      if (!next) {
        clearPending();
        requestAnimationFrame(goBottom);
      }
      return next;
    });
  };

  const handleScroll = () => {
    const wantPaused = !atBottom();
    if (wantPaused !== paused) {
      setPaused(wantPaused);
      if (!wantPaused) clearPending();
    }
  };

  const toggleLevel = (level: LogLevel) => {
    setLevels((prev) => {
      const next = new Set(prev);
      if (next.has(level)) next.delete(level);
      else next.add(level);
      return next;
    });
  };

  return (
    <div className="logs-page">
      <LogToolbar
        search={search}
        onSearchChange={setSearch}
        levels={levels}
        onToggleLevel={toggleLevel}
      />
      <LogViewer
        ref={viewerRef}
        lines={visibleLines}
        search={search}
        onScroll={handleScroll}
      />
      <button
        type="button"
        className={`scroll-ctl${paused ? " resume" : ""}`}
        onClick={handleTogglePause}
      >
        {paused ? `▶ Resume (${pending} new)` : "⏸ Pause"}
      </button>
    </div>
  );
}
