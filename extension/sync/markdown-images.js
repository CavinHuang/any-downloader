// Markdown/HTML 图片解析与替换 —— 零依赖纯逻辑（文章同步图片链路）。
// parseMarkdownImages 逐行移植自 code-box sync/lib/markdown-images.ts:10-124
// （GPL-3.0，手写状态机：支持 <括号 URL>、反斜杠转义、可选标题、可选 title 的
//   "..." 与 (...) 两种形态）。
// processImages 移植自 code-box sync/adapters/code-adapter.ts:244-333：
//   HTML <img src> 与 Markdown ![](url) 双格式提取 → 逐张 uploadFn →
//   同 URL 去重（uploadedMap）→ 失败保留原 URL 不中断 → 每张间隔 300ms
//   （delayFn 可注入，测试传 noop）。
// 无 fetch、无 chrome.*、无 DOM；uploadFn/delayFn 均由调用方注入。
self.ADL = self.ADL || {};
(() => {
  /**
   * 解析 Markdown 图片语法（markdown-images.ts 状态机照抄）。
   * 返回 [{full, alt, src}]，full = 从 `![` 到 `)` 的原文。
   */
  function parseMarkdownImages(markdown) {
    const results = [];
    const md = String(markdown || "");
    const len = md.length;
    let i = 0;

    const findClosingBracket = (start) => {
      for (let j = start; j < len; j++) {
        const ch = md[j];
        if (ch === "\\") {
          j++;
          continue;
        }
        if (ch === "]") return j;
      }
      return -1;
    };

    while (i < len) {
      const start = md.indexOf("![", i);
      if (start === -1) break;

      const altStart = start + 2;
      const altEnd = findClosingBracket(altStart);
      if (altEnd === -1 || md[altEnd + 1] !== "(") {
        i = altStart;
        continue;
      }

      let k = altEnd + 2;
      while (k < len && /\s/.test(md[k])) k++;

      let url = "";
      if (md[k] === "<") {
        // <URL> 角括号形态：URL 可含未配对括号
        const close = md.indexOf(">", k + 1);
        if (close === -1) {
          i = altEnd + 1;
          continue;
        }
        url = md.slice(k + 1, close);
        k = close + 1;
      } else {
        // 普通形态：括号深度感知（URL 内允许成对括号），空白截断
        const urlStart = k;
        let depth = 0;
        while (k < len) {
          const ch = md[k];
          if (ch === "\\") {
            k += 2;
            continue;
          }
          if (ch === "(") {
            depth++;
          } else if (ch === ")") {
            if (depth === 0) break;
            depth--;
          } else if (/\s/.test(ch) && depth === 0) {
            break;
          }
          k++;
        }
        url = md.slice(urlStart, k);
      }

      if (!url) {
        i = altEnd + 1;
        continue;
      }

      // 可选标题："..." 或 (...)（转义感知）
      while (k < len && /\s/.test(md[k])) k++;
      if (k < len && (md[k] === '"' || md[k] === "'")) {
        const quote = md[k];
        k++;
        while (k < len) {
          const ch = md[k];
          if (ch === "\\") {
            k += 2;
            continue;
          }
          if (ch === quote) {
            k++;
            break;
          }
          k++;
        }
        while (k < len && /\s/.test(md[k])) k++;
      } else if (k < len && md[k] === "(") {
        k++;
        while (k < len) {
          const ch = md[k];
          if (ch === "\\") {
            k += 2;
            continue;
          }
          if (ch === ")") {
            k++;
            break;
          }
          k++;
        }
        while (k < len && /\s/.test(md[k])) k++;
      }

      if (md[k] !== ")") {
        i = altEnd + 1;
        continue;
      }

      const full = md.slice(start, k + 1);
      const alt = md.slice(altStart, altEnd);
      results.push({ full, alt, src: url });
      i = k + 1;
    }

    return results;
  }

  /**
   * 提取 HTML <img src="...">（code-adapter.ts:255 的正则）。
   * 返回 [{full, src}]，full = 整个 <img ...> 标签。
   */
  function extractHtmlImages(html) {
    const out = [];
    const re = /<img[^>]+src="([^"]+)"[^>]*>/gi;
    let m;
    while ((m = re.exec(String(html || ""))) !== null) {
      out.push({ full: m[0], src: m[1] });
    }
    return out;
  }

  /**
   * 是否跳过该图（code-adapter.ts:280-287）：data URI 永不跳过；
   * 其余命中 skipPatterns 任一子串即跳过。
   */
  function shouldSkip(src, skipPatterns) {
    if (!src) return true;
    if (src.startsWith("data:")) return false;
    return (skipPatterns || []).some((p) => src.includes(p));
  }

  /**
   * 图片处理主流程（code-adapter.ts:244-333 纯逻辑版）。
   * @param content    HTML 或 Markdown 文本
   * @param uploadFn   (src) => Promise<{url, attrs?}>；抛错 = 该图失败保留原 URL
   * @param opts       {skipPatterns?, onProgress?, delayFn?}
   * @returns Promise<string> 替换后的内容
   */
  async function processImages(content, uploadFn, opts) {
    const o = opts || {};
    const skipPatterns = o.skipPatterns || [];
    const delayFn = o.delayFn || ((ms) => new Promise((r) => setTimeout(r, ms)));
    const text = String(content || "");

    // 双格式提取：HTML 在前、Markdown 在后（与 code-adapter.ts:252-264 一致）
    const matches = [];
    for (const h of extractHtmlImages(text)) {
      matches.push({ full: h.full, src: h.src, alt: undefined, type: "html" });
    }
    for (const mdImg of parseMarkdownImages(text)) {
      matches.push({ full: mdImg.full, src: mdImg.src, alt: mdImg.alt, type: "markdown" });
    }
    if (!matches.length) return text;

    let result = text;
    const uploadedMap = new Map(); // 同 URL 去重
    let processed = 0;

    for (const { full, src, alt, type } of matches) {
      if (!src) continue;
      if (shouldSkip(src, skipPatterns)) continue;

      processed++;
      if (o.onProgress) {
        try {
          o.onProgress(processed, matches.length);
        } catch (_) {}
      }

      try {
        let uploadResult = uploadedMap.get(src);
        if (!uploadResult) {
          uploadResult = await uploadFn(src);
          uploadedMap.set(src, uploadResult);
        }

        let replacement;
        if (type === "html") {
          replacement = '<img src="' + uploadResult.url + '"';
          if (uploadResult.attrs) {
            for (const [key, value] of Object.entries(uploadResult.attrs)) {
              replacement += ' ' + key + '="' + value + '"';
            }
          }
          replacement += " />";
        } else {
          replacement = "![" + (alt || "") + "](" + uploadResult.url + ")";
        }
        result = result.replace(full, replacement);
      } catch (_) {
        // 上传失败：保留原 URL，继续处理其他图片（不中断同步）
      }

      // 避免请求过快（code-adapter.ts:329 的 300ms；测试注入 noop）
      await delayFn(300);
    }

    return result;
  }

  ADL.syncImages = { parseMarkdownImages, extractHtmlImages, shouldSkip, processImages };
})();
