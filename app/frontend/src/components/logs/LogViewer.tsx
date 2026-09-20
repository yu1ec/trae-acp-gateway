import { forwardRef } from "react";
import type { ParsedLogLine } from "../../lib/logParse";
import LogLine from "./LogLine";

interface LogViewerProps {
  lines: ParsedLogLine[];
  search: string;
  onScroll: () => void;
}

const LogViewer = forwardRef<HTMLDivElement, LogViewerProps>(function LogViewer(
  { lines, search, onScroll },
  ref,
) {
  return (
    <div className="log-viewer" ref={ref} onScroll={onScroll}>
      {lines.map((line, i) => (
        <LogLine key={`${line.raw}-${i}`} line={line} search={search} />
      ))}
    </div>
  );
});

export default LogViewer;
