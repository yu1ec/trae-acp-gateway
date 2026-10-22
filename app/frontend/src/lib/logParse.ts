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

export interface ParsedJson {
  prefix: string;
  pretty: string;
}

export interface ParsedLogLine {
  raw: string;
  timestamp?: string;
  level?: LogLevel;
  message: string;
  json?: ParsedJson;
}

const JSON_FIELD_RE = /\bline=([\[{])/;
const QUOTED_JSON_RE = /\bline="((?:\\.|[^"\\])*)"/;

/**
 * `line="{\"a\":1}"` — Rust prints the payload as an escaped string when the
 * log field is a `String`, so the JSON has to be unescaped before parsing.
 */
function parseQuotedJson(message: string): ParsedJson | undefined {
  const match = message.match(QUOTED_JSON_RE);
  if (!match) return undefined;

  try {
    const inner: unknown = JSON.parse(`"${match[1]}"`);
    if (typeof inner !== "string") return undefined;
    const value: unknown = JSON.parse(inner);
    if (value === null || typeof value !== "object") return undefined;
    return {
      prefix: message.slice(0, match.index ?? 0),
      pretty: JSON.stringify(value, null, 2),
    };
  } catch {
    return undefined;
  }
}

/** Pretty-print the compact JSON that debug logs put after `line=`. */
function extractJson(message: string): ParsedJson | undefined {
  const quoted = parseQuotedJson(message);
  if (quoted) return quoted;

  const match = message.match(JSON_FIELD_RE);
  const start = match
    ? (match.index ?? 0) + match[0].length - 1
    : Math.min(
        ...[message.indexOf("{"), message.indexOf("[")].filter((i) => i >= 0),
      );

  if (!Number.isFinite(start) || start < 0) {
    return undefined;
  }

  try {
    const value: unknown = JSON.parse(message.slice(start));
    if (value === null || typeof value !== "object") {
      return undefined;
    }
    return {
      prefix: message.slice(0, start),
      pretty: JSON.stringify(value, null, 2),
    };
  } catch {
    return undefined;
  }
}

export function parseLogLine(raw: string): ParsedLogLine {
  const match = raw.match(LOG_RE);
  if (!match) {
    return { raw, message: raw };
  }

  const message = raw.slice(match[0].length);
  return {
    raw,
    timestamp: match[1],
    level: match[2] as LogLevel,
    message,
    json: extractJson(message),
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
