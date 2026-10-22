import { forwardRef } from "react";
import type { StoredLogLine } from "../../hooks/useLogs";
import LogLine from "./LogLine";

interface LogViewerProps {
  lines: StoredLogLine[];
  search: string;
  expandedJsonIds: Set<number>;
  onToggleJson: (id: number, opening: boolean) => void;
  onScroll: () => void;
}

const LogViewer = forwardRef<HTMLDivElement, LogViewerProps>(function LogViewer(
  { lines, search, expandedJsonIds, onToggleJson, onScroll },
  ref,
) {
  return (
    <div className="log-viewer" ref={ref} onScroll={onScroll}>
      {lines.map((line) => (
        <LogLine
          key={line.id}
          line={line}
          search={search}
          jsonOpen={expandedJsonIds.has(line.id)}
          onToggleJson={(opening) => onToggleJson(line.id, opening)}
        />
      ))}
    </div>
  );
});

export default LogViewer;
