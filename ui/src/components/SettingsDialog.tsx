import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { open as pickFolder } from "@tauri-apps/plugin-dialog";
import { api, MirrorLatency, RemoteStatus, Settings } from "../api";
import { t } from "../i18n";
import { showToast } from "../toast";
import {
  checkForUpdate,
  getUpdateState,
  installUpdate,
  restartToUpdate,
  subscribeUpdate,
} from "../updater";
import { ExtensionDialog } from "./ExtensionDialog";

const SAVE_KEYS = [
  "default_dir",
  "segments",
  "task_bps",
  "global_bps",
  "proxy_mode",
  "proxy",
  "conflict",
  "after_done",
  "clipboard_monitor",
  "ffmpeg_path",
  "yt_dlp_path",
  "max_concurrent",
  "max_auto_retry",
  "auto_speed_up",
  "auto_categorize",
  "ytdlp_auto_update",
  "schedule_enabled",
  "schedule_window",
  "schedule_outside",
  "schedule_outside_bps",
  "update_auto",
  "run_command",
  "yt_dlp_subs",
  "yt_dlp_meta",
  "yt_dlp_cookies",
  "yt_dlp_quality",
  "prevent_sleep",
  "auto_resume_on_start",
  "preserve_mtime",
  "hf_mirror_enabled",
  "hf_mirror_site",
  "hf_token",
  "github_mirror_enabled",
  "github_mirror_site",
  "github_mirror_custom",
] as const;

const GH_PRESET_SITES = [
  "https://gh-proxy.org",
  "https://gh-proxy.com",
  "https://cdn.gh-proxy.org",
  "https://edgeone.gh-proxy.org",
  "https://hk.gh-proxy.org",
  "https://ghfast.top",
  "https://wget.la",
];

type SettingsTab = "general" | "sites" | "media" | "advanced" | "about";

const SETTINGS_TABS: { key: SettingsTab; labelKey: string }[] = [
  { key: "general", labelKey: "settingsTabGeneral" },
  { key: "sites", labelKey: "settingsTabSites" },
  { key: "media", labelKey: "settingsTabMedia" },
  { key: "advanced", labelKey: "settingsTabAdvanced" },
  { key: "about", labelKey: "settingsTabAbout" },
];

// 数值输入允许范围（§3.2/A21）：blur 与保存时 clamp，不依赖 HTML min/max 兜底
const NUM_BOUNDS: Partial<Record<keyof Settings, [number, number]>> = {
  segments: [1, 32],
  task_bps: [0, Number.MAX_SAFE_INTEGER],
  global_bps: [0, Number.MAX_SAFE_INTEGER],
  max_concurrent: [0, Number.MAX_SAFE_INTEGER],
  max_auto_retry: [0, Number.MAX_SAFE_INTEGER],
  schedule_outside_bps: [0, Number.MAX_SAFE_INTEGER],
};

export function SettingsDialog({
  onBack: _onBack,
  onDirtyChange,
  initialTab,
  onOpenNetDiag,
}: {
  onBack?: () => void;
  onDirtyChange?: (dirty: boolean) => void;
  initialTab?: SettingsTab;
  onOpenNetDiag?: () => void;
}) {
  const [tab, setTab] = useState<SettingsTab>(initialTab ?? "general");

  useEffect(() => {
    if (initialTab) setTab(initialTab);
  }, [initialTab]);
  const [s, setS] = useState<Settings | null>(null);
  const upd = useSyncExternalStore(subscribeUpdate, getUpdateState);
  const [saved, setSaved] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [clipOn, setClipOn] = useState(true);
  const [afterDone, setAfterDone] = useState("none");
  const [autoOn, setAutoOn] = useState(false);
  const [version, setVersion] = useState("");
  const [nmOk, setNmOk] = useState<boolean | null>(null);
  const [remote, setRemote] = useState<RemoteStatus | null>(null);
  const [remoteBusy, setRemoteBusy] = useState(false);
  const [invalid, setInvalid] = useState<Record<string, boolean>>({});
  const [extOpen, setExtOpen] = useState(false);
  const [mirrorLatencies, setMirrorLatencies] = useState<MirrorLatency[] | null>(null);
  const [probingMirrors, setProbingMirrors] = useState(false);
  const [sysProxy, setSysProxy] = useState<string | null | undefined>(undefined);
  const [baseline, setBaseline] = useState("");
  const savedValues = useRef<Settings | null>(null);
  const savedExtras = useRef<{ clip: boolean; done: string; auto: boolean }>({
    clip: true,
    done: "none",
    auto: false,
  });
  const savedTimer = useRef<number | undefined>(undefined);

  useEffect(() => () => window.clearTimeout(savedTimer.current), []);

  const snapshotOf = (v: Settings, clip: boolean, done: string, auto: boolean) =>
    JSON.stringify([v, clip, done, auto]);

  useEffect(() => {
    Promise.all([
      api.getSettings(),
      api.getAutostart().catch(() => false),
      getVersion().catch(() => ""),
      api.nmRegistered().catch(() => null as boolean | null),
      api.remoteStatus().catch(() => null),
      api.getSystemProxy().catch(() => null),
    ])
      .then(([v, auto, ver, nm, rm, sp]) => {
        const clip = v.clipboard_monitor !== "off";
        const done = v.after_done || "none";
        setS(v);
        savedValues.current = v;
        savedExtras.current = { clip, done, auto };
        setClipOn(clip);
        setAfterDone(done);
        setAutoOn(auto);
        setVersion(ver);
        setNmOk(nm);
        setRemote(rm);
        setSysProxy(sp);
        setBaseline(snapshotOf(v, clip, done, auto));
      })
      .catch((e) => setSaveError(String(e)));
  }, []);

  const refreshSystemProxy = async () => {
    try {
      setSysProxy(await api.getSystemProxy());
    } catch {
      setSysProxy(null);
    }
  };

  const dirty =
    !!s && baseline !== "" && snapshotOf(s, clipOn, afterDone, autoOn) !== baseline;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  const field = (key: keyof Settings) => s?.[key] ?? "";
  const set = (key: keyof Settings, value: string) => {
    if (s) setS({ ...s, [key]: value === "" ? null : value });
  };

  const resetChanges = () => {
    if (!savedValues.current) return;
    setS({ ...savedValues.current });
    setClipOn(savedExtras.current.clip);
    setAfterDone(savedExtras.current.done);
    setAutoOn(savedExtras.current.auto);
    setInvalid({});
    setSaveError(null);
  };

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
    if (saving || !s) return;
    setSaveError(null);
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
    const mode = next.proxy_mode || (next.proxy ? "custom" : "system");
    next.proxy_mode = mode;
    const rawProxy = (next.proxy ?? "").trim();
    if (
      mode === "custom" &&
      rawProxy !== "" &&
      !/^(https?|socks5h?):\/\//i.test(rawProxy)
    ) {
      setInvalid((m) => ({ ...m, proxy: true }));
      setSaveError(t("proxyAddressInvalid"));
      return;
    }
    setInvalid((m) => ({ ...m, proxy: false }));
    setS(next);
    setSaving(true);
    try {
      for (const key of SAVE_KEYS) {
        const v = next[key];
        if (v != null || savedValues.current?.[key] != null) {
          await api.setSetting(key, v ?? "");
        }
      }
      await api.setAutostart(autoOn);
      setBaseline(snapshotOf(next, clipOn, afterDone, autoOn));
      savedValues.current = next;
      savedExtras.current = { clip: clipOn, done: afterDone, auto: autoOn };
      setSaved(true);
      showToast(t("saved"), "success");
      window.clearTimeout(savedTimer.current);
      savedTimer.current = window.setTimeout(() => setSaved(false), 2500);
    } catch (e) {
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

  // 友好速度输入：存储为 B/s，界面以 KB/s 或 MB/s 编辑
  const speedInput = (key: "task_bps" | "global_bps" | "schedule_outside_bps") => {
    const rawBps = Number(field(key) || "0");
    const useMb = rawBps >= 1024 * 1024 && rawBps % (1024 * 1024) === 0;
    const unit = useMb ? 1024 * 1024 : 1024;
    const displayVal = rawBps > 0 ? String(Math.round((rawBps / unit) * 100) / 100) : "";
    return (
      <div className="speed-unit-input">
        <input
          type="number"
          min={0}
          step="any"
          value={displayVal}
          placeholder={t("speedUnlimited")}
          onChange={(e) => {
            const val = e.target.value.trim();
            if (val === "" || Number(val) <= 0) {
              set(key, "");
            } else if (Number.isFinite(Number(val))) {
              set(key, String(Math.round(Number(val) * unit)));
            }
          }}
        />
        <select
          aria-label={t("speed")}
          value={String(unit)}
          onChange={(e) => {
            const nextUnit = Number(e.target.value);
            if (rawBps > 0) {
              const curNum = rawBps / unit;
              set(key, String(Math.round(curNum * nextUnit)));
            }
          }}
        >
          <option value="1024">{t("speedUnitKB")}</option>
          <option value="1048576">{t("speedUnitMB")}</option>
        </select>
      </div>
    );
  };

  return (
    <>
      <section className="settings-page" aria-label={t("settingsTitle")}>
        <div className="settings-tabs" role="tablist" aria-label={t("settingsTitle")}>
          {SETTINGS_TABS.map((item) => (
            <button
              key={item.key}
              type="button"
              role="tab"
              aria-selected={tab === item.key}
              className={`settings-tab${tab === item.key ? " active" : ""}`}
              onClick={() => setTab(item.key)}
            >
              {t(item.labelKey)}
            </button>
          ))}
        </div>

        <div className="settings-page-body">
          {!s ? (
            !saveError && <div className="hint">{t("probing")}</div>
          ) : (
            <div className="settings-cards">
              {tab === "general" && (
                <>
                  <div className="settings-card">
                    <div className="settings-card-title">{t("saveLocation")}</div>
                    <label>{t("defaultDir")}</label>
                    <div className="settings-field-row">
                      <input
                        value={field("default_dir")}
                        placeholder={t("defaultDir")}
                        onChange={(e) => set("default_dir", e.target.value)}
                      />
                      <button
                        type="button"
                        onClick={async () => {
                          try {
                            const picked = await pickFolder({ directory: true });
                            if (typeof picked === "string") set("default_dir", picked);
                          } catch (error) {
                            setSaveError(String(error));
                          }
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
                  </div>

                  <div className="settings-card">
                    <div className="settings-card-title">{t("transferPreferences")}</div>
                    <div className="grid2">
                      <div>
                        <label>{t("taskSpeed")}</label>
                        {speedInput("task_bps")}
                      </div>
                      <div>
                        <label>{t("globalSpeed")}</label>
                        {speedInput("global_bps")}
                      </div>
                    </div>
                    <div className="grid2">
                      <div>
                        <label>{t("maxConcurrent")}</label>
                        {numInput("max_concurrent", "1")}
                      </div>
                      <div>
                        <label>{t("maxAutoRetry")}</label>
                        {numInput("max_auto_retry", "3")}
                      </div>
                    </div>
                    <label className="settings-check">
                      <input
                        type="checkbox"
                        checked={field("auto_speed_up") !== "off"}
                        onChange={(e) => set("auto_speed_up", e.target.checked ? "on" : "off")}
                      />
                      <span>{t("autoSpeedUp")}</span>
                    </label>
                    <label className="settings-check">
                      <input
                        type="checkbox"
                        checked={field("preserve_mtime") !== "off"}
                        onChange={(e) => set("preserve_mtime", e.target.checked ? "on" : "off")}
                      />
                      <span>{t("preserveMtime")}</span>
                    </label>
                  </div>

                  <div className="settings-card">
                    <div className="settings-card-title">{t("behaviorPreferences")}</div>
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
                      <option value="extract_zip">{t("afterDoneExtractZip")}</option>
                      <option value="run_command">{t("afterDoneRun")}</option>
                      <option value="shutdown">{t("afterDoneShutdown")}</option>
                      <option value="sleep">{t("afterDoneSleep")}</option>
                    </select>
                    {afterDone === "run_command" && (
                      <>
                        <label>{t("runCommandLabel")}</label>
                        <input
                          value={field("run_command")}
                          placeholder={t("runCommandPlaceholder")}
                          onChange={(e) => set("run_command", e.target.value)}
                        />
                      </>
                    )}
                    <label className="settings-check">
                      <input
                        type="checkbox"
                        checked={clipOn}
                        onChange={(e) => {
                          const on = e.target.checked;
                          setClipOn(on);
                          set("clipboard_monitor", on ? "on" : "off");
                        }}
                      />
                      <span>{t("clipboardMonitor")}</span>
                    </label>
                    <label className="settings-check">
                      <input
                        type="checkbox"
                        checked={field("auto_categorize") === "on"}
                        onChange={(e) => set("auto_categorize", e.target.checked ? "on" : "off")}
                      />
                      <span>{t("autoCategorize")}</span>
                    </label>
                    <label className="settings-check">
                      <input
                        type="checkbox"
                        checked={field("prevent_sleep") !== "off"}
                        onChange={(e) => set("prevent_sleep", e.target.checked ? "on" : "off")}
                      />
                      <span>{t("preventSleep")}</span>
                    </label>
                    <label className="settings-check">
                      <input
                        type="checkbox"
                        checked={field("auto_resume_on_start") !== "off"}
                        onChange={(e) =>
                          set("auto_resume_on_start", e.target.checked ? "on" : "off")
                        }
                      />
                      <span>{t("autoResumeOnStart")}</span>
                    </label>
                    <label className="settings-check">
                      <input
                        type="checkbox"
                        checked={autoOn}
                        onChange={(e) => setAutoOn(e.target.checked)}
                      />
                      <span>{t("autostart")}</span>
                    </label>
                  </div>
                </>
              )}

              {tab === "sites" && (
                <>
                  <div className="settings-card">
                    <div className="netdiag-card-head">
                      <div className="settings-card-title" style={{ marginBottom: 0 }}>
                        {t("proxySection")}
                      </div>
                      {onOpenNetDiag && (
                        <button
                          type="button"
                          className="text-button"
                          onClick={onOpenNetDiag}
                        >
                          {t("netDiagOpenFromProxy")}
                        </button>
                      )}
                    </div>
                    <label htmlFor="proxy-mode">{t("proxyModeLabel")}</label>
                    <select
                      id="proxy-mode"
                      value={field("proxy_mode") || (field("proxy") ? "custom" : "system")}
                      onChange={(e) => {
                        set("proxy_mode", e.target.value);
                        setInvalid((m) => ({ ...m, proxy: false }));
                      }}
                    >
                      <option value="system">{t("proxyModeSystem")}</option>
                      <option value="custom">{t("proxyModeCustom")}</option>
                      <option value="direct">{t("proxyModeDirect")}</option>
                    </select>

                    {(field("proxy_mode") || (field("proxy") ? "custom" : "system")) === "system" && (
                      <div className="proxy-status-row">
                        <span className="proxy-status-text">
                          {sysProxy === undefined
                            ? t("probing")
                            : sysProxy
                              ? (
                                  <>
                                    {t("proxySystemDetected").replace("{proxy}", "")}
                                    <code>{sysProxy}</code>
                                  </>
                                )
                              : t("proxySystemNone")}
                        </span>
                        <button
                          type="button"
                          className="text-button"
                          onClick={() => void refreshSystemProxy()}
                        >
                          {t("proxySystemRefresh")}
                        </button>
                      </div>
                    )}

                    {(field("proxy_mode") || (field("proxy") ? "custom" : "system")) === "custom" && (
                      <>
                        <label htmlFor="proxy-url">{t("proxyAddressLabel")}</label>
                        <input
                          id="proxy-url"
                          value={field("proxy")}
                          className={invalid.proxy ? "input-err" : ""}
                          placeholder={t("proxyAddressPlaceholder")}
                          onChange={(e) => {
                            set("proxy", e.target.value);
                            if (invalid.proxy) setInvalid((m) => ({ ...m, proxy: false }));
                          }}
                        />
                        {invalid.proxy && <div className="err-line">{t("proxyAddressInvalid")}</div>}
                        <div className="hint" style={{ marginTop: 6 }}>{t("proxyAddressHint")}</div>
                      </>
                    )}

                    {(field("proxy_mode") || (field("proxy") ? "custom" : "system")) === "direct" && (
                      <div className="hint" style={{ marginTop: 10 }}>{t("proxyDirectHint")}</div>
                    )}
                  </div>

                  <div className="settings-card">
                    <div className="settings-card-title">Hugging Face 加速</div>
                    <label className="settings-check">
                      <input
                        type="checkbox"
                        checked={field("hf_mirror_enabled") !== "off"}
                        onChange={(e) =>
                          set("hf_mirror_enabled", e.target.checked ? "on" : "off")
                        }
                      />
                      <span>{t("hfMirrorEnable")}</span>
                    </label>
                    <label htmlFor="hf-mirror-site">{t("hfMirrorSiteLabel")}</label>
                    <input
                      id="hf-mirror-site"
                      value={field("hf_mirror_site")}
                      placeholder="https://hf-mirror.com"
                      onChange={(e) => set("hf_mirror_site", e.target.value)}
                    />
                    {mirrorLatencies && mirrorLatencies.some((m) => m.group === "huggingface") && (
                      <div className="mirror-pills">
                        {mirrorLatencies
                          .filter((m) => m.group === "huggingface")
                          .map((m) => {
                            const activeSite = field("hf_mirror_site") || "https://hf-mirror.com";
                            return (
                              <button
                                key={`${m.group}:${m.site}`}
                                type="button"
                                className={`mirror-pill${activeSite === m.site ? " active" : ""}${
                                  m.latency_ms == null ? " unavailable" : ""
                                }`}
                                onClick={() => set("hf_mirror_site", m.site)}
                              >
                                {m.site.replace(/^https?:\/\//, "")}:{" "}
                                {m.latency_ms != null ? `${m.latency_ms} ms` : t("mirrorUnavailable")}
                              </button>
                            );
                          })}
                      </div>
                    )}
                    <label htmlFor="hf-token">{t("hfTokenLabel")}</label>
                    <input
                      id="hf-token"
                      type="password"
                      value={field("hf_token")}
                      placeholder="hf_..."
                      onChange={(e) => set("hf_token", e.target.value)}
                    />
                  </div>

                  <div className="settings-card">
                    <div className="settings-card-title">GitHub Releases & 文件加速</div>
                    <label className="settings-check">
                      <input
                        type="checkbox"
                        checked={field("github_mirror_enabled") === "on"}
                        onChange={(e) =>
                          set("github_mirror_enabled", e.target.checked ? "on" : "off")
                        }
                      />
                      <span>{t("githubMirrorEnable")}</span>
                    </label>
                    <div className="settings-field-row">
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <label htmlFor="gh-mirror-site">{t("githubMirrorSiteLabel")}</label>
                        <select
                          id="gh-mirror-site"
                          value={field("github_mirror_site") || "auto"}
                          onChange={(e) => set("github_mirror_site", e.target.value)}
                        >
                          <option value="auto">{t("githubMirrorAuto")}</option>
                          {GH_PRESET_SITES.map((u) => (
                            <option key={u} value={u}>
                              {u}
                            </option>
                          ))}
                          <option value="custom">{t("githubMirrorCustom")}</option>
                        </select>
                      </div>
                      <button
                        type="button"
                        disabled={probingMirrors}
                        onClick={async () => {
                          setProbingMirrors(true);
                          try {
                            setMirrorLatencies(await api.probeSiteMirrors());
                          } catch (e) {
                            setSaveError(String(e));
                          } finally {
                            setProbingMirrors(false);
                          }
                        }}
                      >
                        {probingMirrors ? t("probingMirrors") : t("probeMirrorsBtn")}
                      </button>
                    </div>
                    {field("github_mirror_site") === "custom" && (
                      <>
                        <label htmlFor="gh-mirror-custom">{t("githubMirrorCustom")}</label>
                        <input
                          id="gh-mirror-custom"
                          value={field("github_mirror_custom")}
                          placeholder={t("githubMirrorCustomPlaceholder")}
                          onChange={(e) => set("github_mirror_custom", e.target.value)}
                        />
                      </>
                    )}
                    {mirrorLatencies && mirrorLatencies.some((m) => m.group === "github") && (
                      <div className="mirror-pills">
                        {mirrorLatencies
                          .filter((m) => m.group === "github")
                          .map((m) => (
                            <button
                              key={`${m.group}:${m.site}`}
                              type="button"
                              className={`mirror-pill${
                                field("github_mirror_site") === m.site ? " active" : ""
                              }${m.latency_ms == null ? " unavailable" : ""}`}
                              onClick={() => set("github_mirror_site", m.site)}
                            >
                              {m.site.replace(/^https?:\/\//, "")}:{" "}
                              {m.latency_ms != null ? `${m.latency_ms} ms` : t("mirrorUnavailable")}
                            </button>
                          ))}
                      </div>
                    )}
                  </div>
                </>
              )}

              {tab === "media" && (
                <div className="settings-card">
                  <div className="settings-card-title">{t("toolPreferences")}</div>
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
                  <label htmlFor="ytdlp-quality">{t("ytdlpQualityLabel")}</label>
                  <select
                    id="ytdlp-quality"
                    value={field("yt_dlp_quality") || "best"}
                    onChange={(e) => set("yt_dlp_quality", e.target.value)}
                  >
                    <option value="best">{t("ytdlpQualityBest")}</option>
                    <option value="2160">{t("ytdlpQuality2160")}</option>
                    <option value="1080">{t("ytdlpQuality1080")}</option>
                    <option value="720">{t("ytdlpQuality720")}</option>
                    <option value="audio">{t("ytdlpQualityAudio")}</option>
                  </select>
                  <label htmlFor="ytdlp-cookies">{t("ytdlpCookiesLabel")}</label>
                  <input
                    id="ytdlp-cookies"
                    value={field("yt_dlp_cookies")}
                    placeholder={t("ytdlpCookiesPlaceholder")}
                    onChange={(e) => set("yt_dlp_cookies", e.target.value)}
                  />
                  <label className="settings-check">
                    <input
                      type="checkbox"
                      checked={field("ytdlp_auto_update") !== "off"}
                      onChange={(e) =>
                        set("ytdlp_auto_update", e.target.checked ? "on" : "off")
                      }
                    />
                    <span>{t("ytdlpAutoUpdate")}</span>
                  </label>
                  <label className="settings-check">
                    <input
                      type="checkbox"
                      checked={field("yt_dlp_subs") === "on"}
                      onChange={(e) => set("yt_dlp_subs", e.target.checked ? "on" : "off")}
                    />
                    <span>{t("ytdlpSubs")}</span>
                  </label>
                  <label className="settings-check">
                    <input
                      type="checkbox"
                      checked={field("yt_dlp_meta") === "on"}
                      onChange={(e) => set("yt_dlp_meta", e.target.checked ? "on" : "off")}
                    />
                    <span>{t("ytdlpMeta")}</span>
                  </label>
                </div>
              )}

              {tab === "advanced" && (
                <>
                  <div className="settings-card">
                    <div className="settings-card-title">{t("schedule")}</div>
                    <label className="settings-check">
                      <input
                        type="checkbox"
                        checked={field("schedule_enabled") === "on"}
                        onChange={(e) =>
                          set("schedule_enabled", e.target.checked ? "on" : "off")
                        }
                      />
                      <span>{t("schedule")}</span>
                    </label>
                    {field("schedule_enabled") === "on" && (
                      <>
                        <label>{t("scheduleWindow")}</label>
                        <input
                          value={field("schedule_window")}
                          placeholder="23:00-07:00"
                          onChange={(e) => set("schedule_window", e.target.value)}
                        />
                        <label>{t("scheduleOutside")}</label>
                        <select
                          value={field("schedule_outside") || "pause"}
                          onChange={(e) => set("schedule_outside", e.target.value)}
                        >
                          <option value="pause">{t("outsidePause")}</option>
                          <option value="limit">{t("outsideLimit")}</option>
                        </select>
                        {field("schedule_outside") === "limit" && (
                          <>
                            <label>{t("scheduleOutsideBps")}</label>
                            {speedInput("schedule_outside_bps")}
                          </>
                        )}
                      </>
                    )}
                  </div>

                  <div className="settings-card">
                    <div className="settings-card-title">{t("remoteAdd")}</div>
                    <label className="settings-check">
                      <input
                        type="checkbox"
                        checked={!!remote?.enabled}
                        disabled={remoteBusy}
                        onChange={async (e) => {
                          setRemoteBusy(true);
                          try {
                            setRemote(await api.setRemoteEnabled(e.target.checked));
                          } catch (err) {
                            setSaveError(String(err));
                          } finally {
                            setRemoteBusy(false);
                          }
                        }}
                      />
                      <span>{t("remoteAdd")}</span>
                    </label>
                    {remote?.enabled && (
                      <div style={{ display: "flex", gap: 14, alignItems: "flex-start", marginTop: 10 }}>
                        {remote.qr && (
                          <img
                            src={`data:image/svg+xml;utf8,${encodeURIComponent(remote.qr)}`}
                            width={118}
                            height={118}
                            alt="QR"
                            style={{
                              borderRadius: 8,
                              border: "1px solid var(--border)",
                              flexShrink: 0,
                            }}
                          />
                        )}
                        <div style={{ fontSize: 12, color: "var(--muted)", wordBreak: "break-all" }}>
                          {t("remoteScanHint")}
                          {remote.urls.map((u) => (
                            <div key={u}>{u}</div>
                          ))}
                        </div>
                      </div>
                    )}
                    <div style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap", marginTop: 12 }}>
                      <span style={{ fontSize: 12, display: "inline-flex", alignItems: "center", gap: 6 }}>
                        {t("nmStatus")}:
                        {nmOk == null ? (
                          <span style={{ color: "var(--muted)" }}>…</span>
                        ) : nmOk ? (
                          <span style={{ color: "var(--ok-ink)" }}>{t("nmOk")}</span>
                        ) : (
                          <span style={{ color: "var(--err-ink)" }}>{t("nmMissing")}</span>
                        )}
                      </span>
                      <button type="button" className="text-button" onClick={() => setExtOpen(true)}>
                        {t("extInstallBtn")}
                      </button>
                    </div>
                  </div>
                </>
              )}

              {tab === "about" && (
                <div className="settings-card">
                  <div className="settings-card-title">{t("updateSection")}</div>
                  <label className="settings-check">
                    <input
                      type="checkbox"
                      checked={field("update_auto") !== "off"}
                      onChange={(e) => set("update_auto", e.target.checked ? "on" : "off")}
                    />
                    <span>{t("updateAuto")}</span>
                  </label>
                  <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap", marginTop: 10 }}>
                    {version && (
                      <span style={{ color: "var(--muted)", fontSize: 12 }}>v{version}</span>
                    )}
                    <button
                      type="button"
                      className="text-button"
                      onClick={() => checkForUpdate()}
                      disabled={upd.status === "checking" || upd.status === "installing"}
                    >
                      {upd.status === "checking" ? t("updateChecking") : t("updateCheckNow")}
                    </button>
                    {upd.status === "upToDate" && (
                      <span style={{ color: "var(--ok-ink)", fontSize: 12 }}>{t("updateUpToDate")}</span>
                    )}
                    {upd.status === "available" && (
                      <>
                        <span style={{ fontSize: 12 }}>
                          {t("updateAvailableTo").replace("{v}", upd.version ?? "")}
                        </span>
                        <button type="button" className="text-button" onClick={() => installUpdate()}>
                          {t("updateDownload")}
                        </button>
                      </>
                    )}
                    {upd.status === "installing" && (
                      <span style={{ color: "var(--muted)", fontSize: 12 }}>
                        {t("updateInstalling")}
                        {upd.total != null
                          ? ` ${Math.min(100, Math.round((upd.downloaded / upd.total) * 100))}%`
                          : "…"}
                      </span>
                    )}
                    {upd.status === "ready" && (
                      <>
                        <span style={{ fontSize: 12 }}>{t("updateReady")}</span>
                        <button type="button" className="text-button" onClick={restartToUpdate}>
                          {t("updateRestart")}
                        </button>
                      </>
                    )}
                    {upd.status === "error" && (
                      <span style={{ color: "var(--err-ink)", fontSize: 12 }}>
                        {t("updateFailed")}: {upd.error}
                      </span>
                    )}
                  </div>
                </div>
              )}
            </div>
          )}
          {saveError && <div className="error" style={{ maxWidth: 780 }}>{saveError}</div>}
        </div>

        <div className="settings-footer">
          <button type="button" onClick={openLogs}>
            {t("openLogs")}
          </button>
          {dirty && <span className="unsaved-pill">{t("unsavedChanges")}</span>}
          <div className="spacer" />
          {saved && <span className="saved">{t("saved")}</span>}
          <button type="button" onClick={resetChanges} disabled={!dirty || saving}>
            {t("resetChanges")}
          </button>
          <button type="button" className="primary" onClick={save} disabled={saving || !s}>
            {t("save")}
            {saving ? "…" : ""}
          </button>
        </div>
      </section>
      {extOpen && <ExtensionDialog onClose={() => setExtOpen(false)} />}
    </>
  );
}
