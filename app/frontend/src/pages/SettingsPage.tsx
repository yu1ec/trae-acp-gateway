import {
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  checkForUpdate,
  downloadAndInstallUpdate,
  getAppVersion,
  getConfig,
  getPermissionStatus,
  getTraeCliStatus,
  installTraeCli,
  listenUpdateProgress,
  openExternalUrl,
  openPermissionSettings,
  resetMediaLibraryPermission,
  revealAppInFinder,
  saveConfig,
  showThoughtsWindow,
  LOG_LEVEL_OPTIONS,
  type AppConfig,
  type LogLevelSetting,
  type PermissionPane,
  type PermissionStatus,
  type TraeCliStatus,
  type UpdateDownloadProgress,
} from "../api/tauri";
import {
  applyTheme,
  loadTheme,
  subscribeTheme,
  THEME_OPTIONS,
  type ThemeSetting,
} from "../lib/theme";
import "../styles/settings.css";

const TRAE_CLI_DOCS_URL = "https://docs.trae.cn/cli_about-trae-code-cli-2";

const TABS = [
  { id: "general", label: "常规" },
  { id: "updates", label: "更新" },
  { id: "permissions", label: "权限" },
] as const;

type SettingsTab = (typeof TABS)[number]["id"];

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function formatSpeed(bytesPerSec: number): string {
  return `${formatBytes(bytesPerSec)}/s`;
}

/** One label/control pair. The copy column flexes, the control column keeps
 *  its intrinsic width, so rows line up across every section. */
function SettingRow({
  label,
  hint,
  control,
  top = false,
}: {
  label: ReactNode;
  hint?: ReactNode;
  control: ReactNode;
  top?: boolean;
}) {
  return (
    <div className={top ? "setting-row top" : "setting-row"}>
      <div className="setting-copy">
        <div className="setting-label">{label}</div>
        {hint != null && <p className="settings-hint">{hint}</p>}
      </div>
      <div className="setting-control">{control}</div>
    </div>
  );
}

function Switch({
  id,
  label,
  checked,
  disabled = false,
  onChange,
}: {
  id?: string;
  label: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="switch">
      <input
        id={id}
        type="checkbox"
        role="switch"
        aria-label={label}
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className="switch-track" aria-hidden="true" />
    </label>
  );
}

export default function SettingsPage() {
  const [tab, setTab] = useState<SettingsTab>("general");
  const [theme, setTheme] = useState<ThemeSetting>(loadTheme);
  const [port, setPort] = useState(60111);
  // No longer editable: keep the configured value so saving doesn't reset it.
  const workdirRef = useRef("");
  const [logLevel, setLogLevel] = useState<LogLevelSetting>("info");
  const [autostart, setAutostart] = useState(false);
  const [autoCheckUpdate, setAutoCheckUpdate] = useState(false);
  const [thoughtPanelEnabled, setThoughtPanelEnabled] = useState(false);
  const [appVersion, setAppVersion] = useState("");
  const [updateAvailable, setUpdateAvailable] = useState(false);
  const [autoInstall, setAutoInstall] = useState(false);
  const [latestVersion, setLatestVersion] = useState<string | null>(null);
  const [releaseDownloadUrl, setReleaseDownloadUrl] = useState<string | null>(
    null,
  );
  const [accelerateDownload, setAccelerateDownload] = useState(true);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [installingUpdate, setInstallingUpdate] = useState(false);
  const [downloadProgress, setDownloadProgress] =
    useState<UpdateDownloadProgress | null>(null);
  const [status, setStatus] = useState("");
  const [manualDownloadNeeded, setManualDownloadNeeded] = useState(false);
  const [traeCliStatus, setTraeCliStatus] = useState<TraeCliStatus | null>(
    null,
  );
  const [installingTraeCli, setInstallingTraeCli] = useState(false);
  const [traeCliInstallFailed, setTraeCliInstallFailed] = useState(false);
  const [permissionStatus, setPermissionStatus] =
    useState<PermissionStatus | null>(null);

  const refreshPermissionStatus = async () => {
    try {
      setPermissionStatus(await getPermissionStatus());
    } catch {
      setPermissionStatus(null);
    }
  };

  const refreshTraeCliStatus = async () => {
    try {
      const next = await getTraeCliStatus();
      setTraeCliStatus(next);
      if (next.installed) setTraeCliInstallFailed(false);
    } catch {
      setTraeCliStatus(null);
    }
  };

  // Another window may change the theme while this one is open.
  useEffect(() => subscribeTheme(setTheme), []);

  useEffect(() => {
    getConfig()
      .then((cfg) => {
        setPort(cfg.port);
        workdirRef.current = cfg.workdir ?? "";
        setLogLevel(cfg.log_level ?? "info");
        setAutostart(cfg.autostart);
        setAutoCheckUpdate(cfg.auto_check_update);
        setThoughtPanelEnabled(cfg.thought_panel_enabled);
      })
      .catch((e: unknown) => setStatus(`读取配置失败：${e}`));

    getAppVersion()
      .then((v) => setAppVersion(v))
      .catch(() => setAppVersion("unknown"));

    void refreshTraeCliStatus();
    void refreshPermissionStatus();
  }, []);

  const handleInstallTraeCli = async () => {
    setInstallingTraeCli(true);
    setTraeCliInstallFailed(false);
    try {
      const next = await installTraeCli();
      setTraeCliStatus(next);
      setStatus("TRAE CLI 安装完成。");
    } catch (e: unknown) {
      setTraeCliInstallFailed(true);
      setStatus(`TRAE CLI 安装失败：${e}`);
      await refreshTraeCliStatus();
    } finally {
      setInstallingTraeCli(false);
    }
  };

  const handleTheme = (next: ThemeSetting) => {
    setTheme(next);
    applyTheme(next);
  };

  const handleSave = async () => {
    try {
      const cfg: AppConfig = {
        port: parseInt(String(port), 10) || 60111,
        workdir: workdirRef.current,
        log_level: logLevel,
        autostart,
        auto_check_update: autoCheckUpdate,
        thought_panel_enabled: thoughtPanelEnabled,
      };
      await saveConfig(cfg);
      void refreshTraeCliStatus();
      setStatus("已保存。网关正在运行时会自动重启。");
    } catch (e: unknown) {
      setStatus(`保存失败：${e}`);
    }
  };

  const handleCheckUpdate = async () => {
    setCheckingUpdate(true);
    setManualDownloadNeeded(false);
    try {
      const info = await checkForUpdate();
      setUpdateAvailable(info.update_available);
      setAutoInstall(info.auto_install);
      setLatestVersion(info.latest_version);
      setReleaseDownloadUrl(info.download_url);
      if (info.update_available && info.latest_version) {
        setStatus(`发现新版本 v${info.latest_version}`);
      } else {
        setStatus("已是最新版本。");
      }
    } catch (e: unknown) {
      setStatus(`检查更新失败：${e}`);
    } finally {
      setCheckingUpdate(false);
    }
  };

  const handleInstallUpdate = async () => {
    setInstallingUpdate(true);
    setDownloadProgress(null);
    setManualDownloadNeeded(false);
    let unlisten: (() => void) | undefined;
    try {
      unlisten = await listenUpdateProgress((p) => setDownloadProgress(p));
      setStatus("正在下载并安装更新，完成后会自动重启…");
      await downloadAndInstallUpdate(accelerateDownload);
      setStatus("更新已安装，正在重启应用…");
      setDownloadProgress(null);
    } catch (e: unknown) {
      setManualDownloadNeeded(true);
      setStatus(`安装更新失败：${e}`);
    } finally {
      unlisten?.();
      setInstallingUpdate(false);
    }
  };

  const handleOpenPermission = async (pane: PermissionPane) => {
    try {
      await openPermissionSettings(pane);
    } catch (e: unknown) {
      setStatus(`打开系统设置失败：${e}`);
    }
  };

  const handleRevealApp = async () => {
    try {
      await revealAppInFinder();
    } catch (e: unknown) {
      setStatus(`在 Finder 中显示失败：${e}`);
    }
  };

  const handleResetMediaLibrary = async () => {
    try {
      await resetMediaLibraryPermission();
      setStatus("已清除媒体库授权记录。");
      await refreshPermissionStatus();
    } catch (e: unknown) {
      setStatus(`清除媒体库授权失败：${e}`);
    }
  };

  const progressPercent =
    downloadProgress?.total && downloadProgress.total > 0
      ? Math.min(
          100,
          (downloadProgress.downloaded / downloadProgress.total) * 100,
        )
      : undefined;

  return (
    <div className="settings-page">
      <nav
        className="settings-tabs"
        role="tablist"
        aria-label="设置分类"
        onKeyDown={(e) => {
          if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
          e.preventDefault();
          const index = TABS.findIndex((item) => item.id === tab);
          const delta = e.key === "ArrowRight" ? 1 : -1;
          const next = TABS[(index + delta + TABS.length) % TABS.length];
          setTab(next.id);
          document.getElementById(`settings-tab-${next.id}`)?.focus();
        }}
      >
        {TABS.map((item) => (
          <button
            key={item.id}
            type="button"
            role="tab"
            id={`settings-tab-${item.id}`}
            aria-selected={tab === item.id}
            aria-controls="settings-panel"
            tabIndex={tab === item.id ? 0 : -1}
            className={
              tab === item.id ? "settings-tab active" : "settings-tab"
            }
            onClick={() => setTab(item.id)}
          >
            {item.label}
          </button>
        ))}
      </nav>

      <main
        className="settings-content"
        id="settings-panel"
        role="tabpanel"
        aria-labelledby={`settings-tab-${tab}`}
      >
        {tab === "general" && (
          <>
            <section className="settings-section">
              <h3 className="section-heading">外观</h3>
              <SettingRow
                label={<label htmlFor="theme">主题</label>}
                hint="立即生效，并同步到日志与思考面板。"
                control={
                  <select
                    id="theme"
                    className="control-select"
                    value={theme}
                    onChange={(e) =>
                      handleTheme(e.target.value as ThemeSetting)
                    }
                  >
                    {THEME_OPTIONS.map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {opt.label}
                      </option>
                    ))}
                  </select>
                }
              />
            </section>

            <section className="settings-section">
              <h3 className="section-heading">网关</h3>
              <SettingRow
                label={<label htmlFor="port">端口</label>}
                hint="本地监听端口。保存后如果网关正在运行，会自动重启。"
                control={
                  <input
                    type="number"
                    id="port"
                    className="control-input"
                    min={1}
                    max={65535}
                    value={port}
                    onChange={(e) => setPort(Number(e.target.value))}
                  />
                }
              />
              <SettingRow
                label={<label htmlFor="log_level">日志级别</label>}
                hint="控制日志窗口中记录和显示的内容。"
                control={
                  <select
                    id="log_level"
                    className="control-select"
                    value={logLevel}
                    onChange={(e) =>
                      setLogLevel(e.target.value as LogLevelSetting)
                    }
                  >
                    {LOG_LEVEL_OPTIONS.map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {opt.label}
                      </option>
                    ))}
                  </select>
                }
              />
              <SettingRow
                label={<label htmlFor="autostart">登录时启动</label>}
                hint="登录系统后在后台启动网关。"
                control={
                  <Switch
                    id="autostart"
                    label="登录时启动"
                    checked={autostart}
                    onChange={setAutostart}
                  />
                }
              />
            </section>

            <section className="settings-section">
              <h3 className="section-heading">运行环境</h3>
              <SettingRow
                label="TRAE CLI"
                hint={
                  traeCliStatus == null ? (
                    "正在检测…"
                  ) : traeCliStatus.installed ? (
                    <>
                      已找到 <code>{traeCliStatus.path ?? ""}</code>
                    </>
                  ) : (
                    "未找到，需要安装后才能运行 Agent。"
                  )
                }
                control={
                  traeCliStatus?.installed ? (
                    <span className="status-badge">已就绪</span>
                  ) : traeCliStatus == null ? null : (
                    <button
                      type="button"
                      className="btn"
                      disabled={installingTraeCli}
                      onClick={() => void handleInstallTraeCli()}
                    >
                      {installingTraeCli ? "安装中…" : "安装"}
                    </button>
                  )
                }
              />
              {traeCliInstallFailed && (
                <div className="settings-callout warn">
                  自动安装失败，请按官网说明手动安装。
                  <div className="callout-actions">
                    <button
                      type="button"
                      className="link-button"
                      onClick={() => void openExternalUrl(TRAE_CLI_DOCS_URL)}
                    >
                      打开官网安装说明
                    </button>
                    <button
                      type="button"
                      className="link-button"
                      onClick={() => void refreshTraeCliStatus()}
                    >
                      重新检测
                    </button>
                  </div>
                </div>
              )}
            </section>

            <section className="settings-section">
              <h3 className="section-heading">思考面板</h3>
              <SettingRow
                label={
                  <label htmlFor="thought_panel">启用思考面板</label>
                }
                hint="悬浮显示 ACP 思考流，拖到屏幕边缘可自动隐藏。"
                control={
                  <Switch
                    id="thought_panel"
                    label="启用思考面板"
                    checked={thoughtPanelEnabled}
                    onChange={setThoughtPanelEnabled}
                  />
                }
              />
              <SettingRow
                label="思考窗口"
                hint="在独立窗口中查看当前会话的思考过程。"
                control={
                  <button
                    type="button"
                    className="btn"
                    disabled={!thoughtPanelEnabled}
                    onClick={() => void showThoughtsWindow()}
                  >
                    打开
                  </button>
                }
              />
            </section>
          </>
        )}

        {tab === "updates" && (
          <>
            <section className="settings-section">
              <h3 className="section-heading">版本</h3>
              <SettingRow
                label="当前版本"
                control={
                  <span className="value-mono">v{appVersion || "…"}</span>
                }
              />
              <SettingRow
                label={<label htmlFor="auto_update">启动时检查更新</label>}
                hint="启动时自动向 GitHub Releases 查询新版本。"
                control={
                  <Switch
                    id="auto_update"
                    label="启动时检查更新"
                    checked={autoCheckUpdate}
                    onChange={setAutoCheckUpdate}
                  />
                }
              />
              {updateAvailable && autoInstall && (
                <SettingRow
                  label={<label htmlFor="accelerate">加速下载</label>}
                  hint="优先使用国内镜像，下载更稳定。"
                  control={
                    <Switch
                      id="accelerate"
                      label="加速下载"
                      checked={accelerateDownload}
                      disabled={installingUpdate}
                      onChange={setAccelerateDownload}
                    />
                  }
                />
              )}
              <div className="settings-action-row">
                <button
                  type="button"
                  className="btn"
                  disabled={checkingUpdate || installingUpdate}
                  onClick={() => void handleCheckUpdate()}
                >
                  {checkingUpdate ? "检查中…" : "检查更新"}
                </button>
                {updateAvailable && autoInstall && (
                  <button
                    type="button"
                    className="btn btn-primary"
                    disabled={installingUpdate}
                    onClick={() => void handleInstallUpdate()}
                  >
                    {installingUpdate
                      ? "下载中…"
                      : `安装 v${latestVersion ?? ""} 并重启`}
                  </button>
                )}
              </div>

              {updateAvailable && (
                <div
                  className={
                    autoInstall
                      ? "settings-callout info"
                      : "settings-callout warn"
                  }
                >
                  <strong>发现新版本 v{latestVersion}</strong>
                  <p>
                    {autoInstall
                      ? "更新会自动下载、原地安装并重启应用，不需要手动拖拽安装包。"
                      : "此版本没有提供自动更新包，请手动下载安装包；从下一个版本开始将支持应用内更新。"}
                  </p>
                </div>
              )}

              {installingUpdate && downloadProgress && (
                <div className="update-progress">
                  {downloadProgress.total != null ? (
                    <progress
                      max={downloadProgress.total}
                      value={downloadProgress.downloaded}
                    />
                  ) : (
                    <progress />
                  )}
                  <div className="update-progress-meta">
                    <span>
                      {formatBytes(downloadProgress.downloaded)}
                      {downloadProgress.total != null
                        ? ` / ${formatBytes(downloadProgress.total)}`
                        : ""}
                    </span>
                    <span>{formatSpeed(downloadProgress.bytes_per_sec)}</span>
                    {progressPercent != null && (
                      <span>{progressPercent.toFixed(0)}%</span>
                    )}
                  </div>
                </div>
              )}

              {(manualDownloadNeeded ||
                (updateAvailable && !autoInstall)) &&
                releaseDownloadUrl && (
                  <div className="callout-actions">
                    <button
                      type="button"
                      className="link-button"
                      onClick={() => void openExternalUrl(releaseDownloadUrl)}
                    >
                      在浏览器中手动下载安装包
                    </button>
                  </div>
                )}
            </section>
          </>
        )}

        {tab === "permissions" && (
          <section className="settings-section">
            <div className="section-heading-row">
              <h3 className="section-heading">系统权限</h3>
              <button
                type="button"
                className="btn btn-ghost"
                onClick={() => void refreshPermissionStatus()}
              >
                重新检测
              </button>
            </div>
            {permissionStatus?.platform === "macos" ? (
              <>
                {permissionStatus.translocated && (
                  <div className="settings-callout warn">
                    当前应用从 macOS 隔离路径运行。请先把应用拖到「应用程序」并重新打开，否则系统授权和自动更新都无法持久生效。
                  </div>
                )}
                {!permissionStatus.translocated &&
                  !permissionStatus.in_applications && (
                    <div className="settings-callout warn">
                      当前应用不在「应用程序」目录。建议把 App 拖到
                      `/Applications` 后再运行，自动更新需要能原地重写应用包。
                    </div>
                  )}

                <SettingRow
                  top
                  label="完全磁盘访问（可选）"
                  hint={
                    <>
                      {permissionStatus.full_disk_access == null
                        ? "无法检测当前状态"
                        : permissionStatus.full_disk_access
                          ? "已授予"
                          : "未授予"}
                      。只有 Agent 需要读取桌面、文稿、下载或其它受保护目录时才需要。
                    </>
                  }
                  control={
                    <div className="setting-actions">
                      <button
                        type="button"
                        className="btn"
                        onClick={() =>
                          void handleOpenPermission("full-disk-access")
                        }
                      >
                        打开设置
                      </button>
                      <button
                        type="button"
                        className="btn btn-ghost"
                        onClick={() => void handleRevealApp()}
                      >
                        在 Finder 中显示
                      </button>
                    </div>
                  }
                />

                <SettingRow
                  top
                  label="文件与文件夹"
                  hint="Agent 首次访问桌面、文稿、下载或外接磁盘时会由 macOS 弹出授权，按提示允许即可。"
                  control={
                    <button
                      type="button"
                      className="btn"
                      onClick={() =>
                        void handleOpenPermission("files-and-folders")
                      }
                    >
                      打开设置
                    </button>
                  }
                />

                <SettingRow
                  top
                  label="通知"
                  hint="用于提示 Gateway 启动失败、发现新版本等事件。"
                  control={
                    <button
                      type="button"
                      className="btn"
                      onClick={() => void handleOpenPermission("notifications")}
                    >
                      打开设置
                    </button>
                  }
                />

                <SettingRow
                  top
                  label="媒体与 Apple 音乐"
                  hint="网关不需要媒体库权限。如果误授权，可清除记录；下次 Agent 扫描受保护目录时 macOS 可能再次询问，选择「不允许」即可。"
                  control={
                    <button
                      type="button"
                      className="btn btn-ghost"
                      onClick={() => void handleResetMediaLibrary()}
                    >
                      清除授权记录
                    </button>
                  }
                />

                {permissionStatus.app_path && (
                  <p className="settings-footnote">
                    应用位置：{permissionStatus.app_path}
                  </p>
                )}
              </>
            ) : (
              <p className="settings-empty">
                当前平台不需要额外的系统隐私授权。
              </p>
            )}
          </section>
        )}
      </main>

      <footer className="settings-footer">
        <span
          id="status"
          className="settings-status"
          role="status"
          aria-live="polite"
        >
          {status}
        </span>
        <button
          id="save"
          type="button"
          className="btn btn-primary"
          onClick={() => void handleSave()}
        >
          保存
        </button>
      </footer>
    </div>
  );
}
