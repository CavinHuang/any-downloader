import { useEffect, useState } from "react";
import {
  api,
  BrowserSniffItem,
  ExtensionBridgeStatus,
  listenBrowserSniffEvents,
  PageSniffItem,
} from "../api";
import { fmtBytes } from "../App";
import { t } from "../i18n";
import { store } from "../store";
import { showToast } from "../toast";
import { useOverlay } from "../useOverlay";
import { Icon } from "./Icon";

// 嗅探中心抽屉：
// 1) 网页嗅探（全平台免证书即用）：输入任意网页链接直接提取视频/HLS/音频/文件；
// 2) 浏览器扩展嗅探（双通道本地桥接 + 实时推送，免证书带登录 Cookie）。
export function SniffPanel({ onClose }: { onClose: () => void }) {
  const [tab, setTab] = useState<"web" | "browser">("web");

  // —— 网页嗅探状态
  const [pageUrl, setPageUrl] = useState("");
  const [scanning, setScanning] = useState(false);
  const [scannedOnce, setScannedOnce] = useState(false);
  const [webItems, setWebItems] = useState<PageSniffItem[]>([]);
  const [webSel, setWebSel] = useState<Set<string>>(new Set());
  const [webBusy, setWebBusy] = useState(false);

  // —— 浏览器扩展嗅探状态
  const [bridgeStatus, setBridgeStatus] = useState<ExtensionBridgeStatus | null>(null);
  const [browserItems, setBrowserItems] = useState<BrowserSniffItem[]>([]);

  const [errLine, setErrLine] = useState<string | null>(null);
  const { ref, onKeyDown } = useOverlay(onClose);

  const loadBridgeStatus = async () => {
    const st = await api.extensionBridgeStatus();
    setBridgeStatus(st);
    setBrowserItems([...st.items].sort((a, b) => b.at - a.at));
  };

  useEffect(() => {
    loadBridgeStatus().catch(() => {});
    let unBrowser: (() => void) | null = null;
    let alive = true;
    listenBrowserSniffEvents((item) => {
      setBrowserItems((cur) => {
        const dedup = cur.filter((x) => x.url !== item.url);
        return [item, ...dedup].slice(0, 100);
      });
    }).then((f) => {
      if (alive) unBrowser = f;
      else f();
    });
    return () => {
      alive = false;
      unBrowser?.();
    };
  }, []);

  const runWebSniff = async () => {
    const u = pageUrl.trim();
    if (!u || scanning) return;
    setScanning(true);
    setErrLine(null);
    setScannedOnce(true);
    try {
      const found = await api.sniffPageUrl(u);
      setWebItems(found);
      setWebSel(new Set(found.map((x) => x.url)));
    } catch (e) {
      setWebItems([]);
      setErrLine(String(e));
    } finally {
      setScanning(false);
    }
  };

  const pushWebSelected = async (startNow: boolean) => {
    const chosen = webItems.filter((x) => webSel.has(x.url));
    if (chosen.length === 0 || webBusy) return;
    setWebBusy(true);
    setErrLine(null);
    try {
      const singleImmediate = startNow && chosen.length === 1;
      const rs = await Promise.allSettled(
        chosen.map(async (it) => {
          const task = await api.addTask({
            url: it.url,
            filename: it.filename && it.filename !== "video" ? it.filename : undefined,
            headers: { referer: pageUrl.trim() },
          });
          if (singleImmediate) {
            await api.startDownload(task.id);
          }
        })
      );
      const ok = rs.filter((r) => r.status === "fulfilled").length;
      if (startNow && chosen.length > 1 && ok > 0) {
        await api.startQueue();
      }
      await store.refresh();
      showToast(
        startNow
          ? t("startedN").replace("{n}", String(ok))
          : t("queuedAutoN").replace("{n}", String(ok)),
        ok > 0 ? "success" : "warn"
      );
    } catch (e) {
      setErrLine(String(e));
    } finally {
      setWebBusy(false);
    }
  };

  const copy = async (url: string) => {
    try {
      await navigator.clipboard.writeText(url);
      showToast(t("sniffItemCopied"), "info");
    } catch (e) {
      setErrLine(String(e));
    }
  };

  const fmtTime = (ms: number) => {
    const d = new Date(ms);
    const p = (n: number) => String(n).padStart(2, "0");
    return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
  };
  const hostOf = (url: string) => {
    try {
      return new URL(url).hostname;
    } catch {
      return url;
    }
  };

  return (
    <div className="drawer-mask" onClick={onClose}>
      <div
        className="drawer"
        role="dialog"
        aria-modal="true"
        aria-label={t("sniffPanel")}
        ref={ref}
        onKeyDown={onKeyDown}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="dialog-header">
          <div>
            <h3>{t("sniffPanel")}</h3>
            <p className="dialog-description">{t("sniffPanelDesc")}</p>
          </div>
          <button className="icon-button" onClick={onClose} aria-label={t("close")}>
            <Icon name="close" />
          </button>
        </div>

        <div className="sniff-tabs" role="tablist">
          <button
            role="tab"
            aria-selected={tab === "web"}
            className={tab === "web" ? "active" : ""}
            onClick={() => setTab("web")}
          >
            {t("sniffTabWeb")}
          </button>
          <button
            role="tab"
            aria-selected={tab === "browser"}
            className={tab === "browser" ? "active" : ""}
            onClick={() => {
              setTab("browser");
              void loadBridgeStatus();
            }}
          >
            {t("sniffTabBrowser")}
            {browserItems.length > 0 ? ` (${browserItems.length})` : ""}
          </button>
        </div>

        {tab === "web" ? (
          <>
            <form
              className="sniff-web-form"
              onSubmit={(e) => {
                e.preventDefault();
                void runWebSniff();
              }}
            >
              <input
                value={pageUrl}
                onChange={(e) => setPageUrl(e.target.value)}
                placeholder={t("sniffWebPlaceholder")}
                disabled={scanning}
                autoFocus
              />
              <button className="primary" type="submit" disabled={scanning || !pageUrl.trim()}>
                {scanning ? t("sniffWebScanning") : t("sniffWebStart")}
              </button>
            </form>

            {webItems.length === 0 ? (
              <div className="sniff-empty">
                {scanning
                  ? t("sniffWebScanning")
                  : scannedOnce
                    ? t("sniffWebNoHit")
                    : t("sniffWebEmpty")}
              </div>
            ) : (
              <>
                <div className="sniff-web-actions">
                  <button
                    className="primary"
                    disabled={webSel.size === 0 || webBusy}
                    onClick={() => void pushWebSelected(true)}
                  >
                    {t("sniffDownloadSelected").replace("{n}", String(webSel.size))}
                  </button>
                  <button
                    disabled={webSel.size === 0 || webBusy}
                    onClick={() => void pushWebSelected(false)}
                  >
                    {t("sniffQueueSelected").replace("{n}", String(webSel.size))}
                  </button>
                </div>
                {webItems.map((it) => {
                  const checked = webSel.has(it.url);
                  return (
                    <div className="sniff-item" key={it.url}>
                      <label className="sniff-web-row">
                        <input
                          type="checkbox"
                          style={{ width: "auto" }}
                          checked={checked}
                          onChange={() => {
                            setWebSel((prev) => {
                              const next = new Set(prev);
                              if (next.has(it.url)) next.delete(it.url);
                              else next.add(it.url);
                              return next;
                            });
                          }}
                        />
                        <span className="sniff-tag">{it.kind.toUpperCase()}</span>
                        <span className="sniff-web-name" title={it.url}>{it.filename || it.url}</span>
                      </label>
                      <div className="meta">
                        <span>{hostOf(it.url)}</span>
                        {it.bytes != null && <span>{fmtBytes(it.bytes)}</span>}
                        <button className="text-button" onClick={() => void copy(it.url)}>
                          {t("sniffCopyLink")}
                        </button>
                      </div>
                    </div>
                  );
                })}
              </>
            )}
          </>
        ) : (
          <>
            <p className="hint" style={{ marginBottom: 10 }}>{t("sniffBrowserGuide")}</p>
            <div className="sniff-controls">
              <div className="sniff-status">
                <span className={"sniff-dot" + (bridgeStatus?.running ? "" : " off")} aria-hidden="true" />
                <span>
                  {bridgeStatus?.running
                    ? t("sniffBridgeListen").replace("{port}", String(bridgeStatus.port))
                    : t("sniffBridgeDown")}
                </span>
              </div>
              <div className="meta" style={{ marginTop: 4 }}>
                <span className="sniff-tag">
                  {bridgeStatus?.nmRegistered ? t("sniffNmOk") : t("sniffNmMissing")}
                </span>
                {bridgeStatus?.lastSeenAt && (
                  <span>
                    {t("sniffLastSeen").replace("{time}", fmtTime(bridgeStatus.lastSeenAt))}
                  </span>
                )}
              </div>
            </div>
            {browserItems.length === 0 ? (
              <div className="sniff-empty" style={{ whiteSpace: "pre-line" }}>
                {t("sniffBrowserEmpty")}
              </div>
            ) : (
              <>
                <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 4 }}>
                  <button
                    className="text-button"
                    onClick={() =>
                      void api
                        .extensionBridgeClear()
                        .then(() => setBrowserItems([]))
                        .catch((e) => setErrLine(String(e)))
                    }
                  >
                    {t("sniffClear")}
                  </button>
                </div>
                {browserItems.map((it) => (
                  <div className="sniff-item" key={it.id || it.url}>
                    <div className="t1">
                      <span className="sniff-tag">{it.kind.toUpperCase()}</span>
                      <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {it.filename || it.page_title || it.url}
                      </span>
                    </div>
                    <div className="meta">
                      <span>{hostOf(it.url)}</span>
                      {it.bytes != null && <span>{fmtBytes(it.bytes)}</span>}
                      {it.duration_sec != null && <span>{Math.round(it.duration_sec)}s</span>}
                      {it.variants.length > 0 && it.variants[0].label && (
                        <span className="sniff-tag">{it.variants[0].label}</span>
                      )}
                      <span>{fmtTime(it.at)}</span>
                    </div>
                    <div className="ops">
                      <button
                        className="primary"
                        onClick={() =>
                          void api
                            .extensionBridgeDownload(it.id)
                            .then(() => {
                              void store.refresh();
                              showToast(t("startedN").replace("{n}", "1"), "success");
                            })
                            .catch((e) => setErrLine(String(e)))
                        }
                      >
                        {t("sniffItemDownload")}
                      </button>
                      {it.variants.slice(0, 3).map((v) => (
                        <button
                          key={v.url}
                          onClick={() =>
                            void api
                              .extensionBridgeDownload(it.id, v.url)
                              .then(() => {
                                void store.refresh();
                                showToast(t("startedN").replace("{n}", "1"), "success");
                              })
                              .catch((e) => setErrLine(String(e)))
                          }
                        >
                          {v.label || v.resolution || "画质"}
                        </button>
                      ))}
                      <button className="text-button" onClick={() => void copy(it.url)}>
                        {t("sniffCopyLink")}
                      </button>
                    </div>
                  </div>
                ))}
              </>
            )}
          </>
        )}
        {errLine && <div className="error">{errLine}</div>}
      </div>
    </div>
  );
}
