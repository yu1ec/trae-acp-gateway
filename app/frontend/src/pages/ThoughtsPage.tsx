import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import {
  closeCurrentWindow,
  getWindowScaleFactor,
  getWindowSize,
  isWindowAlwaysOnTop,
  setWindowAlwaysOnTop,
  setWindowLogicalSize,
  setWindowPhysicalSize,
  showSettingsWindow,
} from "../api/tauri";
import { useThoughts } from "../hooks/useThoughts";
import "../styles/thoughts.css";

const OPACITY_KEY = "trae-thoughts-opacity";
const SKIN_KEY = "trae-thoughts-skin";
const PINNED_KEY = "trae-thoughts-pinned";
const EXPANDED_SIZE_KEY = "trae-thoughts-expanded-size";
const DEFAULT_OPACITY = 0.82;
// The rolled-up state is a single pill; keep the logical window height in sync
// so the transparent area does not cover the desktop behind it.
const COLLAPSED_HEIGHT = 60;
// Collapsed segments render a one-line ticker; only the tail can be visible,
// so keeping the last few hundred characters bounds the DOM for long turns.
const TICKER_CHARS = 400;

interface Skin {
  id: "green" | "black";
  label: string;
  /** Space-separated RGB channels, consumed by `rgb(var(--paper-rgb) / a)`. */
  paper: string;
  ink: string;
  accent: string;
  /** Drives the native color-scheme so scrollbars and form controls match. */
  dark: boolean;
}

// Two HUD skins only: a pale signal-green glass and a near-black terminal
// glass. Ink is inverted for the dark skin so every derived alpha stays legible.
const SKINS: Skin[] = [
  {
    id: "green",
    label: "绿色",
    paper: "233 244 216",
    ink: "30 42 25",
    accent: "44 122 64",
    dark: false,
  },
  {
    id: "black",
    label: "黑色",
    paper: "11 14 13",
    ink: "228 242 232",
    accent: "96 240 158",
    dark: true,
  },
];

function loadOpacity(): number {
  try {
    const raw = Number(localStorage.getItem(OPACITY_KEY));
    if (Number.isFinite(raw) && raw >= 0.15 && raw <= 1) return raw;
  } catch {
    // storage unavailable
  }
  return DEFAULT_OPACITY;
}

function loadSkin(): Skin {
  try {
    const id = localStorage.getItem(SKIN_KEY);
    const found = SKINS.find((skin) => skin.id === id);
    if (found) return found;
  } catch {
    // storage unavailable
  }
  return SKINS[0];
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
  const [skin, setSkin] = useState<Skin>(loadSkin);
  const [pinned, setPinned] = useState(() => loadPinned() ?? true);
  const [prefsOpen, setPrefsOpen] = useState(false);
  const [following, setFollowing] = useState(true);
  const [updatesWhilePaused, setUpdatesWhilePaused] = useState(0);
  const [collapsed, setCollapsed] = useState(false);
  const [collapsedSegments, setCollapsedSegments] = useState<Set<string>>(
    () => new Set(),
  );
  const viewerRef = useRef<HTMLDivElement>(null);
  const prefsRef = useRef<HTMLDivElement>(null);
  const expandedSizeRef = useRef<TauriWindowSize | null>(loadExpandedSize());
  // Scroll anchoring fires while appending; read the live value instead of the
  // one captured when this render's handler was created.
  const followingRef = useRef(following);
  followingRef.current = following;

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

  const scrollToBottom = useCallback(() => {
    const el = viewerRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, []);

  useEffect(() => {
    if (following) {
      setUpdatesWhilePaused(0);
      requestAnimationFrame(scrollToBottom);
    } else {
      setUpdatesWhilePaused((n) => n + 1);
    }
    // Only react to stream/clear changes, not to the pause flag itself.
  }, [segments]);

  const handleScroll = () => {
    if (!followingRef.current) return;
    const el = viewerRef.current;
    if (!el) return;
    const atBottom = el.scrollTop + el.clientHeight >= el.scrollHeight - 8;
    if (!atBottom) setFollowing(false);
  };

  const handleOpacity = (value: number) => {
    setOpacity(value);
    try {
      localStorage.setItem(OPACITY_KEY, String(value));
    } catch {
      // storage unavailable
    }
  };

  const handleSkinChange = (next: Skin) => {
    setSkin(next);
    try {
      localStorage.setItem(SKIN_KEY, next.id);
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
    if (!next) requestAnimationFrame(scrollToBottom);
  }, [scrollToBottom]);

  const handleRollUp = () => {
    if (!collapsed) setPrefsOpen(false);
    void setRolledUp(!collapsed);
  };

  const handleClose = () => {
    void closeCurrentWindow();
  };

  // The appearance popover does not fit in the pill, so revealing it also
  // unrolls the panel.
  const handleSkin = () => {
    if (collapsed) {
      void setRolledUp(false);
      setPrefsOpen(true);
      return;
    }
    setPrefsOpen((open) => !open);
  };

  const latest = segments[segments.length - 1];
  const tickerText = !enabled
    ? "思考面板未启用"
    : latest
      ? latest.text || latest.prompt
      : "等待思考输出…";

  const panelStyle = {
    "--thought-alpha": String(opacity),
    "--paper-rgb": skin.paper,
    "--ink-rgb": skin.ink,
    "--accent": skin.accent,
    colorScheme: skin.dark ? "dark" : "light",
  } as CSSProperties;

  const live = enabled && latest != null && latest.ended_at == null;

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
                onClick={handleSkin}
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
                    <span className="thoughts-prefs-label">皮肤</span>
                    <div className="thoughts-skins">
                      {SKINS.map((item) => (
                        <button
                          key={item.id}
                          type="button"
                          className={`thoughts-skin${
                            item.id === skin.id ? " is-selected" : ""
                          }`}
                          onClick={() => handleSkinChange(item)}
                          aria-pressed={item.id === skin.id}
                        >
                          <span
                            className="thoughts-skin-dot"
                            style={{ background: `rgb(${item.paper})` }}
                          />
                          {item.label}
                        </button>
                      ))}
                    </div>
                  </div>
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
              segments.map((segment, index) => {
                const isCollapsed = collapsedSegments.has(segment.session_id);
                const isLive = segment.ended_at == null;
                return (
                  <section
                    key={`${index}-${segment.session_id}`}
                    className={`thought-segment${isLive ? " is-live" : ""}`}
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
                      <span className="thought-segment-prompt">
                        {segment.prompt || "（未记录提问内容）"}
                      </span>
                      {isLive ? (
                        <span
                          className="thought-segment-live"
                          aria-label="思考中"
                        />
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
                        <pre className="thoughts-text">{segment.text}</pre>
                        <div className="thought-segment-foot">
                          <span>{formatClock(segment.started_at)}</span>
                          {segment.ended_at != null && (
                            <span>
                              {formatDuration(
                                segment.started_at,
                                segment.ended_at,
                              )}
                            </span>
                          )}
                          {segment.workdir && (
                            <span
                              className="thought-segment-workdir"
                              title={segment.workdir}
                            >
                              {basename(segment.workdir)}
                            </span>
                          )}
                          <span>#{segment.session_id.slice(-6)}</span>
                        </div>
                      </>
                    )}
                  </section>
                );
              })
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
              scrollToBottom();
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

