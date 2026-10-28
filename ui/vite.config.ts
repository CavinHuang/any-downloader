import react from "@vitejs/plugin-react";
import { resolve } from "node:path";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: {
    outDir: "dist",
    target: "chrome110",
    // 双入口：主界面 + 微信视频窗口的悬浮下载图标（sniff-overlay 窗口）
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        overlay: resolve(__dirname, "sniff-overlay.html"),
      },
    },
  },
});
