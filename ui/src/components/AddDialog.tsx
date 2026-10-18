import { useEffect, useState } from "react";
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
  // dirty 基线（§3.6）：打开时初始值；dir 异步载入后定格
  const [baselineDir, setBaselineDir] = useState("");

  const dirty =
    urls !== "" ||
    filename !== "" ||
    checksum !== "" ||
    mirrorText !== "" ||
    proxy !== "" ||
    dir !== baselineDir;

  // §3.6：表单浮层 Esc / 遮罩在有未提交修改时先确认
  const requestClose = () => {
    if (busy) return;
    if (dirty && !window.confirm(t("dirtyConfirm"))) return;
    onClose();
  };
  const { ref, onKeyDown } = useOverlay(requestClose);

  // 默认目录：设置值 > 系统下载目录
  useEffect(() => {
    if (!desktopAvailable) return;
    api.getSettings().then(async (s) => {
      const d = s.default_dir || (await api.defaultDownloadDir());
      setDir(d);
      setBaselineDir(d);
    }).catch(e => setError(String(e)));
  }, [desktopAvailable]);

  const urlList = urls.split(/\s+/).filter((u) => /^https?:\/\//i.test(u));
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

  // A3：checksum/mirrors 真正传入 add_task（不再做摆设字段），仅单链接启用；
  // A14：批量 Promise.allSettled 逐条尝试，单条失败不中断，结束汇总成败。
  const doAdd = async () => {
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
        await api.startDownload(task.id);
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
      if (urlList.length === 1 && dup + fail === 0) {
        onAdded(); // 单条成功：路径最短，直接关（等价旧行为）
        return;
      }
      showToast(summary, dup + fail > 0 ? "warn" : "success");
      onAdded();
      return;
    }
    // 全部失败：留在框内给可读原因（§4.1 失败不静默）；单链接失败优先展示原始原因
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
        <div className="dialog-header"><div><div className="eyebrow">KEEP SOMETHING GOOD</div><h3>{t("addTitle")}</h3></div><button className="icon-button" onClick={requestClose} disabled={busy} aria-label={t("close")}><Icon name="close" /></button></div>
        <p className="dialog-description">{t("addDescription")}</p>
        {!desktopAvailable && <div className="probe">{t("desktopRequired")}</div>}
        <fieldset className="add-fields" disabled={busy}>
        <label htmlFor="collection-urls">{t("urlLabel")}</label>
        <textarea
          rows={4}
          id="collection-urls"
          value={urls}
          onChange={(e) => setUrls(e.target.value)}
          placeholder="https://…"
          autoFocus
        />
        <details className="advanced-options"><summary>{t("advancedOptions")}</summary>
        <label htmlFor="collection-checksum">
          {t("checksumLabel")}
          {multi ? `（${t("singleOnly")}）` : ""}
        </label>
        <input
          id="collection-checksum"
          value={checksum}
          disabled={multi}
          placeholder={t("checksumPlaceholder")}
          onChange={(e) => setChecksum(e.target.value)}
        />
        <label htmlFor="collection-mirrors">
          {t("mirrorLabel")}
          {multi ? `（${t("singleOnly")}）` : ""}
        </label>
        <textarea
          id="collection-mirrors"
          rows={2}
          value={mirrorText}
          disabled={multi}
          placeholder={"https://mirror1.example.com/...\nhttps://mirror2.example.com/..."}
          onChange={(e) => setMirrorText(e.target.value)}
        />
        </details>
        <div className="grid2">
          <div>
            <label htmlFor="collection-dir">{t("dirLabel")}</label>
            <input id="collection-dir" value={dir} onChange={(e) => setDir(e.target.value)} />
          </div>
          <div>
            <label htmlFor="collection-filename">{t("filenameLabel")}</label>
            <input id="collection-filename" value={filename} onChange={(e) => setFilename(e.target.value)} />
          </div>
        </div>
        </fieldset>
        {probe && (
          <div className="probe">
            <b>{t("probeResult")}: </b>
            {probe.filename ?? "?"} · {probe.total_bytes ? fmtBytes(probe.total_bytes) : "?"} ·{" "}
            {probe.mime ?? "?"} · {probe.segmented ? t("probeSegmented") : t("probeSingle")}
          </div>
        )}
        {error && <div className="error">{error}</div>}
        <div className="modal-actions">
          <button onClick={doProbe} disabled={!desktopAvailable || !firstUrl || probing || busy}>
            {probing ? t("probing") : t("probe")}
          </button>
          <div className="spacer" />
          <button onClick={requestClose} disabled={busy}>{t("cancelBtn")}</button>
          <button className="primary" onClick={doAdd} disabled={!desktopAvailable || busy || !firstUrl}>
            {busy ? t("addingCollection") : t("addAndStart")}
          </button>
        </div>
      </div>
    </div>
  );
}
