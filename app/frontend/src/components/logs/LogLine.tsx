import { splitHighlight, type ParsedLogLine } from "../../lib/logParse";

interface LogLineProps {
  line: ParsedLogLine;
  search: string;
}

function HighlightedText({ text, query }: { text: string; query: string }) {
  const parts = splitHighlight(text, query);
  return (
    <>
      {parts.map((part, i) =>
        part.mark ? (
          <mark key={i}>{part.text}</mark>
        ) : (
          <span key={i}>{part.text}</span>
        ),
      )}
    </>
  );
}

export default function LogLine({ line, search }: LogLineProps) {
  const isErr = line.level === "ERROR" || line.level === "FATAL";

  if (!line.level || !line.timestamp) {
    return (
      <div className="log-line">
        <HighlightedText text={line.raw} query={search} />
      </div>
    );
  }

  return (
    <div className={`log-line${isErr ? " err" : ""}`}>
      <span className="ts">{line.timestamp} </span>
      <span className={`lv-${line.level}`}>{line.level}</span>
      <HighlightedText text={line.message} query={search} />
    </div>
  );
}
