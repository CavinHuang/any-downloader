import type { Task } from "./api";
import type { IconName } from "./components/Icon";

export type Category = "all" | "video" | "audio" | "archive" | "doc" | "software" | "image" | "other";

export const CATEGORIES: { key: Category; icon: IconName; label: string }[] = [
  { key: "all", icon: "grid", label: "catAll" },
  { key: "video", icon: "video", label: "catVideo" },
  { key: "audio", icon: "audio", label: "catAudio" },
  { key: "image", icon: "image", label: "catImage" },
  { key: "doc", icon: "doc", label: "catDoc" },
  { key: "archive", icon: "archive", label: "catArchive" },
  { key: "software", icon: "software", label: "catSoftware" },
  { key: "other", icon: "other", label: "catOther" },
];

const RULES: [RegExp, Category][] = [
  [/\.(mp4|mkv|webm|avi|mov|wmv|flv|m4v|ts|m3u8|mpd)$/i, "video"],
  [/\.(mp3|flac|m4a|aac|ogg|wav|ape|opus)$/i, "audio"],
  [/\.(zip|rar|7z|tar|gz|bz2|xz)$/i, "archive"],
  [/\.(pdf|epub|docx?|xlsx?|pptx?|md|txt|csv|html?)$/i, "doc"],
  [/\.(exe|msi|apk|dmg|pkg|deb|appimage)$/i, "software"],
  [/\.(jpe?g|png|gif|webp|svg|avif|heic|bmp|tiff?)$/i, "image"],
];

export function categoryOf(task: Task): Category {
  if (task.kind === "hls") return "video";
  const name = task.filename || task.url.split("?")[0];
  return RULES.find(([rule]) => rule.test(name))?.[1] ?? "other";
}

export function sourceOf(url: string): string {
  try { return new URL(url).hostname.replace(/^www\./, ""); } catch { return url; }
}
