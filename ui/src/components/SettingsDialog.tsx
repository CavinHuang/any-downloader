import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { open as pickFolder } from "@tauri-apps/plugin-dialog";
import { api, Settings } from "../api";
import { t } from "../i18n";
import { useOverlay } from "../useOverlay";

const SAVE_KEYS = [
  "default_dir",
  "segments",
  "task_bps",
  "global_bps",
  "proxy",
  "conflict",
  "after_done",
  "clipboard_monitor",
  "ffmpeg_path",
  "yt_dlp_path",
  "max_concurrent",
  "max_auto_retry",
  "auto_categorize",
] as const;

// 数值输入允许范围（§3.2/A21）：blur 与保存时 clamp，不依赖 HTML min/max 兜底
const NUM_BOUNDS: Partial<Record<keyof Settings, [number, number]>> = {
  segments: [1, 32],
  task_bps: [0, Number.MAX_SAFE_INTEGER],
  global_bps: [0, Number.MAX_SAFE_INTEGER],
  max_concurrent: [0, Number.MAX_SAFE_INTEGER],
  max_auto_retry: [0, Number.MAX_SAFE_INTEGER],
};

export function SettingsDialog({ onClose }: { onClose: () => void }) {
  // hooks 全部置于顶部、无条件执行（条件渲染只在 return 阶段）
  const [s, setS] = useState<Settings | null>(null);
  const [saved, setSaved] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [clipOn, setClipOn] = useState(true);
  const [afterDone, setAfterDone] = useState("none");
  const [autoOn, setAutoOn] = useState(false);
  const [version, setVersion] = useState("");
  const [nmOk, setNmOk] = useState<boolean | null>(null);
  const [invalid, setInvalid] = useState<Record<string, boolean>>({});
  // dirty 基线（§3.6）：载入值；保存成功后基线重置为已保存值
  const [baseline, setBaseline] = useState("");

  const snapshotOf = (v: Settings, clip: boolean, done: string, auto: boolean) =>
    JSON.stringify([v, clip, done, auto]);

  useEffect(() => {
    // 一次性并发载入；全部就位后再定基线（避免 autoOn 迟到造成假 dirty）
    Promise.all([
      api.getSettings(),
      api.getAutostart().catch(() => false),
      getVersion().catch(() => ""),
      api.nmRegistered().catch(() => null as boolean | null),
    ]).then(([v, auto, ver, nm]) => {
      const clip = v.clipboard_monitor !== "off";
      const done = v.after_done || "none";
      setS(v);
      setClipOn(clip);
      setAfterDone(done);
      setAutoOn(auto);
      setVersion(ver);
      setNmOk(nm);
      setBaseline(snapshotOf(v, clip, done, auto));
    });
  }, []);

  const dirty =
    !!s && baseline !== "" && snapshotOf(s, clipOn, afterDone, autoOn) !== baseline;

  // §3.6：Esc / 遮罩 / 取消在有未保存修改时先确认
  const requestClose = () => {
    if (dirty && !window.confirm(t("dirtyConfirm"))) return;
    onClose();
  };
  const { ref, onKeyDown } = useOverlay(requestClose);

  if (!s) return null;

  const field = (key: keyof Settings) => s[key] ?? "";
  const set = (key: keyof Settings, value: string) =>
    setS({ ...s, [key]: value === "" ? null : value });

  const clampField = (key: keyof Settings) => {
    const bounds = NUM_BOUNDS[key];
    if (!bounds) return;
    const raw = field(key);
    if (raw === "") {
      setInvalid((m) => ({ ...m, [key]: false }));
      return;
    }
    const n = Number(raw);
    if (!Number.isFinite(n)) {
      setInvalid((m) => ({ ...m, [key]: true }));
      return;
    }
    setInvalid((m) => ({ ...m, [key]: false }));
    const clamped = String(Math.min(bounds[1], Math.max(bounds[0], n)));
    if (clamped !== raw) set(key, clamped);
  };

  const save = async () => {
    if (saving) return;
    setSaveError(null);
    // 保存时 clamp + 校验（§3.2）
    const next: Settings = { ...s };
    let bad = false;
    for (const key of Object.keys(NUM_BOUNDS) as (keyof Settings)[]) {
      const raw = next[key] ?? "";
      if (raw === "") continue;
      const n = Number(raw);
      const bounds = NUM_BOUNDS[key]!;
      if (!Number.isFinite(n)) {
        bad = true;
        setInvalid((m) => ({ ...m, [key]: true }));
        continue;
      }
      next[key] = String(Math.min(bounds[1], Math.max(bounds[0], n)));
    }
    if (bad) {
      setSaveError(t("invalidNumber"));
      return;
    }
    setS(next);
    setSaving(true);
    try {
      for (const key of SAVE_KEYS) {
        const v = next[key];
        if (v != null) await api.setSetting(key, v);
      }
      await api.setAutostart(autoOn);
      // 基线重置为已保存值（§3.6）：此后取消/Esc 不再提示
      setBaseline(snapshotOf(next, clipOn, afterDone, autoOn));
      setSaved(true);
      // §3.6 取舍：保留自动关闭——「改一项 → 存 → 关」保持 1 次点击；
      // 800ms 保证「已保存」可读。失败静默/遮罩丢改动的风险已由
      // 错误行不关闭 + dirty 确认消除。
      setTimeout(onClose, 800);
    } catch (e) {
      // 保存失败：停留原地 + 框内错误行（带原因），不关闭
      setSaved(false);
      setSaveError(`${t("saveFailed")}: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const openLogs = async () => {
    try {
      await api.openLogsDir();
    } catch (e) {
      setSaveError(String(e));
    }
  };

  const numInput = (key: keyof Settings, placeholder: string) => (
    <div>
      <input
        type="number"
        min={NUM_BOUNDS[key]?.[0]}
        max={NUM_BOUNDS[key]?.[1]}
        value={field(key)}
        placeholder={placeholder}
        className={invalid[key] ? "input-err" : ""}
        onChange={(e) => set(key, e.target.value)}
        onBlur={() => clampField(key)}
      />
      {invalid[key] && <div className="err-line">{t("invalidNumber")}</div>}
    </div>
  );

  return (
    <div className="modal-mask" onClick={requestClose}>
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label={t("settingsTitle")}
        ref={ref}
        onKeyDown={onKeyDown}
        onClick={(e) => e.stopPropagation()}
      >
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
            {numInput("segments", "8")}
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
            {numInput("task_bps", "0")}
          </div>
          <div>
            <label>{t("globalSpeed")}</label>
            {numInput("global_bps", "0")}
          </div>
        </div>
        <div className="grid2">
          <div>
            <label>{t("maxConcurrent")}</label>
            {numInput("max_concurrent", "0")}
          </div>
          <div>
            <label>{t("maxAutoRetry")}</label>
            {numInput("max_auto_retry", "3")}
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
        <label>{t("ytdlpPath")}</label>
        <input
          value={field("yt_dlp_path")}
          placeholder={t("ytdlpPlaceholder")}
          onChange={(e) => set("yt_dlp_path", e.target.value)}
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
          {t("autoCategorize")}
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
            <span style={{ color: "var(--ok-ink)" }}>{t("nmOk")}</span>
          ) : (
            <span style={{ color: "var(--err-ink)" }}>{t("nmMissing")}</span>
          )}
        </label>
        {version && (
          <div style={{ marginTop: 10, color: "var(--muted)", fontSize: 11 }}>
            v{version}
          </div>
        )}
        {saveError && <div className="error">{saveError}</div>}
        <div className="modal-actions">
          <button onClick={openLogs}>{t("openLogs")}</button>
          <div className="spacer" />
          {saved && <span className="saved">{t("saved")}</span>}
          <button onClick={requestClose}>{t("cancelBtn")}</button>
          <button className="primary" onClick={save} disabled={saving}>
            {t("save")}
            {saving ? "…" : ""}
          </button>
        </div>
      </div>
    </div>
  );
}
