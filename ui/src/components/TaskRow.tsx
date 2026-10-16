import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { api, isActive, Task } from "../api";
import { fmtBytes, fmtSpeed } from "../App";
import { ROW_H } from "../const";
import { t } from "../i18n";
import { store } from "../store";
import { showToast } from "../toast";

export function TaskRow({
  task,
  onDetail,
  selected,
  onToggleSelect,
}: {
  task: Task;
  onDetail: () => void;
  /** 是否被批量选择选中（App 持有选中集合，行内只受控渲染） */
  selected: boolean;
  /** shift=true 时由 App 做范围选择 */
  onToggleSelect: (shift: boolean) => void;
}) {
  const live = useSyncExternalStore(store.subscribe, () => store.liveOf(task.id));
  const segs = live?.segs ?? null;
  const [flash, setFlash] = useState(false);
  const flashTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(flashTimer.current), []);

  // 操作错误唯一口径（A18/§4.1）：error toast + 出错行闪红 2s；行内不渲染错误文本
  // （56px 固定行高是虚拟滚动硬约束）。捕获与自动清除的时序逻辑保留，仅换出口。
  const guard = (fn: () => Promise<unknown>) => async () => {
    try {
      await fn();
    } catch (e) {
      showToast(String(e), "error");
      setFlash(true);
      window.clearTimeout(flashTimer.current);
      flashTimer.current = window.setTimeout(() => setFlash(false), 2000);
    }
    store.refresh();
  };

  const active = isActive(task.status);
  const total = task.total_bytes;
  // 优先实时值；否则用持久化的 downloaded_bytes（暂停/失败后恢复显示）
  const downloaded =
    live?.downloaded ??
    (task.status === "done" ? task.total_bytes : task.downloaded_bytes || 0);
  const bps = live?.bps ?? 0;
  const pct =
    total && total > 0 ? Math.min(100, Math.floor(((downloaded || 0) / total) * 100)) : 0;
  const name = task.filename || displayName(task.url);
  // A7：exe/msi/apk/dmg 完成态显示警告，且「打开」前确认（§4.5）
  const isExe = /\.(exe|msi|apk|dmg)$/i.test(name);
  const indet = !total && active; // A11：总大小未知且进行中 → indeterminate（multi 不渲染，一律单条）

  const openFile = guard(async () => {
    if (task.status === "done" && isExe && !window.confirm(t("exeWarn"))) return;
    await api.openTask(task.id);
  });

  return (
    <div
      className={`row${flash ? " err-flash" : ""}${selected ? " selected" : ""}`}
      style={{ height: ROW_H }}
      title={t("doubleClickHint")}
      onDoubleClick={onDetail}
    >
      <div
        className="c-sel"
        onClick={(e) => e.stopPropagation()}
        onDoubleClick={(e) => e.stopPropagation()}
      >
        <input
          type="checkbox"
          className="sel-cb"
          checked={selected}
          title={t("selRangeHint")}
          aria-label={t("selRangeHint")}
          onClick={(e) => {
            e.stopPropagation();
            onToggleSelect(e.shiftKey);
          }}
        />
      </div>
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
          {task.status === "done" && isExe ? (
            <span className="warn-text" title={t("exeWarn")}>
              {t("exeWarn")}
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
          <div className={indet ? "bar indet" : "bar"}>
            {!indet && (
              <div
                className={`fill ${task.status}`}
                style={{ width: `${task.status === "done" ? 100 : pct}%` }}
              />
            )}
          </div>
        )}
        <span className="pct">
          {task.kind === "hls" && total
            ? `${downloaded || 0}/${total} ${t("segsUnit")}${task.status === "done" ? "" : ` ${pct}%`}`
            : total
              ? `${fmtBytes(downloaded || 0)} / ${fmtBytes(total)}${task.status === "done" ? "" : ` ${pct}%`}`
              : task.status === "done"
                ? "✓"
                : // A11：「—」时说明总大小未知，避免误读为卡死
                  <span title={t("sizeUnknown")}>—</span>}
        </span>
      </div>
      <div className="c-speed">
        {active && bps > 0 ? fmtSpeed(bps) : ""}
      </div>
      <div className="c-actions" onClick={(e) => e.stopPropagation()}>
        {active ? (
          <>
            <ActionBtn label={t("pause")} onClick={guard(() => api.pauseTask(task.id))} />
            <ActionBtn label={t("cancel")} onClick={guard(() => api.cancelTask(task.id))} />
          </>
        ) : task.status === "done" ? (
          <>
            {/* A1：完成态固定 [打开][文件夹][详情]，行内 ≤3；删除统一收进详情抽屉 */}
            <ActionBtn label={t("open")} onClick={openFile} />
            <ActionBtn
              label={t("openFolder")}
              onClick={guard(() => api.revealTask(task.id))}
            />
            <ActionBtn label={t("detail")} onClick={onDetail} />
          </>
        ) : (
          <>
            {task.status === "failed" || task.status === "canceled" ? (
              <ActionBtn
                primary
                label={t("retry")}
                onClick={guard(async () => {
                  await api.retryTask(task.id);
                  await api.startDownload(task.id);
                })}
              />
            ) : (
              <ActionBtn
                primary
                label={t("start")}
                onClick={guard(() => api.startDownload(task.id))}
              />
            )}
            <ActionBtn label={t("detail")} onClick={onDetail} />
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
