import { useEffect, useRef, useState } from "react";
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

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function formatSpeed(bytesPerSec: number): string {
  return `${formatBytes(bytesPerSec)}/s`;
}

export default function SettingsPage() {
  const [theme, setTheme] = useState<ThemeSetting>(loadTheme);
  const [port, setPort] = useState(60111);
  // No longer editable: keep the configured value so saving doesn't reset it.
  const workdirRef = useRef("");
  const [traeCmd, setTraeCmd] = useState("traecli");
  const [traeArgs, setTraeArgs] = useState("acp, serve");
  const [sandbox, setSandbox] = useState(true);
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
      setTraeCliStatus(await getTraeCliStatus());
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
        setTraeCmd(cfg.trae_cmd);
        setTraeArgs(cfg.trae_args.join(", "));
        setSandbox(cfg.sandbox);
        setLogLevel(cfg.log_level ?? "info");
        setAutostart(cfg.autostart);
        setAutoCheckUpdate(cfg.auto_check_update);
        setThoughtPanelEnabled(cfg.thought_panel_enabled);
      })
      .catch((e: unknown) => setStatus(`Error: ${e}`));

    getAppVersion()
      .then((v) => setAppVersion(v))
      .catch(() => setAppVersion("unknown"));

    void refreshTraeCliStatus();
    void refreshPermissionStatus();
  }, []);

  const handleInstallTraeCli = async () => {
    setInstallingTraeCli(true);
    try {
      const next = await installTraeCli();
      setTraeCliStatus(next);
      setStatus("TRAE CLI 安装完成。");
    } catch (e: unknown) {
      setStatus(`TRAE CLI 安装失败: ${e}`);
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
        trae_cmd: traeCmd.trim() || "traecli",
        trae_args: traeArgs
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
        sandbox,
        log_level: logLevel,
        autostart,
        auto_check_update: autoCheckUpdate,
        thought_panel_enabled: thoughtPanelEnabled,
      };
      await saveConfig(cfg);
      void refreshTraeCliStatus();
      setStatus("Saved. Gateway restarted if it was running.");
    } catch (e: unknown) {
      setStatus(`Error: ${e}`);
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
        setStatus(`Update available: v${info.latest_version}`);
      } else {
        setStatus("Already up to date.");
      }
    } catch (e: unknown) {
      setStatus(`Update check failed: ${e}`);
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
      setStatus(`Install failed: ${e}`);
    } finally {
      unlisten?.();
      setInstallingUpdate(false);
    }
  };

  const handleOpenPermission = async (pane: PermissionPane) => {
    try {
      await openPermissionSettings(pane);
    } catch (e: unknown) {
      setStatus(`打开系统设置失败: ${e}`);
    }
  };

  const handleRevealApp = async () => {
    try {
      await revealAppInFinder();
    } catch (e: unknown) {
      setStatus(`在 Finder 中显示失败: ${e}`);
    }
  };

  const handleResetMediaLibrary = async () => {
    try {
      await resetMediaLibraryPermission();
      setStatus("已清除媒体库授权记录。");
      await refreshPermissionStatus();
    } catch (e: unknown) {
      setStatus(`清除媒体库授权失败: ${e}`);
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
      <h2 className="section-title">Appearance</h2>
      <div className="row">
        <label htmlFor="theme">主题</label>
        <select
          id="theme"
          value={theme}
          onChange={(e) => handleTheme(e.target.value as ThemeSetting)}
        >
          {THEME_OPTIONS.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {opt.label}
            </option>
          ))}
        </select>
      </div>
      <div className="row">
        <span className="settings-hint">
          立即生效，并同步到日志与思考面板。
        </span>
      </div>

      <h2 className="section-title">Gateway</h2>
      <div className="row">
        <label htmlFor="port">Port</label>
        <input
          type="number"
          id="port"
          min={1}
          max={65535}
          value={port}
          onChange={(e) => setPort(Number(e.target.value))}
        />
      </div>
      <div className="row">
        <label htmlFor="trae_cmd">Trae command</label>
        <input
          type="text"
          id="trae_cmd"
          value={traeCmd}
          onChange={(e) => setTraeCmd(e.target.value)}
        />
      </div>
      <div className="row">
        <label htmlFor="trae_args">Trae args (comma separated)</label>
        <input
          type="text"
          id="trae_args"
          value={traeArgs}
          onChange={(e) => setTraeArgs(e.target.value)}
        />
      </div>
      <div className="row">
        <label>TRAE CLI</label>
        <div className="inline">
          <span>
            {traeCliStatus == null
              ? "检测中…"
              : traeCliStatus.installed
                ? `已安装：${traeCliStatus.path ?? ""}`
                : "未安装"}
          </span>
          {traeCliStatus && !traeCliStatus.installed && (
            <button
              type="button"
              disabled={installingTraeCli}
              onClick={() => void handleInstallTraeCli()}
            >
              {installingTraeCli ? "安装中…" : "自动安装"}
            </button>
          )}
        </div>
      </div>
      <div className="row checks">
        <label>
          <input
            type="checkbox"
            checked={sandbox}
            onChange={(e) => setSandbox(e.target.checked)}
          />{" "}
          Sandbox
        </label>
      </div>
      <div className="row">
        <label htmlFor="log_level">Log level</label>
        <select
          id="log_level"
          value={logLevel}
          onChange={(e) => setLogLevel(e.target.value as LogLevelSetting)}
        >
          {LOG_LEVEL_OPTIONS.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {opt.label}
            </option>
          ))}
        </select>
      </div>
      <div className="row checks">
        <label>
          <input
            type="checkbox"
            checked={autostart}
            onChange={(e) => setAutostart(e.target.checked)}
          />{" "}
          Launch at login
        </label>
      </div>

      <h2 className="section-title">Thinking panel</h2>
      <div className="row checks">
        <label>
          <input
            type="checkbox"
            checked={thoughtPanelEnabled}
            onChange={(e) => setThoughtPanelEnabled(e.target.checked)}
          />{" "}
          启用思考面板
        </label>
      </div>
      <div className="row inline">
        <button
          type="button"
          disabled={!thoughtPanelEnabled}
          onClick={() => void showThoughtsWindow()}
        >
          打开思考面板
        </button>
        <span className="settings-hint">
          悬浮显示 ACP 思考流
        </span>
      </div>

      <h2 className="section-title">System permissions</h2>
      {permissionStatus?.platform === "macos" ? (
        <>
          {permissionStatus.translocated && (
            <div className="row permission-warning">
              当前应用从 macOS 隔离路径运行。请先把应用拖到「应用程序」并重新打开，否则系统授权和自动更新都无法持久生效。
            </div>
          )}
          {!permissionStatus.translocated && !permissionStatus.in_applications && (
            <div className="row permission-warning">
              当前应用不在「应用程序」目录。建议把 App 拖到 `/Applications` 后再运行，自动更新需要能原地重写应用包。
            </div>
          )}
          <div className="row permission-row">
            <div>
              <label>完全磁盘访问（可选）</label>
              <span className="settings-hint">
                {permissionStatus.full_disk_access == null
                  ? "无法检测当前状态"
                  : permissionStatus.full_disk_access
                    ? "已授予"
                    : "未授予"}
                。只有 Agent 需要读取桌面、文稿、下载或其它受保护目录时才需要。
              </span>
            </div>
            <div className="inline">
              <button
                type="button"
                onClick={() => void handleOpenPermission("full-disk-access")}
              >
                打开设置
              </button>
              <button type="button" onClick={() => void handleRevealApp()}>
                在 Finder 中显示
              </button>
            </div>
          </div>
          <div className="row permission-row">
            <div>
              <label>文件与文件夹</label>
              <span className="settings-hint">
                Agent 首次访问桌面、文稿、下载或外接磁盘时会由 macOS 弹出授权，按提示允许即可。
              </span>
            </div>
            <button
              type="button"
              onClick={() => void handleOpenPermission("files-and-folders")}
            >
              打开设置
            </button>
          </div>
          <div className="row permission-row">
            <div>
              <label>通知</label>
              <span className="settings-hint">
                用于提示 Gateway 启动失败、发现新版本等事件。
              </span>
            </div>
            <button
              type="button"
              onClick={() => void handleOpenPermission("notifications")}
            >
              打开设置
            </button>
          </div>
          <div className="row permission-row">
            <div>
              <label>媒体与 Apple 音乐</label>
              <span className="settings-hint">
                网关不需要媒体库权限。如果误授权，可清除记录；下次 Agent 扫描受保护目录时 macOS 可能再次询问，选择「不允许」即可。
              </span>
            </div>
            <button
              type="button"
              className="link-button"
              onClick={() => void handleResetMediaLibrary()}
            >
              清除授权记录
            </button>
          </div>
          {permissionStatus.app_path && (
            <div className="row">
              <span className="settings-hint">
                应用位置：{permissionStatus.app_path}
              </span>
            </div>
          )}
          <div className="row">
            <button type="button" onClick={() => void refreshPermissionStatus()}>
              重新检测
            </button>
          </div>
        </>
      ) : (
        <div className="row">
          <span className="settings-hint">
            当前平台不需要额外的系统隐私授权。
          </span>
        </div>
      )}

      <h2 className="section-title">Updates</h2>
      <div className="row">
        <label>Current version</label>
        <span>v{appVersion || "…"}</span>
      </div>
      <div className="row checks">
        <label>
          <input
            type="checkbox"
            checked={autoCheckUpdate}
            onChange={(e) => setAutoCheckUpdate(e.target.checked)}
          />{" "}
          Check for updates on startup
        </label>
      </div>
      {updateAvailable && autoInstall && (
        <div className="row checks">
          <label>
            <input
              type="checkbox"
              checked={accelerateDownload}
              disabled={installingUpdate}
              onChange={(e) => setAccelerateDownload(e.target.checked)}
            />{" "}
            加速下载（优先国内镜像）
          </label>
        </div>
      )}
      <div className="row inline update-actions">
        <button
          type="button"
          disabled={checkingUpdate || installingUpdate}
          onClick={() => void handleCheckUpdate()}
        >
          {checkingUpdate ? "Checking…" : "Check now"}
        </button>
        {updateAvailable && autoInstall && (
          <button
            type="button"
            disabled={installingUpdate}
            onClick={() => void handleInstallUpdate()}
          >
            {installingUpdate
              ? "Downloading…"
              : `安装 v${latestVersion ?? ""} 并重启`}
          </button>
        )}
      </div>
      {updateAvailable && (
        <div className="row">
          <span className="settings-hint">
            {autoInstall
              ? "更新会自动下载、原地安装并重启应用，不需要手动拖拽安装包。"
              : "此版本没有提供自动更新包，请手动下载安装包；从下一个版本开始将支持应用内更新。"}
          </span>
        </div>
      )}
      {installingUpdate && downloadProgress && (
        <div className="row update-progress">
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
      {(manualDownloadNeeded || (updateAvailable && !autoInstall)) &&
        releaseDownloadUrl && (
        <div className="row">
          <button
            type="button"
            className="link-button"
            onClick={() => void openExternalUrl(releaseDownloadUrl)}
          >
            在浏览器中手动下载安装包
          </button>
        </div>
      )}

      <button id="save" type="button" onClick={() => void handleSave()}>
        Save
      </button>
      <div id="status">{status}</div>
    </div>
  );
}
