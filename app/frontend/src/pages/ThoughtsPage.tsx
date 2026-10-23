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
  isWindowAlwaysOnTop,
  openExternalUrl,
  setWindowAlwaysOnTop,
  setWindowLogicalSize,
  setWindowPhysicalSize,
  showSettingsWindow,
  type ThoughtSegment,
} from "../api/tauri";
import { useThoughts } from "../hooks/useThoughts";
import "../styles/thoughts.css";

const OPACITY_KEY = "trae-thoughts-opacity";
const PINNED_KEY = "trae-thoughts-pinned";
const EXPANDED_SIZE_KEY = "trae-thoughts-expanded-size";
const DEFAULT_OPACITY = 0.82;
// The rolled-up state is a single pill; keep the logical window height in sync
// so the transparent area does not cover the desktop behind it.
const COLLAPSED_HEIGHT = 60;
// Collapsed segments render a one-line ticker; only the tail can be visible,
// so keeping the last few hundred characters bounds the DOM for long turns.
const TICKER_CHARS = 400;

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
// The group header shows the project folder and the opening question; ids stay
// in the tooltip because nobody recognises a hex string.
const PROMPT_HINT_CHARS = 18;
const LIVE_GROUP_SELECTOR = '[data-live-group="true"]';

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
  firstPrompt: string;
}

interface GroupMeta {
  /** Human name: the project folder, disambiguated only when it must be. */
  name: string;
  /** Opening question of the conversation, truncated for the group header. */
  hint: string;
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
        firstPrompt: "",
      };
      groups.set(key, group);
    }
    group.segments.push(segment);
    group.startedAt = Math.min(group.startedAt, segment.started_at);
    group.lastActivity = Math.max(
      group.lastActivity,
      segment.ended_at ?? segment.started_at,
    );
    group.live = group.live || segment.ended_at == null;
    if (!group.firstPrompt && segment.prompt.trim()) {
      group.firstPrompt = segment.prompt;
    }
  }

  // Live conversations first, then most recent activity, so the session the
  // user is watching never sinks below finished history.
  return [...groups.values()].sort((a, b) => {
    if (a.live !== b.live) return a.live ? -1 : 1;
    return b.lastActivity - a.lastActivity;
  });
}

function promptHint(prompt: string): string {
  const line = prompt.trim().replace(/\s+/g, " ");
  if (!line) return "";
  return line.length > PROMPT_HINT_CHARS
    ? `${line.slice(0, PROMPT_HINT_CHARS)}…`
    : line;
}

/**
 * Name conversations the way the user thinks about them: the project folder.
 * Only conversations whose lifetimes overlap get an ordinal, so two turns in
 * the same project at different times never look like two running windows.
 */
function describeGroups(groups: ThoughtGroup[]): Map<string, GroupMeta> {
  const byName = new Map<string, ThoughtGroup[]>();
  for (const group of groups) {
    const name = basename(group.workdir) || "未标记会话";
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
      meta.set(group.key, {
        name: concurrent ? `${name} · ${ordinal}` : name,
        hint: promptHint(group.firstPrompt),
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

export default function ThoughtsPage() {
  const { segments, enabled } = useThoughts();
  const [opacity, setOpacity] = useState(loadOpacity);
  const [pinned, setPinned] = useState(() => loadPinned() ?? true);
  const [prefsOpen, setPrefsOpen] = useState(false);
  const [following, setFollowing] = useState(true);
  const [updatesWhilePaused, setUpdatesWhilePaused] = useState(0);
  const [collapsed, setCollapsed] = useState(false);
  const [collapsedSegments, setCollapsedSegments] = useState<Set<string>>(
    () => new Set(),
  );
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

  // The window is created always-on-top; replay the user's last pin choice and
  // fall back to the live window state when nothing has been stored yet.
  useEffect(() => {
    const stored = loadPinned();
    if (stored == null) {
      void isWindowAlwaysOnTop().then((value) => {
        if (value != null) setPinned(value);
      });
      return;
    }
    setPinned(stored);
    void setWindowAlwaysOnTop(stored);
  }, []);

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

  const scrollToLive = useCallback(() => {
    const el = viewerRef.current;
    if (!el) return;
    const live = el.querySelector<HTMLElement>(LIVE_GROUP_SELECTOR);
    autoScrollAtRef.current = performance.now();
    if (!live) {
      el.scrollTop = el.scrollHeight;
      return;
    }
    // Keep the streaming conversation in view even when finished groups follow
    // it: park its top when the block fits, otherwise its newest line.
    const view = el.getBoundingClientRect();
    const rect = live.getBoundingClientRect();
    const delta =
      rect.height < view.height - 24
        ? rect.top - (view.top + 8)
        : rect.bottom - (view.bottom - 12);
    el.scrollTop = Math.min(
      el.scrollHeight,
      Math.max(0, el.scrollTop + delta),
    );
  }, []);

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
    const live = el.querySelector<HTMLElement>(LIVE_GROUP_SELECTOR);
    if (!live) {
      setFollowing(false);
      return;
    }
    const view = el.getBoundingClientRect();
    const rect = live.getBoundingClientRect();
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

  const handlePin = async () => {
    const next = !pinned;
    setPinned(next);
    try {
      localStorage.setItem(PINNED_KEY, String(next));
    } catch {
      // storage unavailable
    }
    try {
      await setWindowAlwaysOnTop(next);
    } catch {
      setPinned(!next);
    }
  };

  const toggleSegment = (sessionId: string) => {
    setCollapsedSegments((prev) => {
      const next = new Set(prev);
      if (next.has(sessionId)) next.delete(sessionId);
      else next.add(sessionId);
      return next;
    });
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

  // Concurrent clients interleave turns in one flat stream, so the panel groups
  // them by conversation first; a single conversation degrades to the old list.
  const groups = groupThoughts(segments);
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
  let active = segments[segments.length - 1];
  for (let i = segments.length - 1; i >= 0; i -= 1) {
    if (segments[i].ended_at == null) {
      active = segments[i];
      break;
    }
  }
  const activeMeta = active ? groupMeta.get(segmentKey(active)) : undefined;
  const tickerText = !enabled
    ? "思考面板未启用"
    : active
      ? active.text || active.prompt
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
    const isCollapsed = collapsedSegments.has(segment.session_id);
    const isLive = segment.ended_at == null;
    return (
      <section
        key={`${index}-${segment.session_id}`}
        className={`thought-segment${isLive ? " is-live" : ""}`}
        style={{ "--session-hue": String(meta.hue) } as CSSProperties}
      >
        <button
          type="button"
          className="thought-segment-head"
          onClick={() => toggleSegment(segment.session_id)}
          aria-expanded={!isCollapsed}
          title={segment.prompt || segment.text}
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
            {segment.prompt || "（未记录提问内容）"}
          </span>
          {isLive ? (
            <span className="thought-segment-live" aria-label="思考中" />
          ) : (
            <span className="thought-segment-time">
              {formatClock(segment.started_at)}
            </span>
          )}
        </button>

        {isCollapsed ? (
          <div className="thought-segment-ticker-row">
            <ThoughtTicker text={segment.text} />
          </div>
        ) : (
          <>
            <ThoughtBody text={segment.text} />
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
        )}
      </section>
    );
  };

  return (
    <div
      className={`thoughts-window${collapsed ? " is-collapsed" : ""}`}
      style={panelStyle}
      data-tauri-drag-region="deep"
    >
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
              onClick={() => void handlePin()}
              title={pinned ? "取消置顶" : "置顶"}
              aria-label={pinned ? "取消置顶" : "置顶"}
              aria-pressed={pinned}
            >
              {PinIcon}
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
                </div>
              )}
            </div>

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
          >
            {segments.length > 0 ? (
              grouped ? (
                groups.map((group) => {
                  const meta = groupMeta.get(group.key) ?? {
                    name: "未标记会话",
                    hint: "",
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
                      data-live-group={group.live ? "true" : undefined}
                    >
                      <button
                        type="button"
                        className="thought-group-head"
                        onClick={() => toggleGroup(group)}
                        aria-expanded={expanded}
                        title={
                          group.workdir
                            ? `${meta.name} · ${group.workdir}`
                            : meta.name
                        }
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
                          title={last.prompt || undefined}
                        >
                          {promptHint(last.prompt) || "（未记录提问内容）"}
                        </div>
                      )}
                    </section>
                  );
                })
              ) : (
                segments.map((segment, index) => {
                  const meta = groupMeta.get(segmentKey(segment)) ?? {
                    name: "未标记会话",
                    hint: "",
                    hue: sessionHue(segmentKey(segment)),
                  };
                  // One conversation fills the list, so the project tag reads as
                  // a header for the stream instead of repeating on every turn.
                  return renderSegment(segment, meta, index, index === 0);
                })
              )
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
              setFollowing(true);
              scrollToLive();
            }}
          >
            ↓ 继续滚动
            {updatesWhilePaused > 0 ? `（${updatesWhilePaused} 次更新）` : ""}
          </button>
        )}

      </div>
    </div>
  );
}

