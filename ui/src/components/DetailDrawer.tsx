import { useEffect, useState, useSyncExternalStore } from "react";
import { api, isActive, Task } from "../api";
import { fmtBytes, fmtSpeed } from "../App";
import { errorCodeOf, formatTaskError, t } from "../i18n";
import { store } from "../store";
import { showToast } from "../toast";
import { useOverlay } from "../useOverlay";
import { Icon, IconName } from "./Icon";
import { categoryOf } from "../fileTypes";
import { estimateRemainingSecs, fmtFinishClock, fmtRemaining } from "../utils/eta";

interface Variant {
  bandwidth: number;
  resolution: string | null;
  url: string;
}

export function DetailDrawer({
  task,
  onClose,
  onChanged,
  onRemoved,
}: {
  task: Task;
  onClose: () => void;
  onChanged: (t: Task) => void;
  onRemoved: () => void;
}) {
  const live = useSyncExternalStore(store.subscribe, () => store.liveOf(task.id));
  const active = isActive(task.status);
  const done = task.status === "done";
  const label = task.filename || task.url;
  const isExe = /\.(exe|msi|apk|dmg)$/i.test(task.filename ?? "");

  const [name, setName] = useState(task.filename ?? "");
  const [renaming, setRenaming] = useState(false);
  const [newUrl, setNewUrl] = useState(task.url);
  const [relinking, setRelinking] = useState(false);
  const [hashAlgo, setHashAlgo] = useState<"sha256" | "md5" | "sha1" | "sha512">("sha256");
  const [computedHash, setComputedHash] = useState<string | null>(null);
  const [hashing, setHashing] = useState(false);
  const [expectedHash, setExpectedHash] = useState("");
  const [prio, setPrio] = useState(String(task.priority));
  const [errLine, setErrLine] = useState<string | null>(null);

  useEffect(() => {
    setName(task.filename ?? "");
  }, [task.id, task.filename]);

  useEffect(() => {
    setNewUrl(task.url);
  }, [task.id, task.url]);

  useEffect(() => {
    setComputedHash(null);
    setExpectedHash("");
  }, [task.id]);

  useEffect(() => {
    setPrio(String(task.priority));
  }, [task.id, task.priority]);

  const { ref, onKeyDown } = useOverlay(onClose);

  let variants: Variant[] = [];
  try {
    variants = task.variants_json ? JSON.parse(task.variants_json) : [];
  } catch {
    variants = [];
  }

  const downloaded = live?.downloaded ?? (done ? task.total_bytes : task.downloaded_bytes || 0);
  const total = task.total_bytes;
  const pct = total && total > 0 ? Math.min(100, Math.floor(((downloaded || 0) / total) * 100)) : 0;
  const segs = live?.segs ?? null;
  const activeThreads = segs
    ? segs.filter((sg) => sg.written < sg.end - sg.start).length || segs.length
    : 0;
  const etaSecs = estimateRemainingSecs(task, live);
  const etaClock = etaSecs != null ? fmtFinishClock(etaSecs) : null;
  const errCode = errorCodeOf(task.error);

  const doRename = async () => {
    if (!name.trim() || name === task.filename) return;
    setRenaming(true);
    try {
      onChanged(await api.renameTask(task.id, name.trim()));
    } catch (e) {
      setErrLine(String(e));
    } finally {
      setRenaming(false);
    }
  };

  const reveal = async () => {
    try {
      await api.revealTask(task.id);
    } catch (e) {
      setErrLine(String(e));
    }
  };

  const openFile = async () => {
    try {
      if (done && isExe && !window.confirm(t("exeWarn"))) return;
      await api.openTask(task.id);
    } catch (e) {
      setErrLine(String(e));
    }
  };

  const retry = async () => {
    try {
      await api.retryTask(task.id);
      await api.startDownload(task.id);
      store.refresh();
    } catch (e) {
      setErrLine(String(e));
    }
  };

  const start = async () => {
    try {
      await api.startDownload(task.id);
      store.refresh();
    } catch (e) {
      setErrLine(String(e));
    }
  };

  const pause = async () => {
    try {
      onChanged(await api.pauseTask(task.id));
      await store.refresh();
    } catch (e) {
      setErrLine(String(e));
    }
  };

  const moveToQueue = async () => {
    try {
      onChanged(await api.moveToQueue(task.id));
      await store.refresh();
      showToast(t("movedToQueue"), "info");
    } catch (e) {
      setErrLine(String(e));
    }
  };

  const reorder = async (dir: "top" | "up" | "down") => {
    try {
      await api.reorderQueue(task.id, dir);
      await store.refresh();
    } catch (e) {
      setErrLine(String(e));
    }
  };

  const copyUrl = async () => {
    try {
      await navigator.clipboard.writeText(task.url);
      showToast(t("copied"), "info");
    } catch (e) {
      setErrLine(String(e));
    }
  };

  const doRelink = async () => {
    const trimmed = newUrl.trim();
    if (!trimmed || trimmed === task.url) return;
    setRelinking(true);
    setErrLine(null);
    try {
      const updated = await api.updateTaskUrl(task.id, trimmed);
      await api.startDownload(updated.id);
      onChanged(updated);
      await store.refresh();
      showToast(t("relinkStarted"), "success");
    } catch (e) {
      setErrLine(String(e));
    } finally {
      setRelinking(false);
    }
  };

  const doChecksum = async () => {
    setHashing(true);
    setErrLine(null);
    try {
      setComputedHash(await api.computeTaskChecksum(task.id, hashAlgo));
    } catch (e) {
      setErrLine(String(e));
    } finally {
      setHashing(false);
    }
  };

  const cleanExpected = expectedHash
    .trim()
    .replace(/^(?:sha-?512|sha-?256|sha-?1|md5)\s*[:=]\s*/i, "")
    .split(/\s+/)[0]
    ?.toLowerCase() ?? "";
  const hashMatch =
    computedHash && cleanExpected
      ? computedHash.toLowerCase() === cleanExpected
      : null;

  const remove = async () => {
    if (!window.confirm(`${t("remove")} ${label}?`)) return;
    try {
      await api.removeTask(task.id);
      onRemoved();
    } catch (e) {
      setErrLine(String(e));
    }
  };

  return (
    <div className="drawer-mask" onClick={onClose}>
      <div
        className="drawer"
        role="dialog"
        aria-modal="true"
        aria-label={label}
        ref={ref}
        onKeyDown={onKeyDown}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="dialog-header"><div className="eyebrow">{t("detailEyebrow")}</div><button className="icon-button" onClick={onClose} aria-label={t("close")}><Icon name="close" /></button></div>
        <div className={`detail-file-icon file-badge ${categoryOf(task)}`}><Icon name={categoryOf(task) as IconName} size={30} /></div>
        <h3 className="detail-title">{label}</h3>

        {(active || (total != null && total > 0 && task.status !== "done")) && (
          <div className="detail-live">
            <div className="detail-live-head">
              <span>
                {total ? `${pct}% · ${fmtBytes(downloaded || 0)} / ${fmtBytes(total)}` : fmtBytes(downloaded || 0)}
                {active && activeThreads > 1 ? ` · ${t("threadsN").replace("{n}", String(activeThreads))}` : ""}
                {etaSecs != null && etaClock
                  ? ` · ${fmtRemaining(etaSecs)} · ${t("etaFinishAt").replace("{time}", etaClock)}`
                  : ""}
              </span>
              {active && live && live.bps > 0 && <strong>{fmtSpeed(live.bps)}</strong>}
            </div>
            {segs && segs.length > 1 && total ? (
              <>
                <div className="bar multi" style={{ marginTop: 6 }}>
                  {segs.map((sg, i) => {
                    const len = sg.end - sg.start;
                    const filled = Math.min(100, (sg.written / len) * 100);
                    const isDone = sg.written >= len;
                    return (
                      <div
                        key={i}
                        className="seg"
                        style={{
                          left: `${(sg.start / total) * 100}%`,
                          width: `${(len / total) * 100}%`,
                        }}
                      >
                        <div className={`seg-fill ${isDone ? "done" : ""}`} style={{ width: `${filled}%` }} />
                      </div>
                    );
                  })}
                </div>
                <div className="seg-table" aria-label={t("segConnections")}>
                  {segs.map((sg, i) => {
                    const len = Math.max(1, sg.end - sg.start);
                    const isDone = sg.written >= len;
                    const segPct = isDone ? 100 : Math.min(99, Math.floor((sg.written / len) * 100));
                    const fillPct = isDone
                      ? 100
                      : sg.written > 0
                        ? Math.max(1.5, Math.min(99.5, (sg.written / len) * 100))
                        : 0;
                    const spd = live?.segSpeeds?.[i] ?? 0;
                    return (
                      <div key={sg.start} className="seg-row">
                        <span className="seg-idx">#{i + 1}</span>
                        <span className="seg-range">{fmtBytes(sg.written)} / {fmtBytes(len)}</span>
                        <div
                          className="seg-bar"
                          role="progressbar"
                          aria-label={`#${i + 1}`}
                          aria-valuenow={segPct}
                          aria-valuemin={0}
                          aria-valuemax={100}
                        >
                          <div
                            className={`seg-bar-fill${
                              isDone
                                ? " done"
                                : task.status === "paused"
                                  ? " paused"
                                  : task.status === "failed"
                                    ? " failed"
                                    : ""
                            }`}
                            style={{ width: `${fillPct}%` }}
                          />
                        </div>
                        <span className="seg-pct">{isDone ? t("segDone") : `${segPct}%`}</span>
                        <span className="seg-spd">{isDone ? "✓" : spd > 0 ? fmtSpeed(spd) : "—"}</span>
                      </div>
                    );
                  })}
                </div>
              </>
            ) : (
              <div className="bar" style={{ marginTop: 6 }}>
                <div className={`fill ${task.status}`} style={{ width: `${pct}%` }} />
              </div>
            )}
          </div>
        )}

        <table className="kv">
          <tbody>
            <tr>
              <td>{t("statusField")}</td>
              <td>
                <code>{t(`status_${task.status}` as never)}</code>
              </td>
            </tr>
            {etaSecs != null && etaClock && (
              <tr>
                <td>{t("etaFinishField")}</td>
                <td>
                  {etaClock}（{t("etaRemainingBrief").replace("{dur}", fmtRemaining(etaSecs))}）
                </td>
              </tr>
            )}
            {task.error != null && (
              <tr>
                <td>{t("errorLabel")}</td>
                <td>
                  <code className="err">{formatTaskError(task.error)}</code>
                  {errCode === "interstitial_page" && (
                    <div style={{ marginTop: 6 }}>
                      <button
                        type="button"
                        className="text-button"
                        onClick={() => api.openExternalUrl(task.url).catch(() => {})}
                      >
                        {t("openInBrowser")}
                      </button>
                    </div>
                  )}
                </td>
              </tr>
            )}
            <tr>
              <td>{t("rename")}</td>
              <td>
                <div style={{ display: "flex", gap: 6 }}>
                  <input
                    value={name}
                    disabled={active}
                    onChange={(e) => setName(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && doRename()}
                  />
                  <button
                    style={{ whiteSpace: "nowrap" }}
                    disabled={active || renaming || name === task.filename}
                    onClick={doRename}
                  >
                    {t("rename")}
                  </button>
                </div>
              </td>
            </tr>
            <tr>
              <td>{t("priorityLabel")}</td>
              <td>
                <div style={{ display: "flex", gap: 6, alignItems: "center", flexWrap: "wrap" }}>
                  <select
                    style={{ flex: 1, minWidth: 110 }}
                    value={prio}
                    disabled={active}
                    onChange={async (e) => {
                      const p = Number(e.target.value);
                      setPrio(e.target.value);
                      try {
                        onChanged(await api.setPriority(task.id, p));
                      } catch (e2) {
                        setErrLine(String(e2));
                      }
                    }}
                  >
                    <option value="30">{t("prioHigh")}</option>
                    <option value="0">{t("prioDefault")}</option>
                    <option value="-10">{t("prioLow")}</option>
                  </select>
                  {task.status === "queued" && (
                    <>
                      <button type="button" onClick={() => reorder("top")}>{t("moveTop")}</button>
                      <button type="button" onClick={() => reorder("up")}>{t("moveUp")}</button>
                      <button type="button" onClick={() => reorder("down")}>{t("moveDown")}</button>
                    </>
                  )}
                </div>
              </td>
            </tr>
            <tr>
              <td>{t("urlLabelDetail")}</td>
              <td className="break">
                <div>{task.url}</div>
                <button type="button" className="text-button" style={{ marginTop: 4 }} onClick={copyUrl}>
                  {t("copyLink")}
                </button>
              </td>
            </tr>
            {!active && !done && (
              <tr>
                <td>{t("relinkLabel")}</td>
                <td>
                  <div className="relink-row">
                    <input
                      value={newUrl}
                      placeholder={t("relinkPlaceholder")}
                      onChange={(e) => setNewUrl(e.target.value)}
                      onKeyDown={(e) => e.key === "Enter" && doRelink()}
                    />
                    <button
                      type="button"
                      disabled={relinking || !newUrl.trim() || newUrl.trim() === task.url}
                      onClick={doRelink}
                    >
                      {relinking ? "…" : t("relinkBtn")}
                    </button>
                  </div>
                  <div className="hint" style={{ marginTop: 4 }}>{t("relinkHint")}</div>
                </td>
              </tr>
            )}
            {done && (
              <tr>
                <td>{t("verifySection")}</td>
                <td>
                  <div className="verify-controls">
                    <select
                      aria-label={t("verifySection")}
                      value={hashAlgo}
                      onChange={(e) => {
                        setHashAlgo(e.target.value as "sha256" | "md5" | "sha1" | "sha512");
                        setComputedHash(null);
                      }}
                    >
                      <option value="sha256">SHA-256</option>
                      <option value="md5">MD5</option>
                      <option value="sha1">SHA-1</option>
                      <option value="sha512">SHA-512</option>
                    </select>
                    <button type="button" disabled={hashing} onClick={doChecksum}>
                      {hashing ? t("verifyComputing") : t("verifyComputeBtn")}
                    </button>
                  </div>
                  {computedHash && (
                    <div className="verify-result">
                      <code className="break">{computedHash}</code>
                      <button
                        type="button"
                        className="text-button"
                        onClick={() => {
                          navigator.clipboard.writeText(computedHash).then(
                            () => showToast(t("copied"), "info"),
                            () => {}
                          );
                        }}
                      >
                        {t("copyLink")}
                      </button>
                      <input
                        style={{ marginTop: 6 }}
                        value={expectedHash}
                        placeholder={t("verifyExpectedPlaceholder")}
                        onChange={(e) => setExpectedHash(e.target.value)}
                      />
                      {hashMatch !== null && (
                        <div className={hashMatch ? "verify-badge ok" : "verify-badge err"}>
                          {hashMatch ? t("verifyMatched") : t("verifyMismatched")}
                        </div>
                      )}
                    </div>
                  )}
                </td>
              </tr>
            )}
            {task.final_url && (
              <tr>
                <td>{t("finalUrl")}</td>
                <td className="break">{task.final_url}</td>
              </tr>
            )}
            {task.dir && (
              <tr>
                <td>{t("dirField")}</td>
                <td className="break">{task.dir}</td>
              </tr>
            )}
            {task.total_bytes != null && (
              <tr>
                <td>{t("sizeField")}</td>
                <td>
                  {task.kind === "hls" && task.status !== "done"
                    ? `${task.total_bytes} ${t("segsUnit")}`
                    : fmtBytes(task.total_bytes)}
                </td>
              </tr>
            )}
            {task.mime && (
              <tr>
                <td>{t("mime")}</td>
                <td>{task.mime}</td>
              </tr>
            )}
            {task.kind === "hls" &&
              task.status === "done" &&
              (task.filename ?? "").endsWith(".ts") && (
                <tr>
                  <td />
                  <td className="err">{t("tsHint")}</td>
                </tr>
              )}
            {task.etag && (
              <tr>
                <td>{t("etag")}</td>
                <td className="break">
                  <code>{task.etag}</code>
                </td>
              </tr>
            )}
            {variants.length > 0 && (
              <tr>
                <td>{t("quality")}</td>
                <td>
                  <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                    {variants.map((v) => {
                      const same = v.url === task.url;
                      return (
                        <button
                          key={v.url}
                          className={same ? "chip-active" : ""}
                          style={{ fontSize: 11, minHeight: 0 }}
                          disabled={same}
                          onClick={async () => {
                            try {
                              const nt = await api.addTask({
                                url: v.url,
                                dir: task.dir ?? undefined,
                                filename: task.filename ?? undefined,
                              });
                              await api.startDownload(nt.id);
                              onChanged(nt);
                            } catch (e) {
                              setErrLine(String(e));
                            }
                          }}
                        >
                          {v.resolution ?? `${Math.round(v.bandwidth / 1000)}kbps`}
                          {same ? " ✓" : ""}
                        </button>
                      );
                    })}
                  </div>
                </td>
              </tr>
            )}
          </tbody>
        </table>
        {errLine && <div className="error">{errLine}</div>}
        <div className="modal-actions">
          {!active && (
            <button className="danger" onClick={remove}>
              {t("remove")}
            </button>
          )}
          <div className="spacer" />
          {!done && task.status !== "queued" && (
            <button type="button" onClick={moveToQueue}>
              {t("moveToQueue")}
            </button>
          )}
          {task.status === "queued" && (
            <button type="button" onClick={pause}>
              {t("pause")}
            </button>
          )}
          {task.status === "failed" || task.status === "canceled" ? (
            <button className="primary" onClick={retry}>
              {t("retry")}
            </button>
          ) : task.status === "queued" ? (
            <button className="primary" onClick={start}>
              {t("startNow")}
            </button>
          ) : task.status === "paused" || task.status === "waiting_retry" ? (
            <button className="primary" onClick={start}>
              {t("start")}
            </button>
          ) : done ? (
            <button onClick={openFile}>{t("open")}</button>
          ) : null}
          {!active && task.dir && (
            <button onClick={reveal}>{t("openFolder")}</button>
          )}
          <button onClick={onClose}>{t("close")}</button>
        </div>
      </div>
    </div>
  );
}
