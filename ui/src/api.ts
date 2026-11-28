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
  kind: "http" | "hls" | "dash" | "ytdlp" | "content";
  status: TaskStatus;
  priority: number;
  queue_order?: number;
  headers_ref: string | null;
  headers_json: string | null;
  downloaded_bytes: number;
  variants_json: string | null;
  error: unknown;
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
  final_url?: string | null;
  resolved_from_interstitial?: boolean;
  is_interstitial?: boolean;
  interstitial_title?: string | null;
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
  proxy_mode: string | null;
  proxy: string | null;
  conflict: string | null;
  after_done: string | null;
  clipboard_monitor: string | null;
  ffmpeg_path: string | null;
  yt_dlp_path: string | null;
  max_concurrent: string | null;
  max_auto_retry: string | null;
  auto_speed_up?: string | null;
  auto_categorize: string | null;
  ytdlp_auto_update: string | null;
  schedule_enabled: string | null;
  schedule_window: string | null;
  schedule_outside: string | null;
  schedule_outside_bps: string | null;
  update_auto: string | null;
  run_command: string | null;
  yt_dlp_subs: string | null;
  yt_dlp_meta: string | null;
  yt_dlp_cookies: string | null;
  yt_dlp_quality: string | null;
  prevent_sleep: string | null;
  auto_resume_on_start: string | null;
  preserve_mtime: string | null;
  hf_mirror_enabled: string | null;
  hf_mirror_site: string | null;
  hf_token: string | null;
  github_mirror_enabled: string | null;
  github_mirror_site: string | null;
  github_mirror_custom: string | null;
}

export interface SiteFileItem {
  relative_path: string;
  filename: string;
  url: string;
  size: number | null;
  checksum: string | null;
  mirrors: string[];
  headers: Record<string, string>;
}

export interface MirrorLatency {
  site: string;
  group: string;
  latency_ms: number | null;
}

export type SiteInspectResult =
  | {
      kind: "files";
      site: string;
      title: string;
      subtitle: string | null;
      folder_name: string | null;
      files: SiteFileItem[];
    }
  | {
      kind: "playlist";
      site: string;
      title: string;
      total_count: number;
      entries: PlaylistEntry[];
    }
  | {
      kind: "fallback_ytdlp";
      url: string;
    };

export interface MediaFormatOption {
  format_id: string;
  label: string;
  ext: string;
  resolution: string | null;
  fps: number | null;
  filesize: number | null;
}

export interface PlaylistEntry {
  id: string;
  title: string;
  url: string;
  duration_sec: number | null;
}

export type MediaInspectResult =
  | {
      kind: "video";
      title: string;
      duration_sec: number | null;
      thumbnail: string | null;
      formats: MediaFormatOption[];
    }
  | {
      kind: "playlist";
      title: string;
      total_count: number;
      entries: PlaylistEntry[];
    };

export interface QueueStatus {
  paused: boolean;
  max_concurrent: number;
  active: number;
  queued: number;
}

export interface PageSniffItem {
  url: string;
  filename: string;
  kind: string;
  mime: string | null;
  bytes: number | null;
  source: string;
}

export interface BrowserSniffVariant {
  url: string;
  label?: string | null;
  bandwidth?: number | null;
  resolution?: string | null;
  height?: number | null;
}

export interface BrowserSniffItem {
  id: string;
  url: string;
  tab_url?: string | null;
  page_title?: string | null;
  filename?: string | null;
  kind: string;
  mime?: string | null;
  bytes?: number | null;
  duration_sec?: number | null;
  variants: BrowserSniffVariant[];
  headers: Record<string, string>;
  at: number;
}

export interface ExtensionBridgeStatus {
  running: boolean;
  port: number;
  hasToken: boolean;
  nmRegistered: boolean;
  lastSeenAt: number | null;
  items: BrowserSniffItem[];
}

export interface RemoteStatus {
  enabled: boolean;
  port: number;
  urls: string[];
  qr: string | null;
}

export interface IpInfo {
  ip: string;
  country: string | null;
  region: string | null;
  city: string | null;
  isp: string | null;
  asn: string | null;
  latency_ms: number;
  source: string;
}

export interface NetworkIpReport {
  proxy_mode: "system" | "custom" | "direct";
  effective_proxy: string | null;
  active: IpInfo | null;
  active_error: string | null;
  direct: IpInfo | null;
}

export interface SpeedProgress {
  phase: string;
  elapsed_ms: number;
  bytes: number;
  current_bps: number;
  avg_bps: number;
  peak_bps: number;
  latency_ms: number | null;
  jitter_ms: number | null;
  server: string;
}

export interface SpeedTestResult {
  server: string;
  latency_ms: number | null;
  jitter_ms: number | null;
  avg_bps: number;
  peak_bps: number;
  bytes: number;
  elapsed_ms: number;
  canceled: boolean;
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
    proxy?: string;
    format?: string;
  }) => invoke<Task>("add_task", params),
  setPriority: (id: string, priority: number) =>
    invoke<Task>("set_priority", { id, priority }),
  removeTask: (id: string) => invoke<boolean>("remove_task", { id }),
  retryTask: (id: string) => invoke<Task>("retry_task", { id }),
  startDownload: (id: string) => invoke<void>("start_download", { id }),
  pauseTask: (id: string) => invoke<Task>("pause_task", { id }),
  moveToQueue: (id: string) => invoke<Task>("move_to_queue", { id }),
  cancelTask: (id: string) => invoke<Task>("cancel_task", { id }),
  queueStatus: () => invoke<QueueStatus>("queue_status"),
  startQueue: () => invoke<number>("start_queue"),
  pauseQueue: () => invoke<number>("pause_queue"),
  reorderQueue: (id: string, direction: "top" | "up" | "down") =>
    invoke<void>("reorder_queue", { id, direction }),
  setMaxConcurrent: (value: number) =>
    invoke<QueueStatus>("set_max_concurrent", { value }),
  getSettings: () => invoke<Settings>("get_settings"),
  setSetting: (key: string, value: string) =>
    invoke<void>("set_setting", { key, value }),
  getSystemProxy: () => invoke<string | null>("get_system_proxy"),
  defaultDownloadDir: () => invoke<string>("default_download_dir"),
  revealTask: (id: string) => invoke<void>("reveal_task", { id }),
  openTask: (id: string) => invoke<void>("open_task", { id }),
  openExternalUrl: (url: string) => invoke<void>("open_external_url", { url }),
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
  updateTaskUrl: (id: string, url: string) =>
    invoke<Task>("update_task_url", { id, url }),
  computeTaskChecksum: (id: string, algo: "sha256" | "md5" | "sha1" | "sha512") =>
    invoke<string>("compute_task_checksum", { id, algo }),
  exportTasks: (path: string) => invoke<number>("export_tasks", { path }),
  importTasks: (path: string) => invoke<number>("import_tasks", { path }),
  probeUrl: (url: string) => invoke<ProbeInfo>("probe_url", { url }),
  inspectMediaUrl: (url: string) =>
    invoke<MediaInspectResult>("inspect_media_url", { url }),
  inspectSiteUrl: (url: string) =>
    invoke<SiteInspectResult>("inspect_site_url", { url }),
  probeSiteMirrors: () => invoke<MirrorLatency[]>("probe_site_mirrors"),
  searchContent: (query: string) =>
    invoke<{ id: string; filename: string | null }[]>("search_content", { query }),
  sniffPageUrl: (url: string) => invoke<PageSniffItem[]>("sniff_page_url", { url }),
  extensionBridgeStatus: () =>
    invoke<ExtensionBridgeStatus>("extension_bridge_status"),
  extensionBridgeClear: () => invoke<void>("extension_bridge_clear"),
  extensionBridgeDownload: (id: string, variantUrl?: string) =>
    invoke<Task>("extension_bridge_download", {
      id,
      variantUrl: variantUrl ?? null,
    }),
  remoteStatus: () => invoke<RemoteStatus>("remote_status"),
  setRemoteEnabled: (on: boolean) => invoke<RemoteStatus>("set_remote_enabled", { on }),
  detectNetworkIp: () => invoke<NetworkIpReport>("detect_network_ip"),
  runSpeedTest: (server?: string) =>
    invoke<SpeedTestResult>("run_speed_test", { server: server ?? "auto" }),
  cancelSpeedTest: () => invoke<void>("cancel_speed_test"),
  checkAppUpdate: () => invoke<{ version: string } | null>("check_app_update"),
  installAppUpdate: () => invoke<void>("install_app_update"),
};

export function listenEngineEvents(
  cb: (ev: EngineEvent) => void
): Promise<() => void> {
  return listen<EngineEvent>("engine-event", (e) => cb(e.payload));
}

export function listenBrowserSniffEvents(
  cb: (item: BrowserSniffItem) => void
): Promise<() => void> {
  return listen<BrowserSniffItem>("browser-sniff-event", (e) => cb(e.payload));
}

export function listenAppUpdateProgress(
  cb: (p: { downloaded: number; total: number | null }) => void
): Promise<() => void> {
  return listen<{ downloaded: number; total: number | null }>(
    "app-update-progress",
    (e) => cb(e.payload)
  );
}

export function listenSpeedTestProgress(
  cb: (p: SpeedProgress) => void
): Promise<() => void> {
  return listen<SpeedProgress>("net-speed-progress", (e) => cb(e.payload));
}
