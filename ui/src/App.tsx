import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { api, isActive, listenEngineEvents, Task, TaskStatus } from "./api";

// 剪贴板事件监听（复用 tauri event 通道）
function listenEngineEventsClip(cb: (url: string) => void) {
  return listen<{ url: string }>("clipboard-url", (e) => cb(e.payload.url));
}
import { open as pickFile, save as pickSave } from "@tauri-apps/plugin-dialog";
import { t } from "./i18n";
import { ROW_H } from "./const";
import { dismissToast, getToasts, showToast, subscribeToasts } from "./toast";
import {
  dismissUpdate,
  getUpdateState,
  installUpdate,
  restartToUpdate,
  scheduleStartupCheck,
  subscribeUpdate,
} from "./updater";
import { AddDialog } from "./components/AddDialog";
import { DetailDrawer } from "./components/DetailDrawer";
import { SettingsDialog } from "./components/SettingsDialog";
import { TaskRow } from "./components/TaskRow";
import { store } from "./store";
import { CATEGORIES, categoryOf, Category } from "./fileTypes";
import { Icon } from "./components/Icon";
import { EmptyCollection } from "./components/EmptyCollection";
import { TransferPanel } from "./components/TransferPanel";

type Filter = "all" | "active" | "done" | "failed" | "other";
const OVERSCAN = 8;

export function App() {
  const tasks = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const toasts = useSyncExternalStore(subscribeToasts, getToasts);
  const upd = useSyncExternalStore(subscribeUpdate, getUpdateState);
  const [filter, setFilter] = useState<Filter>("all");
  const [catFilter, setCatFilter] = useState<Category>("all");
  const [showAdd, setShowAdd] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [draftUrl, setDraftUrl] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);
  const rowsRef = useRef<HTMLDivElement>(null);
  // A12：只存 id，详情任务渲染时从 store 快照按 id 取（打开期间实时更新）
  const [detailId, setDetailId] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [inTauri] = useState(() => "__TAURI_INTERNALS__" in window);
  const [totalBps, setTotalBps] = useState(0);
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
    // 自动更新：启动后静默检查一次（失败不打扰，§更新见 docs/RELEASE.md）
    scheduleStartupCheck();
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
  }, [tasks, filter, catFilter, search, sortKey, sortDir, speedRevision]);

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
    setSelected((prev) => {
      const next = new Set(prev);
      if (shift && lastIndexRef.current !== null) {
        // 范围语义跟随锚点行：锚点→当前行整体 选/清（以当前行为准）
        const anchor = lastIndexRef.current;
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
    const c = { all: tasks.length, active: 0, done: 0, failed: 0, other: 0 };
    for (const x of tasks) {
      if (isActive(x.status)) c.active += 1;
      else if (x.status === "done") c.done += 1;
      else if (x.status === "failed") c.failed += 1;
      else c.other += 1;
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
    <div className={`app${tasks.length > 0 ? " has-collection" : ""}`}>
      <header className="masthead">
        <button className="brand" aria-label={t("home")} onClick={() => { setFilter("all"); setCatFilter("all"); setSearch(""); }}>
          <img className="brand-mark" src="/brand-mark.png" alt="" aria-hidden="true" />
          <span>anything<span className="brand-period">.</span></span>
        </button>
        <span className="masthead-divider" />
        <span className="workspace-label">{t("personalSpace")}</span>
        <div className="spacer" />
        <span className="edition-label">A LITTLE LESS LOST.</span>
        <div className="header-actions">
          <button className="icon-button" title={t("importTasks")} aria-label={t("importTasks")} onClick={importTasks}><Icon name="import" /></button>
          <button className="icon-button" title={t("exportTasks")} aria-label={t("exportTasks")} onClick={exportTasks}><Icon name="export" /></button>
          <span className="action-divider" />
          <button className="icon-button" title={t("settings")} aria-label={t("settings")} onClick={() => requireDesktop() && setShowSettings(true)}><Icon name="settings" /></button>
        </div>
      </header>

      <div className="studio">
        <main className="main">
          <section className="intro">
            <div>
              <div className="eyebrow"><span className="tiny-cross">✳</span> {t("collectionEyebrow")}</div>
              <h1>{t("collectionHeading")}<span className="heading-period">{t("headingPunctuation")}</span></h1>
              <p className="intro-description">{t("collectionDescription")}</p>
            </div>
            <button className="new-collection" onClick={openAdd} title="Ctrl+N" aria-label={t("newCollection")}>
              <Icon name="plus" size={23} /><span>{t("newCollection")}</span><span className="new-shortcut">↗</span>
            </button>
          </section>

          <form className="link-entry" onSubmit={e => { e.preventDefault(); openAdd(); }}>
            <span className="link-entry-icon"><Icon name="link" size={19} /></span>
            <input aria-label={t("quickLinkLabel")} value={draftUrl} onChange={e => setDraftUrl(e.target.value)} placeholder={t("quickLinkPlaceholder")} />
            <span className="link-entry-hint">{t("quickLinkHint")}</span>
            <button className="link-submit" type="submit" aria-label={t("newCollection")} title={t("newCollection")}><Icon name="arrow" size={20} /></button>
          </form>

          <div className="collection-section">
            <div className="collection-topline">
              <div className="collection-title"><h2>{t("myCollection")}</h2><span>{String(tasks.length).padStart(2, "0")}</span></div>
              <label className="search-box"><Icon name="search" size={16} /><input ref={searchRef} aria-label={t("search")} placeholder={t("searchCollection")} value={search} onChange={e => setSearch(e.target.value)} onKeyDown={e => { if (e.key === "Escape") setSearch(""); }} /><kbd>⌃ K</kbd></label>
            </div>

            <nav className="status-tabs" aria-label={t("sideStatus")}>
              {(["all", "active", "done", "other", "failed"] as Filter[]).map(key => (
                <button key={key} className={filter === key ? "status-tab active" : "status-tab"} onClick={() => setFilter(key)} aria-pressed={filter === key}>
                  {key === "active" && <span className="tab-live-dot" />}{t(key)}<span>{counts[key]}</span>
                </button>
              ))}
            </nav>

            <div className="collection-tools">
              <div className="category-filters" role="group" aria-label={t("sideCategory")}>
                {CATEGORIES.map(({key, label}) => <button key={key} className={catFilter === key ? "category-chip active" : "category-chip"} aria-pressed={catFilter === key} onClick={() => setCatFilter(key)}>{t(label)}</button>)}
              </div>
              <label className="sort-control"><span className="sr-only">{t("sortCollection")}</span><select value={sortKey ? sortKey + ":" + sortDir : "default"} onChange={e => {
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

            {clipUrl && <div className="banner clip" role="status">
              <Icon name="link" size={16} /><div className="clip-info"><b>{t("clipboardFound")}</b><span className="clip-url">{clipUrl}</span></div>
              <button onClick={addFromClip} disabled={clipBusy}>{t("download")}{clipBusy ? "…" : ""}</button><button onClick={() => setClipUrl(null)}>{t("ignore")}</button>
            </div>}

            {upd.status === "available" || upd.status === "installing" || upd.status === "ready" || upd.status === "error" ? (
              <div className="banner update" role="status">
                <Icon name="arrow" size={16} />
                <div className="clip-info">
                  <b>
                    {upd.status === "available"
                      ? t("updateAvailableTo").replace("{v}", upd.version ?? "")
                      : upd.status === "installing"
                        ? t("updateInstalling")
                        : upd.status === "ready"
                          ? t("updateReady")
                          : t("updateFailed")}
                  </b>
                  {upd.status === "installing" && (
                    <span className="clip-url">
                      {upd.total != null
                        ? `${Math.min(100, Math.round((upd.downloaded / upd.total) * 100))}% · ${fmtBytes(upd.downloaded)} / ${fmtBytes(upd.total)}`
                        : fmtBytes(upd.downloaded)}
                    </span>
                  )}
                  {upd.status === "error" && upd.error && <span className="clip-url">{upd.error}</span>}
                </div>
                {upd.status === "available" && <button onClick={() => installUpdate()}>{t("updateDownload")}</button>}
                {upd.status === "ready" && <button onClick={restartToUpdate}>{t("updateRestart")}</button>}
                {upd.status !== "installing" && <button onClick={dismissUpdate}>{t("ignore")}</button>}
              </div>
            ) : null}

            {selected.size > 0 ? (
              <div className="batchbar" role="toolbar" aria-label={t("selectedN").replace("{n}", String(selected.size))}>
                <span className="sel-count">{t("selectedN").replace("{n}", String(selected.size))}</span>
                <button onClick={selStart} disabled={busyOp !== null}>{t("start")}</button>
                <button onClick={selPause} disabled={busyOp !== null}>{t("pause")}</button>
                <button onClick={selRetry} disabled={busyOp !== null}>{t("retry")}</button>
                <button onClick={selCancel} disabled={busyOp !== null}>{t("cancel")}</button>
                <button className="danger" onClick={selRemove} disabled={busyOp !== null}>{t("remove")}</button>
                <div className="spacer" /><button className="icon-button" aria-label={t("clearSel")} title={t("clearSel")} onClick={() => setSelected(new Set())} disabled={busyOp !== null}><Icon name="close" size={15} /></button>
              </div>
            ) : (
              <div className="list-caption">
                <label className="selection-label"><input ref={headerCbRef} type="checkbox" className="sel-cb" checked={allSelected} onChange={toggleAll} aria-label={t("selectAll")} /><span>{t("collectionList")}</span></label>
                <div className="spacer" />
                {filter === "failed" && counts.failed > 0 && <button className="text-button" onClick={retryAllFailed} disabled={busyOp !== null}>{t("retryAllFailed")}</button>}
                {filter === "done" && counts.done > 0 && <button className="text-button" onClick={clearDone} disabled={busyOp !== null}>{t("clearDone")}</button>}
                <span>{filtered.length} {t("itemsUnit")}</span>
              </div>
            )}

            <div className="rows" ref={rowsRef} onScroll={e => setScrollTop(e.currentTarget.scrollTop)}>
              {showSkeleton ? (
                <div aria-hidden="true">{[56, 38, 46].map((w, i) => <div className="skel-row" key={i}><span className="skel-file" /><div className="skel" style={{width: w + "%"}} /></div>)}</div>
              ) : inTauri && firstLoad ? null : filtered.length === 0 ? (
                <div className="empty">
                  {search.trim() ? <><Icon name="search" size={35} /><h3>{t("emptySearch")}</h3><button onClick={() => setSearch("")}>{t("clearSearch")}</button></>
                  : tasks.length === 0 ? <><EmptyCollection /><span className="empty-number">YOUR COLLECTION STARTS HERE</span><h3>{t("emptyCollectionTitle")}</h3><p>{t("emptyCollectionDescription")}</p><button className="text-button empty-action" onClick={openAdd}>{t("addFirstCollection")}<Icon name="arrow" size={16} /></button></>
                  : <><Icon name="folder" size={35} /><h3>{t("emptyFiltered")}</h3><button onClick={() => { setFilter("all"); setCatFilter("all"); }}>{t("clearFilter")}</button></>}
                </div>
              ) : (
                <div style={{height: filtered.length * ROW_H, position: "relative"}}>
                  <div style={{transform: `translateY(${start * ROW_H}px)`}}>
                    {visible.map((task, vi) => <TaskRow key={task.id} task={task} onDetail={() => setDetailId(task.id)} selected={selected.has(task.id)} onToggleSelect={shift => toggleSelect(task, start + vi, shift)} />)}
                  </div>
                </div>
              )}
            </div>
          </div>
        </main>
        <TransferPanel tasks={tasks} bps={totalBps} active={counts.active} paused={pausedCount} inTauri={inTauri} busy={busyOp !== null} onPause={pauseAll} onResume={resumeAll} category={catFilter} onCategory={setCatFilter} />
      </div>

      <footer className="app-footer"><span className="footer-brand">ANYTHING, WELL KEPT.</span><span>{t("footerNote")}</span><span className="footer-mode">{!inTauri && <span className="preview-indicator" />}{t(inTauri ? "desktopMode" : "previewMode")}</span></footer>

      <div className="toast-stack">{toasts.map(x => <div key={x.id} className={`banner toast ${x.kind}`} role={x.kind === "error" ? "alert" : "status"}><span className="banner-text">{x.text}</span><button className="banner-close" aria-label={t("close")} onClick={() => dismissToast(x.id)}>×</button></div>)}</div>
      {showAdd && <AddDialog initialUrls={draftUrl} desktopAvailable={inTauri} onClose={() => setShowAdd(false)} onAdded={() => { setShowAdd(false); setDraftUrl(""); store.refresh(); }} />}
      {showSettings && <SettingsDialog onClose={() => setShowSettings(false)} />}
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
