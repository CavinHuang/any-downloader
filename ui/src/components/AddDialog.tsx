import { useEffect, useState } from "react";
import { open as pickFolder } from "@tauri-apps/plugin-dialog";
import { api, ProbeInfo } from "../api";
import { fmtBytes } from "../App";
import { t } from "../i18n";
import { store } from "../store";
import { showToast } from "../toast";
import { useOverlay } from "../useOverlay";
import { Icon } from "./Icon";

export function AddDialog({
  onClose,
  onAdded,
  initialUrls = "",
  desktopAvailable = true,
}: {
  onClose: () => void;
  onAdded: () => void;
  initialUrls?: string;
  desktopAvailable?: boolean;
}) {
  const [urls, setUrls] = useState(initialUrls);
  const [dir, setDir] = useState("");
  const [filename, setFilename] = useState("");
  const [checksum, setChecksum] = useState("");
  const [mirrorText, setMirrorText] = useState("");
  const [proxy, setProxy] = useState("");
  const [probe, setProbe] = useState<ProbeInfo | null>(null);
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

  const urlList = urls
    .split(/\s+/)
    .filter((u) => /^(?:https?|thunder|flashget|qqdl):\/\//i.test(u));
  const firstUrl = urlList[0] ?? "";
  const single = urlList.length === 1;
  const multi = urlList.length > 1;
  const mirrors = mirrorText.split(/\s+/).filter((u) => /^https?:\/\//i.test(u));

  const doProbe = async () => {
    if (!firstUrl) return;
    setProbing(true);
    setProbe(null);
    setError(null);
    try {
      setProbe(await api.probeUrl(firstUrl));
    } catch (e) {
      setError(String(e));
    } finally {
      setProbing(false);
    }
  };

  const doAdd = async (startImmediately: boolean) => {
    if (busy || urlList.length === 0) return;
    setBusy(true);
    setError(null);
    const results = await Promise.allSettled(
      urlList.map(async (url) => {
        const task = await api.addTask({
          url,
          dir: dir || undefined,
          filename: single ? filename || undefined : undefined,
          checksum: single && checksum.trim() ? checksum.trim() : undefined,
          mirrors: single && mirrors.length > 0 ? mirrors : undefined,
          proxy: single && proxy.trim() ? proxy.trim() : undefined,
        });
        if (startImmediately) {
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
    store.refresh();
    const summary = t("addSummary")
      .replace("{ok}", String(ok))
      .replace("{dup}", String(dup))
      .replace("{fail}", String(fail));
    if (ok > 0) {
      if (!startImmediately) {
        showToast(t("queuedN").replace("{n}", String(ok)), dup + fail > 0 ? "warn" : "success");
        onAdded();
        return;
      }
      if (urlList.length === 1 && dup + fail === 0) {
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
      urlList.length === 1
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
          placeholder="https://…"
          autoFocus
        />

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
          <button onClick={() => void doAdd(false)} disabled={!desktopAvailable || busy || !firstUrl}>
            {t("addToQueue")}
          </button>
          <button className="primary" onClick={() => void doAdd(true)} disabled={!desktopAvailable || busy || !firstUrl}>
            {busy ? t("addingTasks") : t("addAndStart")}
          </button>
        </div>
      </div>
    </div>
  );
}
