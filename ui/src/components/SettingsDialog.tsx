import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { open as pickFolder } from "@tauri-apps/plugin-dialog";
import { api, Settings } from "../api";
import { t } from "../i18n";

export function SettingsDialog({ onClose }: { onClose: () => void }) {
  // hooks 全部置于顶部、无条件执行（条件渲染只在 return 阶段）
  const [s, setS] = useState<Settings | null>(null);
  const [saved, setSaved] = useState(false);
  const [clipOn, setClipOn] = useState(true);
  const [afterDone, setAfterDone] = useState("none");
  const [autoOn, setAutoOn] = useState(false);
  const [version, setVersion] = useState("");
  const [nmOk, setNmOk] = useState<boolean | null>(null);

  useEffect(() => {
    api.getSettings().then((v) => {
      setS(v);
      // 从加载结果同步派生状态（useState 初值只在首渲染取一次，不能依赖异步数据）
      setClipOn(v.clipboard_monitor !== "off");
      setAfterDone(v.after_done || "none");
    });
    api.getAutostart().then(setAutoOn).catch(() => {});
    getVersion().then(setVersion).catch(() => {});
    api.nmRegistered().then(setNmOk).catch(() => setNmOk(null));
  }, []);

  if (!s) return null;

  const field = (key: keyof Settings) => s[key] ?? "";
  const set = (key: keyof Settings, value: string) =>
    setS({ ...s, [key]: value === "" ? null : value });

  const save = async () => {
    for (const key of [
      "default_dir",
      "segments",
      "task_bps",
      "global_bps",
      "proxy",
      "conflict",
      "after_done",
      "clipboard_monitor",
      "ffmpeg_path",
      "max_concurrent",
      "max_auto_retry",
      "auto_categorize",
    ] as const) {
      const v = s[key];
      if (v != null) await api.setSetting(key, v);
    }
    await api.setAutostart(autoOn).catch(() => {});
    setSaved(true);
    setTimeout(onClose, 500);
  };

  return (
    <div className="modal-mask" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h3>{t("settingsTitle")}</h3>
        <label>{t("defaultDir")}</label>
        <div style={{ display: "flex", gap: 6 }}>
          <input
            value={field("default_dir")}
            placeholder={t("defaultDir")}
            onChange={(e) => set("default_dir", e.target.value)}
          />
          <button
            style={{ whiteSpace: "nowrap" }}
            onClick={async () => {
              const picked = await pickFolder({ directory: true });
              if (typeof picked === "string") set("default_dir", picked);
            }}
          >
            {t("browse")}
          </button>
        </div>
        <div className="grid2">
          <div>
            <label>{t("segments")}</label>
            <input
              type="number"
              min={1}
              max={32}
              value={field("segments")}
              placeholder="8"
              onChange={(e) => set("segments", e.target.value)}
            />
          </div>
          <div>
            <label>{t("conflict")}</label>
            <select
              value={field("conflict") || "rename"}
              onChange={(e) => set("conflict", e.target.value)}
            >
              <option value="rename">{t("conflictRename")}</option>
              <option value="overwrite">{t("conflictOverwrite")}</option>
              <option value="skip">{t("conflictSkip")}</option>
              <option value="ask">{t("conflictAsk")}</option>
            </select>
          </div>
        </div>
        <div className="grid2">
          <div>
            <label>{t("taskSpeed")}</label>
            <input
              type="number"
              min={0}
              value={field("task_bps")}
              placeholder="0"
              onChange={(e) => set("task_bps", e.target.value)}
            />
          </div>
          <div>
            <label>{t("globalSpeed")}</label>
            <input
              type="number"
              min={0}
              value={field("global_bps")}
              placeholder="0"
              onChange={(e) => set("global_bps", e.target.value)}
            />
          </div>
        </div>
        <div className="grid2">
          <div>
            <label>{t("maxConcurrent")}</label>
            <input
              type="number"
              min={0}
              value={field("max_concurrent")}
              placeholder="0"
              onChange={(e) => set("max_concurrent", e.target.value)}
            />
          </div>
          <div>
            <label>{t("maxAutoRetry")}</label>
            <input
              type="number"
              min={0}
              value={field("max_auto_retry")}
              placeholder="3"
              onChange={(e) => set("max_auto_retry", e.target.value)}
            />
          </div>
        </div>
        <label>{t("proxy")}</label>
        <input
          value={field("proxy")}
          placeholder="socks5h://127.0.0.1:1080"
          onChange={(e) => set("proxy", e.target.value)}
        />
        <label>{t("ffmpegPath")}</label>
        <input
          value={field("ffmpeg_path")}
          placeholder="C:\ffmpeg\bin\ffmpeg.exe"
          onChange={(e) => set("ffmpeg_path", e.target.value)}
        />
        <label>{t("afterDone")}</label>
        <select
          value={afterDone}
          onChange={(e) => {
            setAfterDone(e.target.value);
            set("after_done", e.target.value);
          }}
        >
          <option value="none">{t("afterDoneNone")}</option>
          <option value="open_file">{t("afterDoneOpenFile")}</option>
          <option value="open_folder">{t("afterDoneOpenFolder")}</option>
          <option value="shutdown">{t("afterDoneShutdown")}</option>
          <option value="sleep">{t("afterDoneSleep")}</option>
        </select>
        <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <input
            type="checkbox"
            style={{ width: "auto" }}
            checked={clipOn}
            onChange={(e) => {
              const on = e.target.checked;
              setClipOn(on);
              set("clipboard_monitor", on ? "on" : "off");
            }}
          />
          {t("clipboardMonitor")}
        </label>
        <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <input
            type="checkbox"
            style={{ width: "auto" }}
            checked={field("auto_categorize") === "on"}
            onChange={(e) =>
              set("auto_categorize", e.target.checked ? "on" : "off")
            }
          />
          自动分类子目录（视频/音乐/压缩包/文档/软件/图片）
        </label>
        <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <input
            type="checkbox"
            style={{ width: "auto" }}
            checked={autoOn}
            onChange={(e) => setAutoOn(e.target.checked)}
          />
          {t("autostart")}
        </label>
        <label style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 10 }}>
          {t("nmStatus")}:
          {nmOk == null ? (
            <span style={{ color: "var(--muted)" }}>…</span>
          ) : nmOk ? (
            <span style={{ color: "var(--ok)" }}>{t("nmOk")}</span>
          ) : (
            <span style={{ color: "var(--err)" }}>{t("nmMissing")}</span>
          )}
        </label>
        <button
          style={{ marginTop: 12 }}
          onClick={() => api.openLogsDir()}
        >
          {t("openLogs")}
        </button>
        {version && (
          <div style={{ marginTop: 10, color: "var(--muted)", fontSize: 11 }}>
            v{version}
          </div>
        )}
        <div className="modal-actions">
          <div className="spacer" />
          {saved && <span className="saved">{t("saved")}</span>}
          <button className="primary" onClick={save}>
            {t("save")}
          </button>
        </div>
      </div>
    </div>
  );
}
