import { useEffect, useRef, useState } from "react";
import {
  checkForUpdate,
  downloadAndInstallUpdate,
  getAppVersion,
  getConfig,
  getTraeCliStatus,
  installTraeCli,
  listenUpdateProgress,
  openExternalUrl,
  saveConfig,
  LOG_LEVEL_OPTIONS,
  type AppConfig,
  type LogLevelSetting,
  type TraeCliStatus,
  type UpdateDownloadProgress,
} from "../api/tauri";
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
  const [port, setPort] = useState(60111);
  // No longer editable: keep the configured value so saving doesn't reset it.
  const workdirRef = useRef("");
  const [traeCmd, setTraeCmd] = useState("traecli");
  const [traeArgs, setTraeArgs] = useState("acp, serve");
  const [sandbox, setSandbox] = useState(true);
  const [logLevel, setLogLevel] = useState<LogLevelSetting>("info");
  const [autostart, setAutostart] = useState(false);
  const [autoCheckUpdate, setAutoCheckUpdate] = useState(false);
  const [appVersion, setAppVersion] = useState("");
  const [updateAvailable, setUpdateAvailable] = useState(false);
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

  const refreshTraeCliStatus = async () => {
    try {
      setTraeCliStatus(await getTraeCliStatus());
    } catch {
      setTraeCliStatus(null);
    }
  };

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
      })
      .catch((e: unknown) => setStatus(`Error: ${e}`));

    getAppVersion()
      .then((v) => setAppVersion(v))
      .catch(() => setAppVersion("unknown"));

    void refreshTraeCliStatus();
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
      await downloadAndInstallUpdate(accelerateDownload);
      setStatus("Installer launched. Follow the on-screen steps to finish.");
      setDownloadProgress(null);
    } catch (e: unknown) {
      setManualDownloadNeeded(true);
      setStatus(`Install failed: ${e}`);
    } finally {
      unlisten?.();
      setInstallingUpdate(false);
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
      {updateAvailable && (
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
        {updateAvailable && (
          <button
            type="button"
            disabled={installingUpdate}
            onClick={() => void handleInstallUpdate()}
          >
            {installingUpdate
              ? "Downloading…"
              : `Install v${latestVersion ?? ""}`}
          </button>
        )}
      </div>
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
      {manualDownloadNeeded && releaseDownloadUrl && (
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
