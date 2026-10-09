import { useEffect, useState } from "react";
import { api, ProbeInfo } from "../api";
import { fmtBytes } from "../App";
import { t } from "../i18n";

export function AddDialog({
  onClose,
  onAdded,
}: {
  onClose: () => void;
  onAdded: () => void;
}) {
  const [urls, setUrls] = useState("");
  const [dir, setDir] = useState("");
  const [filename, setFilename] = useState("");
  const [checksum, setChecksum] = useState("");
  const [mirrorText, setMirrorText] = useState("");
  const [probe, setProbe] = useState<ProbeInfo | null>(null);
  const [probing, setProbing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // 默认目录：设置值 > 系统下载目录
  useEffect(() => {
    api.getSettings().then(async (s) => {
      setDir(s.default_dir || (await api.defaultDownloadDir()));
    });
  }, []);

  const firstUrl = urls.split(/\s+/).filter(Boolean)[0] ?? "";

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

  const doAdd = async () => {
    const list = urls.split(/\s+/).filter((u) => /^https?:\/\//i.test(u));
    if (list.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      for (const url of list) {
        let task: { id: string };
        try {
          task = await api.addTask({
            url,
            dir: dir || undefined,
            filename: filename || undefined,
          });
        } catch (e) {
          const msg = String(e);
          if (msg.includes("task_exists")) {
            throw new Error(t("duplicate"));
          }
          throw e;
        }
        await api.startDownload(task.id);
      }
      onAdded();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  return (
    <div className="modal-mask" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h3>{t("addTitle")}</h3>
        <label>{t("urlLabel")}</label>
        <textarea
          rows={4}
          value={urls}
          onChange={(e) => setUrls(e.target.value)}
          placeholder="https://…"
          autoFocus
        />
        <label>校验值（sha256/sha1/md5，可选）</label>
        <input
          value={checksum}
          placeholder="sha256:abcd… 或直接粘贴 64/40/32 位 hex"
          onChange={(e) => setChecksum(e.target.value)}
        />
        <label>镜像链接（可选，每行一个）</label>
        <textarea
          rows={2}
          value={mirrorText}
          placeholder="https://mirror1.example.com/...
https://mirror2.example.com/..."
          onChange={(e) => setMirrorText(e.target.value)}
        />
        <div className="grid2">
          <div>
            <label>{t("dirLabel")}</label>
            <input value={dir} onChange={(e) => setDir(e.target.value)} />
          </div>
          <div>
            <label>{t("filenameLabel")}</label>
            <input value={filename} onChange={(e) => setFilename(e.target.value)} />
          </div>
        </div>
        {probe && (
          <div className="probe">
            <b>{t("probeResult")}: </b>
            {probe.filename ?? "?"} · {probe.total_bytes ? fmtBytes(probe.total_bytes) : "?"} ·{" "}
            {probe.mime ?? "?"} · {probe.segmented ? "multi-thread ✓" : "single-thread"}
          </div>
        )}
        {error && <div className="error">{error}</div>}
        <div className="modal-actions">
          <button onClick={doProbe} disabled={!firstUrl || probing}>
            {probing ? t("probing") : t("probe")}
          </button>
          <div className="spacer" />
          <button onClick={onClose}>{t("cancelBtn")}</button>
          <button className="primary" onClick={doAdd} disabled={busy || !urls.trim()}>
            {t("addAndStart")}
          </button>
        </div>
      </div>
    </div>
  );
}
