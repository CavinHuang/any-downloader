// Tauri IPC 封装：类型化命令 + 引擎事件订阅。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface Task {
  id: string;
  url: string;
  final_url: string | null;
  filename: string | null;
  dir: string | null;
  total_bytes: number | null;
  etag: string | null;
  last_modified: string | null;
  mime: string | null;
  kind: "http" | "hls" | "content";
  status: TaskStatus;
  priority: number;
  headers_ref: string | null;
  headers_json: string | null;
  downloaded_bytes: number;
  variants_json: string | null;
  error: Record<string, unknown> | null;
  created_at: number;
  updated_at: number;
}

export type TaskStatus =
  | "queued"
  | "probing"
  | "downloading"
  | "paused"
  | "waiting_retry"
  | "merging"
  | "verifying"
  | "decrypting"
  | "done"
  | "failed"
  | "canceled";

export interface ProbeInfo {
  filename: string | null;
  total_bytes: number | null;
  mime: string | null;
  segmented: boolean;
}

export interface SegInfo {
  start: number;
  written: number;
  end: number;
}

export type EngineEvent =
  | {
      type: "progress";
      task_id: string;
      downloaded: number;
      total: number | null;
      bps: number;
      segments: SegInfo[] | null;
    }
  | { type: "state_changed"; task_id: string; status: TaskStatus };

export interface Settings {
  default_dir: string | null;
  segments: string | null;
  task_bps: string | null;
  global_bps: string | null;
  proxy: string | null;
  conflict: string | null;
  after_done: string | null;
  clipboard_monitor: string | null;
  ffmpeg_path: string | null;
  yt_dlp_path: string | null;
  max_concurrent: string | null;
  max_auto_retry: string | null;
  auto_categorize: string | null;
  ytdlp_auto_update: string | null;
  schedule_enabled: string | null;
  schedule_window: string | null;
  schedule_outside: string | null;
  schedule_outside_bps: string | null;
  update_auto: string | null;
}

export interface RemoteStatus {
  enabled: boolean;
  port: number;
  urls: string[];
  qr: string | null;
}

const running = new Set([
  "probing",
  "downloading",
  "merging",
  "verifying",
  "decrypting",
]);

export const isActive = (s: TaskStatus) => running.has(s);

export const api = {
  listTasks: (status?: string) =>
    invoke<Task[]>("list_tasks", { status: status ?? null }),
  tasksUpdatedSince: (sinceMs: number) =>
    invoke<Task[]>("tasks_updated_since", { sinceMs }),
  getTask: (id: string) => invoke<Task | null>("get_task", { id }),
  addTask: (params: {
    url: string;
    dir?: string;
    filename?: string;
    headers?: Record<string, string>;
    checksum?: string;
    mirrors?: string[];
  }) => invoke<Task>("add_task", params),
  setPriority: (id: string, priority: number) =>
    invoke<Task>("set_priority", { id, priority }),
  removeTask: (id: string) => invoke<boolean>("remove_task", { id }),
  retryTask: (id: string) => invoke<Task>("retry_task", { id }),
  startDownload: (id: string) => invoke<void>("start_download", { id }),
  pauseTask: (id: string) => invoke<Task>("pause_task", { id }),
  cancelTask: (id: string) => invoke<Task>("cancel_task", { id }),
  getSettings: () => invoke<Settings>("get_settings"),
  setSetting: (key: string, value: string) =>
    invoke<void>("set_setting", { key, value }),
  defaultDownloadDir: () => invoke<string>("default_download_dir"),
  revealTask: (id: string) => invoke<void>("reveal_task", { id }),
  openTask: (id: string) => invoke<void>("open_task", { id }),
  getAutostart: () => invoke<boolean>("get_autostart"),
  setAutostart: (enable: boolean) => invoke<void>("set_autostart", { enable }),
  openLogsDir: () => invoke<void>("open_logs_dir"),
  nmRegistered: () => invoke<boolean>("nm_registered"),
  extensionBundle: () =>
    invoke<{ path: string | null; present: boolean }>("extension_bundle"),
  openExtensionDir: (path: string) =>
    invoke<void>("open_extension_dir", { path }),
  openExtensionDoc: () => invoke<void>("open_extension_doc"),
  renameTask: (id: string, filename: string) =>
    invoke<Task>("rename_task", { id, filename }),
  exportTasks: (path: string) => invoke<number>("export_tasks", { path }),
  importTasks: (path: string) => invoke<number>("import_tasks", { path }),
  probeUrl: (url: string) => invoke<ProbeInfo>("probe_url", { url }),
  remoteStatus: () => invoke<RemoteStatus>("remote_status"),
  setRemoteEnabled: (on: boolean) => invoke<RemoteStatus>("set_remote_enabled", { on }),
};

export function listenEngineEvents(
  cb: (ev: EngineEvent) => void
): Promise<() => void> {
  return listen<EngineEvent>("engine-event", (e) => cb(e.payload));
}
