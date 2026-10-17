// 自动更新仓：tauri-plugin-updater + GitHub Releases（latest.json）分发。
// 状态机：idle → checking → upToDate | available → installing → ready（relaunch 生效）。
// 启动静默检查一次（失败不打扰）；设置页可手动检查；发现新版本主界面挂横幅。
// 非 Tauri 环境（界面预览）全部静默。

import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

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
    // 静默检查（启动）失败回到 idle；手动检查展示原因
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

/** 关闭横幅（安装包已缓存/忽略本次，不回滚已完成的安装） */
export function dismissUpdate() {
  if (busy) return;
  set({ status: "idle", error: null });
}

/** 启动静默检查：延迟避开首屏，失败不打扰 */
export function scheduleStartupCheck(delayMs = 4000) {
  if (!("__TAURI_INTERNALS__" in window)) return;
  setTimeout(() => checkForUpdate(true), delayMs);
}
