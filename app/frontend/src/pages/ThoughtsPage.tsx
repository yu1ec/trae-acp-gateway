import {
  memo,
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import Markdown, { type Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import {
  closeCurrentWindow,
  getWindowScaleFactor,
  getWindowSize,
  openExternalUrl,
  setWindowAlwaysOnTop,
  setWindowLogicalSize,
  setWindowPhysicalSize,
  showSettingsWindow,
  type ThoughtSegment,
} from "../api/tauri";
import { useThoughts } from "../hooks/useThoughts";
import { useEdgeHide } from "../hooks/useEdgeHide";
import "../styles/thoughts.css";

const OPACITY_KEY = "trae-thoughts-opacity";
const PINNED_KEY = "trae-thoughts-pinned";
const EXPANDED_SIZE_KEY = "trae-thoughts-expanded-size";
const HIDE_SHORT_KEY = "trae-thoughts-hide-short";
const DEFAULT_OPACITY = 0.82;
// The rolled-up state is a single pill; keep the logical window height in sync
// so the transparent area does not cover the desktop behind it.
const COLLAPSED_HEIGHT = 60;
// Collapsed segments render a one-line ticker; only the tail can be visible,
// so keeping the last few hundred characters bounds the DOM for long turns.
const TICKER_CHARS = 400;
// Auxiliary gateway requests (title generation, summaries) finish quickly and
// think briefly. The protocol has no request type, so flag them heuristically
// and let the user hide them.
const SHORT_TURN_CHARS = 600;
const SHORT_TURN_MS = 6000;
// Once a turn owns the focus, let it keep it until it has been quiet for a
// moment; concurrent requests often write in the same window and swapping on
// every snapshot would make the panel flicker.
const FOCUS_HOLD_MS = 1200;
// The one open segment is the scroll anchor: new text always grows here, so
// the viewport never chases output that appears in a different block.
const FOCUS_SELECTOR = '[data-focus-segment="true"]';

function loadOpacity(): number {
  try {
    const raw = Number(localStorage.getItem(OPACITY_KEY));
    if (Number.isFinite(raw) && raw >= 0.15 && raw <= 1) return raw;
  } catch {
    // storage unavailable
  }
  return DEFAULT_OPACITY;
}

function loadPinned(): boolean | null {
  try {
    const raw = localStorage.getItem(PINNED_KEY);
    if (raw === "true") return true;
    if (raw === "false") return false;
  } catch {
    // storage unavailable
  }
  return null;
}

function loadExpandedSize(): TauriWindowSize | null {
  try {
    const raw = localStorage.getItem(EXPANDED_SIZE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<TauriWindowSize>;
    if (
      typeof parsed.width === "number" &&
      typeof parsed.height === "number" &&
      parsed.width > 0 &&
      parsed.height > 0
    ) {
      return { width: parsed.width, height: parsed.height };
    }
  } catch {
    // storage unavailable or malformed
  }
  return null;
}

function saveExpandedSize(size: TauriWindowSize) {
  try {
    localStorage.setItem(EXPANDED_SIZE_KEY, JSON.stringify(size));
  } catch {
    // storage unavailable
  }
}

function loadHideShort(): boolean {
  try {
    return localStorage.getItem(HIDE_SHORT_KEY) === "true";
  } catch {
    return false;
  }
}

function formatClock(ms: number): string {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return "";
  const time = date.toLocaleTimeString("zh-CN", { hour12: false });
  const today = new Date();
  const sameDay =
    date.getFullYear() === today.getFullYear() &&
    date.getMonth() === today.getMonth() &&
    date.getDate() === today.getDate();
  if (sameDay) return time;
  const day = `${String(date.getMonth() + 1).padStart(2, "0")}-${String(
    date.getDate(),
  ).padStart(2, "0")}`;
  return `${day} ${time}`;
}

function formatDuration(startedAt: number, endedAt: number): string {
  const seconds = Math.max(0, Math.round((endedAt - startedAt) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m${String(seconds % 60).padStart(2, "0")}s`;
}

function basename(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

// One gateway request starts a fresh ACP session, so the per-turn id cannot
// identify the conversation. The client's own session id does; give each one a
// stable colour so interleaved turns stay attributable at a glance.
const SESSION_HUES = [152, 193, 39, 275, 340, 218];
// The group header reads as "project · opening summary"; it needs enough room
// to describe how the conversation started, but still fits on one row.
const PROMPT_HINT_CHARS = 56;
// A turn that only carried screenshots has no textual opening; skip it when
// picking the summary instead of labelling the group "[Image #1]".
const IMAGE_MARKER = /\[image #\d+\]/gi;

function sessionHue(key: string): number {
  let hash = 0;
  for (let i = 0; i < key.length; i += 1) {
    hash = (hash * 31 + key.charCodeAt(i)) | 0;
  }
  return SESSION_HUES[Math.abs(hash) % SESSION_HUES.length];
}

/** Grouping key: the client conversation when there is one, else this turn. */
function segmentKey(segment: ThoughtSegment): string {
  const client = segment.client_session?.trim();
  return client ? `client:${client}` : `turn:${segment.session_id}`;
}

/** One conversation: every turn that shares a client session id. */
interface ThoughtGroup {
  key: string;
  workdir: string;
  segments: ThoughtSegment[];
  startedAt: number;
  lastActivity: number;
  live: boolean;
}

interface GroupMeta {
  /** Human name: the project folder, disambiguated only when it must be. */
  name: string;
  /** Opening-content summary of the conversation, truncated for the header. */
  hint: string;
  /** Untruncated opening, shown in the tooltip. */
  opening: string;
  hue: number;
}

function groupThoughts(segments: ThoughtSegment[]): ThoughtGroup[] {
  const groups = new Map<string, ThoughtGroup>();
  for (const segment of segments) {
    const key = segmentKey(segment);
    let group = groups.get(key);
    if (!group) {
      group = {
        key,
        workdir: segment.workdir,
        segments: [],
        startedAt: segment.started_at,
        lastActivity: segment.ended_at ?? segment.started_at,
        live: false,
      };
      groups.set(key, group);
    }
    group.segments.push(segment);
    // A conversation's title/summary requests carry no cwd, so they would
    // otherwise name the group after the gateway's fallback (often `/`). Prefer
    // any turn that actually reported where it ran.
    if (segment.workdir_reported) group.workdir = segment.workdir;
    group.startedAt = Math.min(group.startedAt, segment.started_at);
    group.lastActivity = Math.max(
      group.lastActivity,
      segment.ended_at ?? segment.started_at,
    );
    group.live = group.live || segment.ended_at == null;
  }

  // Live conversations first, then most recent activity, so the session the
  // user is watching never sinks below finished history.
  return [...groups.values()].sort((a, b) => {
    if (a.live !== b.live) return a.live ? -1 : 1;
    return b.lastActivity - a.lastActivity;
  });
}

function promptHint(prompt: string): string {
  const line = prompt
    .replace(IMAGE_MARKER, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (!line) return "";
  return line.length > PROMPT_HINT_CHARS
    ? `${line.slice(0, PROMPT_HINT_CHARS)}…`
    : line;
}

/** Strip the protocol wrappers clients put around a prompt (e.g. `<session>`),
 *  so the row shows the question rather than the envelope. */
function displayPrompt(prompt: string): string {
  // Strip invisible padding (BOM, zero-width) first: it defeats the `^` anchor
  // and leaves the envelope visibly intact.
  const trimmed = prompt
    .replace(/[\uFEFF\u200B-\u200D\u2060]/g, "")
    .trim()
    .replace(/\s+/g, " ");
  const bare = trimmed
    .replace(/^(?:<[^>]+>\s*)+/, "")
    .replace(/(?:\s*<\/[^>]+>)+$/, "")
    .trim();
  return bare || trimmed;
}

/** Hosts prepend environment and reminder material to the first turn. It
 *  describes the machine rather than the conversation, so it must not become
 *  the group's summary. */
function isInjectedPrompt(text: string): boolean {
  const t = text.trimStart();
  return (
    t.startsWith("# Environment") ||
    t.startsWith("<environment") ||
    t.startsWith("<system-reminder>") ||
    t.startsWith("You have been invoked in the following environment")
  );
}

/** Opening content: the first turn that actually carries a question, so
 *  image-only or context-only first turns do not claim the label. */
function openingPrompt(group: ThoughtGroup): string {
  const prompts = group.segments
    .map((segment) => displayPrompt(segment.prompt))
    .filter(Boolean);
  const usable = prompts.find((prompt) => {
    const text = prompt.replace(IMAGE_MARKER, "").trim();
    // A bare slash command says what the user typed, not what they asked for.
    return (
      text.length > 0 &&
      !isInjectedPrompt(text) &&
      !/^\/[a-z][\w:-]*$/i.test(text)
    );
  });
  return usable ?? "";
}

/** A finished turn that was short and quick is almost always an auxiliary
 *  request (title, summary), not something the user is waiting on. */
function isShortTurn(segment: ThoughtSegment): boolean {
  if (segment.ended_at == null) return false;
  return (
    segment.text.trim().length < SHORT_TURN_CHARS &&
    segment.ended_at - segment.started_at < SHORT_TURN_MS
  );
}

function turnChars(segment: ThoughtSegment): number {
  return segment.text.trim().length;
}

/** Project label for a group header. `/` is a real workdir but has no folder
 *  name, so say what it is instead of showing a lone slash. */
function workdirLabel(workdir: string): string {
  const base = basename(workdir);
  if (base && base !== "/") return base;
  return workdir.trim() === "/" ? "根目录" : "未标记会话";
}

/**
 * Name conversations the way the user thinks about them: the project folder
 * plus how the conversation opened. Only conversations whose lifetimes overlap
 * get an ordinal, so two turns in the same project at different times never
 * look like two running windows.
 */
function describeGroups(groups: ThoughtGroup[]): Map<string, GroupMeta> {
  const byName = new Map<string, ThoughtGroup[]>();
  for (const group of groups) {
    const name = workdirLabel(group.workdir);
    const list = byName.get(name);
    if (list) list.push(group);
    else byName.set(name, [group]);
  }

  const meta = new Map<string, GroupMeta>();
  for (const [name, list] of byName) {
    const ordered = [...list].sort((a, b) => a.startedAt - b.startedAt);
    const ranges = ordered.map((group) => ({
      start: group.startedAt,
      end: group.live ? Number.POSITIVE_INFINITY : group.lastActivity,
    }));
    let ordinal = 0;
    ordered.forEach((group, index) => {
      const concurrent = ranges.some(
        (range, other) =>
          other !== index &&
          range.start <= ranges[index].end &&
          ranges[index].start <= range.end,
      );
      if (concurrent) ordinal += 1;
      const opening = openingPrompt(group);
      meta.set(group.key, {
        name: concurrent ? `${name} · ${ordinal}` : name,
        hint: promptHint(opening),
        opening,
        hue: sessionHue(group.key),
      });
    });
  }
  return meta;
}

/** Coloured conversation tag: the panel's answer to "whose thought is this?" */
function SessionBadge({
  label,
  hue,
  sessionId,
}: {
  label: string;
  hue: number;
  sessionId: string | null;
}) {
  return (
    <span
      className={`thought-session${sessionId ? "" : " is-unknown"}`}
      style={{ "--session-hue": String(hue) } as CSSProperties}
      title={
        sessionId
          ? `${label} · 会话 ${sessionId}`
          : `${label} · 客户端未报告会话标识`
      }
    >
      <span className="thought-session-dot" aria-hidden="true" />
      <span className="thought-session-label">{label}</span>
    </span>
  );
}

/** Collapsed segment body: one line that follows the newest reasoning text. */
function ThoughtTicker({ text }: { text: string }) {
  const ref = useRef<HTMLSpanElement>(null);
  const line = text.trim().replace(/\s+/g, " ").slice(-TICKER_CHARS);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.scrollLeft = el.scrollWidth;
  }, [line]);

  return (
    <span ref={ref} className="thought-segment-ticker" title={line}>
      {line || "（无内容）"}
    </span>
  );
}

// Only absolute http(s) URLs survive. react-markdown's default already drops
// `javascript:` and friends, but an allowlist keeps encoded or unknown schemes
// out of `href`/`src` entirely, so nothing attacker-controlled reaches an
// attribute that the webview could execute.
function safeUrlTransform(url: string): string {
  return /^https?:\/\//i.test(url.trim()) ? url : "";
}

// Reasoning is markdown-ish prose. Raw HTML stays escaped (react-markdown's
// default, no rehype-raw) and links leave the webview instead of replacing the
// panel. Highlighting is class-based too: rehype-highlight emits <span> nodes,
// never HTML strings, so fenced code cannot inject markup.
const markdownComponents: Components = {
  a({ href, children }) {
    return (
      <a
        href={href}
        title={href}
        onClick={(event) => {
          event.preventDefault();
          if (href && /^https?:\/\//i.test(href)) void openExternalUrl(href);
        }}
      >
        {children}
      </a>
    );
  },
};

/** Rendered markdown body. Memoed so finished turns skip re-parsing while the
 *  live turn streams. */
const ThoughtBody = memo(function ThoughtBody({ text }: { text: string }) {
  return (
    <div className="thoughts-text">
      <Markdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[
          [rehypeHighlight, { detect: false, ignoreMissing: true }],
        ]}
        urlTransform={safeUrlTransform}
        components={markdownComponents}
      >
        {text}
      </Markdown>
    </div>
  );
});

/**
 * Cut streaming markdown at the last finished block boundary: a blank line or
 * a closed fenced code block. Everything before it is settled text that can be
 * parsed once and memoised; only the tail keeps changing while tokens arrive.
 *
 * An unterminated fence is excluded from the settled part even when blank lines
 * appear inside it, otherwise the split would break the code block in two.
 */
function splitStreamingMarkdown(text: string): [string, string] {
  const lines = text.split("\n");
  let inFence = false;
  let fenceStart = 0;
  let offset = 0;
  let stableEnd = 0;
  for (const line of lines) {
    const isFence = /^\s{0,3}(```|~~~)/.test(line);
    if (inFence) {
      if (isFence) {
        inFence = false;
        stableEnd = offset + line.length + 1;
      }
    } else if (isFence) {
      inFence = true;
      fenceStart = offset;
    } else if (line.trim() === "") {
      stableEnd = offset + line.length + 1;
    }
    offset += line.length + 1;
  }
  if (inFence) stableEnd = Math.min(stableEnd, fenceStart);
  return [text.slice(0, stableEnd), text.slice(stableEnd)];
}

/** Settled prefix of a streaming turn. Its text stops changing, so the memo
 *  keeps the parsed tree (and its layout) untouched for the rest of the turn. */
const StableMarkdown = memo(function StableMarkdown({ text }: { text: string }) {
  return (
    <Markdown
      remarkPlugins={[remarkGfm]}
      rehypePlugins={[[rehypeHighlight, { detect: false, ignoreMissing: true }]]}
      urlTransform={safeUrlTransform}
      components={markdownComponents}
    >
      {text}
    </Markdown>
  );
});

/**
 * Live body: the finished prefix stays frozen while only the tail re-renders.
 * The tail skips syntax highlighting so a half-typed code block does not
 * repaint its colours on every poll.
 */
function StreamingBody({ text }: { text: string }) {
  const [stable, tail] = splitStreamingMarkdown(text);
  return (
    <div className="thoughts-text">
      {stable && <StableMarkdown text={stable} />}
      {tail && (
        <Markdown
          remarkPlugins={[remarkGfm]}
          urlTransform={safeUrlTransform}
          components={markdownComponents}
        >
          {tail}
        </Markdown>
      )}
    </div>
  );
}

/**
 * Rolled-up body: while a turn is live, the newest reasoning text drifts across
 * the pill from right to left. Once thinking ends the drift halts on the newest
 * text instead of looping forever.
 */
function RollingTicker({ text, live }: { text: string; live: boolean }) {
  const line = text.trim().replace(/\s+/g, " ");
  const content = line.slice(-TICKER_CHARS) || "等待思考输出…";
  // Longer passages need more time; short ones still drift gently.
  const duration = Math.min(60, Math.max(14, content.length * 0.14));
  const wrapRef = useRef<HTMLDivElement>(null);
  const firstCopyRef = useRef<HTMLSpanElement>(null);
  const [overflowing, setOverflowing] = useState(false);

  // The second copy exists only so the marquee can loop without a seam. Text
  // that already fits the pill would show twice, so measure first.
  useEffect(() => {
    const wrap = wrapRef.current;
    const copy = firstCopyRef.current;
    if (!wrap || !copy) return;
    const measure = () => {
      // The trailing spacer belongs to the loop, not to the text itself.
      const gap = parseFloat(getComputedStyle(copy).paddingRight) || 0;
      const copyWidth = copy.getBoundingClientRect().width - gap;
      setOverflowing(copyWidth > wrap.clientWidth + 1);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(wrap);
    return () => observer.disconnect();
  }, [content]);

  const scrolling = live && overflowing;

  return (
    <div
      ref={wrapRef}
      className={`thoughts-marquee${!live && overflowing ? " is-tail" : ""}`}
      title={content}
    >
      <div
        className={`thoughts-marquee-track${scrolling ? " is-scrolling" : ""}`}
        style={{ animationDuration: `${duration}s` }}
      >
        <span ref={firstCopyRef}>{content}</span>
        {scrolling && <span aria-hidden="true">{content}</span>}
      </div>
    </div>
  );
}

function ToolIcon({ children }: { children: ReactNode }) {
  return (
    <svg
      viewBox="0 0 20 20"
      width="19"
      height="19"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

const CloseIcon = (
  <ToolIcon>
    <path d="M5.2 5.2l9.6 9.6M14.8 5.2l-9.6 9.6" />
  </ToolIcon>
);

const ShirtIcon = (
  <ToolIcon>
    <path d="M7.6 3.6L4.4 5.2 3.1 8.7l2.6 1.1v6.6h8.6V9.8l2.6-1.1-1.3-3.5-3.2-1.6a2.6 2.6 0 0 1-4.4 0z" />
  </ToolIcon>
);

const RollUpIcon = (
  <ToolIcon>
    <path d="M5 12.4l5-4.8 5 4.8" />
  </ToolIcon>
);

const UnrollIcon = (
  <ToolIcon>
    <path d="M5 7.6l5 4.8 5-4.8" />
  </ToolIcon>
);

const PinIcon = (
  <ToolIcon>
    <path d="M7.5 3.6h5l-.7 4.2 2.3 2v1.3H5.9V9.8l2.3-2z" />
    <path d="M10 11.1v5.3" />
  </ToolIcon>
);

const EdgeHideIcon = (
  <ToolIcon>
    <rect x="3" y="4.2" width="14" height="11.6" rx="2.2" />
    <path d="M7.3 4.6v10.8" />
    <path d="M11.6 8.3l1.9 1.7-1.9 1.7" />
  </ToolIcon>
);

const ClearIcon = (
  <ToolIcon>
    <path d="M4.2 5.6h11.6" />
    <path d="M8.1 5.6V4.2h3.8v1.4" />
    <path d="M5.6 5.6l.7 10.2h7.4l.7-10.2" />
    <path d="M8.6 8.4v4.9M11.4 8.4v4.9" />
  </ToolIcon>
);

export default function ThoughtsPage() {
  const { segments, enabled, clear } = useThoughts();
  const edgeHide = useEdgeHide();
  const [opacity, setOpacity] = useState(loadOpacity);
  const [pinPref, setPinPref] = useState(() => loadPinned() ?? true);
  // Edge auto-hide only stays reachable while the panel sits above other
  // windows, so the mode forces the pin on; the saved choice returns with it.
  const pinned = edgeHide.enabled || pinPref;
  const [prefsOpen, setPrefsOpen] = useState(false);
  const [following, setFollowing] = useState(true);
  const [updatesWhilePaused, setUpdatesWhilePaused] = useState(0);
  const [collapsed, setCollapsed] = useState(false);
  // Single-focus timeline: exactly one turn is open at a time. The focus
  // follows the newest live writer unless the user picks a turn by hand.
  const [pinnedFocusId, setPinnedFocusId] = useState<string | null>(null);
  const [focusFolded, setFocusFolded] = useState(false);
  const [hideShort, setHideShort] = useState(loadHideShort);
  // While the user reads scrolled-back history, the focus must not jump to a
  // segment streaming elsewhere; freeze the value the follow mode had.
  const frozenFocusRef = useRef<string | null>(null);
  // Explicit expand/collapse choices per conversation; null means "follow the
  // default" (live and newest conversations stay open, older history folds).
  const [groupToggles, setGroupToggles] = useState<Map<string, boolean>>(
    () => new Map(),
  );
  const viewerRef = useRef<HTMLDivElement>(null);
  const prefsRef = useRef<HTMLDivElement>(null);
  const expandedSizeRef = useRef<TauriWindowSize | null>(loadExpandedSize());
  // Scroll anchoring fires while appending; read the live value instead of the
  // one captured when this render's handler was created.
  const followingRef = useRef(following);
  followingRef.current = following;
  // Programmatic scrolling emits scroll events too; ignore our own for a moment
  // so streaming text cannot mistake the follow scroll for a user gesture.
  const autoScrollAtRef = useRef(0);
  // Follow-scrolling eases towards the newest line instead of jumping there, so
  // a burst of incoming text does not flash the panel.
  const scrollAnimRef = useRef(0);

  // The thoughts window is transparent; the page background carries the
  // user-controlled alpha.
  useEffect(() => {
    const body = document.body.style.background;
    const html = document.documentElement.style.background;
    document.body.style.background = "transparent";
    document.documentElement.style.background = "transparent";
    return () => {
      document.body.style.background = body;
      document.documentElement.style.background = html;
    };
  }, []);

  // The window is created always-on-top; keep the OS state in step with the
  // derived pin flag so edge-hide can force it on and off again.
  useEffect(() => {
    setWindowAlwaysOnTop(pinned).catch(() => {
      // The window can disappear mid-toggle; the stored choice still stands.
    });
  }, [pinned]);

  // Dismiss the appearance popover on any click outside it.
  useEffect(() => {
    if (!prefsOpen) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!prefsRef.current?.contains(event.target as Node)) {
        setPrefsOpen(false);
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [prefsOpen]);

  const stopScrollAnimation = useCallback(() => {
    if (scrollAnimRef.current) {
      cancelAnimationFrame(scrollAnimRef.current);
      scrollAnimRef.current = 0;
    }
  }, []);

  // Ease-out over roughly one poll interval: each new poll re-targets the same
  // animation, so text grows into place instead of snapping.
  const smoothScrollTo = useCallback(
    (el: HTMLDivElement, target: number) => {
      stopScrollAnimation();
      const from = el.scrollTop;
      const distance = target - from;
      if (Math.abs(distance) < 1) return;
      if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
        el.scrollTop = target;
        return;
      }
      const duration = Math.min(260, Math.max(90, Math.abs(distance) * 0.9));
      const startedAt = performance.now();
      const step = (now: number) => {
        const progress = Math.min(1, (now - startedAt) / duration);
        const eased = 1 - (1 - progress) ** 3;
        el.scrollTop = from + distance * eased;
        autoScrollAtRef.current = performance.now();
        scrollAnimRef.current =
          progress < 1 ? requestAnimationFrame(step) : 0;
      };
      scrollAnimRef.current = requestAnimationFrame(step);
    },
    [stopScrollAnimation],
  );

  useEffect(
    () => () => stopScrollAnimation(),
    [stopScrollAnimation],
  );

  const scrollToLive = useCallback(() => {
    const el = viewerRef.current;
    if (!el) return;
    // Anchor the open turn, not the conversation group: concurrent requests
    // grow in different blocks, and only the focused one may move the view.
    const focus = el.querySelector<HTMLElement>(FOCUS_SELECTOR);
    autoScrollAtRef.current = performance.now();
    if (!focus) {
      smoothScrollTo(el, el.scrollHeight - el.clientHeight);
      return;
    }
    // Keep the streaming turn in view even when finished turns follow it: park
    // its top when the block fits, otherwise its newest line.
    const view = el.getBoundingClientRect();
    const rect = focus.getBoundingClientRect();
    const delta =
      rect.height < view.height - 24
        ? rect.top - (view.top + 8)
        : rect.bottom - (view.bottom - 12);
    smoothScrollTo(
      el,
      Math.min(
        el.scrollHeight - el.clientHeight,
        Math.max(0, el.scrollTop + delta),
      ),
    );
  }, [smoothScrollTo]);

  useEffect(() => {
    if (following) {
      setUpdatesWhilePaused(0);
      requestAnimationFrame(scrollToLive);
    } else {
      setUpdatesWhilePaused((n) => n + 1);
    }
    // Only react to stream/clear changes, not to the pause flag itself.
  }, [segments]);

  const handleScroll = () => {
    if (!followingRef.current) return;
    if (performance.now() - autoScrollAtRef.current < 150) return;
    const el = viewerRef.current;
    if (!el) return;
    if (el.scrollTop + el.clientHeight >= el.scrollHeight - 8) return;
    const focus = el.querySelector<HTMLElement>(FOCUS_SELECTOR);
    if (!focus) {
      setFollowing(false);
      return;
    }
    const view = el.getBoundingClientRect();
    const rect = focus.getBoundingClientRect();
    const gap =
      rect.height < view.height - 24
        ? rect.top - (view.top + 8)
        : rect.bottom - (view.bottom - 12);
    if (Math.abs(gap) > 24) setFollowing(false);
  };

  const handleOpacity = (value: number) => {
    setOpacity(value);
    try {
      localStorage.setItem(OPACITY_KEY, String(value));
    } catch {
      // storage unavailable
    }
  };

  const handlePin = () => {
    const next = !pinPref;
    setPinPref(next);
    try {
      localStorage.setItem(PINNED_KEY, String(next));
    } catch {
      // storage unavailable
    }
  };

  const handleHideShort = (next: boolean) => {
    setHideShort(next);
    try {
      localStorage.setItem(HIDE_SHORT_KEY, String(next));
    } catch {
      // storage unavailable
    }
  };

  /** Shrink the window to the pill, or restore the pre-collapse size. */
  const setRolledUp = useCallback(async (next: boolean) => {
    try {
      if (next) {
        const size = await getWindowSize();
        if (size) {
          expandedSizeRef.current = size;
          saveExpandedSize(size);
          const scale = await getWindowScaleFactor();
          await setWindowLogicalSize(size.width / scale, COLLAPSED_HEIGHT);
        }
      } else {
        const size = expandedSizeRef.current;
        if (size) await setWindowPhysicalSize(size);
      }
    } catch {
      // Keep the in-page state usable even if the window API is unavailable.
    }
    setCollapsed(next);
    if (!next) requestAnimationFrame(scrollToLive);
  }, [scrollToLive]);

  const handleRollUp = () => {
    if (!collapsed) setPrefsOpen(false);
    void setRolledUp(!collapsed);
  };

  const handleClose = () => {
    void closeCurrentWindow();
  };

  // Clearing drops the whole transcript, so every piece of state that points
  // into it (focus, folds, paused-follow counter) resets with it.
  const handleClear = () => {
    setPinnedFocusId(null);
    setFocusFolded(false);
    setGroupToggles(new Map());
    frozenFocusRef.current = null;
    setFollowing(true);
    setUpdatesWhilePaused(0);
    void clear();
  };

  // The appearance popover would be left floating over a docked panel.
  const handleEdgeHide = () => {
    setPrefsOpen(false);
    void edgeHide.toggle();
  };

  // The appearance popover does not fit in the pill, so revealing it also
  // unrolls the panel.
  const handlePrefs = () => {
    if (collapsed) {
      void setRolledUp(false);
      setPrefsOpen(true);
      return;
    }
    setPrefsOpen((open) => !open);
  };

  // Auxiliary requests can be hidden on request. The focus is derived from the
  // visible list so a hidden turn never leaves the panel with nothing open.
  const visibleSegments = hideShort
    ? segments.filter((segment) => !isShortTurn(segment))
    : segments;
  const hiddenCount = segments.length - visibleSegments.length;

  // The focus follows the live turn that wrote most recently; once nothing is
  // streaming, the newest substantive turn stays open as the resting state so
  // the panel does not park on a title generator.
  let autoFocusId: string | null = null;
  const liveSegments = visibleSegments.filter((s) => s.ended_at == null);
  if (liveSegments.length > 0) {
    autoFocusId = liveSegments.reduce((best, s) =>
      s.updated_at >= best.updated_at ? s : best,
    ).session_id;
  } else if (visibleSegments.length > 0) {
    const substantive = visibleSegments.filter((s) => !isShortTurn(s));
    const last = substantive.length
      ? substantive[substantive.length - 1]
      : visibleSegments[visibleSegments.length - 1];
    autoFocusId = last.session_id;
  }

  // While the user reads scrolled-back history, hold the focus where it was so
  // text arriving in another turn cannot relayout the passage being read.
  useEffect(() => {
    if (!following) return;
    const current = frozenFocusRef.current;
    const currentSegment = visibleSegments.find(
      (s) => s.session_id === current,
    );
    // Keep a writer that is still active; only hand the focus over once it has
    // been quiet long enough for the switch to be meaningful.
    if (
      currentSegment &&
      currentSegment.ended_at == null &&
      autoFocusId != null &&
      autoFocusId !== current &&
      Date.now() - currentSegment.updated_at < FOCUS_HOLD_MS
    ) {
      return;
    }
    frozenFocusRef.current = autoFocusId;
  }, [following, autoFocusId, visibleSegments]);

  const pinnedVisible =
    pinnedFocusId &&
    visibleSegments.some((s) => s.session_id === pinnedFocusId)
      ? pinnedFocusId
      : null;
  const frozenVisible =
    frozenFocusRef.current &&
    visibleSegments.some((s) => s.session_id === frozenFocusRef.current)
      ? frozenFocusRef.current
      : null;
  const focusId = focusFolded
    ? null
    : (pinnedVisible ?? (frozenVisible ?? autoFocusId));

  /** Open a turn (locking the focus), or fold it when it is already open. */
  const activateSegment = (segment: ThoughtSegment) => {
    if (focusId === segment.session_id) {
      setPinnedFocusId(null);
      setFocusFolded(true);
      return;
    }
    setPinnedFocusId(segment.session_id);
    setFocusFolded(false);
    if (segment.ended_at == null) {
      // Watching a live turn should keep it anchored while it writes.
      frozenFocusRef.current = segment.session_id;
      setFollowing(true);
    } else {
      // Reading history must not be dragged away by a turn streaming below.
      setFollowing(false);
    }
    requestAnimationFrame(scrollToLive);
  };

  // Concurrent clients interleave turns in one flat stream, so the panel groups
  // them by conversation first; a single conversation degrades to the old list.
  const groups = groupThoughts(visibleSegments);
  const groupMeta = describeGroups(groups);
  const grouped = groups.length > 1;
  const newestGroupKey = groups[0]?.key;

  const isGroupExpanded = (group: ThoughtGroup): boolean =>
    groupToggles.get(group.key) ?? (group.live || group.key === newestGroupKey);

  const toggleGroup = (group: ThoughtGroup) => {
    const next = !isGroupExpanded(group);
    setGroupToggles((prev) => new Map(prev).set(group.key, next));
  };

  // With concurrent clients the newest-created segment may already be done
  // while an older one still streams, so the rolled-up pill follows the newest
  // turn that is actually live.
  let active = visibleSegments[visibleSegments.length - 1];
  for (let i = visibleSegments.length - 1; i >= 0; i -= 1) {
    if (visibleSegments[i].ended_at == null) {
      active = visibleSegments[i];
      break;
    }
  }
  const activeMeta = active ? groupMeta.get(segmentKey(active)) : undefined;
  const tickerText = !enabled
    ? "思考面板未启用"
    : active
      ? active.text || displayPrompt(active.prompt)
      : "等待思考输出…";

  const panelStyle = {
    "--thought-alpha": String(opacity),
  } as CSSProperties;

  const live = enabled && active != null && active.ended_at == null;

  const renderSegment = (
    segment: ThoughtSegment,
    meta: GroupMeta,
    index: number,
    showBadge: boolean,
  ) => {
    const open = segment.session_id === focusId;
    const isLive = segment.ended_at == null;
    const prompt = displayPrompt(segment.prompt);
    return (
      <section
        key={`${index}-${segment.session_id}`}
        className={`thought-segment${isLive ? " is-live" : ""}${
          open ? " is-open" : " is-folded"
        }`}
        style={{ "--session-hue": String(meta.hue) } as CSSProperties}
        data-focus-segment={open ? "true" : undefined}
      >
        <button
          type="button"
          className="thought-segment-head"
          onClick={() => activateSegment(segment)}
          aria-expanded={open}
          title={prompt || segment.text}
        >
          <span className="thought-segment-index">
            {String(index + 1).padStart(2, "0")}
          </span>
          {showBadge && (
            <SessionBadge
              label={meta.name}
              hue={meta.hue}
              sessionId={segment.client_session}
            />
          )}
          <span className="thought-segment-prompt">
            {prompt || "（未记录提问内容）"}
          </span>
          {isLive ? (
            <span className="thought-segment-live" aria-label="思考中" />
          ) : (
            <span className="thought-segment-time">
              {formatClock(segment.started_at)}
            </span>
          )}
        </button>

        {open ? (
          <>
            {isLive ? (
              <StreamingBody text={segment.text} />
            ) : (
              <ThoughtBody text={segment.text} />
            )}
            <div className="thought-segment-foot">
              <span>{formatClock(segment.started_at)}</span>
              {segment.ended_at != null && (
                <span>
                  {formatDuration(segment.started_at, segment.ended_at)}
                </span>
              )}
              {segment.workdir && (
                <span className="thought-segment-workdir" title={segment.workdir}>
                  {basename(segment.workdir)}
                </span>
              )}
              <span title={`ACP session ${segment.session_id}`}>
                acp:{segment.session_id.slice(-6)}
              </span>
            </div>
          </>
        ) : (
          <button
            type="button"
            className="thought-segment-ticker-row"
            onClick={() => activateSegment(segment)}
            title="切换到这个思考"
          >
            <ThoughtTicker text={segment.text} />
            {isLive ? (
              <span className="thought-segment-meta is-live">思考中</span>
            ) : (
              <span className="thought-segment-meta">
                {segment.ended_at != null
                  ? `${formatDuration(segment.started_at, segment.ended_at)} · ${turnChars(
                      segment,
                    )} 字`
                  : ""}
              </span>
            )}
          </button>
        )}
      </section>
    );
  };

  return (
    <div
      className={`thoughts-window${collapsed ? " is-collapsed" : ""}${
        edgeHide.hidden ? " is-edge-hidden" : ""
      }`}
      style={panelStyle}
      data-tauri-drag-region="deep"
    >
      {edgeHide.enabled && (
        <button
          type="button"
          className={`thoughts-edge-hotzone is-${edgeHide.side}${
            edgeHide.hidden ? " is-visible" : ""
          }${live ? " is-live" : ""}`}
          data-tauri-drag-region="false"
          onClick={() => void edgeHide.reveal()}
          onPointerEnter={() => void edgeHide.reveal()}
          onPointerDown={() => void edgeHide.reveal()}
          tabIndex={edgeHide.hidden ? 0 : -1}
          title="展开思考面板"
          aria-label="展开思考面板"
        />
      )}

      <div className="thoughts-card">
        <div className="thoughts-toolbar">
          <div className="thoughts-tools">
            <button
              type="button"
              className="thoughts-icon-btn"
              onClick={handleClose}
              title="关闭"
              aria-label="关闭"
            >
              {CloseIcon}
            </button>

            <button
              type="button"
              className={`thoughts-icon-btn${pinned ? " is-active" : ""}`}
              onClick={handlePin}
              disabled={edgeHide.enabled}
              title={
                edgeHide.enabled
                  ? "贴边隐藏时始终置顶"
                  : pinned
                    ? "取消置顶"
                    : "置顶"
              }
              aria-label={
                edgeHide.enabled
                  ? "贴边隐藏时始终置顶"
                  : pinned
                    ? "取消置顶"
                    : "置顶"
              }
              aria-pressed={pinned}
            >
              {PinIcon}
            </button>

            <button
              type="button"
              className={`thoughts-icon-btn${edgeHide.enabled ? " is-active" : ""}`}
              onClick={handleEdgeHide}
              title={
                edgeHide.enabled
                  ? "关闭贴边隐藏"
                  : "贴边隐藏（鼠标移开后自动收起）"
              }
              aria-label={edgeHide.enabled ? "关闭贴边隐藏" : "贴边隐藏"}
              aria-pressed={edgeHide.enabled}
            >
              {EdgeHideIcon}
            </button>

            <div className="thoughts-prefs-wrap" ref={prefsRef}>
              <button
                type="button"
                className={`thoughts-icon-btn${prefsOpen ? " is-active" : ""}`}
                onClick={handlePrefs}
                title="外观"
                aria-label="外观"
                aria-expanded={prefsOpen}
              >
                {ShirtIcon}
              </button>

              {prefsOpen && !collapsed && (
                <div
                  className="thoughts-prefs"
                  data-tauri-drag-region="false"
                  role="dialog"
                  aria-label="外观设置"
                >
                  <div className="thoughts-prefs-row">
                    <span className="thoughts-prefs-label">透明度</span>
                    <input
                      type="range"
                      min={15}
                      max={100}
                      value={Math.round(opacity * 100)}
                      onChange={(e) =>
                        handleOpacity(Number(e.target.value) / 100)
                      }
                      aria-label="窗口透明度"
                    />
                    <span className="thoughts-prefs-value">
                      {Math.round(opacity * 100)}%
                    </span>
                  </div>
                  <div className="thoughts-prefs-row">
                    <span className="thoughts-prefs-label">短请求</span>
                    <label className="thoughts-prefs-check">
                      <input
                        type="checkbox"
                        checked={hideShort}
                        onChange={(e) => handleHideShort(e.target.checked)}
                      />
                      <span>隐藏自动请求（标题/摘要）</span>
                    </label>
                  </div>
                </div>
              )}
            </div>

            <button
              type="button"
              className="thoughts-icon-btn"
              onClick={handleClear}
              disabled={segments.length === 0}
              title="清空思考记录"
              aria-label="清空思考记录"
            >
              {ClearIcon}
            </button>

            <button
              type="button"
              className="thoughts-icon-btn"
              onClick={handleRollUp}
              title={collapsed ? "展开" : "卷起"}
              aria-label={collapsed ? "展开" : "卷起"}
              aria-expanded={!collapsed}
            >
              {collapsed ? UnrollIcon : RollUpIcon}
            </button>
          </div>
        </div>

        {!collapsed && (
          <div className="thoughts-feed" aria-hidden="true">
            <span className={`thoughts-feed-dot${live ? " is-live" : ""}`} />
            <span>{enabled ? "THOUGHT STREAM" : "OFFLINE"}</span>
          </div>
        )}

        {collapsed ? (
          <div className="thoughts-collapsed">
            {enabled && active && activeMeta && (
              <SessionBadge
                label={activeMeta.name}
                hue={activeMeta.hue}
                sessionId={active.client_session}
              />
            )}
            <RollingTicker text={tickerText} live={live} />
          </div>
        ) : !enabled ? (
          <div className="thoughts-empty">
            <div className="thoughts-empty-kicker">OFFLINE</div>
            <div className="thoughts-empty-title">思考面板未启用</div>
            <div className="thoughts-empty-hint">
              请在设置中勾选「启用思考面板」并保存。
            </div>
            <button
              type="button"
              className="thoughts-btn"
              onClick={() => void showSettingsWindow()}
            >
              打开设置
            </button>
          </div>
        ) : (
          <div
            ref={viewerRef}
            className="thoughts-viewer"
            data-tauri-drag-region="false"
            onScroll={handleScroll}
            onWheel={stopScrollAnimation}
          >
            {visibleSegments.length > 0 ? (
              grouped ? (
                groups.map((group) => {
                  const meta = groupMeta.get(group.key) ?? {
                    name: "未标记会话",
                    hint: "",
                    opening: "",
                    hue: sessionHue(group.key),
                  };
                  const expanded = isGroupExpanded(group);
                  const last = group.segments[group.segments.length - 1];
                  return (
                    <section
                      key={group.key}
                      className={`thought-group${group.live ? " is-live" : ""}`}
                      style={
                        { "--session-hue": String(meta.hue) } as CSSProperties
                      }
                    >
                      <button
                        type="button"
                        className="thought-group-head"
                        onClick={() => toggleGroup(group)}
                        aria-expanded={expanded}
                        title={[meta.name, meta.opening, group.workdir]
                          .filter(Boolean)
                          .join(" · ")}
                      >
                        <span className="thought-group-dot" aria-hidden="true" />
                        <span className="thought-group-name">{meta.name}</span>
                        {meta.hint && (
                          <span className="thought-group-hint">{meta.hint}</span>
                        )}
                        <span className="thought-group-count">
                          {group.segments.length} 轮
                        </span>
                        {group.live ? (
                          <span
                            className="thought-group-live"
                            aria-label="思考中"
                          />
                        ) : (
                          <span className="thought-group-time">
                            {formatClock(group.lastActivity)}
                          </span>
                        )}
                      </button>

                      {expanded ? (
                        group.segments.map((segment, index) =>
                          renderSegment(segment, meta, index, false),
                        )
                      ) : (
                        <div
                          className="thought-group-summary"
                          title={displayPrompt(last.prompt) || undefined}
                        >
                          {promptHint(displayPrompt(last.prompt)) ||
                            "（未记录提问内容）"}
                        </div>
                      )}
                    </section>
                  );
                })
              ) : (
                visibleSegments.map((segment, index) => {
                  const meta = groupMeta.get(segmentKey(segment)) ?? {
                    name: "未标记会话",
                    hint: "",
                    opening: "",
                    hue: sessionHue(segmentKey(segment)),
                  };
                  // One conversation fills the list, so the project tag reads as
                  // a header for the stream instead of repeating on every turn.
                  return renderSegment(segment, meta, index, index === 0);
                })
              )
            ) : hiddenCount > 0 ? (
              <div className="thoughts-empty-hint thoughts-viewer-empty">
                已隐藏 {hiddenCount} 个短请求
              </div>
            ) : (
              <div className="thoughts-empty-hint thoughts-viewer-empty">
                等待思考输出…
              </div>
            )}
          </div>
        )}

        {!collapsed && enabled && !following && (
          <button
            type="button"
            className="thoughts-follow"
            onClick={() => {
              setPinnedFocusId(null);
              setFocusFolded(false);
              frozenFocusRef.current = null;
              setFollowing(true);
              scrollToLive();
            }}
          >
            ↓ 继续滚动
            {updatesWhilePaused > 0 ? " · 有新内容" : ""}
          </button>
        )}

      </div>
    </div>
  );
}

