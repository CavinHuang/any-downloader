// 文章同步操作页脚本（extension/sync.html 的 UI 编排，零依赖原生 DOM）。
// 页面流程照抄 code-box tabs/sync.tsx（GPL-3.0）并按本仓库形态裁剪：
//   列出命中 ADL.ARTICLES_RE 的标签页（lastAccessed 降序）→ 内容脚本提取
//   （title/正文 Markdown/封面）→ 平台登录态批量检测（TTL 缓存）→ 平台多选
//   + 仅草稿开关 → 并发 3 发布 → 逐平台状态 / 历史 / 取消。
// 设计承诺（code-box sync.tsx:489 同款）：使用浏览器已登录账号同步，
// 数据不经过任何第三方服务器。
(() => {
  const $ = (id) => document.getElementById(id);
  const state = { tabs: [], article: null, mdOriginal: "", platforms: [] };

  // ------------------------------------------------------------ 轻提示
  // S5：语义成对 token（info=accent-soft/ink，err=err-soft/ink，见 sync.html §3.8）；
  // role 随错误态切换 alert/status，读屏可闻
  function toast(msg, isErr) {
    const t = $("toast");
    t.textContent = msg;
    t.className = isErr ? "toast-err" : "toast-info";
    t.setAttribute("role", isErr ? "alert" : "status");
    t.style.display = "block";
    clearTimeout(toast._t);
    // 错误至少停留 5s（规范 §3.8），普通提示 2.5s
    toast._t = setTimeout(() => (t.style.display = "none"), isErr ? 5000 : 2500);
  }

  // ------------------------------------------------------------ 平台列表
  async function buildPlatforms() {
    const accounts = await ADL.syncEngine.getCmsAccounts();
    const cms = accounts.map((a) =>
      ADL.syncPlatforms.cmsPlatform(a, (id) => ADL.syncEngine.getCmsPassword(id))
    );
    state.platforms = ADL.syncPlatforms.ALL.concat(cms);
    ADL.syncEngine.setPlatforms(state.platforms);
  }

  function renderPlatforms() {
    const box = $("platforms");
    box.textContent = "";
    const groups = { 社区: [], 自建站: [] };
    for (const p of state.platforms) (groups[p.group] || (groups[p.group] = [])).push(p);
    for (const g of Object.keys(groups)) {
      const head = document.createElement("div");
      head.className = "group";
      head.textContent = g;
      box.append(head);
      for (const p of groups[g]) {
        const row = document.createElement("label");
        row.className = "plat";
        const cb = document.createElement("input");
        cb.type = "checkbox";
        cb.value = p.id;
        cb.dataset.name = p.name;
        // S4：勾选变化持久化（chrome.storage.local.syncTargets），下次打开自动恢复
        cb.addEventListener("change", persistTargets);
        const name = document.createElement("span");
        name.textContent = p.name;
        const fmt = document.createElement("span");
        fmt.className = "fmt";
        fmt.textContent = p.contentFormat;
        const badge = document.createElement("span");
        badge.className = "badge";
        badge.id = "badge-" + p.id;
        badge.textContent = "检测中…";
        row.append(cb, name, fmt, badge);
        box.append(row);
      }
    }
  }

  // S4：同步目标平台勾选持久化（固定同步某几个平台的用户不必每次重勾）
  const TARGETS_KEY = "syncTargets";
  function currentTargets() {
    const map = {};
    for (const cb of $("platforms").querySelectorAll("input[type=checkbox]")) {
      map[cb.value] = cb.checked;
    }
    return map;
  }
  async function persistTargets() {
    try {
      await chrome.storage.local.set({ [TARGETS_KEY]: currentTargets() });
    } catch (_) {}
  }
  async function restoreTargets() {
    let saved = null;
    try {
      saved = (await chrome.storage.local.get(TARGETS_KEY))[TARGETS_KEY] || null;
    } catch (_) {}
    if (!saved) return;
    for (const cb of $("platforms").querySelectorAll("input[type=checkbox]")) {
      if (Object.prototype.hasOwnProperty.call(saved, cb.value)) {
        cb.checked = !!saved[cb.value];
      }
    }
  }

  async function detectAuth(force) {
    const btn = $("btnDetect");
    btn.disabled = true;
    btn.textContent = "检测中…";
    try {
      const results = await ADL.syncEngine.checkAllPlatformsAuth(state.platforms, force);
      for (const r of results) {
        const badge = $("badge-" + r.id);
        if (!badge) continue;
        if (r.ok) {
          badge.textContent = "已登录" + (r.username ? "：" + r.username : "");
          badge.className = "badge ok";
        } else {
          badge.textContent = "未登录" + (r.error ? "（" + r.error + "）" : "");
          badge.className = "badge no";
          badge.title = r.error || "";
        }
      }
    } catch (e) {
      toast("登录检测失败：" + (e && e.message), true);
    }
    btn.disabled = false;
    btn.textContent = "重新检测登录";
  }

  // ------------------------------------------------------------ 来源标签页
  async function refreshTabs() {
    const sel = $("srcTabs");
    sel.textContent = "";
    state.tabs = [];
    try {
      const tabs = await chrome.tabs.query({ url: ["http://*/*", "https://*/*"] });
      state.tabs = tabs
        .filter((t) => t.url && ADL.ARTICLES_RE.test(t.url))
        .sort((a, b) => (b.lastAccessed || 0) - (a.lastAccessed || 0)); // 最近访问优先
    } catch (_) {}
    if (!state.tabs.length) {
      const opt = document.createElement("option");
      opt.textContent = "（未发现支持的文章页，请先打开一篇文章）";
      sel.append(opt);
      return;
    }
    for (const t of state.tabs) {
      const opt = document.createElement("option");
      opt.value = String(t.id);
      opt.textContent = (t.title || t.url || "").slice(0, 80);
      sel.append(opt);
    }
  }

  async function extract() {
    const sel = $("srcTabs");
    const tabId = Number(sel.value);
    if (!tabId) {
      toast("请先打开一篇支持的文章页", true);
      return;
    }
    const btn = $("btnExtract");
    btn.disabled = true;
    btn.textContent = "提取中…";
    try {
      const r = await chrome.tabs.sendMessage(tabId, { type: "adl-sync-extract" });
      if (!r || !r.ok) {
        toast(r && r.error ? r.error : "提取失败", true);
        return;
      }
      state.article = r.article;
      state.mdOriginal = r.article.markdown || "";
      $("title").value = r.article.title || "";
      $("md").value = state.mdOriginal;
      updateEditorCount();
      const bits = [r.article.siteLabel, r.article.wordCount + " 字"];
      if (r.article.cover) bits.push("有封面");
      $("srcInfo").textContent = bits.join(" · ");
      $("srcInfo").style.display = "block";
      toast("已提取：" + r.article.title);
    } catch (e) {
      toast(
        "提取失败：内容脚本未注入（请刷新文章页后重试）——" + ((e && e.message) || e),
        true
      );
    } finally {
      btn.disabled = false;
      btn.textContent = "提取文章";
    }
  }

  // ------------------------------------------------------------ 发布状态区
  // S3：失败原因渲染为 .errmsg 红字警示（原 .lnk 无 href 假链接样式已废除）；
  // .lnk 只在成功且有草稿链接时使用
  function setStatusRow(id, stage, result) {
    const row = $("status-" + id);
    if (!row) return;
    const st = row.querySelector(".st");
    const link = row.querySelector(".lnk");
    const msgEl = row.querySelector(".errmsg");
    st.className = "st " + stage;
    if (stage === "starting") {
      st.textContent = "发布中…";
      st.title = "";
      link.textContent = "";
      link.removeAttribute("href");
      msgEl.textContent = "";
      msgEl.title = "";
    } else if (stage === "completed" && result && result.ok) {
      st.textContent = "✓ 成功";
      st.title = "";
      msgEl.textContent = "";
      msgEl.title = "";
      if (result.draftUrl) {
        link.textContent = "打开草稿";
        link.href = result.draftUrl;
      }
    } else {
      st.textContent = "✕ 失败";
      const msg = (result && (result.error || (result.ok === false && "未知错误"))) || "未知错误";
      st.title = msg;
      link.textContent = "";
      link.removeAttribute("href");
      msgEl.textContent = msg.length > 46 ? msg.slice(0, 46) + "…" : msg;
      msgEl.title = msg;
    }
  }

  async function startSync() {
    if (ADL.syncEngine.isSyncing()) {
      toast("已有同步在进行中", true);
      return;
    }
    const ids = [...$("platforms").querySelectorAll("input:checked")].map((c) => c.value);
    if (!ids.length) {
      toast("请至少勾选一个同步目标", true);
      return;
    }
    const title = $("title").value.trim();
    if (!title) {
      toast("请填写标题", true);
      return;
    }
    const md = $("md").value;
    if (!md.trim() && !state.article) {
      toast("请先提取文章或粘贴 Markdown 正文", true);
      return;
    }
    const dirty = state.article ? md !== state.mdOriginal : true;
    const article = {
      title,
      markdown: md,
      // 未编辑 → 用提取的净化 HTML（保真）；编辑过 → 基础渲染器降级生成
      html: dirty ? ADL.syncMdHtml.mdToHtml(md) : state.article.html,
      cover: state.article ? state.article.cover : "",
      url: state.article ? state.article.url : "",
    };
    const options = {
      draftOnly: $("draftOnly").checked, // 默认 true（code-box engine.ts:332）
      enableImageUpload: $("uploadImgs").checked, // 默认 false：保留原始图片链接
    };

    const box = $("status");
    box.textContent = "";
    for (const id of ids) {
      const meta = state.platforms.find((p) => p.id === id);
      const row = document.createElement("div");
      row.className = "row";
      row.id = "status-" + id;
      const nm = document.createElement("span");
      nm.className = "nm";
      nm.textContent = (meta && meta.name) || id;
      const st = document.createElement("span");
      st.className = "st pending";
      st.textContent = "待同步";
      const link = document.createElement("a");
      link.className = "lnk";
      link.target = "_blank";
      link.rel = "noreferrer";
      const msgEl = document.createElement("span");
      msgEl.className = "errmsg"; // S3：失败原因走红字警示，不再挂 .lnk
      row.append(nm, st, link, msgEl);
      box.append(row);
    }

    $("btnSync").disabled = true;
    try {
      const results = await ADL.syncEngine.syncToMultiplePlatforms(ids, article, {
        onDetailProgress: (p) => setStatusRow(p.platform, p.stage, p.result),
      }, options);
      const okCount = results.filter((r) => r.ok).length;
      toast(`同步完成：${okCount}/${results.length} 成功`, okCount === 0);
      loadHistory();
    } catch (e) {
      toast("同步失败：" + ((e && e.message) || e), true);
    } finally {
      $("btnSync").disabled = false;
    }
  }

  // ------------------------------------------------------------ 历史
  function renderHistoryItem(entry) {
    const div = document.createElement("div");
    div.className = "hitem";
    const head = document.createElement("div");
    head.className = "htitle";
    head.textContent = entry.title || "（无标题）";
    const time = document.createElement("span");
    time.className = "htime";
    time.textContent = new Date(entry.ts).toLocaleString("zh-CN");
    head.append(time);
    div.append(head);
    for (const r of entry.results || []) {
      const line = document.createElement("div");
      line.className = "hline";
      if (r.ok) {
        const a = document.createElement("a");
        a.href = r.draftUrl || "#";
        a.target = "_blank";
        a.rel = "noreferrer";
        a.textContent = r.name + " ✓ 草稿";
        line.append(a);
      } else {
        line.textContent = r.name + " ✕ " + (r.error || "失败");
        line.title = r.error || "";
      }
      div.append(line);
    }
    return div;
  }

  async function loadHistory() {
    const box = $("history");
    box.textContent = "";
    const list = await ADL.syncEngine.getSyncHistory();
    if (!list.length) {
      const empty = document.createElement("div");
      empty.className = "hempty";
      empty.textContent = "暂无同步历史";
      box.append(empty);
      return;
    }
    for (const entry of list) box.append(renderHistoryItem(entry));
  }

  // ------------------------------------------------------------ CMS 账号
  async function renderCmsList() {
    const box = $("cmsList");
    box.textContent = "";
    const accounts = await ADL.syncEngine.getCmsAccounts();
    if (!accounts.length) {
      const empty = document.createElement("div");
      empty.className = "hempty";
      empty.textContent = "尚未添加自建站账号";
      box.append(empty);
      return;
    }
    const labels = { wordpress: "WordPress", typecho: "Typecho", metaweblog: "MetaWeblog" };
    for (const a of accounts) {
      const row = document.createElement("div");
      row.className = "cmsrow";
      const nm = document.createElement("span");
      const label = (a.name || labels[a.type] || a.type) + "（" + a.url + " · " + a.username + "）";
      nm.textContent = label;
      const del = document.createElement("button");
      del.textContent = "删除";
      del.className = "mini";
      del.addEventListener("click", async () => {
        // S7：破坏性-单条，与清空历史/应用端删除口径一致（§4.5 所有入口统一）
        if (!confirm("删除账号 " + label + "？不可恢复")) return;
        try {
          await ADL.syncEngine.removeCmsAccount(a.id);
          await boot();
          toast("账号已删除");
        } catch (e) {
          toast("删除失败：" + ((e && e.message) || e), true);
        }
      });
      row.append(nm, del);
      box.append(row);
    }
  }

  function toggleCmsForm() {
    const f = $("cmsForm");
    f.style.display = f.style.display === "none" ? "block" : "none";
  }

  async function saveCmsForm() {
    const type = $("cmsType").value;
    const url = $("cmsUrl").value.trim();
    const username = $("cmsUser").value.trim();
    const password = $("cmsPwd").value;
    const name = $("cmsName").value.trim();
    const endpoint = $("cmsEndpoint").value.trim();
    if (!url || !username) {
      toast("站点地址与用户名必填", true);
      return;
    }
    if (!/^https?:\/\//i.test(url)) {
      toast("站点地址需以 http(s):// 开头", true);
      return;
    }
    await ADL.syncEngine.saveCmsAccount(
      {
        type,
        name: name || type + "：" + username,
        url,
        username,
        endpoint: type === "metaweblog" ? endpoint : "",
      },
      password
    );
    $("cmsUrl").value = "";
    $("cmsUser").value = "";
    $("cmsPwd").value = "";
    $("cmsName").value = "";
    $("cmsEndpoint").value = "";
    toggleCmsForm();
    await boot();
    toast("账号已保存（密码明文存储于本机扩展存储）");
  }

  // ------------------------------------------------------------ 启动
  async function boot() {
    await buildPlatforms();
    renderPlatforms();
    await restoreTargets(); // S4：恢复上次的平台勾选
    renderCmsList();
    await refreshTabs();
    // ?tabId= 入参：直接锁定来源并自动提取；无入参时自动选中最近访问的
    // 第一篇文章并提取（code-box tabs/sync.tsx:111-120 同款，免手动点击）
    const want = Number(new URL(location.href).searchParams.get("tabId"));
    if (want && state.tabs.some((t) => t.id === want)) {
      $("srcTabs").value = String(want);
    } else if (state.tabs.length) {
      $("srcTabs").value = String(state.tabs[0].id); // refreshTabs 已按 lastAccessed 降序
    }
    if ($("srcTabs").value) extract();
    detectAuth(false);
    loadHistory();
  }

  function wire() {
    $("md").addEventListener("input", updateEditorCount);
    $("btnRefreshTabs").addEventListener("click", refreshTabs);
    $("btnExtract").addEventListener("click", extract);
    $("btnDetect").addEventListener("click", () => detectAuth(true));
    $("btnSync").addEventListener("click", startSync);
    $("btnCancel").addEventListener("click", () => {
      toast(ADL.syncEngine.cancelSync() ? "已请求取消" : "当前没有进行中的同步");
    });
    $("btnClearHistory").addEventListener("click", async () => {
      // S1：破坏性-批量，确认文案含数量与后果（§4.5）
      try {
        const list = await ADL.syncEngine.getSyncHistory();
        if (!list.length) {
          toast("暂无可清空的同步历史");
          return;
        }
        if (!confirm("清空全部 " + list.length + " 条同步历史？不可恢复")) return;
        await ADL.syncEngine.clearSyncHistory();
        loadHistory();
        toast("已清空同步历史");
      } catch (e) {
        toast("操作失败：" + ((e && e.message) || e), true);
      }
    });
    $("btnCmsAdd").addEventListener("click", toggleCmsForm);
    $("cmsType").addEventListener("change", () => {
      $("cmsEndpointWrap").style.display = $("cmsType").value === "metaweblog" ? "block" : "none";
    });
    $("btnCmsSave").addEventListener("click", saveCmsForm);
    $("btnCmsCancel").addEventListener("click", toggleCmsForm);
  }

  wire();
  function updateEditorCount() {
    $("editor-count").textContent = $("md").value.length.toLocaleString("zh-CN") + " 字符";
  }
  boot();
})();
