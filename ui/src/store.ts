// 任务数据仓：以列表刷新为基线，引擎事件做增量合并（避免 10Hz 全表重渲染）。
import { EngineEvent, isActive, Task, TaskStatus, api } from "./api";

type Listener = () => void;

export interface SegInfo {
  start: number;
  written: number;
  end: number;
}

export interface Live {
  downloaded: number;
  bps: number;
  segs: SegInfo[] | null;
  /** 上次分段快照时间戳（ms），供速度增量计算 */
  segsT?: number;
  /** 各分段实时速度（B/s，由相邻快照增量计算），与 segs 一一对应 */
  segSpeeds: number[] | null;
  /** 最近一次收到进度或状态刷新的时间戳（ms），用于衰减卡顿/陈旧速率 */
  updatedAt: number;
}

class TaskStore {
  private tasks = new Map<string, Task>();
  private live = new Map<string, Live>();
  private listeners = new Set<Listener>();
  private snapshot: Task[] = [];
  private totalBps = 0;

  subscribe = (l: Listener) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  getSnapshot = (): Task[] => this.snapshot;

  getTotalBps = (): number => this.totalBps;

  liveOf = (id: string): Live | undefined => this.live.get(id);

  private recomputeTotalBps() {
    let sum = 0;
    for (const t of this.tasks.values()) {
      if (t.status === "downloading") {
        sum += this.live.get(t.id)?.bps ?? 0;
      }
    }
    this.totalBps = sum;
  }

  private emit(rebuild = true) {
    // 排序：活跃 > 排队(按 priority DESC, queue_order ASC) > 暂停/重试 > 终态
    if (rebuild) {
      const rank = (t: Task) =>
        isActive(t.status)
          ? 0
          : t.status === "queued"
            ? 1
            : t.status === "paused" || t.status === "waiting_retry"
              ? 2
              : 3;
      this.snapshot = [...this.tasks.values()].sort((a, b) => {
        const dr = rank(a) - rank(b);
        if (dr !== 0) return dr;
        if (rank(a) === 1) {
          const dp = b.priority - a.priority;
          if (dp !== 0) return dp;
          return (a.queue_order || a.created_at) - (b.queue_order || b.created_at);
        }
        return b.created_at - a.created_at;
      });
    }
    this.listeners.forEach((l) => l());
  }

  private lastSync = 0;
  private everFull = false;

  /// 全量（首次/手动刷新）；此后轮询走增量（updated_at > lastSync）
  async refresh() {
    const list = await api.listTasks();
    this.lastSync = Date.now();
    this.tasks = new Map(list.map((t) => [t.id, t]));
    for (const id of [...this.live.keys()]) {
      const t = this.tasks.get(id);
      if (!t || !isActive(t.status)) this.live.delete(id);
    }
    this.everFull = true;
    this.recomputeTotalBps();
    this.emit();
  }

  /// 增量轮询：仅拉取 updated_at > lastSync 的行合并进本地
  async pollIncremental() {
    if (!this.everFull) return this.refresh();
    try {
      const rows = await api.tasksUpdatedSince(this.lastSync - 50);
      for (const t of rows) {
        this.tasks.set(t.id, t);
        if (!isActive(t.status)) this.live.delete(t.id);
      }
      if (rows.length > 0) {
        this.lastSync = rows.reduce((m, x) => Math.max(m, x.updated_at), this.lastSync);
        this.recomputeTotalBps();
        this.emit();
      }
    } catch {
      // 后端不可达时保持静默，下轮再试
    }
  }

  upsert(t: Task) {
    this.tasks.set(t.id, t);
    if (!isActive(t.status)) this.live.delete(t.id);
    this.recomputeTotalBps();
    this.emit();
  }

  pruneStaleSpeeds(maxAgeMs = 3500) {
    const now = Date.now();
    let changed = false;
    for (const [id, cur] of this.live.entries()) {
      if (cur.bps > 0 && now - cur.updatedAt > maxAgeMs) {
        this.live.set(id, { ...cur, bps: 0, segSpeeds: null });
        changed = true;
      }
    }
    if (changed) {
      this.recomputeTotalBps();
      this.emit(false);
    }
  }

  applyEvent(ev: EngineEvent) {
    if (ev.type === "progress") {
      let t = this.tasks.get(ev.task_id);
      if (!t) return; // 不在列表（如刚清除）→ 忽略，等 refresh
      const metadataChanged = ev.total != null && t.total_bytes !== ev.total;
      if (ev.total != null && t.total_bytes !== ev.total) {
        t = { ...t, total_bytes: ev.total };
        this.tasks.set(ev.task_id, t);
      }
      const prev = this.live.get(ev.task_id);
      const now = Date.now();
      // 分段速度：按段起点 start 匹配相邻两次快照的 written 增量，避免动态分裂后索引错位
      let segSpeeds: number[] | null = null;
      if (ev.segments && prev?.segs && prev.segsT) {
        const dt = (now - prev.segsT) / 1000;
        if (dt > 0.2) {
          const prevByStart = new Map(prev.segs.map((p) => [p.start, p]));
          segSpeeds = ev.segments.map((sg) => {
            const p = prevByStart.get(sg.start);
            return p && sg.written >= p.written
              ? Math.max(0, Math.round((sg.written - p.written) / dt))
              : 0;
          });
        }
      }
      this.live.set(ev.task_id, {
        downloaded: ev.downloaded,
        bps: ev.bps,
        // 分段快照：事件缺失时保留上次值（HLS/单连接恒为 null）
        segs: ev.segments ?? prev?.segs ?? null,
        segsT: now,
        segSpeeds: segSpeeds ?? prev?.segSpeeds ?? null,
        updatedAt: now,
      });
      this.recomputeTotalBps();
      // 进度只更新行订阅。保持列表快照引用稳定，避免每次事件重排全量任务。
      this.emit(metadataChanged);
    } else {
      const t = this.tasks.get(ev.task_id);
      if (t) {
        const nextStatus = ev.status as TaskStatus;
        this.tasks.set(ev.task_id, {
          ...t,
          status: nextStatus,
          downloaded_bytes: this.live.get(ev.task_id)?.downloaded ?? t.downloaded_bytes,
        });
        if (!isActive(nextStatus)) {
          this.live.delete(ev.task_id);
        } else if (nextStatus !== "downloading") {
          const prev = this.live.get(ev.task_id);
          if (prev) {
            this.live.set(ev.task_id, {
              ...prev,
              bps: 0,
              segSpeeds: null,
              updatedAt: Date.now(),
            });
          }
        }
      }
      this.recomputeTotalBps();
      this.emit();
    }
  }
}

export const store = new TaskStore();
