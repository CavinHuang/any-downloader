import { useEffect, useMemo, useRef, useState } from "react";
import type { Task } from "../api";
import { fmtBytes } from "../App";
import { t } from "../i18n";
import { CATEGORIES, categoryOf, type Category } from "../fileTypes";
import { Icon } from "./Icon";

export function TransferPanel({ tasks, bps, active, paused, inTauri, busy, onPause, onResume, onCategory, category }: {
  tasks: Task[]; bps: number; active: number; paused: number; inTauri: boolean; busy: boolean;
  onPause: () => void; onResume: () => void; onCategory: (category: Category) => void; category: Category;
}) {
  const [history, setHistory] = useState<number[]>(Array(40).fill(0));
  const currentBps = useRef(bps);
  useEffect(() => { currentBps.current = bps; }, [bps]);
  useEffect(() => {
    const timer = window.setInterval(() => setHistory(h => [...h.slice(1), currentBps.current]), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const max = Math.max(...history, 1024);
  const points = history.map((v, i) => `${i * 240 / 39},${65 - v / max * 51}`).join(" ");
  const summary = useMemo(() => {
    const counts: Partial<Record<Category, number>> = {};
    let completed = 0;
    let bytes = 0;
    for (const task of tasks) {
      const key = categoryOf(task);
      counts[key] = (counts[key] ?? 0) + 1;
      if (task.status === "done") {
        completed += 1;
        bytes += task.total_bytes ?? task.downloaded_bytes;
      }
    }
    return { counts, completed, bytes };
  }, [tasks]);
  const speed = fmtBytes(bps).split(" ");
  const groups = CATEGORIES.filter(c => c.key !== "all" && c.key !== "other");
  return (
    <aside className="inspector">
      <section className="transfer-panel">
        <div className="section-kicker"><span>{t("transferNow")}</span><span className={`connection-dot${inTauri ? " connected" : ""}`} title={t(inTauri ? "desktopMode" : "previewMode")} /></div>
        <div className="speed-value">{speed[0]}<span>{speed[1]}/s</span></div>
        <div className="speed-caption">{t(active > 0 ? "transferRunning" : "transferIdle")}</div>
        <svg className="speed-chart" viewBox="0 0 240 76" preserveAspectRatio="none" role="img" aria-label={t("speedHistory")}>
          <path d="M0 20h240M0 43h240M0 66h240" className="chart-grid" />
          <path d={`M0 66 L${points} L240 66 Z`} fill="currentColor" opacity=".07" />
          <polyline points={points} fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round" />
        </svg>
        <div className="chart-labels"><span>−40s</span><span>{t("now")}</span></div>
        <div className="transfer-controls">
          <span><b>{String(active).padStart(2, "0")}</b> {t("inTransit")}</span>
          <button className="icon-button" title={t(active > 0 ? "pauseAll" : "resumeAll")} aria-label={t(active > 0 ? "pauseAll" : "resumeAll")} disabled={busy || (active === 0 && paused === 0)} onClick={active > 0 ? onPause : onResume}><Icon name={active > 0 ? "pause" : "play"} size={15} /></button>
        </div>
      </section>
      <section className="shelf-panel">
        <div className="section-kicker">{t("collectionIndex")}<span>INDEX</span></div>
        <div className="shelf-items">
          {groups.map(({key, icon, label}, i) => {
            const count = summary.counts[key] ?? 0;
            return <button key={key} className={`shelf-item${category === key ? " active" : ""}`} onClick={() => onCategory(category === key ? "all" : key)} aria-pressed={category === key}>
              <span className="shelf-number">0{i + 1}</span><Icon name={icon} size={16} /><span>{t(label)}</span><span className="shelf-count">{String(count).padStart(2, "0")}</span>
            </button>;
          })}
        </div>
      </section>
      <section className="saved-panel"><span>{t("safelyCollected")}</span><div>{fmtBytes(summary.bytes)}<span> / {summary.completed} {t("itemsUnit")}</span></div></section>
      <div className="inspector-note"><Icon name="folder" size={16} /><span>{t("localNote")}</span></div>
    </aside>
  );
}
