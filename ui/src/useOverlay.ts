import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), summary, [tabindex]:not([tabindex="-1"])';

/**
 * 浮层键盘基线（规范 §4.2 / A8）：
 * - Esc 关闭（onEsc 由调用方给，可带 dirty 确认）；
 * - Tab / Shift+Tab 焦点循环困于浮层内；
 * - 关闭（卸载）后焦点归还打开前的触发元素；
 * - 打开时焦点落首交互元素（§3.6 标准；AddDialog 的 autoFocus 字段已就位时不抢占）。
 * 用回调 ref 而非 useRef：SettingsDialog 载入期间返回 null，modal 挂载晚于组件，
 * 回调 ref 保证「层内 DOM 真正出现」时才执行焦点逻辑。
 */
export function useOverlay(onEsc: () => void) {
  // 在 textarea 的 autoFocus 执行前记录打开者，关闭后不会把焦点还给已卸载的输入框。
  const openerRef = useRef(document.activeElement as HTMLElement | null);
  const [node, setNode] = useState<HTMLDivElement | null>(null);
  const onEscRef = useRef(onEsc);
  useEffect(() => {
    onEscRef.current = onEsc;
  });

  useEffect(() => {
    if (!node) return;
    // 此刻焦点仍在触发元素上（层内尚未聚焦），记下以便归还
    const prev = openerRef.current;
    if (!node.contains(document.activeElement)) {
      const first = node.querySelector<HTMLElement>(FOCUSABLE);
      first?.focus();
    }
    return () => {
      if (prev?.isConnected) prev.focus({ preventScroll: true });
    };
  }, [node]);

  const onKeyDown = (e: ReactKeyboardEvent) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      onEscRef.current();
      return;
    }
    if (e.key !== "Tab") return;
    if (!node) return;
    const els = Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE))
      .filter(el => el.getClientRects().length > 0 && getComputedStyle(el).visibility !== "hidden");
    if (els.length === 0) return;
    const first = els[0];
    const last = els[els.length - 1];
    const cur = document.activeElement;
    const inside = cur instanceof HTMLElement && node.contains(cur) && cur !== node;
    if (e.shiftKey) {
      if (!inside || cur === first) {
        e.preventDefault();
        last.focus();
      }
    } else if (!inside || cur === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return { ref: setNode, onKeyDown };
}
