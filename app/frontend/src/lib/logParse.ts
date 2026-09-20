export type LogLevel =
  | "TRACE"
  | "DEBUG"
  | "INFO"
  | "WARN"
  | "ERROR"
  | "FATAL";

export const LOG_LEVELS: LogLevel[] = [
  "TRACE",
  "DEBUG",
  "INFO",
  "WARN",
  "ERROR",
  "FATAL",
];

const LOG_RE =
  /^(\d{4}-\d{2}-\d{2}T[\d:]+(?:\.\d+)?Z?)\s+(TRACE|DEBUG|INFO|WARN|ERROR|FATAL)\b/;

export interface ParsedLogLine {
  raw: string;
  timestamp?: string;
  level?: LogLevel;
  message: string;
}

export function parseLogLine(raw: string): ParsedLogLine {
  const match = raw.match(LOG_RE);
  if (!match) {
    return { raw, message: raw };
  }

  return {
    raw,
    timestamp: match[1],
    level: match[2] as LogLevel,
    message: raw.slice(match[0].length),
  };
}

export function lineMatchesSearch(line: ParsedLogLine, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return line.raw.toLowerCase().includes(q);
}

export function splitHighlight(text: string, query: string): Array<{ text: string; mark: boolean }> {
  const q = query.trim();
  if (!q) return [{ text, mark: false }];

  const lowerText = text.toLowerCase();
  const lowerQuery = q.toLowerCase();
  const parts: Array<{ text: string; mark: boolean }> = [];
  let start = 0;

  while (start < text.length) {
    const idx = lowerText.indexOf(lowerQuery, start);
    if (idx === -1) {
      parts.push({ text: text.slice(start), mark: false });
      break;
    }
    if (idx > start) {
      parts.push({ text: text.slice(start, idx), mark: false });
    }
    parts.push({ text: text.slice(idx, idx + q.length), mark: true });
    start = idx + q.length;
  }

  return parts;
}
