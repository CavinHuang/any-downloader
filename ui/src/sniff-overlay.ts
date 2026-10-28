// 悬浮下载图标逻辑（sniff-overlay 窗口，微信视频号宿主窗右上角）：
// - sniff-event 新条目 → .has 微光（提示「这条能下」）
// - 单击 → sniff_overlay_download（下载最近一条，含 isaac64-xor 解密）
//   成功闪 ✓、无条目提示「先播放一条视频」
// 窗口本体 40x40、不抢焦点（WS_EX_NOACTIVATE 语义，bootstrap 里 set_focusable(false)）。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const btn = document.getElementById("btn") as HTMLButtonElement;
const tip = document.getElementById("tip") as HTMLDivElement;
let busy = false;
let tipTimer = 0;

function showTip(text: string) {
  tip.textContent = text;
  tip.classList.add("on");
  clearTimeout(tipTimer);
  tipTimer = window.setTimeout(() => tip.classList.remove("on"), 1800);
}

function setState(cls: "has" | "ok" | "err" | null) {
  btn.classList.remove("has", "ok", "err");
  if (cls) btn.classList.add(cls);
}

listen("sniff-event", () => {
  setState("has");
}).catch(() => {});

btn.addEventListener("click", async () => {
  if (busy) return;
  busy = true;
  try {
    await invoke<{ ok: boolean }>("sniff_overlay_download");
    setState("ok");
    showTip("已创建下载任务");
  } catch (e) {
    setState("err");
    showTip(String(e).includes("no_item") ? "先播放一条视频" : "下载失败，请重试");
  } finally {
    busy = false;
    setTimeout(() => setState("has"), 1200);
  }
});
