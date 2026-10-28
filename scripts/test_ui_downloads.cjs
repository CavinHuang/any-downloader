// Run against a Vite dev server with Playwright and Microsoft Edge available.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const url = process.env.UI_TEST_URL || 'http://127.0.0.1:5173/';
const assert = require('node:assert/strict');
const path = require('node:path');
const output = path.resolve(__dirname, '../.smoke/downloads');
require('node:fs').mkdirSync(output, { recursive: true });
const fixtures = [
  ['山野之间 · A quiet film.mp4', 'downloading', 'https://vimeo.com/quiet-film', 892000000, 561000000],
  ['设计的日常 — Notes on design.pdf', 'done', 'https://archive.org/design.pdf', 18200000, 18200000],
  ['午后，窗边的光.jpg', 'done', 'https://images.unsplash.com/afternoon.jpg', 8400000, 8400000],
  ['坂本龍一 - Playing the Piano.flac', 'paused', 'https://archive.org/piano.flac', 126000000, 42000000],
  ['Editorial typefaces.zip', 'failed', 'https://example.com/typefaces.zip', 48000000, 12000000],
  ['Visual Studio Code.exe', 'queued', 'https://update.code.visualstudio.com/stable', 110000000, 0],
  ['来自海岸的声音.wav', 'done', 'https://archive.org/coast.wav', 62000000, 62000000],
  ['一份小小的阅读清单.md', 'done', 'https://example.com/reading.md', 12000, 12000],
].map(([filename, status, url, total_bytes, downloaded_bytes], i) => ({id: String(i + 1), filename, status, url, total_bytes, downloaded_bytes, kind: 'http', priority: 0, dir: 'D:\\Downloads', final_url: null, mime: null, etag: null, last_modified: null, headers_ref: null, headers_json: null, variants_json: null, error: status === 'failed' ? {code: 'network_timeout'} : null, created_at: Date.now() - i * 1000, updated_at: Date.now()}));

async function mock(context) {
  await context.addInitScript(({fixtures}) => {
    window.__qaTasks = fixtures;
    window.__qaCalls = [];
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    window.__qaSettings = {default_dir: 'D:\\Downloads', segments:'8', conflict:'rename', task_bps:'0', global_bps:'0', max_concurrent:'3', max_auto_retry:'3', clipboard_monitor:'on', auto_categorize:'off', after_done:'none'};
    let callback = 0;
    window.__TAURI_INTERNALS__ = {
      metadata: {currentWindow:{label:'main'}, currentWebview:{label:'main'}},
      transformCallback(fn) { const id = ++callback; window['_'+id] = fn; return id; },
      unregisterCallback() {},
      async invoke(command, args = {}) {
        window.__qaCalls.push({command, args});
        if (command === 'list_tasks') return window.__qaTasks;
        if (command === 'tasks_updated_since') return [];
        if (command === 'search_content') return [];
        if (command === 'get_settings') return window.__qaSettings;
        if (command === 'default_download_dir') return 'D:\\Downloads';
        if (command === 'get_autostart') return false;
        if (command === 'nm_registered') return true;
        if (command === 'plugin:app|version') return '0.4.0';
        if (command === 'plugin:event|listen') return ++callback;
        if (command === 'probe_url') return {filename:'test.pdf', total_bytes:1024, mime:'application/pdf', segmented:true};
        if (command === 'set_setting') {window.__qaSettings[args.key] = args.value; return;}
        if (command === 'plugin:dialog|open' && args.options?.directory) return 'D:\\Downloads\\Custom';
        if (command === 'plugin:dialog|save' || command === 'plugin:dialog|open') return 'D:\\Downloads\\anything-tasks.json';
        if (command === 'export_tasks' || command === 'import_tasks') return window.__qaTasks.length;
        if (command === 'add_task') {
          if (args.url.includes('/duplicate.pdf')) throw 'task_exists';
          if (args.url.includes('/unavailable.pdf')) throw 'network_error: unavailable';
          const task = {...window.__qaTasks[1], id:String(Date.now()), filename:args.filename || 'test.pdf', url:args.url, status:'queued'};
          window.__qaTasks.push(task); return task;
        }
        const task = window.__qaTasks.find(t => t.id === args.id);
        if (command === 'start_download') { task.status = 'downloading'; return; }
        if (command === 'pause_task') {task.status = 'paused'; return task;}
        if (command === 'cancel_task') {task.status = 'canceled'; return task;}
        if (command === 'retry_task') {task.status = 'queued'; return task;}
        if (command === 'rename_task') {task.filename = args.filename; return task;}
        if (command === 'remove_task') {window.__qaTasks = window.__qaTasks.filter(t=>t.id !== args.id); return true;}
        return undefined;
      }
    };
  }, {fixtures});
}


(async () => {
  const browser = await chromium.launch({ channel:'msedge', headless:true });
  try {
    const context = await browser.newContext({locale:'zh-CN',viewport:{width:1280,height:820},colorScheme:'light'});
    await mock(context);
    context.setDefaultTimeout(10000);
    const page = await context.newPage();
    page.on('dialog', dialog => dialog.accept());
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    await page.goto(url);
    await page.locator('.row').first().waitFor();
    await page.evaluate(async () => {
      const {store} = await import('/src/store.ts');
      store.applyEvent({type:'progress',task_id:'1',downloaded:561000000,total:892000000,bps:12800000,segments:null});
    });
    await page.waitForTimeout(700);
    await page.screenshot({animations:'disabled',path:path.join(output,'downloads-light.png')});
    const metrics = await page.evaluate(() => {
      const rows = document.querySelector('.rows');
      return {top:rows.getBoundingClientRect().top,height:rows.clientHeight,overflow:document.documentElement.scrollWidth > innerWidth};
    });
    console.log('Layout',metrics);
    assert.ok(metrics.top < 290);
    assert.ok(metrics.height >= 450);
    assert.equal(metrics.overflow,false);
    assert.match(await page.locator('.transfer-speed strong').textContent(),/12.2 MB/);
    assert.match(await page.locator('.task-downloading .c-speed').textContent(),/12.2 MB/);
    assert.match(await page.locator('.task-downloading .c-eta').textContent(),/26/);
    for (const [label, status, n] of [['正在下载','downloading',1],['排队与重试','queued',1],['已暂停','paused',1],['已完成','done',4],['下载失败','failed',1],['已取消','canceled',0]]) {
      await page.locator('.task-nav button').filter({hasText:label}).click();
      assert.equal(await page.locator('.row').count(),n,label);
      if (n) assert.ok(await page.locator('.row').first().getAttribute('class').then(c => c.includes('task-'+status)));
    }
    await page.locator('.task-nav button').filter({hasText:'全部下载'}).click();
    await page.locator('.type-control select').selectOption('doc');
    assert.equal(await page.locator('.row').count(),2);
    await page.locator('.type-control select').selectOption('all');
    await page.locator('.search-box input').fill('不存在');
    await page.getByRole('button',{name:'清除搜索',exact:true}).click();
    assert.equal(await page.locator('.row').count(),8);
    await page.locator('.sort-control select').selectOption('name:1');
    const names = await page.locator('.row .name').allTextContents();
    assert.deepEqual(names, [...names].sort((a,b)=>a.localeCompare(b)));
    await page.locator('.sort-control select').selectOption('default');
    await page.locator('.row input').first().click();
    await page.locator('.row input').nth(2).click({modifiers:['Shift']});
    assert.equal(await page.locator('.row.selected').count(),3);
    assert.equal(await page.getByRole('checkbox',{name:'全选本列表',exact:true}).evaluate(e => e.indeterminate),true);
    await page.getByRole('button',{name:'清除选择',exact:true}).click();
    await page.locator('.row .name').filter({hasText:'设计的日常'}).click();
    await page.getByRole('dialog').waitFor();
    await page.screenshot({animations:'disabled',path:path.join(output,'detail.png')});
    await page.keyboard.press('Escape');
    await page.locator('.link-entry input').fill('not-a-url');
    await page.getByRole('button',{name:'立即下载',exact:true}).click();
    assert.ok(await page.getByText('请输入有效的 HTTP 或 HTTPS 下载链接，多个链接用空格分隔。').isVisible());
    await page.locator('.link-entry input').fill('https://example.com/direct.pdf');
    await page.getByRole('button',{name:'立即下载',exact:true}).click();
    await page.waitForFunction(() => window.__qaCalls.some(c => c.command==='add_task' && c.args.url.endsWith('/direct.pdf')));
    assert.equal(await page.getByRole('dialog').count(),0,'quick download starts without dialog');
    await page.waitForFunction(() => document.querySelector('.link-entry input').value==='');
    assert.ok(await page.evaluate(() => window.__qaCalls.some(c=>c.command==='start_download')));
    await page.locator('.link-entry input').evaluate(input => {
      const data = new DataTransfer(); data.setData('text/plain','https://example.com/batch.pdf\nhttps://example.com/duplicate.pdf\nhttps://example.com/unavailable.pdf');
      input.dispatchEvent(new ClipboardEvent('paste',{clipboardData:data,bubbles:true,cancelable:true}));
    });
    assert.equal(await page.locator('.link-entry input').inputValue(),'https://example.com/batch.pdf https://example.com/duplicate.pdf https://example.com/unavailable.pdf');
    await page.getByRole('button',{name:'立即下载',exact:true}).click();
    await page.waitForFunction(() => document.querySelector('.link-entry input').value==='https://example.com/duplicate.pdf https://example.com/unavailable.pdf');
    assert.ok(await page.getByText('network_error: unavailable',{exact:true}).isVisible());
    await page.locator('.link-entry input').fill('');
    await page.getByRole('button',{name:'新建下载',exact:true}).first().click();
    await page.getByRole('dialog').waitFor();
    await page.locator('textarea').first().fill('https://example.com/options.pdf');
    await page.getByRole('button',{name:'浏览…',exact:true}).click();
    assert.match(await page.locator('#download-dir').inputValue(),/Custom$/);
    await page.screenshot({animations:'disabled',path:path.join(output,'add.png')});
    await page.getByRole('button',{name:'添加并开始',exact:true}).click();
    await page.getByRole('dialog').waitFor({state:'detached'});
    assert.ok(await page.evaluate(() => window.__qaCalls.some(c=>c.command==='add_task' && c.args.url.endsWith('/options.pdf') && c.args.dir.endsWith('Custom'))));
    await page.getByRole('button',{name:'设置',exact:true}).click();
    await page.getByRole('dialog').waitFor();
    await page.screenshot({animations:'disabled',path:path.join(output,'settings.png')});
    await page.getByRole('button',{name:'保存',exact:true}).click();
    await page.getByRole('dialog').waitFor({state:'detached'});
    await page.getByRole('button',{name:'导出',exact:true}).click();
    assert.ok(await page.evaluate(()=>window.__qaCalls.some(c=>c.command==='export_tasks')));
    await page.evaluate(async () => { const {getToasts,dismissToast} = await import('/src/toast.ts'); getToasts().forEach(toast => dismissToast(toast.id)); });
    for (const [width,height] of [[1080,720],[860,540],[390,844]]) {
      await page.setViewportSize({width,height});
      await page.screenshot({animations:'disabled',path:path.join(output,'window-'+width+'.png')});
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
      if (width>=860) assert.equal(await page.locator('.task-downloading .c-speed').first().isVisible(),true);
    }
    await page.setViewportSize({width:1280,height:820});
    await page.emulateMedia({colorScheme:'dark'});
    await page.waitForTimeout(250);
    await page.screenshot({animations:'disabled',path:path.join(output,'downloads-dark.png')});
    await page.keyboard.press('Control+k');
    assert.equal(await page.locator('.search-box input').evaluate(e=>e===document.activeElement),true);
    await page.keyboard.press('Escape');
    await page.evaluate(async () => {
      const template = window.__qaTasks[1];
      window.__qaTasks = Array.from({length:10000},(_,i)=>({...template,id:String(i),filename:'File '+i+'.pdf'}));
      const {store} = await import('/src/store.ts'); await store.refresh();
      const rows = document.querySelector('.rows'); rows.scrollTop = rows.scrollHeight;
    });
    await page.waitForTimeout(250);
    assert.ok(await page.locator('.row').count()<30,'virtual scrolling');
    await page.locator('.type-control select').selectOption('image');
    await page.getByRole('button',{name:'清除筛选',exact:true}).click();
    assert.ok(await page.locator('.row').count()>0);
    const previewContext = await browser.newContext({locale:'zh-CN',viewport:{width:1280,height:820}});
    const preview = await previewContext.newPage();
    preview.on('pageerror',e=>errors.push(e.message));
    await preview.goto(url);
    await preview.locator('.empty h3').waitFor();
    await preview.screenshot({animations:'disabled',path:path.join(output,'empty.png')});
    await preview.getByRole('button',{name:'新建下载',exact:true}).first().click();
    assert.equal(await preview.getByRole('button',{name:'添加并开始',exact:true}).isDisabled(),true);
    await preview.keyboard.press('Escape');
    const enContext = await browser.newContext({locale:'en-US',viewport:{width:860,height:540},colorScheme:'light'});
    await mock(enContext);
    const english = await enContext.newPage();
    english.on('pageerror',e=>errors.push(e.message));
    await english.goto(url);
    await english.locator('.row').first().waitFor();
    await english.screenshot({animations:'disabled',path:path.join(output,'english.png')});
    assert.equal(await english.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
    assert.deepEqual(errors,[]);
    console.log('PASS: task navigation, filtering, sorting, search, range selection, quick and custom downloads, multiline paste, partial failures, folder selection, details, settings, export, live speed/ETA, shortcuts, themes, responsive layout, English, 10000-task virtualization.');
  } finally { await browser.close(); }
})().catch(e=>{console.error(e);process.exit(1);});
