import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, isActive, listenEngineEvents, Task, TaskStatus } from "./api";

// 剪贴板事件监听（复用 tauri event 通道）
function listenEngineEventsClip(cb: (url: string) => void) {
  return import("@tauri-apps/api/event").then(({ listen }) =>
    listen<{ url: string }>("clipboard-url", (e) => cb(e.payload.url))
  );
}
import { open as pickFile, save as pickSave } from "@tauri-apps/plugin-dialog";
import { t } from "./i18n";
import { AddDialog } from "./components/AddDialog";
import { DetailDrawer } from "./components/DetailDrawer";
import { SettingsDialog } from "./components/SettingsDialog";
import { TaskRow } from "./components/TaskRow";
import { store } from "./store";

type Filter = "all" | "active" | "done" | "failed" | "other";
type Category =
  | "all"
  | "video"
  | "audio"
  | "archive"
  | "doc"
  | "software"
  | "image"
  | "other";

const CAT_RULES: [RegExp, Category][] = [
  [/\.(mp4|mkv|webm|avi|mov|wmv|flv|m4v|ts|m3u8|mpd)$/i, "video"],
  [/\.(mp3|flac|m4a|aac|ogg|wav|ape)$/i, "audio"],
  [/\.(zip|rar|7z|tar|gz)$/i, "archive"],
  [/\.(pdf|epub|docx?|xlsx?|pptx?)$/i, "doc"],
  [/\.(exe|msi|apk|dmg)$/i, "software"],
  [/\.(jpe?g|png|gif|webp)$/i, "image"],
];

function categoryOf(t: Task): Category {
  if (t.kind === "hls") return "video";
  const name = (t.filename ?? t.url.split("?")[0] ?? "").toLowerCase();
  for (const [re, cat] of CAT_RULES) {
    if (re.test(name)) return cat;
  }
  return "other";
}

const CAT_META: { key: Category; icon: string; label: string }[] = [
  { key: "all", icon: "🗂", label: "catAll" },
  { key: "video", icon: "🎬", label: "catVideo" },
  { key: "audio", icon: "🎵", label: "catAudio" },
  { key: "archive", icon: "📦", label: "catArchive" },
  { key: "doc", icon: "📄", label: "catDoc" },
  { key: "software", icon: "⚙️", label: "catSoftware" },
  { key: "image", icon: "🖼", label: "catImage" },
  { key: "other", icon: "•", label: "catOther" },
];

const ROW_H = 56;
const OVERSCAN = 8;

export function App() {
  const tasks = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const [filter, setFilter] = useState<Filter>("all");
  const [catFilter, setCatFilter] = useState<Category>("all");
  const [showAdd, setShowAdd] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [detail, setDetail] = useState<Task | null>(null);
  const [search, setSearch] = useState("");
  const [inTauri, setInTauri] = useState(true);
  const [totalBps, setTotalBps] = useState(0);
  const [clipUrl, setClipUrl] = useState<string | null>(null);
  const [seenClips, setSeenClips] = useState<Set<string>>(new Set());
  const [sortKey, setSortKey] = useState<null | "name" | "size" | "speed">(null);
  const [sortDir, setSortDir] = useState<1 | -1>(1);
  const [toast, setToast] = useState<string | null>(null);

  useEffect(() => {
    // 非 Tauri 环境（纯浏览器打开 vite 页）给出提示而非白屏
    setInTauri("__TAURI_INTERNALS__" in window);
  }, []);

  useEffect(() => {
    if (!inTauri) return;
    store.refresh();
    const poll = setInterval(() => store.pollIncremental(), 2000);
    const unlisten = listenEngineEvents((ev) => store.applyEvent(ev));
    const unclip = listenEngineEventsClip((url) => {
      // 同链接本会话只提示一次（DESIGN §8 防误触发）
      setSeenClips((prev) => {
        if (prev.has(url)) return prev;
        setClipUrl(url);
        return new Set(prev).add(url);
      });
    });
    // 汇总速度（从 live 数据聚合，节流 500ms）
    const speedTimer = setInterval(async () => {
      let sum = 0;
      let doneBytes = 0;
      let totalBytes = 0;
      for (const t of store.getSnapshot()) {
        if (isActive(t.status)) {
          sum += store.liveOf(t.id)?.bps ?? 0;
          doneBytes += store.liveOf(t.id)?.downloaded ?? 0;
          totalBytes += t.total_bytes ?? 0;
        }
      }
      setTotalBps(sum);
      try {
        // 任务栏进度条 + 标题栏速度
        const w = getCurrentWindow();
        if (sum > 0) {
          await w.setTitle(`⬇ ${fmtSpeed(sum)} — ${t("appTitle")}`);
          if (totalBytes > 0) {
            await w.setProgressBar({
              status: "Normal" as never,
              progress: Math.round((doneBytes / totalBytes) * 100),
            });
          }
        } else {
          await w.setTitle(t("appTitle"));
          await w.setProgressBar({ status: "None" as never });
        }
      } catch {
        // 非 Tauri 环境忽略
      }
    }, 500);
    return () => {
      clearInterval(poll);
      clearInterval(speedTimer);
      unlisten.then((f) => f());
      unclip.then((f) => f());
    };
  }, [inTauri]);

  const filtered = useMemo(() => {
    const match = (s: TaskStatus): boolean => {
      switch (filter) {
        case "active":
          return isActive(s);
        case "done":
          return s === "done";
        case "failed":
          return s === "failed";
        case "other":
          return s === "queued" || s === "paused" || s === "waiting_retry" || s === "canceled";
        default:
          return true;
      }
    };
    const q = search.trim().toLowerCase();
    const out = tasks.filter(
      (x) =>
        match(x.status) &&
        (catFilter === "all" || categoryOf(x) === catFilter) &&
        (q === "" ||
          x.url.toLowerCase().includes(q) ||
          (x.filename ?? "").toLowerCase().includes(q))
    );
    if (!sortKey) return out;
    const nameOf = (x: Task) => x.filename ?? x.url;
    return out.sort((a, b) => {
      let r = 0;
      if (sortKey === "name") r = nameOf(a).localeCompare(nameOf(b));
      else if (sortKey === "size") r = (a.total_bytes ?? 0) - (b.total_bytes ?? 0);
      else r = (store.liveOf(a.id)?.bps ?? 0) - (store.liveOf(b.id)?.bps ?? 0);
      return r * sortDir;
    });
  }, [tasks, filter, catFilter, search, sortKey, sortDir]);

  const toggleSort = (key: "name" | "size" | "speed") => {
    if (sortKey === key) setSortDir((d) => (d === 1 ? -1 : 1));
    else {
      setSortKey(key);
      setSortDir(1);
    }
  };

  // —— 虚拟滚动（固定行高窗口化，万级行 60fps）
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(600);
  const start = Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN);
  const end = Math.min(filtered.length, Math.ceil((scrollTop + viewportH) / ROW_H) + OVERSCAN);
  const visible = filtered.slice(start, end);

  const counts = useMemo(() => {
    const c = { all: tasks.length, active: 0, done: 0, failed: 0, other: 0 };
    for (const x of tasks) {
      if (isActive(x.status)) c.active += 1;
      else if (x.status === "done") c.done += 1;
      else if (x.status === "failed") c.failed += 1;
      else c.other += 1;
    }
    return c;
  }, [tasks]);

  const catCounts = useMemo(() => {
    const c: Record<string, number> = {};
    for (const x of tasks) {
      const cat = categoryOf(x);
      c[cat] = (c[cat] || 0) + 1;
    }
    return c;
  }, [tasks]);

  return (
    <div className="app">
      <header className="toolbar">
        <div className="brand">
          <span className="logo">⬇</span>
          <b>{t("appTitle")}</b>
        </div>
        <button className="primary" onClick={() => setShowAdd(true)}>
          ＋ {t("newTask")}
        </button>
        <button onClick={() => setShowSettings(true)}>{t("settings")}</button>
        <button
          onClick={async () => {
            const path = await pickSave({
              defaultPath: "anything-tasks.json",
              filters: [{ name: "JSON", extensions: ["json"] }],
            });
            if (!path) return;
            const n = await api.exportTasks(path);
            setToast(t("exported").replace("{n}", String(n)));
            setTimeout(() => setToast(null), 2500);
          }}
        >
          {t("exportTasks")}
        </button>
        <button
          onClick={async () => {
            const path = await pickFile({
              multiple: false,
              filters: [{ name: "JSON", extensions: ["json"] }],
            });
            if (!path || Array.isArray(path)) return;
            const n = await api.importTasks(path);
            setToast(t("imported").replace("{n}", String(n)));
            setTimeout(() => setToast(null), 2500);
            store.refresh();
          }}
        >
          {t("importTasks")}
        </button>
        <div className="spacer" />
        <input
          className="search"
          placeholder={t("search")}
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
        <span className="total-speed">
          {totalBps > 0 ? `${fmtSpeed(totalBps)}` : ""}
        </span>
      </header>

      <div className="body">
        <aside className="sidebar">
          <div className="side-group">
            <div className="side-title">{t("sideStatus")}</div>
            {(
              [
                ["all", "🗂", t("all")],
                ["active", "⏬", t("active")],
                ["done", "✓", t("done")],
                ["failed", "⚠", t("failed")],
                ["other", "…", t("other")],
              ] as [Filter, string, string][]
            ).map(([key, icon, label]) => (
              <button
                key={key}
                className={filter === key ? "side-item active" : "side-item"}
                onClick={() => setFilter(key)}
              >
                <span className="side-icon">{icon}</span>
                {label}
                <span className="side-count">{counts[key]}</span>
              </button>
            ))}
            <div className="side-actions">
              <button
                onClick={async () => {
                  for (const x of tasks) {
                    if (x.status === "downloading") {
                      try {
                        await api.pauseTask(x.id);
                      } catch {}
                    }
                  }
                  store.refresh();
                }}
              >
                全部暂停
              </button>
              <button
                onClick={async () => {
                  for (const x of tasks) {
                    if (x.status === "paused") {
                      try {
                        await api.startDownload(x.id);
                      } catch {}
                    }
                  }
                  store.refresh();
                }}
              >
                全部恢复
              </button>
            </div>
          </div>
          <div className="side-group">
            <div className="side-title">{t("sideCategory")}</div>
            {CAT_META.map(({ key, icon, label }) => (
              <button
                key={key}
                className={catFilter === key ? "side-item active" : "side-item"}
                onClick={() => setCatFilter(key)}
              >
                <span className="side-icon">{icon}</span>
                {t(label as never)}
                <span className="side-count">
                  {key === "all" ? tasks.length : catCounts[key] || 0}
                </span>
              </button>
            ))}
          </div>
        </aside>

        <main className="main">
          {!inTauri && <div className="banner">{t("needTauri")}</div>}
          {toast && <div className="banner toast">{toast}</div>}
          {clipUrl && (
            <div className="banner clip">
              <span className="clip-url">{clipUrl}</span>
              <button
                className="primary"
                onClick={async () => {
                  const url = clipUrl;
                  setClipUrl(null);
                  const task = await api.addTask({ url });
                  await api.startDownload(task.id);
                  store.refresh();
                }}
              >
                {t("download")}
              </button>
              <button onClick={() => setClipUrl(null)}>{t("ignore")}</button>
              <button className="ghost" onClick={() => setClipUrl(null)}>
                ×
              </button>
            </div>
          )}

          <div className="maintool">
            <span className="crumb">
              {t(filter as never)}
              {catFilter !== "all" ? ` · ${t(("cat" + cap(catFilter)) as never)}` : ""}
            </span>
            <div className="spacer" />
            {filter === "failed" && counts.failed > 0 && (
              <button
                className="batch"
                onClick={async () => {
                  for (const x of filtered) {
                    if (x.status !== "failed") continue;
                    try {
                      await api.retryTask(x.id);
                      await api.startDownload(x.id);
                    } catch {
                      // 单个失败不中断
                    }
                  }
                  store.refresh();
                }}
              >
                {t("retryAllFailed")}
              </button>
            )}
            {filter === "done" && counts.done > 0 && (
              <button
                className="batch"
                onClick={async () => {
                  for (const x of filtered) {
                    if (x.status !== "done") continue;
                    try {
                      await api.removeTask(x.id);
                    } catch {
                      // 单个失败不中断
                    }
                  }
                  store.refresh();
                }}
              >
                {t("clearDone")}
              </button>
            )}
          </div>

          <div className="table-head">
            <span
              className="c-file sortable"
              onClick={() => toggleSort("name")}
              title="↕"
            >
              {t("colFile")}
              {sortKey === "name" ? (sortDir === 1 ? " ▲" : " ▼") : ""}
            </span>
            <span className="c-progress">{t("colProgress")}</span>
            <span
              className="c-speed sortable"
              onClick={() => toggleSort("speed")}
              title="↕"
            >
              {t("colSpeed")}
              {sortKey === "speed" ? (sortDir === 1 ? " ▲" : " ▼") : ""}
            </span>
            <span
              className="c-actions sortable"
              onClick={() => toggleSort("size")}
              title="按大小排序"
            >
              {sortKey === "size" ? (sortDir === 1 ? "大小 ▲" : "大小 ▼") : t("colActions")}
            </span>
          </div>

          <div
            className="rows"
            onScroll={(e) => setScrollTop((e.target as HTMLDivElement).scrollTop)}
            ref={(el) => {
              if (el && Math.abs(el.clientHeight - viewportH) > 4) {
                setViewportH(el.clientHeight);
              }
            }}
          >
            {filtered.length === 0 ? (
              <div className="empty">{t("emptyList")}</div>
            ) : (
              <div style={{ height: filtered.length * ROW_H, position: "relative" }}>
                <div style={{ transform: `translateY(${start * ROW_H}px)` }}>
                  {visible.map((task) => (
                    <TaskRow
                      key={task.id}
                      task={task}
                      onDetail={() => setDetail(task)}
                    />
                  ))}
                </div>
              </div>
            )}
          </div>
        </main>
      </div>

      {showAdd && (
        <AddDialog
          onClose={() => setShowAdd(false)}
          onAdded={() => {
            setShowAdd(false);
            store.refresh();
          }}
        />
      )}
      {showSettings && <SettingsDialog onClose={() => setShowSettings(false)} />}
      {detail && (
        <DetailDrawer
          key={detail.id}
          task={detail}
          onClose={() => setDetail(null)}
          onChanged={(nt) => {
            store.upsert(nt);
            setDetail(nt);
          }}
          onRemoved={() => {
            setDetail(null);
            store.refresh();
          }}
        />
      )}
    </div>
  );
}

function cap(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

export function fmtSpeed(bps: number): string {
  return `${fmtBytes(bps)}/s`;
}

export function fmtBytes(n: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(i ? 1 : 0)} ${u[i]}`;
}
