import { isActive, type Task } from "../api";
import { t } from "../i18n";
import type { Live } from "../store";

const MAX_ETA_SECONDS = 365 * 86400;

/**
 * Estimate remaining seconds for an active task using smoothed speed
 * (or smoothed segment completion rate for HLS/DASH streams).
 */
export function estimateRemainingSecs(task: Task, live: Live | undefined): number | null {
  if (!isActive(task.status) || task.total_bytes == null || task.total_bytes <= 0) {
    return null;
  }
  const isStream = task.kind === "hls" || task.kind === "dash";
  const rate = isStream ? (live?.unitRate ?? 0) : (live?.avgBps || live?.bps || 0);
  if (rate <= 0) return null;
  const downloaded = live?.downloaded ?? task.downloaded_bytes ?? 0;
  const remaining = Math.max(0, (task.total_bytes - downloaded) / rate);
  return Number.isFinite(remaining) && remaining <= MAX_ETA_SECONDS ? remaining : null;
}

/** Format a remaining duration into a concise, human-readable string. */
export function fmtRemaining(seconds: number): string {
  const s = Math.max(1, Math.ceil(seconds));
  if (s < 60) return `${s} ${t("secondsShort")}`;
  if (s < 600) {
    const mins = Math.floor(s / 60);
    const remSecs = s % 60;
    return remSecs > 0
      ? `${mins} ${t("minutesShort")} ${remSecs} ${t("secondsShort")}`
      : `${mins} ${t("minutesShort")}`;
  }
  const totalMins = Math.ceil(s / 60);
  if (totalMins < 60) return `${totalMins} ${t("minutesShort")}`;
  const totalHours = Math.floor(totalMins / 60);
  if (totalHours < 24) {
    const remMins = totalMins % 60;
    return remMins > 0
      ? `${totalHours} ${t("hoursShort")} ${remMins} ${t("minutesShort")}`
      : `${totalHours} ${t("hoursShort")}`;
  }
  const days = Math.floor(totalHours / 24);
  const remHours = totalHours % 24;
  return remHours > 0
    ? `${days} ${t("daysShort")} ${remHours} ${t("hoursShort")}`
    : `${days} ${t("daysShort")}`;
}

/** Format the estimated local wall-clock completion time (e.g. `14:32` or `明天 01:15`). */
export function fmtFinishClock(seconds: number, nowMs = Date.now()): string {
  const now = new Date(nowMs);
  const target = new Date(nowMs + Math.max(0, seconds) * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  const hhmm = `${pad(target.getHours())}:${pad(target.getMinutes())}`;
  const startOfNow = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const startOfTarget = new Date(target.getFullYear(), target.getMonth(), target.getDate()).getTime();
  const dayDiff = Math.round((startOfTarget - startOfNow) / 86_400_000);
  if (dayDiff === 0) return hhmm;
  if (dayDiff === 1) return `${t("etaTomorrow")} ${hhmm}`;
  return `${pad(target.getMonth() + 1)}-${pad(target.getDate())} ${hhmm}`;
}
