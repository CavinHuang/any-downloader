import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { api, isActive, Task } from "../api";
import { fmtBytes, fmtSpeed } from "../App";
import { ROW_H } from "../const";
import { t } from "../i18n";
import { store } from "../store";
import { showToast } from "../toast";
import { categoryOf, sourceOf } from "../fileTypes";
import { Icon, IconName } from "./Icon";

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
  const [busy, setBusy] = useState(false);
  const flashTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(flashTimer.current), []);

  // 操作错误唯一口径（A18/§4.1）：error toast + 出错行闪红 2s；行内不渲染错误文本
  // 固定行高是虚拟滚动硬约束，错误反馈不改变布局。
  const guard = (fn: () => Promise<unknown>) => async () => {
    if (busy) return;
    setBusy(true);
    try {
      await fn();
      await store.refresh();
    } catch (e) {
      showToast(String(e), "error");
      setFlash(true);
      window.clearTimeout(flashTimer.current);
      flashTimer.current = window.setTimeout(() => setFlash(false), 2000);
    } finally { setBusy(false); }
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
  const category = categoryOf(task);
  const extension = name.match(/\.([a-z0-9]{1,6})$/i)?.[1]?.toUpperCase() ?? "FILE";
  const remaining = active && task.kind !== "hls" && total != null && bps > 0
    ? Math.max(0, (total - (downloaded || 0)) / bps) : null;

  const openFile = guard(async () => {
    if (task.status === "done" && isExe && !window.confirm(t("exeWarn"))) return;
    await api.openTask(task.id);
  });

  return (
    <div
      className={`row task-${task.status}${flash ? " err-flash" : ""}${selected ? " selected" : ""}`}
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
          readOnly
          onClick={(e) => {
            e.stopPropagation();
            onToggleSelect(e.shiftKey);
          }}
        />
      </div>
      <div className={`file-badge ${category}`}><Icon name={category as IconName} size={23} /><span>{extension}</span></div>
      <div className="c-file">
        <button className="name" title={name} onClick={onDetail}>{name}</button>
        <div className="sub">
          <span className={`status ${task.status}`}><span className={`dot ${task.status}`} />{t(`status_${task.status}` as never)}</span>
          <span className="source" title={task.url}>{sourceOf(task.url)}</span>
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
      <div className={`c-progress ${task.status}`}>
        <div className="progress-summary"><span>{task.status === "done" ? t("status_done") : total ? `${pct}%` : t(`status_${task.status}`)}</span>{task.status === "done" && <Icon name="check" size={14} />}</div>
        {segs && segs.length > 1 && total ? (
          <div
            className="bar multi"
            role="progressbar"
            aria-label={t("colProgress")}
            aria-valuenow={pct}
            aria-valuemin={0}
            aria-valuemax={100}
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
          <div className={indet ? "bar indet" : "bar"} role="progressbar" aria-label={t("colProgress")} aria-valuenow={indet ? undefined : task.status === "done" ? 100 : pct} aria-valuemin={0} aria-valuemax={100}>
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
            ? `${downloaded || 0}/${total} ${t("segsUnit")}`
            : total
              ? `${fmtBytes(downloaded || 0)} / ${fmtBytes(total)}`
              : task.status === "done"
                ? fmtBytes(task.downloaded_bytes)
                : // A11：「—」时说明总大小未知，避免误读为卡死
                  <span title={t("sizeUnknown")}>—</span>}
        </span>
      </div>
      <div className="c-speed">{active && bps > 0 ? fmtSpeed(bps) : "—"}</div>
      <div className="c-eta">{remaining == null ? "—" : fmtRemaining(remaining)}</div>
      <fieldset className="c-actions" disabled={busy} aria-busy={busy} onClick={(e) => e.stopPropagation()}>
        {active ? (
          <>
            <ActionBtn icon="pause" label={t("pause")} onClick={guard(() => api.pauseTask(task.id))} />
            <ActionBtn icon="close" label={t("cancel")} onClick={guard(() => api.cancelTask(task.id))} />
            <ActionBtn icon="more" label={t("detail")} onClick={onDetail} />
          </>
        ) : task.status === "done" ? (
          <>
            {/* A1：完成态固定 [打开][文件夹][详情]，行内 ≤3；删除统一收进详情抽屉 */}
            <ActionBtn icon="arrow" label={t("open")} onClick={openFile} />
            <ActionBtn
              icon="folder"
              label={t("openFolder")}
              onClick={guard(() => api.revealTask(task.id))}
            />
            <ActionBtn icon="more" label={t("detail")} onClick={onDetail} />
          </>
        ) : (
          <>
            {task.status === "failed" || task.status === "canceled" ? (
              <ActionBtn
                primary
                icon="retry"
                label={t("retry")}
                onClick={guard(async () => {
                  await api.retryTask(task.id);
                  await api.startDownload(task.id);
                })}
              />
            ) : (
              <ActionBtn
                primary
                icon="play"
                label={t("start")}
                onClick={guard(() => api.startDownload(task.id))}
              />
            )}
            <ActionBtn icon="more" label={t("detail")} onClick={onDetail} />
          </>
        )}
      </fieldset>
    </div>
  );
}

function ActionBtn({
  label,
  onClick,
  primary,
  icon,
}: {
  label: string;
  onClick: () => void;
  primary?: boolean;
  icon: IconName;
}) {
  return (
    <button className={primary ? "icon-button action-primary" : "icon-button"} onClick={onClick} title={label} aria-label={label}>
      <Icon name={icon} size={16} />
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

function fmtRemaining(seconds: number): string {
  const minutes = Math.max(1, Math.ceil(seconds / 60));
  if (seconds < 60) return `${Math.max(1, Math.ceil(seconds))} ${t("secondsShort")}`;
  if (minutes < 60) return `${minutes} ${t("minutesShort")}`;
  return `${Math.floor(minutes / 60)} ${t("hoursShort")} ${minutes % 60} ${t("minutesShort")}`;
}
