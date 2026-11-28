import { useCallback, useEffect, useRef, useState } from "react";
import { open as pickFolder } from "@tauri-apps/plugin-dialog";
import { api, MediaInspectResult, ProbeInfo, SiteFileItem } from "../api";
import { fmtBytes } from "../App";
import { t } from "../i18n";
import { store } from "../store";
import { showToast } from "../toast";
import { useOverlay } from "../useOverlay";
import { classifyInputUrl, extractLinksFromText } from "../utils/urlDetect";
import { Icon } from "./Icon";

const YTDLP_HOST_RE =
  /(^|\.)(youtube\.com|youtu\.be|youtube-nocookie\.com|x\.com|twitter\.com|bilibili\.com|b23\.tv|tiktok\.com|douyin\.com|iesdouyin\.com)$/i;

const MODEL_INSTALLER_RE =
  /\.(gguf|safetensors|bin|onnx|pt|pth|ckpt|whl|exe|msi|dmg|pkg|zip|7z|tar\.gz|tgz|appimage|deb|rpm)$/i;

function isYtdlpUrl(raw: string): boolean {
  try {
    return YTDLP_HOST_RE.test(new URL(raw).hostname);
  } catch {
    return false;
  }
}

function isSiteAdaptUrl(raw: string): boolean {
  try {
    const u = new URL(raw);
    const h = u.hostname.toLowerCase().replace(/^www\./, "");
    if (h === "huggingface.co" || h === "hf-mirror.com" || h === "modelscope.cn") {
      return true;
    }
    if (h === "bilibili.com" || h.endsWith(".bilibili.com") || h === "b23.tv" || h.endsWith(".b23.tv")) {
      return true;
    }
    if (h === "github.com" && /\/releases(?:\/(?:latest|tag\/[^/]+))?\/?$/i.test(u.pathname)) {
      return true;
    }
    return false;
  } catch {
    return false;
  }
}

function joinSubdirSafe(baseDir: string, folderName: string | null, relPath: string): string | undefined {
  if (!baseDir) return undefined;
  const parts: string[] = [];
  if (folderName) parts.push(folderName);
  const relParts = relPath.split("/").filter((s) => s && s !== "." && s !== "..");
  if (relParts.length > 1) {
    parts.push(...relParts.slice(0, -1));
  }
  if (parts.length === 0) return baseDir;
  const sep = baseDir.includes("\\") ? "\\" : "/";
  const trimmed = baseDir.replace(/[\\/]+$/, "");
  return `${trimmed}${sep}${parts.join(sep)}`;
}

function fmtDurationSec(sec: number | null): string {
  if (!sec || !Number.isFinite(sec) || sec <= 0) return "";
  const s = Math.round(sec);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const rem = s % 60;
  return h > 0
    ? `${h}:${String(m).padStart(2, "0")}:${String(rem).padStart(2, "0")}`
    : `${m}:${String(rem).padStart(2, "0")}`;
}

export function AddDialog({
  onClose,
  onAdded,
  initialUrls = "",
  autoProbe = false,
  desktopAvailable = true,
}: {
  onClose: () => void;
  onAdded: () => void;
  initialUrls?: string;
  autoProbe?: boolean;
  desktopAvailable?: boolean;
}) {
  const [urls, setUrls] = useState(initialUrls);
  const [dir, setDir] = useState("");
  const [filename, setFilename] = useState("");
  const [checksum, setChecksum] = useState("");
  const [mirrorText, setMirrorText] = useState("");
  const [proxy, setProxy] = useState("");
  const [probe, setProbe] = useState<ProbeInfo | null>(null);
  const [mediaInspect, setMediaInspect] = useState<MediaInspectResult | null>(null);
  const [siteInspect, setSiteInspect] = useState<{
    site: string;
    title: string;
    subtitle: string | null;
    folderName: string | null;
    files: SiteFileItem[];
  } | null>(null);
  const [selectedFiles, setSelectedFiles] = useState<Set<string>>(new Set());
  const [selectedFormat, setSelectedFormat] = useState("");
  const [selectedEntries, setSelectedEntries] = useState<Set<string>>(new Set());
  const [probing, setProbing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [baselineDir, setBaselineDir] = useState("");

  const dirty =
    urls !== "" ||
    filename !== "" ||
    checksum !== "" ||
    mirrorText !== "" ||
    proxy !== "" ||
    dir !== baselineDir;

  const requestClose = () => {
    if (busy) return;
    if (dirty && !window.confirm(t("dirtyConfirm"))) return;
    onClose();
  };
  const { ref, onKeyDown } = useOverlay(requestClose);

  useEffect(() => {
    if (!desktopAvailable) return;
    api.getSettings().then(async (s) => {
      const d = s.default_dir || (await api.defaultDownloadDir());
      setDir(d);
      setBaselineDir(d);
    }).catch(e => setError(String(e)));
  }, [desktopAvailable]);

  const urlList = extractLinksFromText(urls);
  const firstUrl = urlList[0] ?? "";
  const single = urlList.length === 1;
  const multi = urlList.length > 1;
  const mirrors = mirrorText.split(/\s+/).filter((u) => /^https?:\/\//i.test(u));
  const ambiguousSingle = single && !probe && !mediaInspect && !siteInspect && classifyInputUrl(firstUrl) === "ambiguous";
  const autoProbedRef = useRef(false);

  const doProbe = useCallback(async () => {
    if (!firstUrl) return;
    setProbing(true);
    setProbe(null);
    setMediaInspect(null);
    setSiteInspect(null);
    setSelectedFiles(new Set());
    setSelectedFormat("");
    setSelectedEntries(new Set());
    setError(null);
    try {
      if (isSiteAdaptUrl(firstUrl)) {
        const res = await api.inspectSiteUrl(firstUrl);
        if (res.kind === "files") {
          setSiteInspect({
            site: res.site,
            title: res.title,
            subtitle: res.subtitle,
            folderName: res.folder_name,
            files: res.files,
          });
          setSelectedFiles(new Set(res.files.map((f) => f.relative_path)));
          if (res.files.length === 1 && !filename.trim()) {
            setFilename(res.files[0].filename);
          }
        } else if (res.kind === "playlist") {
          setMediaInspect(res);
          setSelectedEntries(new Set(res.entries.map((e) => e.url)));
        } else {
          const yres = await api.inspectMediaUrl(res.url);
          setMediaInspect(yres);
          if (yres.kind === "video") {
            if (!filename.trim() && yres.title) setFilename(yres.title);
          } else {
            setSelectedEntries(new Set(yres.entries.map((e) => e.url)));
          }
        }
      } else if (isYtdlpUrl(firstUrl)) {
        const res = await api.inspectMediaUrl(firstUrl);
        setMediaInspect(res);
        if (res.kind === "video") {
          if (!filename.trim() && res.title) {
            setFilename(res.title);
          }
        } else {
          setSelectedEntries(new Set(res.entries.map((e) => e.url)));
        }
      } else {
        setProbe(await api.probeUrl(firstUrl));
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setProbing(false);
    }
  }, [firstUrl, filename]);

  useEffect(() => {
    if (autoProbe && desktopAvailable && firstUrl && !autoProbedRef.current) {
      autoProbedRef.current = true;
      void doProbe();
    }
  }, [autoProbe, desktopAvailable, firstUrl, doProbe]);

  const siteFilesMode = !!siteInspect && siteInspect.files.length > 0;
  const playlistMode = mediaInspect?.kind === "playlist";
  const canSubmit = siteFilesMode
    ? selectedFiles.size > 0
    : playlistMode
      ? selectedEntries.size > 0
      : !!firstUrl;

  const doAdd = async (startImmediately: boolean) => {
    if (busy || !canSubmit) return;
    setBusy(true);
    setError(null);
    const fmtArg = selectedFormat.trim() || undefined;
    const pickedSiteFiles = siteFilesMode
      ? siteInspect.files.filter((f) => selectedFiles.has(f.relative_path))
      : [];
    const mediaItems = playlistMode
      ? mediaInspect.entries
          .filter((e) => selectedEntries.has(e.url))
          .map((e) => ({ url: e.url, title: e.title }))
      : urlList.map((u) => ({ url: u, title: undefined as string | undefined }));
    const totalCount = siteFilesMode ? pickedSiteFiles.length : mediaItems.length;
    const singleImmediate = startImmediately && totalCount === 1;
    const results = siteFilesMode
      ? await Promise.allSettled(
          pickedSiteFiles.map(async (f) => {
            const targetDir =
              pickedSiteFiles.length > 1 || siteInspect.files.length > 1
                ? joinSubdirSafe(dir, siteInspect.folderName, f.relative_path)
                : dir || undefined;
            const customName =
              siteInspect.files.length === 1 && filename.trim() ? filename.trim() : f.filename;
            const mergedMirrors = [...f.mirrors, ...mirrors].filter((u, idx, arr) => arr.indexOf(u) === idx);
            const task = await api.addTask({
              url: f.url,
              dir: targetDir,
              filename: customName,
              headers: Object.keys(f.headers).length > 0 ? f.headers : undefined,
              checksum: checksum.trim() || f.checksum || undefined,
              mirrors: mergedMirrors.length > 0 ? mergedMirrors : undefined,
              proxy: proxy.trim() ? proxy.trim() : undefined,
            });
            if (singleImmediate) {
              await api.startDownload(task.id);
            }
          })
        )
      : await Promise.allSettled(
          mediaItems.map(async (item) => {
            const task = await api.addTask({
              url: item.url,
              dir: dir || undefined,
              filename: playlistMode
                ? item.title || undefined
                : single
                  ? filename || undefined
                  : undefined,
              checksum: !playlistMode && single && checksum.trim() ? checksum.trim() : undefined,
              mirrors: !playlistMode && single && mirrors.length > 0 ? mirrors : undefined,
              proxy: (playlistMode || single) && proxy.trim() ? proxy.trim() : undefined,
              format: fmtArg,
            });
            if (singleImmediate) {
              await api.startDownload(task.id);
            }
          })
        );
    let ok = 0;
    let dup = 0;
    let fail = 0;
    for (const r of results) {
      if (r.status === "fulfilled") ok += 1;
      else if (String(r.reason).includes("task_exists")) dup += 1;
      else fail += 1;
    }
    if (startImmediately && totalCount > 1 && ok > 0) {
      await api.startQueue().catch(() => {});
    }
    store.refresh();
    const summary = t("addSummary")
      .replace("{ok}", String(ok))
      .replace("{dup}", String(dup))
      .replace("{fail}", String(fail));
    if (ok > 0) {
      if (!startImmediately) {
        showToast(t("queuedAutoN").replace("{n}", String(ok)), dup + fail > 0 ? "warn" : "success");
        onAdded();
        return;
      }
      if (totalCount === 1 && dup + fail === 0) {
        onAdded();
        return;
      }
      showToast(summary, dup + fail > 0 ? "warn" : "success");
      onAdded();
      return;
    }
    setBusy(false);
    const failReason = results.find(
      (r) => r.status === "rejected" && !String(r.reason).includes("task_exists")
    );
    setError(
      totalCount === 1
        ? dup === 1
          ? t("duplicate")
          : failReason
            ? String((failReason as PromiseRejectedResult).reason)
            : summary
        : summary
    );
  };

  return (
    <div className="modal-mask" onClick={requestClose}>
      <div
        className="modal"
        role="dialog"
        aria-busy={busy || probing}
        aria-modal="true"
        aria-label={t("addTitle")}
        ref={ref}
        onKeyDown={onKeyDown}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="dialog-header"><div><h3>{t("addTitle")}</h3></div><button className="icon-button" onClick={requestClose} disabled={busy} aria-label={t("close")}><Icon name="close" /></button></div>
        <p className="dialog-description">{t("addDescription")}</p>
        {!desktopAvailable && <div className="probe">{t("desktopRequired")}</div>}
        <fieldset className="add-fields" disabled={busy}>
        <label htmlFor="download-urls">{t("urlLabel")}</label>
        <textarea
          rows={4}
          id="download-urls"
          value={urls}
          onChange={(e) => setUrls(e.target.value)}
          onPaste={(e) => {
            const pasted = e.clipboardData.getData("text");
            const extracted = extractLinksFromText(pasted);
            if (extracted.length > 0 && extracted.join("\n") !== pasted.trim()) {
              e.preventDefault();
              const clean = extracted.join("\n");
              const el = e.currentTarget;
              setUrls(urls.slice(0, el.selectionStart ?? 0) + clean + urls.slice(el.selectionEnd ?? urls.length));
            }
          }}
          placeholder="https://…"
          autoFocus
        />
        {ambiguousSingle && <div className="hint" style={{ marginTop: 4 }}>{t("ambiguousUrlHint")}</div>}

        <div className="grid2">
          <div>
            <label htmlFor="download-dir">{t("dirLabel")}</label>
            <div className="directory-input">
              <input id="download-dir" value={dir} onChange={(e) => setDir(e.target.value)} />
              <button type="button" aria-label={t("browse")} title={t("browse")} disabled={!desktopAvailable} onClick={async () => {
                try {
                  const selected = await pickFolder({ directory: true, defaultPath: dir || undefined });
                  if (typeof selected === "string") setDir(selected);
                } catch (error) { setError(String(error)); }
              }}><Icon name="folder" size={17} /></button>
            </div>
          </div>
          <div>
            <label htmlFor="download-filename">{t("filenameLabel")}{multi ? ` · ${t("singleOnly")}` : ""}</label>
            <input id="download-filename" value={filename} disabled={multi} onChange={(e) => setFilename(e.target.value)} />
          </div>
        </div>
        <details className="advanced-options"><summary>{t("advancedOptions")}</summary>
        <label htmlFor="download-checksum">
          {t("checksumLabel")}
          {multi ? `（${t("singleOnly")}）` : ""}
        </label>
        <input
          id="download-checksum"
          value={checksum}
          disabled={multi}
          placeholder={t("checksumPlaceholder")}
          onChange={(e) => setChecksum(e.target.value)}
        />
        <label htmlFor="download-mirrors">
          {t("mirrorLabel")}
          {multi ? `（${t("singleOnly")}）` : ""}
        </label>
        <textarea
          id="download-mirrors"
          rows={2}
          value={mirrorText}
          disabled={multi}
          placeholder={"https://mirror1.example.com/...\nhttps://mirror2.example.com/..."}
          onChange={(e) => setMirrorText(e.target.value)}
        />
        <label htmlFor="download-proxy">
          {t("proxyPerTaskLabel")}
          {multi ? `（${t("singleOnly")}）` : ""}
        </label>
        <input
          id="download-proxy"
          value={proxy}
          disabled={multi}
          placeholder={t("proxyPerTaskPlaceholder")}
          onChange={(e) => setProxy(e.target.value)}
        />
        </details>
        </fieldset>
        {siteInspect && siteInspect.files.length > 0 && (() => {
          const pickedBytes = siteInspect.files
            .filter((f) => selectedFiles.has(f.relative_path))
            .reduce((acc, f) => acc + (f.size || 0), 0);
          return (
            <div className="probe" style={{ display: "grid", gap: 8 }}>
              <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8, flexWrap: "wrap" }}>
                <div>
                  <b>{siteInspect.title}</b>
                  {siteInspect.subtitle ? ` · ${siteInspect.subtitle}` : ""}
                  <div style={{ color: "var(--muted)", fontSize: 12 }}>
                    {t("siteFilesCount")
                      .replace("{n}", String(siteInspect.files.length))
                      .replace("{sel}", String(selectedFiles.size))
                      .replace("{size}", fmtBytes(pickedBytes))}
                  </div>
                </div>
                {siteInspect.files.length > 1 && (
                  <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                    <button
                      type="button"
                      onClick={() => setSelectedFiles(new Set(siteInspect.files.map((f) => f.relative_path)))}
                    >
                      {t("playlistSelectAll")}
                    </button>
                    <button type="button" onClick={() => setSelectedFiles(new Set())}>
                      {t("playlistClearAll")}
                    </button>
                    <button
                      type="button"
                      onClick={() =>
                        setSelectedFiles(
                          new Set(
                            siteInspect.files
                              .filter((f) => MODEL_INSTALLER_RE.test(f.filename))
                              .map((f) => f.relative_path)
                          )
                        )
                      }
                    >
                      {t("siteFilterModels")}
                    </button>
                  </div>
                )}
              </div>
              <div style={{ maxHeight: 185, overflowY: "auto", display: "grid", gap: 4, borderTop: "1px solid var(--border)", paddingTop: 6 }}>
                {siteInspect.files.map((f) => (
                  <label
                    key={f.relative_path}
                    style={{ display: "flex", alignItems: "center", gap: 8, margin: 0, fontSize: 12.5, cursor: "pointer" }}
                  >
                    <input
                      type="checkbox"
                      style={{ width: "auto" }}
                      checked={selectedFiles.has(f.relative_path)}
                      onChange={(e) => {
                        const next = new Set(selectedFiles);
                        if (e.target.checked) next.add(f.relative_path);
                        else next.delete(f.relative_path);
                        setSelectedFiles(next);
                      }}
                    />
                    <span style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={f.relative_path}>
                      {f.relative_path}
                    </span>
                    {f.checksum && (
                      <span className="probe-badge" style={{ marginBottom: 0 }}>
                        {t("sha256Badge")}
                      </span>
                    )}
                    <span style={{ color: "var(--muted)", flexShrink: 0 }}>
                      {f.size != null ? fmtBytes(f.size) : "?"}
                    </span>
                  </label>
                ))}
              </div>
            </div>
          );
        })()}
        {mediaInspect?.kind === "video" && (
          <div className="probe" style={{ display: "grid", gap: 6 }}>
            <div>
              <b>{mediaInspect.title}</b>
              {mediaInspect.duration_sec ? ` · ${fmtDurationSec(mediaInspect.duration_sec)}` : ""}
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <label htmlFor="media-format" style={{ margin: 0, whiteSpace: "nowrap" }}>
                {t("mediaQualityLabel")}
              </label>
              <select
                id="media-format"
                value={selectedFormat}
                onChange={(e) => setSelectedFormat(e.target.value)}
              >
                <option value="">{t("mediaQualityFollowSetting")}</option>
                {mediaInspect.formats.map((f) => (
                  <option key={f.format_id} value={f.format_id}>
                    {f.label}
                    {f.filesize ? ` (${fmtBytes(f.filesize)})` : ""}
                  </option>
                ))}
              </select>
            </div>
          </div>
        )}
        {mediaInspect?.kind === "playlist" && (
          <div className="probe" style={{ display: "grid", gap: 8 }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8, flexWrap: "wrap" }}>
              <div>
                <b>{t("playlistTitle")}: {mediaInspect.title}</b>{" "}
                <span style={{ color: "var(--muted)", fontSize: 12 }}>
                  {t("playlistEntriesCount")
                    .replace("{n}", String(mediaInspect.entries.length))
                    .replace("{sel}", String(selectedEntries.size))}
                </span>
              </div>
              <div style={{ display: "flex", gap: 6 }}>
                <button
                  type="button"
                  onClick={() => setSelectedEntries(new Set(mediaInspect.entries.map((e) => e.url)))}
                >
                  {t("playlistSelectAll")}
                </button>
                <button type="button" onClick={() => setSelectedEntries(new Set())}>
                  {t("playlistClearAll")}
                </button>
              </div>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <label htmlFor="playlist-format" style={{ margin: 0, whiteSpace: "nowrap" }}>
                {t("mediaQualityLabel")}
              </label>
              <select
                id="playlist-format"
                value={selectedFormat}
                onChange={(e) => setSelectedFormat(e.target.value)}
              >
                <option value="">{t("mediaQualityFollowSetting")}</option>
                <option value="bv*[height<=2160]+ba/b[height<=2160]/b">{t("ytdlpQuality2160")}</option>
                <option value="bv*[height<=1080]+ba/b[height<=1080]/b">{t("ytdlpQuality1080")}</option>
                <option value="bv*[height<=720]+ba/b[height<=720]/b">{t("ytdlpQuality720")}</option>
                <option value="bestaudio[ext=m4a]/bestaudio/best">{t("ytdlpQualityAudio")}</option>
              </select>
            </div>
            <div style={{ maxHeight: 170, overflowY: "auto", display: "grid", gap: 4, borderTop: "1px solid var(--border)", paddingTop: 6 }}>
              {mediaInspect.entries.map((entry, idx) => (
                <label
                  key={entry.url}
                  style={{ display: "flex", alignItems: "center", gap: 8, margin: 0, fontSize: 12.5, cursor: "pointer" }}
                >
                  <input
                    type="checkbox"
                    style={{ width: "auto" }}
                    checked={selectedEntries.has(entry.url)}
                    onChange={(e) => {
                      const next = new Set(selectedEntries);
                      if (e.target.checked) next.add(entry.url);
                      else next.delete(entry.url);
                      setSelectedEntries(next);
                    }}
                  />
                  <span style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {idx + 1}. {entry.title}
                  </span>
                  {entry.duration_sec ? (
                    <span style={{ color: "var(--muted)", flexShrink: 0 }}>
                      {fmtDurationSec(entry.duration_sec)}
                    </span>
                  ) : null}
                </label>
              ))}
            </div>
          </div>
        )}
        {probe && (
          probe.is_interstitial ? (
            <div className="probe warn-box">
              <div>
                <b>{t("interstitialBlocked")}</b>
                {probe.interstitial_title ? ` · ${probe.interstitial_title}` : ""}
              </div>
              {probe.final_url && (
                <div style={{ marginTop: 6 }}>
                  <button
                    type="button"
                    className="text-button"
                    onClick={() => api.openExternalUrl(probe.final_url!).catch(() => {})}
                  >
                    {t("openInBrowser")}
                  </button>
                </div>
              )}
            </div>
          ) : (
            <div className="probe">
              {probe.resolved_from_interstitial && (
                <div className="probe-badge">{t("interstitialResolved")}</div>
              )}
              <b>{t("probeResult")}: </b>
              {probe.filename ?? "?"} · {probe.total_bytes ? fmtBytes(probe.total_bytes) : "?"} ·{" "}
              {probe.mime ?? "?"} · {probe.segmented ? t("probeSegmented") : t("probeSingle")}
            </div>
          )
        )}
        {error && <div className="error">{error}</div>}
        <div className="modal-actions">
          <button onClick={doProbe} disabled={!desktopAvailable || !firstUrl || probing || busy}>
            {probing ? t("probing") : t("probe")}
          </button>
          <div className="spacer" />
          <button onClick={requestClose} disabled={busy}>{t("cancelBtn")}</button>
          <button onClick={() => void doAdd(false)} disabled={!desktopAvailable || busy || !canSubmit}>
            {t("addToQueue")}
          </button>
          <button className="primary" onClick={() => void doAdd(true)} disabled={!desktopAvailable || busy || !canSubmit}>
            {busy ? t("addingTasks") : t("addAndStart")}
          </button>
        </div>
      </div>
    </div>
  );
}
