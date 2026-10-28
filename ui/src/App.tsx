import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { api, isActive, listenEngineEvents, listenSniffEvents, Task, TaskStatus } from "./api";

// 剪贴板事件监听（复用 tauri event 通道）
function listenEngineEventsClip(cb: (url: string) => void) {
  return listen<{ url: string }>("clipboard-url", (e) => cb(e.payload.url));
}
import { open as pickFile, save as pickSave } from "@tauri-apps/plugin-dialog";
import { t } from "./i18n";
import { ROW_H } from "./const";
import { dismissToast, getToasts, showToast, subscribeToasts } from "./toast";
import {
  getUpdateState,
  scheduleAutoChecks,
  subscribeUpdate,
} from "./updater";
import { AddDialog } from "./components/AddDialog";
import { DetailDrawer } from "./components/DetailDrawer";
import { SettingsDialog } from "./components/SettingsDialog";
import { SniffPanel } from "./components/SniffPanel";
import { TaskRow } from "./components/TaskRow";
import { store } from "./store";
import { CATEGORIES, categoryOf, Category } from "./fileTypes";
import { Icon, type IconName } from "./components/Icon";
import { TransferPanel } from "./components/TransferPanel";

type Filter = "all" | "active" | "queued" | "paused" | "done" | "failed" | "canceled";
const TASK_VIEWS: { key: Filter; icon: IconName }[] = [
  { key: "all", icon: "grid" },
  { key: "active", icon: "download" },
  { key: "queued", icon: "clock" },
  { key: "paused", icon: "pause" },
  { key: "done", icon: "check" },
  { key: "failed", icon: "alert" },
  { key: "canceled", icon: "close" },
];
const OVERSCAN = 8;

export function App() {
  const tasks = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const toasts = useSyncExternalStore(subscribeToasts, getToasts);
  const upd = useSyncExternalStore(subscribeUpdate, getUpdateState);
  // 更新提示形态：设置图标角标 + 悬停 tooltip（完整进度/操作在设置 → 05）
  const updateNudge =
    upd.status === "available" ||
    upd.status === "installing" ||
    upd.status === "ready" ||
    upd.status === "error";
  const updateTooltip =
    upd.status === "available"
      ? t("updateAvailableTo").replace("{v}", upd.version ?? "")
      : upd.status === "installing"
        ? `${t("updateInstalling")} ${upd.total != null ? `${Math.min(100, Math.round((upd.downloaded / upd.total) * 100))}%` : ""}`
        : upd.status === "ready"
          ? t("updateReadyTooltip")
          : upd.status === "error"
            ? `${t("updateFailed")}: ${upd.error ?? ""}`
            : t("settings");
  const [filter, setFilter] = useState<Filter>("all");
  const [catFilter, setCatFilter] = useState<Category>("all");
  const [showAdd, setShowAdd] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [draftUrl, setDraftUrl] = useState("");
  const [quickBusy, setQuickBusy] = useState(false);
  const searchRef = useRef<HTMLInputElement>(null);
  const rowsRef = useRef<HTMLDivElement>(null);
  // A12：只存 id，详情任务渲染时从 store 快照按 id 取（打开期间实时更新）
  const [detailId, setDetailId] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  // 全文搜索：文章任务正文命中（设置 → 05 之外的采集能力；docs/MEDIA-ADAPTERS.md）
  const [contentHits, setContentHits] = useState<Set<string>>(new Set());
  const [inTauri] = useState(() => "__TAURI_INTERNALS__" in window);
  const [totalBps, setTotalBps] = useState(0);

  // 正文命中（防抖 250ms）：文章任务的 Markdown 全文搜索（命中任务的 id 集合）
  useEffect(() => {
    const q = search.trim();
    if (!inTauri || q.length < 2) {
      setContentHits(new Set());
      return;
    }
    let alive = true;
    const timer = setTimeout(() => {
      api
        .searchContent(q)
        .then((hits) => {
          if (alive) setContentHits(new Set(hits.map((h) => h.id)));
        })
        .catch(() => {});
    }, 250);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [search, inTauri]);
  // 桌面嗅探：入口角标计数 + 抽屉开关（条目列表在 SniffPanel 内自持）
  const [sniffOpen, setSniffOpen] = useState(false);
  const [sniffCount, setSniffCount] = useState(0);
  const [clipUrl, setClipUrl] = useState<string | null>(null);
  const [clipBusy, setClipBusy] = useState(false);
  const seenClips = useRef(new Set<string>());
  const [sortKey, setSortKey] = useState<null | "name" | "size" | "speed">(null);
  const [sortDir, setSortDir] = useState<1 | -1>(1);
  const speedSortRef = useRef(false);
  const [speedRevision, setSpeedRevision] = useState(0);
  useEffect(() => { speedSortRef.current = sortKey === "speed"; }, [sortKey]);
  // A17：批量操作进行中标记（按钮 disabled + 「…」）
  const [busyOp, setBusyOp] = useState<string | null>(null);
  // —— 批量选择（行多选 + 上下文操作栏）：选中集合按任务 id 持有 ——
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const lastIndexRef = useRef<number | null>(null); // Shift 范围选择锚点（filtered 下标）
  const headerCbRef = useRef<HTMLInputElement | null>(null);
  // A23：首屏骨架 = 首次加载 + 列表为空 + 延迟 300ms
  const [firstLoad, setFirstLoad] = useState(true);
  const [skeletonOn, setSkeletonOn] = useState(false);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.ctrlKey || event.metaKey) || showAdd || showSettings || detailId) return;
      if (event.key.toLowerCase() === "k") { event.preventDefault(); searchRef.current?.focus(); }
      if (event.key.toLowerCase() === "n") { event.preventDefault(); setShowAdd(true); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [showAdd, showSettings, detailId]);

  useEffect(() => {
    if (!firstLoad) return;
    const timer = window.setTimeout(() => setSkeletonOn(true), 300); // 防毫秒级 IPC 闪烁
    return () => window.clearTimeout(timer);
  }, [firstLoad]);

  // 桌面嗅探新条目：masthead 角标 + 轻提示（列表本体在 SniffPanel 里维护）
  useEffect(() => {
    if (!inTauri) return;
    const un = listenSniffEvents((ev) => {
      if (ev.type !== "item") return;
      setSniffCount((n) => n + 1);
      showToast(t("sniffNewToast").replace("{t}", (ev.item.title || ev.item.url).slice(0, 60)), "info");
    }).catch(() => () => {});
    return () => {
      un.then((f) => f());
    };
  }, [inTauri]);

  useEffect(() => {
    if (!inTauri) return;
    store
      .refresh()
      .catch(() => {})
      .finally(() => setFirstLoad(false));
    const poll = setInterval(() => store.pollIncremental(), 2000);
    const unlisten = listenEngineEvents((ev) => store.applyEvent(ev)).catch(() => () => {});
    const unclip = listenEngineEventsClip((url) => {
      // 同链接本会话只提示一次（DESIGN §8 防误触发）
      if (seenClips.current.has(url)) return;
      seenClips.current.add(url);
      setClipUrl(url);
    }).catch(() => () => {});
    // 自动更新：启动后静默首查，此后每 6 小时一轮；发现新版本自动后台下载（§docs/RELEASE.md）
    scheduleAutoChecks();
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
      if (speedSortRef.current) setSpeedRevision(revision => revision + 1);
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
        case "queued":
          return s === "queued" || s === "waiting_retry";
        case "paused":
        case "canceled":
          return s === filter;
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
          (x.filename ?? "").toLowerCase().includes(q) ||
          contentHits.has(x.id))
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
  }, [tasks, filter, catFilter, search, contentHits, sortKey, sortDir, speedRevision]);

  // —— 批量选择：筛选/搜索/数据变化后剪除已不在当前列表的选中项
  //（删除任务后随之自动清空，无需专门处理）——
  useEffect(() => {
    setSelected((prev) => {
      if (prev.size === 0) return prev;
      const alive = new Set(filtered.map((x) => x.id));
      const next = new Set([...prev].filter((id) => alive.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [filtered]);

  // Esc 清除选择（浮层打开时让位给浮层自身的关闭逻辑）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (showAdd || showSettings || detailId) return;
      if (selected.size > 0) setSelected(new Set());
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selected.size, showAdd, showSettings, detailId]);

  const toggleSelect = (task: Task, index: number, shift: boolean) => {
    const anchor = lastIndexRef.current;
    setSelected((prev) => {
      const next = new Set(prev);
      if (shift && anchor !== null) {
        // 范围语义跟随锚点行：锚点→当前行整体 选/清（以当前行为准）
        const [a, b] = [Math.min(anchor, index), Math.max(anchor, index)];
        const add = !prev.has(task.id);
        for (let i = a; i <= b; i += 1) {
          const id = filtered[i]?.id;
          if (id == null) continue;
          if (add) next.add(id);
          else next.delete(id);
        }
      } else if (prev.has(task.id)) {
        next.delete(task.id);
      } else {
        next.add(task.id);
      }
      return next;
    });
    lastIndexRef.current = index;
  };

  const allSelected = filtered.length > 0 && filtered.every((x) => selected.has(x.id));
  const someSelected = selected.size > 0 && !allSelected;
  useEffect(() => {
    if (headerCbRef.current) headerCbRef.current.indeterminate = someSelected;
  }, [someSelected]);

  const toggleAll = () => {
    setSelected((prev) =>
      allSelected ? new Set<string>() : new Set([...prev, ...filtered.map((x) => x.id)])
    );
    lastIndexRef.current = null;
  };

  // —— 虚拟滚动（固定行高窗口化，万级行 60fps）
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(600);
  const start = Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN);
  const end = Math.min(filtered.length, Math.ceil((scrollTop + viewportH) / ROW_H) + OVERSCAN);
  const visible = filtered.slice(start, end);

  useEffect(() => {
    const el = rowsRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setViewportH(el.clientHeight));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (rowsRef.current) rowsRef.current.scrollTop = 0;
    setScrollTop(0);
    lastIndexRef.current = null;
  }, [filter, catFilter, search, sortKey, sortDir]);

  const counts = useMemo(() => {
    const c: Record<Filter, number> = { all: tasks.length, active: 0, queued: 0, paused: 0, done: 0, failed: 0, canceled: 0 };
    for (const x of tasks) {
      if (isActive(x.status)) c.active += 1;
      else if (x.status === "done") c.done += 1;
      else if (x.status === "failed") c.failed += 1;
      else if (x.status === "queued" || x.status === "waiting_retry") c.queued += 1;
      else if (x.status === "paused") c.paused += 1;
      else if (x.status === "canceled") c.canceled += 1;
    }
    return c;
  }, [tasks]);

  // A12：详情任务实时取自 store 快照
  const detailTask = useMemo(
    () => (detailId ? tasks.find((x) => x.id === detailId) ?? null : null),
    [tasks, detailId]
  );

  // —— A17：批量操作统一 Promise.allSettled + 计数 toast + 按钮忙态
  const runBatch = async (op: string, fn: () => Promise<void>) => {
    setBusyOp(op);
    try {
      await fn();
    } catch (error) {
      showToast(String(error), "error");
    } finally {
      setBusyOp(null);
    }
  };
  const countOk = (rs: PromiseSettledResult<unknown>[]) =>
    rs.filter((r) => r.status === "fulfilled").length;
  // 失败项不静默（§1）：ok < total 时追加「失败 k」并转 warn 语义
  const toastBatch = (msg: string, ok: number, total: number) => {
    const fail = total - ok;
    showToast(
      fail > 0 ? `${msg} · ${t("failedN").replace("{n}", String(fail))}` : msg,
      fail > 0 ? "warn" : "success"
    );
  };

  const pauseAll = () =>
    runBatch("pauseAll", async () => {
      const targets = tasks.filter((x) => isActive(x.status));
      const rs = await Promise.allSettled(targets.map((x) => api.pauseTask(x.id)));
      store.refresh();
      toastBatch(t("pausedN").replace("{n}", String(countOk(rs))), countOk(rs), targets.length);
    });

  const resumeAll = () =>
    runBatch("resumeAll", async () => {
      const targets = tasks.filter((x) => x.status === "paused");
      const rs = await Promise.allSettled(targets.map((x) => api.startDownload(x.id)));
      store.refresh();
      toastBatch(t("resumedN").replace("{n}", String(countOk(rs))), countOk(rs), targets.length);
    });

  const retryAllFailed = () =>
    runBatch("retryAll", async () => {
      const targets = filtered.filter((x) => x.status === "failed");
      const rs = await Promise.allSettled(
        targets.map(async (x) => {
          await api.retryTask(x.id);
          await api.startDownload(x.id);
        })
      );
      store.refresh();
      toastBatch(t("retriedN").replace("{n}", String(countOk(rs))), countOk(rs), targets.length);
    });

  // A2：清空记录确认——文案含数量与后果（§4.5 破坏性-批量）
  const clearDone = () =>
    runBatch("clearDone", async () => {
      const targets = filtered.filter((x) => x.status === "done");
      if (targets.length === 0) return;
      if (!window.confirm(t("clearDoneConfirm").replace("{n}", String(targets.length)))) return;
      const rs = await Promise.allSettled(targets.map((x) => api.removeTask(x.id)));
      store.refresh();
      toastBatch(t("clearedN").replace("{n}", String(countOk(rs))), countOk(rs), targets.length);
    });

  // —— 选中项批量操作：targets = 选中 ∩ 适用状态；空集给中性提示不静默（§3.8）
  const runSelected = (
    op: string,
    eligible: (x: Task) => boolean,
    action: (x: Task) => Promise<unknown>,
    msgKey: string
  ) =>
    runBatch(op, async () => {
      const targets = filtered.filter((x) => selected.has(x.id) && eligible(x));
      if (targets.length === 0) {
        showToast(t("noEligible"), "warn");
        return;
      }
      const rs = await Promise.allSettled(targets.map(action));
      store.refresh();
      toastBatch(t(msgKey).replace("{n}", String(countOk(rs))), countOk(rs), targets.length);
    });

  const selStart = () =>
    runSelected(
      "selStart",
      (x) => x.status === "queued" || x.status === "paused" || x.status === "waiting_retry",
      (x) => api.startDownload(x.id),
      "startedN"
    );
  const selPause = () =>
    runSelected("selPause", (x) => isActive(x.status), (x) => api.pauseTask(x.id), "pausedN");
  const selRetry = () =>
    runSelected(
      "selRetry",
      (x) => x.status === "failed" || x.status === "canceled",
      async (x) => {
        await api.retryTask(x.id);
        await api.startDownload(x.id);
      },
      "retriedN"
    );
  const selCancel = () =>
    runSelected("selCancel", (x) => isActive(x.status), (x) => api.cancelTask(x.id), "canceledN");
  // 删除：含数量确认，注明文件保留（§4.5 破坏性-批量）；成功后清空选择
  const selRemove = () =>
    runBatch("selRemove", async () => {
      const targets = filtered.filter((x) => selected.has(x.id));
      if (targets.length === 0) return;
      if (!window.confirm(t("removeConfirm").replace("{n}", String(targets.length)))) return;
      const rs = await Promise.allSettled(targets.map((x) => api.removeTask(x.id)));
      store.refresh();
      toastBatch(t("removedN").replace("{n}", String(countOk(rs))), countOk(rs), targets.length);
      setSelected(new Set());
    });

  // A4：剪贴板「下载」失败不再无声（task_exists → duplicate 文案）
  const addFromClip = async () => {
    const url = clipUrl;
    if (!url) return;
    setClipBusy(true);
    try {
      const task = await api.addTask({ url });
      await api.startDownload(task.id);
      setClipUrl(null);
      store.refresh();
    } catch (e) {
      const msg = String(e);
      const dup = msg.includes("task_exists");
      // 重复 → warn（跳过语义）；其他失败 → error（操作失败带原因，§3.8）
      showToast(dup ? t("duplicate") : msg, dup ? "warn" : "error");
    } finally {
      setClipBusy(false);
    }
  };

  const showSkeleton = inTauri && firstLoad && tasks.length === 0 && skeletonOn;

  const pausedCount = tasks.filter(task => task.status === "paused").length;
  const openAdd = () => setShowAdd(true);
  const requireDesktop = () => {
    if (inTauri) return true;
    showToast(t("desktopRequired"), "warn");
    return false;
  };
  const quickDownload = async () => {
    if (quickBusy || !requireDesktop()) return;
    const urls = draftUrl.trim().split(/\s+/).filter(Boolean);
    if (urls.length === 0) return;
    if (urls.some(url => { try { return !["http:", "https:"].includes(new URL(url).protocol); } catch { return true; } })) {
      showToast(t("invalidDownloadUrl"), "warn");
      return;
    }
    setQuickBusy(true);
    try {
      const results = await Promise.allSettled(urls.map(async url => {
        const task = await api.addTask({ url });
        await api.startDownload(task.id);
      }));
      const failedUrls = urls.filter((_, index) => results[index].status === "rejected");
      const ok = results.length - failedUrls.length;
      setDraftUrl(failedUrls.join(" "));
      if (ok > 0) { setFilter("all"); setCatFilter("all"); setSearch(""); }
      const duplicate = results.filter(result => result.status === "rejected" && String(result.reason).includes("task_exists")).length;
      showToast(t("addSummary").replace("{ok}", String(ok)).replace("{dup}", String(duplicate)).replace("{fail}", String(failedUrls.length - duplicate)), failedUrls.length ? "warn" : "success");
      const error = results.find(result => result.status === "rejected" && !String(result.reason).includes("task_exists"));
      if (error?.status === "rejected") showToast(String(error.reason), "error");
      await store.refresh();
    } catch (error) {
      showToast(String(error), "error");
    } finally {
      setQuickBusy(false);
    }
  };
  const exportTasks = async () => {
    if (!requireDesktop()) return;
    try {
      const path = await pickSave({ defaultPath: "anything-tasks.json", filters: [{ name: "JSON", extensions: ["json"] }] });
      if (!path) return;
      const n = await api.exportTasks(path);
      showToast(t("exported").replace("{n}", String(n)), "success");
    } catch (e) { showToast(String(e), "error"); }
  };
  const importTasks = async () => {
    if (!requireDesktop()) return;
    try {
      const path = await pickFile({ multiple: false, filters: [{ name: "JSON", extensions: ["json"] }] });
      if (!path || Array.isArray(path)) return;
      const n = await api.importTasks(path);
      await store.refresh();
      showToast(t("imported").replace("{n}", String(n)), "success");
    } catch (e) { showToast(String(e), "error"); }
  };

  return (
    <div className="app">
      <aside className="sidebar">
        <button className="brand" aria-label={t("home")} onClick={() => { setFilter("all"); setCatFilter("all"); setSearch(""); }}>
          <span className="brand-icon"><Icon name="download" size={23} /></span>
          <span className="brand-name">Anything<span>Downloader</span></span>
        </button>
        <button className="primary new-task" onClick={openAdd} aria-label={t("newTask")} title={`${t("newTask")} (Ctrl+N)`}><Icon name="plus" size={18} /><span>{t("newTask")}</span><kbd>Ctrl N</kbd></button>
        <div className="nav-label">{t("downloadTasks")}</div>
        <nav className="task-nav" aria-label={t("sideStatus")}>
          {TASK_VIEWS.map(({key, icon}) => (
            <button key={key} className={`nav-item${filter === key ? " active" : ""}`} onClick={() => setFilter(key)} aria-label={t(`view_${key}`)} title={t(`view_${key}`)} aria-current={filter === key ? "page" : undefined}>
              <Icon name={icon} size={18} /><span>{t(`view_${key}`)}</span><span className={`nav-count${key === "failed" && counts.failed > 0 ? " has-errors" : ""}`}>{counts[key]}</span>
            </button>
          ))}
        </nav>
        <div className="sidebar-tools">
          <div className="nav-label">{t("tools")}</div>
          <button className="nav-item" aria-label={t("sniffPanel")} title={t("sniffPanel")} onClick={() => { if (requireDesktop()) { setSniffCount(0); setSniffOpen(true); } }}>
            <Icon name="radar" size={18} /><span>{t("sniffPanel")}</span>{sniffCount > 0 && <span className="nav-count">{sniffCount}</span>}
          </button>
          <button className="nav-item settings-entry" aria-label={t("settings")} title={updateTooltip} onClick={() => requireDesktop() && setShowSettings(true)}>
            <Icon name="settings" size={18} /><span>{t("settings")}</span>{updateNudge && <span className="update-dot" aria-hidden="true" />}
          </button>
        </div>
        <div className="sidebar-bottom"><span className={`connection-dot${inTauri ? " connected" : ""}`} /><span>{t(inTauri ? "desktopMode" : "previewMode")}</span></div>
      </aside>

      <div className="workspace">
        <header className="workspace-header">
          <div className="page-heading"><h1>{t(`view_${filter}`)}</h1><span className="task-total">{t("taskCount").replace("{n}", String(counts[filter]))}</span></div>
          <label className="search-box"><Icon name="search" size={16} /><input ref={searchRef} aria-label={t("search")} placeholder={t("searchTasks")} value={search} onChange={e => setSearch(e.target.value)} onKeyDown={e => { if (e.key === "Escape") setSearch(""); }} /><kbd>Ctrl K</kbd></label>
        </header>

        <main className="main">
          <section className="download-entry" aria-label={t("quickDownload")}>
            <form className="link-entry" onSubmit={e => { e.preventDefault(); void quickDownload(); }}>
              <Icon name="link" size={19} />
              <input aria-label={t("quickLinkLabel")} value={draftUrl} disabled={quickBusy} onChange={e => setDraftUrl(e.target.value)} onPaste={e => {
                const pasted = e.clipboardData.getData("text");
                if (/[\r\n]/.test(pasted)) {
                  e.preventDefault();
                  const input = e.currentTarget;
                  setDraftUrl(draftUrl.slice(0, input.selectionStart ?? 0) + pasted.replace(/\s+/g, " ").trim() + draftUrl.slice(input.selectionEnd ?? draftUrl.length));
                }
              }} placeholder={t("quickLinkPlaceholder")} />
              <button className="primary" type="submit" disabled={quickBusy || !draftUrl.trim()}><Icon name="download" size={16} />{t(quickBusy ? "addingTasks" : "downloadNow")}</button>
            </form>
            <div className="entry-caption"><span>{t("quickLinkHint")}</span><button className="text-button" onClick={openAdd}>{t("downloadOptions")}<Icon name="arrow" size={13} /></button></div>
          </section>

          <section className="task-panel" aria-label={t("downloadTasks")}>
            <div className="task-toolbar" role="toolbar" aria-label={t("taskActions")}>
              <div className="toolbar-actions">
                <button onClick={pauseAll} disabled={busyOp !== null || counts.active === 0}><Icon name="pause" size={15} />{t("pauseAll")}</button>
                <button onClick={resumeAll} disabled={busyOp !== null || pausedCount === 0}><Icon name="play" size={15} />{t("resumeAll")}</button>
                {filter === "failed" && counts.failed > 0 && <button onClick={retryAllFailed} disabled={busyOp !== null}><Icon name="retry" size={15} />{t("retryAllFailed")}</button>}
                {filter === "done" && counts.done > 0 && <button onClick={clearDone} disabled={busyOp !== null}>{t("clearDone")}</button>}
              </div>
              <div className="toolbar-secondary">
                <button className="icon-button" title={t("importTasks")} aria-label={t("importTasks")} onClick={importTasks}><Icon name="import" size={17} /></button>
                <button className="icon-button" title={t("exportTasks")} aria-label={t("exportTasks")} onClick={exportTasks}><Icon name="export" size={17} /></button>
                <span className="toolbar-divider" />
                <label className="type-control"><span className="sr-only">{t("sideCategory")}</span><select value={catFilter} onChange={e => setCatFilter(e.target.value as Category)}>{CATEGORIES.map(({key, label}) => <option key={key} value={key}>{t(label)}</option>)}</select></label>
                <label className="sort-control"><span className="sr-only">{t("sortTasks")}</span><select value={sortKey ? sortKey + ":" + sortDir : "default"} onChange={e => {
                  const [key, direction] = e.target.value.split(":");
                  setSortKey(key === "default" ? null : key as "name" | "size" | "speed");
                  setSortDir(direction === "-1" ? -1 : 1);
                }}>
                  <option value="default">{t("sortDefault")}</option>
                  <option value="name:1">{t("sortNameAsc")}</option>
                  <option value="name:-1">{t("sortNameDesc")}</option>
                  <option value="size:-1">{t("sortSizeDesc")}</option>
                  <option value="size:1">{t("sortSizeAsc")}</option>
                  <option value="speed:-1">{t("sortSpeedDesc")}</option>
                </select></label>
              </div>
            </div>

            {clipUrl && <div className="banner clip" role="status">
              <Icon name="link" size={16} /><div className="clip-info"><b>{t("clipboardFound")}</b><span className="clip-url">{clipUrl}</span></div>
              <button onClick={addFromClip} disabled={clipBusy}>{t("download")}{clipBusy ? "…" : ""}</button><button onClick={() => setClipUrl(null)}>{t("ignore")}</button>
            </div>}

            {selected.size > 0 && (
              <div className="batchbar" role="toolbar" aria-label={t("selectedN").replace("{n}", String(selected.size))}>
                <span className="sel-count">{t("selectedN").replace("{n}", String(selected.size))}</span>
                <button onClick={selStart} disabled={busyOp !== null}>{t("start")}</button>
                <button onClick={selPause} disabled={busyOp !== null}>{t("pause")}</button>
                <button onClick={selRetry} disabled={busyOp !== null}>{t("retry")}</button>
                <button onClick={selCancel} disabled={busyOp !== null}>{t("cancel")}</button>
                <button className="danger" onClick={selRemove} disabled={busyOp !== null}>{t("remove")}</button>
                <div className="spacer" /><button className="icon-button" aria-label={t("clearSel")} title={t("clearSel")} onClick={() => setSelected(new Set())} disabled={busyOp !== null}><Icon name="close" size={15} /></button>
              </div>
            )}
            <div className="table-heading">
              <input ref={headerCbRef} type="checkbox" className="sel-cb" checked={allSelected} onChange={toggleAll} aria-label={t("selectAll")} disabled={filtered.length === 0} />
              <span className="heading-file">{t("colFile")}</span><span>{t("colProgress")}</span><span className="heading-speed">{t("colSpeed")}</span><span className="heading-eta">{t("colRemaining")}</span><span className="heading-actions">{t("colActions")}</span>
            </div>

            <div className="rows" ref={rowsRef} onScroll={e => setScrollTop(e.currentTarget.scrollTop)}>
              {showSkeleton ? (
                <div aria-hidden="true">{[56, 38, 46].map((w, i) => <div className="skel-row" key={i}><span className="skel-file" /><div className="skel" style={{width: w + "%"}} /></div>)}</div>
              ) : inTauri && firstLoad ? null : filtered.length === 0 ? (
                <div className="empty">
                  <span className="empty-icon"><Icon name={search.trim() ? "search" : tasks.length === 0 ? "download" : "folder"} size={30} /></span>
                  {search.trim() ? <><h3>{t("emptySearch")}</h3><p>{t("emptySearchHint")}</p><button onClick={() => setSearch("")}>{t("clearSearch")}</button></>
                  : tasks.length === 0 ? <><h3>{t("emptyTasksTitle")}</h3><p>{t("emptyTasksDescription")}</p><button className="primary" onClick={openAdd}><Icon name="plus" size={16} />{t("newTask")}</button><span className="empty-shortcut">Ctrl + N</span></>
                  : <><h3>{t("emptyFiltered")}</h3><button onClick={() => { setFilter("all"); setCatFilter("all"); }}>{t("clearFilter")}</button></>}
                </div>
              ) : (
                <div style={{height: filtered.length * ROW_H, position: "relative"}}>
                  <div style={{transform: `translateY(${start * ROW_H}px)`}}>
                    {visible.map((task, vi) => <TaskRow key={task.id} task={task} onDetail={() => setDetailId(task.id)} selected={selected.has(task.id)} onToggleSelect={shift => toggleSelect(task, start + vi, shift)} />)}
                  </div>
                </div>
              )}
            </div>
            <div className="list-footer"><span>{t("visibleTasks").replace("{n}", String(filtered.length))}</span><span>{t("taskDetailHint")}</span></div>
          </section>
        </main>
        <TransferPanel tasks={tasks} bps={totalBps} active={counts.active} />
      </div>

      <div className="toast-stack">{toasts.map(x => <div key={x.id} className={`banner toast ${x.kind}`} role={x.kind === "error" ? "alert" : "status"}><span className="banner-text">{x.text}</span><button className="banner-close" aria-label={t("close")} onClick={() => dismissToast(x.id)}>×</button></div>)}</div>
      {showAdd && <AddDialog initialUrls={draftUrl} desktopAvailable={inTauri} onClose={() => setShowAdd(false)} onAdded={() => { setShowAdd(false); setDraftUrl(""); store.refresh(); }} />}
      {showSettings && <SettingsDialog onClose={() => setShowSettings(false)} />}
      {sniffOpen && <SniffPanel onClose={() => setSniffOpen(false)} />}
      {detailId && detailTask && <DetailDrawer key={detailTask.id} task={detailTask} onClose={() => setDetailId(null)} onChanged={nt => store.upsert(nt)} onRemoved={() => { setDetailId(null); store.refresh(); }} />}
    </div>
  );
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
