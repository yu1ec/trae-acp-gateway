import { useEffect, useState } from "react";
import {
  checkForUpdate,
  downloadAndInstallUpdate,
  getAppVersion,
  getConfig,
  pickWorkdir,
  saveConfig,
  type AppConfig,
} from "../api/tauri";
import "../styles/settings.css";

export default function SettingsPage() {
  const [port, setPort] = useState(8080);
  const [workdir, setWorkdir] = useState("");
  const [traeCmd, setTraeCmd] = useState("traecli");
  const [traeArgs, setTraeArgs] = useState("acp, serve");
  const [sandbox, setSandbox] = useState(true);
  const [debug, setDebug] = useState(false);
  const [autostart, setAutostart] = useState(false);
  const [autoCheckUpdate, setAutoCheckUpdate] = useState(false);
  const [appVersion, setAppVersion] = useState("");
  const [updateAvailable, setUpdateAvailable] = useState(false);
  const [latestVersion, setLatestVersion] = useState<string | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [installingUpdate, setInstallingUpdate] = useState(false);
  const [status, setStatus] = useState("");

  useEffect(() => {
    getConfig()
      .then((cfg) => {
        setPort(cfg.port);
        setWorkdir(cfg.workdir);
        setTraeCmd(cfg.trae_cmd);
        setTraeArgs(cfg.trae_args.join(", "));
        setSandbox(cfg.sandbox);
        setDebug(cfg.debug);
        setAutostart(cfg.autostart);
        setAutoCheckUpdate(cfg.auto_check_update);
      })
      .catch((e: unknown) => setStatus(`Error: ${e}`));

    getAppVersion()
      .then((v) => setAppVersion(v))
      .catch(() => setAppVersion("unknown"));
  }, []);

  const handlePick = async () => {
    const dir = await pickWorkdir();
    if (dir) setWorkdir(dir);
  };

  const handleSave = async () => {
    try {
      const cfg: AppConfig = {
        port: parseInt(String(port), 10) || 8080,
        workdir: workdir.trim(),
        trae_cmd: traeCmd.trim() || "traecli",
        trae_args: traeArgs
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
        sandbox,
        debug,
        autostart,
        auto_check_update: autoCheckUpdate,
      };
      await saveConfig(cfg);
      setStatus("Saved. Gateway restarted if it was running.");
    } catch (e: unknown) {
      setStatus(`Error: ${e}`);
    }
  };

  const handleCheckUpdate = async () => {
    setCheckingUpdate(true);
    try {
      const info = await checkForUpdate();
      setUpdateAvailable(info.update_available);
      setLatestVersion(info.latest_version);
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
    try {
      await downloadAndInstallUpdate();
      setStatus("Installer launched. Follow the on-screen steps to finish.");
    } catch (e: unknown) {
      setStatus(`Install failed: ${e}`);
    } finally {
      setInstallingUpdate(false);
    }
  };

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
        <label htmlFor="workdir">Workdir</label>
        <div className="inline">
          <input
            type="text"
            id="workdir"
            value={workdir}
            onChange={(e) => setWorkdir(e.target.value)}
          />
          <button type="button" onClick={() => void handlePick()}>
            Choose…
          </button>
        </div>
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
      <div className="row checks">
        <label>
          <input
            type="checkbox"
            checked={debug}
            onChange={(e) => setDebug(e.target.checked)}
          />{" "}
          Debug logging
        </label>
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
      <div className="row inline update-actions">
        <button
          type="button"
          disabled={checkingUpdate}
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
            {installingUpdate ? "Downloading…" : `Install v${latestVersion ?? ""}`}
          </button>
        )}
      </div>

      <button id="save" type="button" onClick={() => void handleSave()}>
        Save
      </button>
      <div id="status">{status}</div>
    </div>
  );
}
