// 粘贴与快捷输入框的 URL 提取和可下载意图分类（与后端 site_adapt.rs 对齐）。

const DOWNLOADABLE_EXT_RE =
  /\.(?:mp4|mkv|webm|mov|avi|flv|m4v|ts|m3u8|m3u|mpd|wmv|rmvb|mp3|flac|m4a|aac|ogg|wav|opus|ape|wma|gguf|safetensors|onnx|ckpt|pt|pth|bin|model|parquet|h5|pb|tflite|whl|zip|rar|7z|tar|gz|tgz|bz2|xz|zst|lz4|cab|exe|msi|msix|dmg|pkg|apk|xapk|ipa|iso|img|deb|rpm|appimage|flatpak|snap|pdf|epub|mobi|azw3|docx?|xlsx?|pptx?)$/i;

const WEBPAGE_EXT_RE =
  /\.(?:html?|xhtml|php|aspx?|jsp|css|mjs|js|map|svg|ico|woff2?|ttf|eot)$/i;

const GITHUB_DIRECT_HOSTS = new Set([
  "api.github.com",
  "codeload.github.com",
  "gist.github.com",
  "gist.githubusercontent.com",
  "github-releases.githubusercontent.com",
  "media.githubusercontent.com",
  "objects.githubusercontent.com",
  "raw.githubusercontent.com",
  "raw.github.com",
  "release-assets.githubusercontent.com",
]);

const YTDLP_ID_RE = /^[A-Za-z0-9_-]{11}$/;

/** 从任意粘贴文本（含抖音/B站中文分享文案）中提取候选下载链接，按序去重。 */
export function extractLinksFromText(text: string): string[] {
  const re = /(?:https?|thunder|flashget|qqdl):\/\/[^\s"'<>[\]{}\u4e00-\u9fff]+/gi;
  const seen = new Set<string>();
  const out: string[] = [];
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const cleaned = m[0].replace(/[,.;:!?。，！？)]+$/g, "");
    if (!cleaned || seen.has(cleaned)) continue;
    seen.add(cleaned);
    out.push(cleaned);
  }
  return out;
}

export type InputUrlClass = "direct" | "inspect" | "ambiguous";

export function classifyInputUrl(raw: string): InputUrlClass {
  const trimmed = raw.trim();
  if (/^(?:thunder|flashget|qqdl):\/\//i.test(trimmed)) {
    return "direct";
  }
  let u: URL;
  try {
    u = new URL(trimmed);
  } catch {
    return "ambiguous";
  }
  const bare = u.hostname.toLowerCase().replace(/^www\./, "");
  const segs = u.pathname.split("/").filter(Boolean);
  const lp = u.pathname.toLowerCase();

  // Hugging Face
  if (bare === "huggingface.co" || bare === "hf-mirror.com") {
    const idx = segs[0] === "datasets" || segs[0] === "spaces" ? 3 : 2;
    if (segs.length >= idx + 3 && (segs[idx] === "resolve" || segs[idx] === "blob")) {
      return "direct";
    }
    if (segs.length >= (segs[0] === "datasets" || segs[0] === "spaces" ? 3 : 2)) {
      return "inspect";
    }
  }

  // ModelScope
  if (bare === "modelscope.cn" && (segs[0] === "models" || segs[0] === "datasets") && segs.length >= 3) {
    if ((segs[3] === "file" && segs[4] === "view" && segs.length >= 7) || (segs[3] === "resolve" && segs.length >= 6)) {
      return "direct";
    }
    return "inspect";
  }

  // GitHub Releases & Direct Assets
  if (bare === "github.com") {
    if (/\/releases(?:\/(?:latest|tag\/[^/]+))?\/?$/i.test(u.pathname)) {
      return "inspect";
    }
    if (
      lp.includes("/releases/download/") ||
      lp.includes("/releases/latest/download/") ||
      lp.includes("/archive/") ||
      lp.includes("/raw/")
    ) {
      return "direct";
    }
    if (lp.includes("/blob/") && DOWNLOADABLE_EXT_RE.test(decodePathLast(u.pathname))) {
      return "direct";
    }
  }
  if (GITHUB_DIRECT_HOSTS.has(bare)) {
    return "direct";
  }

  // Bilibili
  if (bare === "b23.tv" || bare.endsWith(".b23.tv")) {
    return "inspect";
  }
  if (bare === "bilibili.com" || bare.endsWith(".bilibili.com")) {
    if (
      lp.includes("/video/bv") ||
      lp.includes("/video/av") ||
      lp.includes("/lists/") ||
      lp.includes("/favlist") ||
      lp.includes("/medialist/") ||
      lp.startsWith("/list/ml") ||
      lp.includes("/channel/collectiondetail")
    ) {
      return "inspect";
    }
  }

  // YouTube / X / TikTok / Douyin works
  if (bare === "youtu.be" && segs[0] && YTDLP_ID_RE.test(segs[0])) {
    return "inspect";
  }
  if (bare === "youtube.com" || bare.endsWith(".youtube.com") || bare.endsWith("youtube-nocookie.com")) {
    if (
      YTDLP_ID_RE.test(u.searchParams.get("v") || "") ||
      !!u.searchParams.get("list") ||
      lp === "/playlist" ||
      (["shorts", "live", "embed", "v"].includes(segs[0]) && YTDLP_ID_RE.test(segs[1] || ""))
    ) {
      return "inspect";
    }
  }
  if ((bare === "x.com" || bare.endsWith(".x.com") || bare === "twitter.com" || bare.endsWith(".twitter.com")) &&
      segs.some((s, i) => (s === "status" || s === "statuses") && /^\d+$/.test(segs[i + 1] || ""))) {
    return "inspect";
  }
  if (((bare === "vt.tiktok.com" || bare === "vm.tiktok.com") && segs.length > 0) ||
      ((bare === "tiktok.com" || bare.endsWith(".tiktok.com")) &&
        segs.some((s, i) => (s === "video" || s === "photo") && /^\d+$/.test(segs[i + 1] || "")))) {
    return "inspect";
  }
  if ((bare === "v.douyin.com" && segs.length > 0) ||
      ((bare === "douyin.com" || bare.endsWith(".douyin.com") || bare === "iesdouyin.com") &&
        segs.some((s, i) => (s === "video" || s === "note") && /^\d+$/.test(segs[i + 1] || "")))) {
    return "inspect";
  }

  // Cloud / GitLab / Gitee / arXiv direct rules
  if (
    (bare === "dropbox.com" && (lp.startsWith("/s/") || lp.startsWith("/scl/fi/"))) ||
    (bare === "drive.google.com" && (lp.startsWith("/file/d/") || (lp === "/open" && !!u.searchParams.get("id")))) ||
    (bare === "arxiv.org" && lp.startsWith("/abs/"))
  ) {
    return "direct";
  }

  const last = decodePathLast(u.pathname);
  if (WEBPAGE_EXT_RE.test(last)) {
    return "ambiguous";
  }
  if (DOWNLOADABLE_EXT_RE.test(last)) {
    return "direct";
  }

  const hasQueryHint =
    ["filename", "file", "attname", "download_name"].some((k) => Boolean(u.searchParams.get(k))) ||
    (u.searchParams.get("response-content-disposition") || "").toLowerCase().includes("attachment");
  if (hasQueryHint) {
    return "direct";
  }

  return "ambiguous";
}

function decodePathLast(pathname: string): string {
  const seg = pathname.split("/").filter(Boolean).pop() || "";
  try {
    return decodeURIComponent(seg);
  } catch {
    return seg;
  }
}
