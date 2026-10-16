import { useState } from "react";
import { api, isActive, Task } from "../api";
import { fmtBytes } from "../App";
import { t } from "../i18n";
import { store } from "../store";
import { useOverlay } from "../useOverlay";

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
  const err = task.error as Record<string, unknown> | null;
  const active = isActive(task.status);
  const done = task.status === "done";
  const label = task.filename || task.url;
  const isExe = /\.(exe|msi|apk|dmg)$/i.test(task.filename ?? "");

  const [name, setName] = useState(task.filename ?? "");
  const [renaming, setRenaming] = useState(false);
  const [prio, setPrio] = useState(String(task.priority));
  // A13：所有动作按钮必须 catch → 容器内错误行（失败不再静默）
  const [errLine, setErrLine] = useState<string | null>(null);

  const { ref, onKeyDown } = useOverlay(onClose);

  let variants: Variant[] = [];
  try {
    variants = task.variants_json ? JSON.parse(task.variants_json) : [];
  } catch {
    variants = [];
  }

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

  // A1（P0）：打开所在文件夹（api.revealTask）
  const reveal = async () => {
    try {
      await api.revealTask(task.id);
    } catch (e) {
      setErrLine(String(e));
    }
  };

  // §4.5：可执行文件打开前确认（与行内一致）
  const openFile = async () => {
    try {
      if (done && isExe && !window.confirm(t("exeWarn"))) return;
      await api.openTask(task.id);
    } catch (e) {
      setErrLine(String(e));
    }
  };

  // A13：重试语义与行内/批量统一（retry + start）
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

  // §4.5：删除确认，与行内删除口径一致
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
        <h3>{label}</h3>
        <table className="kv">
          <tbody>
            {/* A6：首列固定字段名「状态」，值列显示译名（不再「下载中 | downloading」同义重复） */}
            <tr>
              <td>{t("statusField")}</td>
              <td>
                <code>{t(`status_${task.status}` as never)}</code>
              </td>
            </tr>
            {err && (
              <tr>
                <td>{t("errorLabel")}</td>
                <td>
                  <code className="err">{String(err.code ?? "")}</code>
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
                <select
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
              </td>
            </tr>
            <tr>
              <td>{t("urlLabelDetail")}</td>
              <td className="break">{task.url}</td>
            </tr>
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
          {/* §3.7：删除按 danger 层级放左侧；A13：状态动作按状态裁剪 */}
          {!active && (
            <button className="danger" onClick={remove}>
              {t("remove")}
            </button>
          )}
          <div className="spacer" />
          {task.status === "failed" || task.status === "canceled" ? (
            <button className="primary" onClick={retry}>
              {t("retry")}
            </button>
          ) : task.status === "queued" ||
            task.status === "paused" ||
            task.status === "waiting_retry" ? (
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
