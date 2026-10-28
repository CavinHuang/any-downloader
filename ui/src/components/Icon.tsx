import type { CSSProperties } from "react";

export type IconName = "plus" | "arrow" | "download" | "search" | "settings" | "import" | "export" | "grid" | "video" | "audio" | "archive" | "doc" | "software" | "image" | "other" | "pause" | "play" | "check" | "retry" | "close" | "folder" | "more" | "link" | "radar" | "clock" | "alert";

const paths: Record<IconName, React.ReactNode> = {
  plus: <path d="M12 5v14M5 12h14" />,
  clock: <><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></>,
  alert: <><circle cx="12" cy="12" r="9" /><path d="M12 7v6m0 4h.01" /></>,
  arrow: <path d="M5 12h14m-6-6 6 6-6 6" />,
  download: <><path d="M12 3v12m-5-5 5 5 5-5M4 16v4h16v-4" /></>,
  search: <><circle cx="10.5" cy="10.5" r="6.5" /><path d="m16 16 4 4" /></>,
  settings: <><path d="M4 7h16M4 17h16" /><circle cx="9" cy="7" r="3" /><circle cx="15" cy="17" r="3" /></>,
  import: <><path d="M12 3v12m-4-4 4 4 4-4M4 16v5h16v-5" /></>,
  export: <><path d="M12 15V3m-4 4 4-4 4 4M4 16v5h16v-5" /></>,
  grid: <><rect x="4" y="4" width="6" height="6" rx="1" /><rect x="14" y="4" width="6" height="6" rx="1" /><rect x="4" y="14" width="6" height="6" rx="1" /><rect x="14" y="14" width="6" height="6" rx="1" /></>,
  video: <><rect x="3" y="5" width="18" height="14" rx="2" /><path d="m10 9 5 3-5 3z" /></>,
  audio: <><path d="M9 17V5l11-2v12M9 8l11-2" /><ellipse cx="6" cy="17" rx="3" ry="3" /><ellipse cx="17" cy="15" rx="3" ry="3" /></>,
  archive: <><rect x="4" y="3" width="16" height="18" rx="2" /><path d="M11 3v2h2v2h-2v2h2v2h-2v3m0 0h3v4h-3z" /></>,
  doc: <><path d="M14 3H5v18h14V8zM14 3v5h5M8 12h8M8 16h6" /></>,
  software: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M3 9h18m-12 4-3 2 3 2m6-4 3 2-3 2" /></>,
  image: <><rect x="3" y="3" width="18" height="18" rx="2" /><circle cx="8" cy="8" r="1.5" /><path d="m3 17 6-5 4 4 3-3 5 5" /></>,
  other: <><path d="M14 3H5v18h14V8zM14 3v5h5" /><path d="M8 13h8m-8 4h5" /></>,
  pause: <><path d="M8 5v14M16 5v14" /></>,
  play: <path d="m8 4 12 8-12 8z" />,
  check: <path d="m5 12 4 4L19 6" />,
  retry: <><path d="M4 10a8 8 0 1 1 1 8M4 4v6h6" /></>,
  close: <path d="m6 6 12 12M6 18 18 6" />,
  folder: <path d="M3 7V4h7l2 3h9v13H3zM3 10h18" />,
  more: <><circle cx="5" cy="12" r="1" /><circle cx="12" cy="12" r="1" /><circle cx="19" cy="12" r="1" /></>,
  link: <><path d="m10 7 2-2a5 5 0 0 1 7 7l-2 2m-3 3-2 2a5 5 0 0 1-7-7l2-2m2 5 6-6" /></>,
  radar: <><circle cx="12" cy="12" r="2.2" /><path d="M12 6.5a5.5 5.5 0 0 1 5.5 5.5M12 2.5a9.5 9.5 0 0 1 9.5 9.5" /><path d="M12 6.5A5.5 5.5 0 0 0 6.5 12M12 2.5A9.5 9.5 0 0 0 2.5 12" /></>,
};

export function Icon({ name, size = 18, style }: { name: IconName; size?: number; style?: CSSProperties }) {
  return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={style}>{paths[name]}</svg>;
}
