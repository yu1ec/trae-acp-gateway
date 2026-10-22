import { useState } from "react";
import { splitHighlight, type ParsedLogLine } from "../../lib/logParse";

interface LogLineProps {
  line: ParsedLogLine;
  search: string;
}

/** One-line summary of the pretty JSON shown while the block is collapsed. */
function jsonPreview(pretty: string, max = 120): string {
  const compact = pretty.replace(/\s+/g, " ").trim();
  return compact.length > max ? `${compact.slice(0, max)}…` : compact;
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
  const [jsonOpen, setJsonOpen] = useState(false);

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
      {line.json ? (
        <div className="log-json-block">
          <HighlightedText text={line.json.prefix} query={search} />
          <button
            type="button"
            className="log-json-toggle"
            aria-expanded={jsonOpen}
            title={jsonOpen ? "Collapse JSON" : "Expand JSON"}
            onClick={() => setJsonOpen((open) => !open)}
          >
            <span className="log-json-caret">{jsonOpen ? "▾" : "▸"}</span>
            {jsonOpen ? (
              "JSON"
            ) : (
              <span className="log-json-preview">
                <HighlightedText
                  text={jsonPreview(line.json.pretty)}
                  query={search}
                />
              </span>
            )}
          </button>
          {jsonOpen && (
            <pre className="log-json">
              <HighlightedText text={line.json.pretty} query={search} />
            </pre>
          )}
        </div>
      ) : (
        <HighlightedText text={line.message} query={search} />
      )}
    </div>
  );
}
