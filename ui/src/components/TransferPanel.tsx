import { useEffect, useMemo, useRef, useState } from "react";
import type { Task } from "../api";
import { fmtBytes, fmtSpeed } from "../App";
import { t } from "../i18n";
import { Icon } from "./Icon";

export function TransferPanel({ tasks, bps, active }: { tasks: Task[]; bps: number; active: number }) {
  const [history, setHistory] = useState<number[]>(Array(40).fill(0));
  const currentBps = useRef(bps);
  useEffect(() => { currentBps.current = bps; }, [bps]);
  useEffect(() => {
    const timer = window.setInterval(() => setHistory(h => [...h.slice(1), currentBps.current]), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const max = Math.max(...history, 1024);
  const points = history.map((v, i) => `${i * 110 / 39},${27 - v / max * 23}`).join(" ");
  const summary = useMemo(() => tasks.reduce((result, task) => {
    if (task.status === "done") {
      result.completed += 1;
      // HLS totals count segments rather than bytes.
      if (task.kind === "hls") result.hasSegmented = true;
      else result.bytes += task.total_bytes ?? task.downloaded_bytes;
    }
    return result;
  }, { completed: 0, bytes: 0, hasSegmented: false }), [tasks]);
  return (
    <footer className="transfer-status" aria-label={t("transferStatus")}>
      <div className="transfer-speed"><Icon name="download" size={15} /><strong>{fmtSpeed(bps)}</strong>
        <svg className="speed-chart" viewBox="0 0 110 30" role="img" aria-label={t("speedHistory")}>
          <path d={`M0 29 L${points} L110 29 Z`} fill="currentColor" opacity=".1" />
          <polyline points={points} fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" />
        </svg>
      </div>
      <span className="transfer-active"><span className={`connection-dot${active > 0 ? " connected" : ""}`} />{active > 0 ? t("activeDownloads").replace("{n}", String(active)) : t("transferIdle")}</span>
      <div className="spacer" />
      <span className="transfer-completed">{t("completedTasks").replace("{n}", String(summary.completed))}</span>
      {!summary.hasSegmented && <span className="transfer-bytes">{t("completedSize")} <strong>{fmtBytes(summary.bytes)}</strong></span>}
    </footer>
  );
}
