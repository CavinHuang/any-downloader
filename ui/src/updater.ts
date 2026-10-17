// 自动更新仓：tauri-plugin-updater + GitHub Releases（latest.json）分发。
// 状态机：idle → checking → upToDate | available → installing → ready（relaunch 生效）。
// 自动链路（设置 update_auto 可关，默认开）：启动后静默检查一次，此后每 6 小时
// 再查；发现新版本自动后台下载安装。提示形态：主界面设置图标出现角标，悬停
// 显示状态 tooltip；完整进度与「立即重启」在设置 → 05 / 关于与更新。
// 非 Tauri 环境（界面预览）全部静默。

import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { api } from "./api";

export type UpdateStatus =
  | "idle" // 未检查 / 已忽略
  | "checking" // 检查中
  | "upToDate" // 已是最新
  | "available" // 有新版本可下载
  | "installing" // 下载安装中（含进度）
  | "ready" // 安装完成，待重启
  | "error"; // 失败（带原因）

export interface UpdateState {
  status: UpdateStatus;
  /** 新版本号（available/installing/ready 阶段有值） */
  version: string | null;
  downloaded: number;
  /** 安装包总字节（服务端未给 Content-Length 时为 null） */
  total: number | null;
  error: string | null;
}

let state: UpdateState = {
  status: "idle",
  version: null,
  downloaded: 0,
  total: null,
  error: null,
};

const listeners = new Set<() => void>();

function set(patch: Partial<UpdateState>) {
  state = { ...state, ...patch };
  listeners.forEach((l) => l());
}

export const subscribeUpdate = (l: () => void) => {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
};

export const getUpdateState = (): UpdateState => state;

let current: Update | null = null;
let busy = false; // 串行化：检查/安装进行中忽略重复触发

export async function checkForUpdate(silent = false): Promise<void> {
  if (busy || !("__TAURI_INTERNALS__" in window)) return;
  busy = true;
  current = null;
  set({ status: "checking", error: null });
  try {
    const update = await check();
    current = update;
    if (update) {
      set({
        status: "available",
        version: update.version,
        downloaded: 0,
        total: null,
      });
    } else {
      set({ status: "upToDate", version: null });
    }
  } catch (e) {
    // 静默检查（启动/周期）失败回到 idle；手动检查展示原因
    set(silent ? { status: "idle" } : { status: "error", error: String(e) });
  } finally {
    busy = false;
  }
}

export async function installUpdate(): Promise<void> {
  if (busy || !current) return;
  busy = true;
  set({ status: "installing", downloaded: 0, total: null, error: null });
  try {
    await current.downloadAndInstall((event) => {
      if (event.event === "Started") {
        set({ total: event.data.contentLength ?? null });
      } else if (event.event === "Progress") {
        set({ downloaded: state.downloaded + event.data.chunkLength });
      }
    });
    set({ status: "ready" });
  } catch (e) {
    set({ status: "error", error: String(e) });
  } finally {
    busy = false;
  }
}

export function restartToUpdate() {
  relaunch().catch((e) => set({ status: "error", error: String(e) }));
}

/** 设置 update_auto（默认开）：自动检查 + 后台下载；关闭后仅手动检查/下载 */
async function autoUpdateEnabled(): Promise<boolean> {
  try {
    const s = await api.getSettings();
    return s.update_auto !== "off";
  } catch {
    return true;
  }
}

async function autoTick() {
  if (busy || !(await autoUpdateEnabled())) return;
  await checkForUpdate(true);
  if (getUpdateState().status === "available") void installUpdate();
}

/** 运行期自动检查：启动后延迟首查，此后每 6 小时一轮（应用常驻托盘） */
export function scheduleAutoChecks(delayMs = 4000) {
  if (!("__TAURI_INTERNALS__" in window)) return;
  const SIX_HOURS = 6 * 60 * 60 * 1000;
  setTimeout(autoTick, delayMs);
  setInterval(autoTick, SIX_HOURS);
}
