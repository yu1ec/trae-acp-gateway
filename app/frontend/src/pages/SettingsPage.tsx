import { useEffect, useState } from "react";
import { getConfig, pickWorkdir, saveConfig, type AppConfig } from "../api/tauri";
import "../styles/settings.css";

export default function SettingsPage() {
  const [port, setPort] = useState(8080);
  const [workdir, setWorkdir] = useState("");
  const [traeCmd, setTraeCmd] = useState("traecli");
  const [traeArgs, setTraeArgs] = useState("acp, serve");
  const [sandbox, setSandbox] = useState(true);
  const [debug, setDebug] = useState(false);
  const [autostart, setAutostart] = useState(false);
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
      })
      .catch((e: unknown) => setStatus(`Error: ${e}`));
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
      };
      await saveConfig(cfg);
      setStatus("Saved. Gateway restarted if it was running.");
    } catch (e: unknown) {
      setStatus(`Error: ${e}`);
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
      <button id="save" type="button" onClick={() => void handleSave()}>
        Save
      </button>
      <div id="status">{status}</div>
    </div>
  );
}
