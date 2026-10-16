// 轻量 toast 仓：顶部 banner 形态（§3.8，不做浮动 toast）。
// 四语义 info/success/warn/error；时长：成功 2.5s、错误/警告 5s，均可手动关。
// 供 App（渲染）、TaskRow（行级错误出口）、AddDialog（批量汇总）等共用，
// 避免经虚拟滚动列表层层透传回调。

export type ToastKind = "info" | "success" | "warn" | "error";

export interface ToastItem {
  id: number;
  kind: ToastKind;
  text: string;
}

const DURATION_MS: Record<ToastKind, number> = {
  info: 2500,
  success: 2500,
  warn: 5000,
  error: 5000,
};

const MAX_VISIBLE = 3;

let items: ToastItem[] = [];
let nextId = 1;
const listeners = new Set<() => void>();

function emit() {
  listeners.forEach((l) => l());
}

export function showToast(text: string, kind: ToastKind = "info") {
  const item: ToastItem = { id: nextId++, kind, text };
  items = [...items.slice(-(MAX_VISIBLE - 1)), item];
  emit();
  setTimeout(() => dismissToast(item.id), DURATION_MS[kind]);
}

export function dismissToast(id: number) {
  if (!items.some((i) => i.id === id)) return;
  items = items.filter((i) => i.id !== id);
  emit();
}

export const subscribeToasts = (l: () => void) => {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
};

export const getToasts = (): ToastItem[] => items;
