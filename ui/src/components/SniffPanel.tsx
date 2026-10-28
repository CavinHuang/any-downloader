import { useEffect, useState } from "react";
import { api, listenSniffEvents, SniffedItem } from "../api";
import { fmtBytes } from "../App";
import { t } from "../i18n";
import { showToast } from "../toast";
import { useOverlay } from "../useOverlay";
import { Icon } from "./Icon";

// 桌面嗅探抽屉（docs/DESKTOP-SNIFF.md）：dl-sniff 捕获条目列表。
// 打开时全量拉取 + 订阅增量；条目下载走 sniff_download（视频号种子已转
// isaac64-xor postprocess，引擎下载完自动解密）。
export function SniffPanel({ onClose }: { onClose: () => void }) {
  const [items, setItems] = useState<SniffedItem[] | null>(null);
  const [enabled, setEnabled] = useState(false);
  const [port, setPort] = useState<number | null>(null);
  const [errLine, setErrLine] = useState<string | null>(null);
  const { ref, onKeyDown } = useOverlay(onClose);

  useEffect(() => {
    api
      .sniffStatus()
      .then((st) => {
        setEnabled(st.enabled);
        setPort(st.port);
        setItems([...st.items].sort((a, b) => b.at - a.at));
      })
      .catch((e) => {
        setItems([]);
        setErrLine(String(e));
      });
    let un: (() => void) | null = null;
    let alive = true;
    listenSniffEvents((ev) => {
      if (ev.type !== "item") return;
      setItems((cur) => {
        const base = cur ?? [];
        // 同 URL 去重保最新（后端同 URL 更新会重发）
        const dedup = base.filter((x) => x.url !== ev.item.url);
        return [ev.item, ...dedup].slice(0, 50);
      });
    }).then((f) => {
      if (alive) un = f;
      else f();
    });
    return () => {
      alive = false;
      un?.();
    };
  }, []);

  const download = async (id: string) => {
    try {
      await api.sniffDownload(id);
      showToast(t("sniffDownloadedToast"), "success");
    } catch (e) {
      setErrLine(String(e));
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

  const clear = async () => {
    try {
      await api.sniffClear();
      setItems([]);
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
            <div className="eyebrow">OUT IN THE WILD.</div>
            <h3>{t("sniffPanel")}</h3>
            <p className="dialog-description">{t("sniffPanelDesc")}</p>
          </div>
          <button className="icon-button" onClick={onClose} aria-label={t("close")}>
            <Icon name="close" />
          </button>
        </div>

        <div className="sniff-status">
          <span className={"sniff-dot" + (enabled ? "" : " off")} aria-hidden="true" />
          {enabled
            ? t("sniffListen").replace("{port}", String(port ?? "—"))
            : t("sniffDisabled")}
        </div>

        {items == null ? (
          <div className="hint">{t("probing")}</div>
        ) : items.length === 0 ? (
          <div className="sniff-empty" style={{ whiteSpace: "pre-line" }}>{t("sniffEmpty")}</div>
        ) : (
          <>
            <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 8 }}>
              <button className="text-button" onClick={clear}>{t("sniffClear")}</button>
            </div>
            {items.map((it) => (
              <div className="sniff-item" key={it.id}>
                <div className="t1">
                  <Icon name="video" size={16} />
                  <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {it.title || it.url}
                  </span>
                </div>
                <div className="meta">
                  <span>{hostOf(it.url)}</span>
                  {it.bytes != null && <span>{fmtBytes(it.bytes)}</span>}
                  <span>{fmtTime(it.at)}</span>
                  {it.seed && <span className="sniff-tag">已配解密密钥</span>}
                </div>
                <div className="ops">
                  <button onClick={() => download(it.id)}>{t("sniffItemDownload")}</button>
                  <button className="text-button" onClick={() => copy(it.url)}>{t("sniffCopyLink")}</button>
                </div>
              </div>
            ))}
          </>
        )}
        {errLine && <div className="error">{errLine}</div>}
      </div>
    </div>
  );
}
