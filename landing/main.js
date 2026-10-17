// 落地页下载按钮：从 GitHub API 取最新 release 的安装包直链与版本号。
// API 带 CORS 头可前端直取；失败时保持指向 releases/latest 的回退链接。
(function () {
  var REPO = "CavinHuang/any-downloader";
  var API = "https://api.github.com/repos/" + REPO + "/releases/latest";
  var VERSIONS = ["dl-version", "dl-version-2"];
  var BUTTONS = ["dl-btn", "dl-btn-2"];

  fetch(API, { headers: { Accept: "application/vnd.github+json" } })
    .then(function (r) { return r.ok ? r.json() : Promise.reject(r.status); })
    .then(function (rel) {
      if (!rel || !rel.assets) return;
      var setup = null;
      for (var i = 0; i < rel.assets.length; i++) {
        if (/x64-setup\.exe$/.test(rel.assets[i].name)) { setup = rel.assets[i]; break; }
      }
      if (rel.tag_name) {
        VERSIONS.forEach(function (id) {
          var el = document.getElementById(id);
          if (el) el.textContent = rel.tag_name;
        });
      }
      if (setup && setup.browser_download_url) {
        BUTTONS.forEach(function (id) {
          var el = document.getElementById(id);
          if (el) el.href = setup.browser_download_url;
        });
        var text = document.getElementById("dl-btn-text");
        if (text) text.textContent = "下载 " + setup.name.replace(/^Anything\.Downloader_/, "").replace(/_x64-setup\.exe$/, "") + " 安装包";
      }
    })
    .catch(function () {
      /* 回退：按钮保持 releases/latest 链接 */
    });
})();
