// 同步引擎（扩展页上下文运行）—— 编排层，照抄 code-box sync/extension/engine.ts
// （GPL-3.0）的状态机与常量，存储键改为 adl-sync-* 前缀：
//   登录检测：批量并发 5、单次超时 12s、缓存 TTL 已登录 5min / 未登录 30s
//     （engine.ts:24-28,193-270；TTL 判定纯函数在 payloads.authCacheDecide）
//   多平台同步：并发 3、单平台 10min 超时、AbortController 取消、
//     stage = starting/completed/failed（engine.ts:274-470）
//   历史：chrome.storage.local 上限 30 条（engine.ts:34-35,457-463）
//   CMS 账号：chrome.storage.local，密码明文存 adl-sync-cms-pwd-<id>
//     （engine.ts:495-576 同款取舍，UI 层已给出明文存储提示）
self.ADL = self.ADL || {};
(() => {
  const AUTH_CACHE_KEY = "adl-sync-auth-cache";
  const AUTH_CHECK_CONCURRENCY = 5; // 登录检测批量并发（engine.ts:27）
  const AUTH_CHECK_TIMEOUT = 12 * 1000; // 登录检测超时（engine.ts:28）
  const PUBLISH_TIMEOUT = 10 * 60 * 1000; // 单平台发布超时（engine.ts:29）
  const CONCURRENCY_LIMIT = 3; // 多平台同步并发（engine.ts:30）
  const CMS_ACCOUNTS_KEY = "adl-sync-cms-accounts";
  const CMS_PWD_PREFIX = "adl-sync-cms-pwd-";
  const HISTORY_KEY = "adl-sync-history";
  const MAX_HISTORY_ITEMS = 30;

  // ============================================================ 存储基件
  async function storageGet(key, fallback) {
    try {
      const r = await chrome.storage.local.get(key);
      return r && r[key] != null ? r[key] : fallback;
    } catch (_) {
      return fallback;
    }
  }
  async function storageSet(key, value) {
    try {
      await chrome.storage.local.set({ [key]: value });
    } catch (_) {}
  }

  // withTimeout（engine.ts:95-109）
  function withTimeout(promise, ms, errorMessage) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(errorMessage)), ms);
      promise.then(
        (result) => {
          clearTimeout(timer);
          resolve(result);
        },
        (error) => {
          clearTimeout(timer);
          reject(error);
        }
      );
    });
  }

  // ============================================================ 平台注册
  // sync.js 启动时把「社区平台 + CMS 账号平台」全量注册进来
  let platformMap = {};
  function setPlatforms(list) {
    platformMap = {};
    for (const p of list || []) platformMap[p.id] = p;
  }
  const getPlatform = (id) => platformMap[id] || null;

  // ============================================================ 登录检测
  // 单平台检测（engine.ts:158-181）
  async function checkPlatformAuth(platformId) {
    const p = getPlatform(platformId);
    if (!p) return { ok: false, error: "平台适配器不存在" };
    try {
      return await withTimeout(
        Promise.resolve(p.checkAuth()),
        AUTH_CHECK_TIMEOUT,
        "登录检测超时（" + AUTH_CHECK_TIMEOUT / 1000 + "秒）"
      );
    } catch (e) {
      return { ok: false, error: String((e && e.message) || e) };
    }
  }

  // 批量检测（engine.ts:193-270）：缓存优先 → 未命中/过期者分批并发 → 回写缓存
  async function checkAllPlatformsAuth(platforms, forceRefresh) {
    const cache = await storageGet(AUTH_CACHE_KEY, {});
    const now = Date.now();
    const results = [];
    const needsCheck = [];
    const decide = ADL.syncBuild.authCacheDecide;

    for (const meta of platforms || []) {
      const r = decide(cache, meta.id, now, !!forceRefresh);
      if (r.fresh) {
        results.push({ ...meta, ...r.item });
      } else {
        needsCheck.push(meta);
      }
    }

    for (let i = 0; i < needsCheck.length; i += AUTH_CHECK_CONCURRENCY) {
      const batch = needsCheck.slice(i, i + AUTH_CHECK_CONCURRENCY);
      const batchResults = await Promise.all(
        batch.map(async (meta) => {
          const auth = await checkPlatformAuth(meta.id);
          cache[meta.id] = {
            ok: !!auth.ok,
            username: auth.username,
            avatar: auth.avatar,
            error: auth.error,
            ts: now,
          };
          return { ...meta, ...auth };
        })
      );
      results.push(...batchResults);
    }

    await storageSet(AUTH_CACHE_KEY, cache);
    return results;
  }

  // ============================================================ 同步执行
  let syncAbortController = null;

  const isSyncing = () => syncAbortController !== null;

  // 取消（engine.ts:308-315）：中断后未开始的平台直接标记「已取消」
  function cancelSync() {
    if (syncAbortController) {
      syncAbortController.abort();
      syncAbortController = null;
      return true;
    }
    return false;
  }

  // 单平台同步（engine.ts:327-378）：10min 超时 + 统一失败包装
  async function syncToPlatform(platformId, article, options) {
    const meta = getPlatform(platformId);
    if (!meta) {
      return { platform: platformId, name: platformId, ok: false, error: "平台适配器不存在" };
    }
    try {
      const r = await withTimeout(
        Promise.resolve(meta.publish(article, options || {})),
        PUBLISH_TIMEOUT,
        "同步超时（" + PUBLISH_TIMEOUT / 60000 + "分钟）"
      );
      return { platform: platformId, name: meta.name, ...r };
    } catch (e) {
      return { platform: platformId, name: meta.name, ok: false, error: String((e && e.message) || e) };
    }
  }

  // 多平台同步（engine.ts:383-455）：并发 3 分批 + 取消 + 进度回调
  //   callbacks.onDetailProgress({platform, name, stage, result})
  //   options = {draftOnly, enableImageUpload, signal 由内部 AbortController 提供}
  async function syncToMultiplePlatforms(platformIds, article, callbacks, options) {
    const cb = callbacks || {};
    const opts = { ...(options || {}) };
    syncAbortController = new AbortController();
    const signal = syncAbortController.signal;
    opts.signal = signal;

    const results = [];

    const syncOne = async (platformId) => {
      const meta = getPlatform(platformId);
      const name = (meta && meta.name) || platformId;
      if (signal.aborted) {
        const cancelled = { platform: platformId, name, ok: false, error: "已取消" };
        if (cb.onDetailProgress) {
          cb.onDetailProgress({ platform: platformId, name, stage: "failed", result: cancelled });
        }
        return cancelled;
      }
      if (cb.onDetailProgress) {
        cb.onDetailProgress({ platform: platformId, name, stage: "starting" });
      }
      const result = await syncToPlatform(platformId, article, opts);
      if (cb.onDetailProgress) {
        cb.onDetailProgress({
          platform: platformId,
          name,
          stage: result.ok ? "completed" : "failed",
          result,
        });
      }
      return result;
    };

    for (let i = 0; i < platformIds.length; i += CONCURRENCY_LIMIT) {
      if (signal.aborted) {
        for (const platformId of platformIds.slice(i)) {
          results.push({ platform: platformId, name: platformId, ok: false, error: "已取消" });
        }
        break;
      }
      const batch = platformIds.slice(i, i + CONCURRENCY_LIMIT);
      results.push(...(await Promise.all(batch.map(syncOne))));
    }

    syncAbortController = null;

    if (!opts || opts.saveHistory !== false) {
      await saveSyncHistory({
        ts: Date.now(),
        title: article.title,
        cover: article.cover || "",
        sourceUrl: article.url || "",
        platforms: platformIds,
        results,
      });
    }

    // 同步结束角标反馈（engine.ts:466-467）：离开同步页后仍可见完成结果
    await updateBadge(results.filter((r) => r.ok).length, results.length);
    return results;
  }

  // 角标反馈（engine.ts:475-491 同款）：全部成功写 ✓（绿），否则写成功数（橙），
  // 3 秒后自动清除。写在全局角标（不带 tabId），不影响背景页按 tabId 写入的
  // 嗅探计数角标；无 chrome.action 的上下文（Node 单测）静默跳过。
  async function updateBadge(successCount, total) {
    try {
      if (typeof chrome === "undefined" || !chrome.action) return;
      const text = successCount === total ? "✓" : String(successCount);
      await chrome.action.setBadgeBackgroundColor({
        color: successCount === total ? "#22C55E" : "#F59E0B",
      });
      await chrome.action.setBadgeText({ text });
      setTimeout(() => {
        try {
          const p = chrome.action.setBadgeText({ text: "" });
          if (p && p.catch) p.catch(() => {});
        } catch (_) {}
      }, 3000);
    } catch (_) {
      // 忽略角标错误（engine.ts:490-492 同款）
    }
  }

  // ============================================================ 历史
  async function getSyncHistory() {
    return storageGet(HISTORY_KEY, []);
  }
  async function clearSyncHistory() {
    await storageSet(HISTORY_KEY, []);
  }
  async function saveSyncHistory(entry) {
    const list = ADL.syncBuild.capHistory(await getSyncHistory(), MAX_HISTORY_ITEMS);
    await storageSet(HISTORY_KEY, [entry, ...list].slice(0, MAX_HISTORY_ITEMS));
  }

  // ============================================================ CMS 账号
  //（engine.ts:495-576；密码明文存储 —— 安全提示文案由 UI 层负责展示）
  async function getCmsAccounts() {
    return storageGet(CMS_ACCOUNTS_KEY, []);
  }
  async function saveCmsAccount(account, password) {
    const accounts = await getCmsAccounts();
    const id = account.id || "cms_" + account.type + "_" + Date.now();
    const saved = {
      id,
      type: account.type,
      name: account.name,
      url: account.url,
      username: account.username,
      endpoint: account.endpoint || "",
    };
    const index = accounts.findIndex((a) => a.id === id);
    if (index >= 0) accounts[index] = saved;
    else accounts.push(saved);
    await storageSet(CMS_ACCOUNTS_KEY, accounts);
    if (password !== undefined && password !== "") {
      await storageSet(CMS_PWD_PREFIX + id, password);
    }
    return saved;
  }
  async function removeCmsAccount(id) {
    const accounts = await getCmsAccounts();
    await storageSet(CMS_ACCOUNTS_KEY, accounts.filter((a) => a.id !== id));
    try {
      await chrome.storage.local.remove(CMS_PWD_PREFIX + id);
    } catch (_) {}
  }
  async function getCmsPassword(id) {
    return storageGet(CMS_PWD_PREFIX + id, null);
  }

  ADL.syncEngine = {
    setPlatforms,
    checkPlatformAuth,
    checkAllPlatformsAuth,
    syncToMultiplePlatforms,
    cancelSync,
    isSyncing,
    getSyncHistory,
    clearSyncHistory,
    getCmsAccounts,
    saveCmsAccount,
    removeCmsAccount,
    getCmsPassword,
  };
})();
