import { useState } from "react";
import { api, isActive, Task } from "../api";
import { fmtBytes, fmtSpeed } from "../App";
import { t } from "../i18n";
import { store } from "../store";
import { useSyncExternalStore } from "react";

const ROW_H = 56;

export function TaskRow({ task, onDetail }: { task: Task; onDetail: () => void }) {
  const live = useSyncExternalStore(store.subscribe, () => store.liveOf(task.id));
  const segs = live?.segs ?? null;
  const [opErr, setOpErr] = useState<string | null>(null);
  // 操作错误不再静默：捕获后行内提示（3s 自动清除）
  const guard = (fn: () => Promise<unknown>) => async () => {
    try {
      await fn();
      setOpErr(null);
    } catch (e) {
      setOpErr(String(e));
      setTimeout(() => setOpErr(null), 3000);
    }
    store.refresh();
  };
  const active = isActive(task.status);
  const total = live ? task.total_bytes : task.total_bytes;
  // 优先实时值；否则用持久化的 downloaded_bytes（暂停/失败后恢复显示）
  const downloaded =
    live?.downloaded ??
    (task.status === "done" ? task.total_bytes : task.downloaded_bytes || 0);
  const bps = live?.bps ?? 0;
  const pct =
    total && total > 0 ? Math.min(100, Math.floor(((downloaded || 0) / total) * 100)) : 0;
  const name = task.filename || displayName(task.url);

  return (
    <div
      className="row"
      style={{ height: ROW_H }}
      onDoubleClick={() => {
        if (task.status === "done") {
          api.openTask(task.id);
        } else {
          onDetail();
        }
      }}
    >
      <div className="c-file">
        <div className="name" title={task.url}>
          <span className={`dot ${task.status}`} />
          {name}
        </div>
        <div className="sub">
          <span className={`status ${task.status}`}>{t(`status_${task.status}` as never)}</span>
          {task.status === "failed" && task.error ? (
            <span className="err" title={JSON.stringify(task.error)}>
              {errorCode(task.error)}
            </span>
          ) : null}
        </div>
      </div>
      <div className="c-progress">
        {segs && segs.length > 1 && total ? (
          <div
            className="bar multi"
            title={
              live?.segSpeeds
                ? segs
                    .map(
                      (sg, i) =>
                        `#${i + 1} ${fmtSpeed(live.segSpeeds?.[i] ?? 0)}`
                    )
                    .join("  ")
                : ""
            }
          >
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
                  <div
                    className={`seg-fill ${isDone ? "done" : ""}`}
                    style={{ width: `${filled}%` }}
                  />
                </div>
              );
            })}
          </div>
        ) : (
          <div className="bar">
            <div
              className={`fill ${task.status}`}
              style={{ width: `${task.status === "done" ? 100 : pct}%` }}
            />
          </div>
        )}
        <span className="pct">
          {task.kind === "hls" && total
            ? `${downloaded || 0}/${total} ${t("segsUnit")}${task.status === "done" ? "" : ` ${pct}%`}`
            : total
              ? `${fmtBytes(downloaded || 0)} / ${fmtBytes(total)}${task.status === "done" ? "" : ` ${pct}%`}`
              : task.status === "done"
                ? "✓"
                : "—"}
        </span>
      </div>
      <div className="c-speed">
        {active && bps > 0 ? fmtSpeed(bps) : ""}
        {opErr && (
          <div className="error" title={opErr}>
            {opErr.length > 40 ? opErr.slice(0, 40) + "…" : opErr}
          </div>
        )}
      </div>
      <div className="c-actions" onClick={(e) => e.stopPropagation()}>
        {active ? (
          <>
            <ActionBtn label={t("pause")} onClick={guard(() => api.pauseTask(task.id))} />
            <ActionBtn label={t("cancel")} onClick={guard(() => api.cancelTask(task.id))} />
          </>
        ) : (
          <>
            {task.status !== "done" && (
              <ActionBtn
                label={task.status === "failed" || task.status === "canceled" ? t("retry") : t("start")}
                primary
                onClick={guard(async () => {
                  if (task.status === "failed" || task.status === "canceled") {
                    await api.retryTask(task.id);
                  }
                  await api.startDownload(task.id);
                })}
              />
            )}
            {task.status === "done" && (
              <ActionBtn label={t("open")} onClick={() => api.openTask(task.id)} />
            )}
            <ActionBtn label={t("detail")} onClick={onDetail} />
            <ActionBtn
              label={t("remove")}
              onClick={guard(async () => {
                if (!window.confirm(`${t("remove")} ${name}?`)) return;
                await api.removeTask(task.id);
              })}
            />
          </>
        )}
      </div>
    </div>
  );
}

function ActionBtn({
  label,
  onClick,
  primary,
}: {
  label: string;
  onClick: () => void;
  primary?: boolean;
}) {
  return (
    <button className={primary ? "btn primary" : "btn"} onClick={onClick}>
      {label}
    </button>
  );
}

function displayName(url: string): string {
  try {
    const u = new URL(url);
    const seg = u.pathname.split("/").filter(Boolean).pop();
    return decodeURIComponent(seg || u.hostname);
  } catch {
    return url;
  }
}

function errorCode(err: Record<string, unknown>): string {
  return String(err.code ?? JSON.stringify(err));
}
