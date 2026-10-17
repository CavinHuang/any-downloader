import { readFile, mkdir, writeFile } from "node:fs/promises";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

// Bundle the built desktop UI into a file that opens without a local server.
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const dist = join(root, "ui", "dist");
const output = join(root, ".smoke", "anything-studio-preview.html");
let html = await readFile(join(dist, "index.html"), "utf8").catch(() => {
  throw new Error("Build the UI first: npm --prefix ui run build");
});

function assetPath(url) {
  const path = resolve(dist, url.replace(/^\//, ""));
  if (!path.startsWith(dist + sep)) throw new Error("Asset is outside the UI build.");
  return path;
}

const scripts = [...html.matchAll(/<script\b[^>]*\bsrc="([^"]+)"[^>]*><\/script>/g)];
for (const [tag, url] of scripts) {
  const code = (await readFile(assetPath(url), "utf8")).replace(/<\/script/gi, "<\\/script");
  html = html.replace(tag, () => `<script type="module">${code}</script>`);
}
const styles = [...html.matchAll(/<link\b[^>]*\brel="stylesheet"[^>]*\bhref="([^"]+)"[^>]*>/g)];
for (const [tag, url] of styles) {
  const css = (await readFile(assetPath(url), "utf8")).replace(/<\/style/gi, "<\\/style");
  html = html.replace(tag, () => `<style>${css}</style>`);
}
if (!scripts.length || !styles.length) throw new Error("Expected a JavaScript and CSS bundle in the UI build.");
await mkdir(dirname(output), { recursive: true });
await writeFile(output, html);
console.log(`Offline preview: ${output}`);
