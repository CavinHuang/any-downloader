import { useState } from "react";
import { api, Task } from "../api";
import { fmtBytes } from "../App";
import { t } from "../i18n";

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
  const active = ["probing", "downloading", "merging", "verifying", "decrypting"].includes(
    task.status
  );
  const [name, setName] = useState(task.filename ?? "");
  const [renaming, setRenaming] = useState(false);
  const [prio, setPrio] = useState(String(task.priority));
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
    } finally {
      setRenaming(false);
    }
  };
  return (
    <div className="drawer-mask" onClick={onClose}>
      <div className="drawer" onClick={(e) => e.stopPropagation()}>
        <h3>{task.filename || task.url}</h3>
        <table className="kv">
          <tbody>
            <tr>
              <td>{t("status_" + task.status as never)}</td>
              <td>
                <code>{task.status}</code>
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
              <td>优先级</td>
              <td>
                <select
                  value={prio}
                  disabled={active}
                  onChange={async (e) => {
                    const p = Number(e.target.value);
                    setPrio(e.target.value);
                    onChanged(await api.setPriority(task.id, p));
                  }}
                >
                  <option value="30">高（30）</option>
                  <option value="0">默认（0）</option>
                  <option value="-10">低（-10）</option>
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
                <td>Dir</td>
                <td className="break">{task.dir}</td>
              </tr>
            )}
            {task.total_bytes != null && (
              <tr>
                <td>Size</td>
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
                          className={same ? "primary" : ""}
                          style={{ fontSize: 11 }}
                          disabled={same}
                          onClick={async () => {
                            const nt = await api.addTask({
                              url: v.url,
                              dir: task.dir ?? undefined,
                              filename: task.filename ?? undefined,
                            });
                            await api.startDownload(nt.id);
                            onChanged(nt);
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
        <div className="modal-actions">
          <button
            onClick={async () => {
              onChanged(await api.retryTask(task.id));
            }}
          >
            {t("retry")}
          </button>
          <div className="spacer" />
          {!active && (
            <button
              onClick={async () => {
                await api.removeTask(task.id);
                onRemoved();
              }}
            >
              {t("remove")}
            </button>
          )}
          <button onClick={onClose}>{t("close")}</button>
        </div>
      </div>
    </div>
  );
}
