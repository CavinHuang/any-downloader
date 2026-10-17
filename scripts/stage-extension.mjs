// 打包前暂存：extension/ → src-tauri/resources/extension/（排除本地测试 fixtures）。
// 由 tauri.conf.json 的 beforeBuildCommand 调用；resources 映射把它装进软件目录
// （Windows 安装目录 / macOS Contents/Resources / Linux AppImage 内），
// 桌面端「安装浏览器扩展」弹窗据此指引用户加载。
import { cpSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const dest = join(root, "src-tauri", "resources", "extension");
rmSync(dest, { recursive: true, force: true });
cpSync(join(root, "extension"), dest, {
  recursive: true,
  // fixtures（本地 node 测试样本）与 .key-info.txt（打包密钥信息）不进安装包
  filter: (src) =>
    !/(^|[\\/])fixtures([\\/]|$)/.test(src) && !src.endsWith(".key-info.txt"),
});
console.log("staged extension ->", dest);
