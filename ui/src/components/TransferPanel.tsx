import { useEffect, useMemo, useRef, useState } from "react";
import type { Task } from "../api";
import { fmtBytes, fmtSpeed } from "../App";
import { t } from "../i18n";
import { fmtFinishClock, fmtRemaining } from "../utils/eta";
import { Icon } from "./Icon";

export function TransferPanel({
  tasks,
  bps,
  active,
  queued = 0,
  etaSecs = null,
}: {
  tasks: Task[];
  bps: number;
  active: number;
  queued?: number;
  etaSecs?: number | null;
}) {
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
      <div className="transfer-speed">
        <Icon name="zap" size={14} />
        <span className="transfer-speed-label">{t("totalSpeedLabel")}</span>
        <strong>{fmtSpeed(bps)}</strong>
        <svg className="speed-chart" viewBox="0 0 110 30" role="img" aria-label={t("speedHistory")}>
          <defs>
            <linearGradient id="speedFillGrad" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="currentColor" stopOpacity="0.22" />
              <stop offset="100%" stopColor="currentColor" stopOpacity="0.02" />
            </linearGradient>
          </defs>
          <path d={`M0 29 L${points} L110 29 Z`} fill="url(#speedFillGrad)" />
          <polyline points={points} fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round" />
        </svg>
      </div>
      <span className="transfer-active">
        <span className={`connection-dot${active > 0 ? " connected" : ""}`} />
        {active > 0 ? t("activeDownloads").replace("{n}", String(active)) : t("transferIdle")}
      </span>
      {active > 0 && etaSecs != null && (
        <span className="transfer-eta">
          <Icon name="clock" size={13} />
          {t("etaTotalFinish")
            .replace("{time}", fmtFinishClock(etaSecs))
            .replace("{dur}", fmtRemaining(etaSecs))}
        </span>
      )}
      {queued > 0 && (
        <span className="transfer-queued">{t("queuedCount").replace("{n}", String(queued))}</span>
      )}
      <div className="spacer" />
      <span className="transfer-completed">{t("completedTasks").replace("{n}", String(summary.completed))}</span>
      <span className="transfer-bytes">{t("completedSize")} <strong>{fmtBytes(summary.bytes)}</strong></span>
    </footer>
  );
}
