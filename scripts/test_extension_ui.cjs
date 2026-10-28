const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const { pathToFileURL } = require('node:url');
const path = require('node:path');
const fs = require('node:fs');
const assert = require('node:assert/strict');
const root = path.resolve(__dirname, '../extension');
const output = path.resolve(__dirname, '../.smoke/extension-downloads');
fs.mkdirSync(output, {recursive:true});
const url = name => pathToFileURL(path.join(root, name)).href;

async function mock(context, offline = false) {
  context.setDefaultTimeout(10000);
  await context.addInitScript(({offline}) => {
    const items = [
      {url:'https://media.example.com/山野之间-A-quiet-film.mp4', type:'video', bytes:892000000},
      {url:'https://media.example.com/自然的回声.flac', type:'audio', bytes:126000000},
      {url:'https://cdn.example.com/设计的日常.pdf', type:'file', bytes:18200000},
      {url:'https://cdn.example.com/窗边的光.jpg', type:'image', bytes:8400000},
    ];
    window.__items = offline ? [] : items;
    window.__offline = offline;
    window.__calls = [];
    window.__storage = {takeover:'off', sniffCfg:{types:{video:true,audio:true,archive:true,doc:true,binary:true,image:false},minSizeMB:1},'adl-sync-cms-accounts':[],'adl-sync-history':[{title:'把生活的细节，慢慢收好', ts:Date.now()-86400000, results:[{name:'知乎',ok:true,draftUrl:'https://example.com/draft/1'}]}]};
    window.__storage['adl-sync-auth-cache'] = Object.fromEntries(['zhihu','jianshu','cnblogs','segmentfault','oschina'].map(id=>[id,{ok:id!=='jianshu',username:id==='zhihu'?'拾光者':undefined,ts:Date.now()}]));
    Object.defineProperty(navigator,'clipboard',{value:{async writeText(text){window.__copied=text;}}});
    window.chrome = {
      runtime:{
        async openOptionsPage(){window.__openedOptions=true;},
        getURL(name){return name;},
        async sendMessage(message, callback){
          window.__calls.push(message);
          if(message.type==='video-download') { const response=window.__videoFail ? {ok:false,error:'app_not_running'} : {ok:true,via:'direct'}; queueMicrotask(()=>callback(response)); return; }
          if(message.type==='ping') return {ok:!window.__offline};
          if(message.type==='list') return {ok:true,items:window.__items};
          if(message.type==='adapters-cfg') return {ok:true,cfg:{}};
          if(message.type==='adapter-probe') return {ok:true,adapterId:'articles',adapterIds:['articles'],resources:window.__articleMode ? [{url:'https://example.com/article.md',filename:'给日常留一份底稿.md',kind:'document'},{url:'https://example.com/image.jpg',filename:'封面.jpg',kind:'image'}]:[]};
          if(message.type==='adapter-push') return window.__pushFail ? {ok:false,error:'app_not_running'} : {ok:true,pushed:2,total:2};
          if(message.type==='takeover-data') return {takeovers:[],ignoredHosts:[]};
          if(message.type==='scan-page') return {ok:true,added:0,total:window.__items.length,items:window.__items};
          if(message.type==='adapter-resolve' && window.__directMode) return {ok:false,error:'unsupported_site'};
          if(message.type==='adapter-resolve') return {ok:true,resources:[{url:'https://example.com/resolved.mp4',filename:'resolved.mp4'}]};
          if(message.type==='push') return window.__resourceFail===message.rec.url ? {ok:false,error:'app_not_running'} : {ok:true,started:true};
          if(message.type==='nm' && message.method==='task.pause' && window.__taskError) return {ok:false,error:{code:'network_error',message:'测试操作失败'}};
          if(message.type==='nm' && message.method==='task.list') return {ok:!window.__offline,result:window.__tasks || [
            {id:'1',filename:'山野之间 · A quiet film.mp4',url:items[0].url,status:'downloading',kind:'http',total_bytes:892000000,downloaded_bytes:561000000,updated_at:3},
            {id:'2',filename:'自然的回声.flac',url:items[1].url,status:'paused',kind:'http',total_bytes:126000000,downloaded_bytes:42000000,updated_at:2},
            {id:'3',filename:'设计的日常.pdf',url:items[2].url,status:'done',kind:'http',total_bytes:18200000,downloaded_bytes:18200000,updated_at:1},
          ]};
          return {ok:true};
        }
      },
      storage:{onChanged:{addListener(){}},local:{async get(key){return Array.isArray(key)?Object.fromEntries(key.map(k=>[k,window.__storage[k]])):{[key]:window.__storage[key]};},async set(data){if(window.__storageFail) throw new Error('storage failed'); Object.assign(window.__storage,data);},async remove(key){delete window.__storage[key];}}},
      tabs:{async query(){return [{id:7,title:'给日常留一份底稿',url:'https://mp.weixin.qq.com/s/example',lastAccessed:1}];},async sendMessage(){return {ok:true,article:{title:'给日常留一份底稿',markdown:'# 给日常留一份底稿\n\n我们收集的，不只是文件。\n\n是某个清晨听到的旋律，是读到一半舍不得关掉的文字，是让人停下来的一束光。\n\n## 把喜欢的慢慢留下\n\n把链接交给 Anything，把时间留给自己。',html:'<h1>给日常留一份底稿</h1><p>把时间留给自己。</p>',siteLabel:'微信文章',wordCount:156,url:'https://mp.weixin.qq.com/s/example'}};}},
      action:{async setBadgeText(){},async setBadgeBackgroundColor(){}}
    };
  }, {offline});
}

async function shot(page, name, fullPage=false) {
  await page.screenshot({path:path.join(output,name+'.png'),animations:'disabled',fullPage});
}

(async()=>{
  const browser = await chromium.launch({channel:'msedge',headless:true});
  const errors = [];
  try {
    const context = await browser.newContext({locale:'zh-CN',viewport:{width:440,height:600},colorScheme:'light'});
    await mock(context);
    const page = await context.newPage();
    page.on('pageerror',e=>errors.push(e.message));
    await page.goto(url('popup.html'));
    await page.locator('#list .item').first().waitFor();
    assert.equal(await page.locator('#list .item').count(),4);
    await shot(page,'popup-resources');
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
    assert.equal(await page.evaluate(()=>document.body.scrollHeight>600),false);
    await page.getByRole('button',{name:'图片 1',exact:true}).click();
    assert.equal(await page.locator('#list .item').count(),1);
    await page.getByRole('button',{name:'全部 4',exact:true}).click();
    await page.locator('#list .item input').first().check();
    assert.equal(await page.locator('#batchbar').isVisible(),true);
    await shot(page,'popup-selection');
    await page.locator('#sel-push').click();
    assert.ok((await page.locator('#download-result').textContent()).includes('已添加 1 项'));
    await page.evaluate(()=>window.__resourceFail=window.__items[1].url);
    await page.locator('#list .item input').nth(0).check();
    await page.locator('#list .item input').nth(1).check();
    await page.locator('#tab-tasks').click();
    await page.locator('#tab-res').click();
    assert.equal(await page.locator('#batchbar').isVisible(),true,'selected resources persist across tabs');
    await page.locator('#sel-push').click();
    await page.waitForFunction(()=>document.querySelector('#download-result').textContent.includes('失败 1 项'));
    assert.equal(await page.locator('#sel-n').textContent(),'1','keep failed resource selected');
    await page.evaluate(()=>window.__resourceFail=null);
    await page.locator('#sel-push').click();
    await page.waitForFunction(()=>document.querySelector('#batchbar').classList.contains('hidden'));
    await page.getByRole('switch',{name:'接管浏览器下载'}).check();
    assert.equal(await page.evaluate(()=>window.__storage.takeover),'on');
    await page.locator('#btn-settings').click();
    await page.getByLabel('图片',{exact:true}).check();
    assert.equal(await page.evaluate(()=>window.__storage.sniffCfg.types.image),true);
    await page.evaluate(()=>window.__storageFail=true);
    await page.getByLabel('图片',{exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('#settings-feedback').textContent.includes('保存失败'));
    assert.equal(await page.getByLabel('图片',{exact:true}).isChecked(),true);
    await page.evaluate(()=>window.__storageFail=false);
    await shot(page,'popup-preferences');
    await page.locator('#btn-settings-close').focus();
    await page.keyboard.press('Shift+Tab');
    assert.equal(await page.locator('#set-video-overlay').evaluate(el=>el===document.activeElement),true);
    await page.keyboard.press('Escape');
    assert.equal(await page.locator('#btn-settings').evaluate(el=>el===document.activeElement),true);
    await page.locator('#tab-tasks').click();
    await page.locator('.task-item').first().waitFor();
    assert.equal(await page.locator('.task-item').count(),3);
    await shot(page,'popup-transfers');
    await page.getByRole('button',{name:'暂停',exact:true}).click();
    assert.ok(await page.evaluate(()=>window.__calls.some(m=>m.method==='task.pause')));
    await page.locator('#task-filter').selectOption('paused');
    assert.equal(await page.locator('.task-item').count(),1);
    await page.locator('#task-filter').selectOption('done');
    assert.equal(await page.getByRole('button',{name:'打开',exact:true}).count(),1);
    await page.locator('#task-filter').selectOption('all');
    await page.evaluate(()=>window.__taskError=true);
    await page.getByRole('button',{name:'暂停',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('#task-result').textContent.includes('测试操作失败'));
    assert.equal(await page.locator('#task-result').getAttribute('class'),'error');
    await page.evaluate(()=>window.__taskError=false);
    await page.locator('#tab-res').click();
    await page.locator('#paste-input').fill('https://example.com/share');
    await page.locator('#paste-input').press('Enter');
    await page.waitForFunction(()=>document.querySelector('#paste-result').textContent.includes('已添加'));
    await page.evaluate(()=>window.__directMode=true);
    await page.locator('#paste-input').fill('https://files.example.com/direct.zip');
    await page.locator('#paste-input').press('Enter');
    await page.waitForFunction(()=>window.__calls.some(call=>call.type==='push' && call.rec.url==='https://files.example.com/direct.zip'));
    await page.evaluate(()=>window.__resourceFail='https://files.example.com/offline.zip');
    await page.locator('#paste-input').fill('https://files.example.com/offline.zip');
    await page.locator('#paste-input').press('Enter');
    await page.waitForFunction(()=>document.querySelector('#paste-result').textContent.includes('失败 1 项'));
    assert.equal(await page.locator('#paste-input').inputValue(),'https://files.example.com/offline.zip');
    await page.locator('#btn-sync').click();
    assert.equal(await page.evaluate(()=>window.__openedOptions),true);
    await page.emulateMedia({colorScheme:'dark'});
    await shot(page,'popup-dark');
    const offlineContext = await browser.newContext({locale:'zh-CN',viewport:{width:440,height:600},colorScheme:'light'});
    await mock(offlineContext,true);
    const offline = await offlineContext.newPage();
    offline.on('pageerror',e=>errors.push(e.message));
    await offline.goto(url('popup.html'));
    await offline.locator('#status.err').waitFor();
    await offline.locator('#status').click();
    await shot(offline,'popup-offline');
    await offline.locator('#tab-tasks').click();
    await offline.locator('[data-offline]').waitFor();

    const syncContext = await browser.newContext({locale:'zh-CN',viewport:{width:1280,height:1000},colorScheme:'light'});
    await mock(syncContext);
    const sync = await syncContext.newPage();
    sync.on('pageerror',e=>errors.push(e.message));
    await sync.route(/^https?:\/\//,route=>route.abort());
    await sync.goto(url('sync.html'));
    await sync.waitForFunction(()=>document.querySelector('#title').value.length>0);
    assert.ok((await sync.locator('#editor-count').textContent()).includes('字符'));
    assert.equal(await sync.locator('.plat').count(),5);
    await sync.locator('#toast').evaluate(el=>el.style.display='none');
    await shot(sync,'article-workspace');
    await sync.locator('#btnSync').click();
    assert.ok((await sync.locator('#toast').textContent()).includes('至少勾选'));
    await sync.evaluate(()=>{
      for(const platform of ADL.syncPlatforms.ALL) {
        platform.checkAuth=async()=>({ok:true,username:'测试用户'});
        platform.publish=async(article,options)=>{window.__publishOptions=options;return {ok:true,draftUrl:'https://example.com/draft/test'};};
      }
    });
    await sync.locator('#platforms input').first().check();
    await sync.locator('#btnSync').click();
    await sync.locator('#status .st.completed').waitFor();
    assert.ok((await sync.locator('#history').textContent()).includes('给日常留一份底稿'));
    await sync.locator('.cms-panel > summary').click();
    await sync.locator('#btnCmsAdd').click();
    assert.equal(await sync.locator('#cmsForm').isVisible(),true);
    await sync.locator('#btnCmsCancel').click();
    await sync.emulateMedia({colorScheme:'dark'});
    await sync.evaluate(()=>scrollTo(0,0));
    await shot(sync,'article-workspace-dark');
    await sync.setViewportSize({width:390,height:844});
    assert.equal(await sync.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
    await shot(sync,'article-mobile');

    const floatContext = await browser.newContext({locale:'zh-CN',viewport:{width:1100,height:850},colorScheme:'light'});
    await mock(floatContext);
    const floating = await floatContext.newPage();
    floating.on('pageerror',e=>errors.push(e.message));
    await floating.setContent('<html lang="zh"><head><title>慢慢收集生活</title><style>body{margin:0;background:#fdfcf9;color:#364138;font:16px/2.1 "Microsoft YaHei UI",sans-serif}article{width:570px;margin:90px auto;padding-right:30px}h1{font:36px/1.6 SimSun,serif}small{color:#788374;letter-spacing:3px}p{margin:24px 0}blockquote{padding:15px 20px;margin:25px 0;border-left:2px solid #bcc6aa;color:#788374}</style></head><body><article><small>DAILY NOTES / 日常笔记</small><h1>留一点时间，<br>给真正喜欢的事。</h1><p>我们收集的，不只是文件。</p><p>是某个清晨听到的旋律，是读到一半舍不得关掉的文字，是让人停下来的一束光。</p><blockquote>把喜欢的慢慢留下，把时间留给自己。</blockquote><p>生活里有很多微小的发现。值得的，都应该有一个安静的位置。</p></article></body></html>');
    await floating.evaluate(()=>{
      window.__articleMode=true;
      window.ADL={matchAdapters:()=>[{id:'articles',label:'文章'}],articleSite:()=>({root:['article']})};
      const attach=Element.prototype.attachShadow;
      window.__qaRoots=[];
      Element.prototype.attachShadow=function(options){const shadow=attach.call(this,options);window.__qaRoots.push(shadow);return shadow;};
    });
    await floating.addScriptTag({path:path.join(root,'actions.js')});
    await floating.evaluate(()=>{
      ADL.articleActionEnv=()=>({title:'留一点时间，给真正喜欢的事。',summary:'把喜欢的慢慢留下，把时间留给自己。',md:'# 日常笔记\n\n给生活留一份底稿。',site:{label:'日常笔记'},pageUrl:'https://example.com/notes',content:document.querySelector('article')});
    });
    await floating.addScriptTag({path:path.join(root,'float-button.js')});
    await floating.getByRole('button',{name:'Anything Downloader · 文章',exact:true}).click();
    await floating.getByRole('dialog',{name:'Anything Downloader 下载面板'}).waitFor();
    await shot(floating,'page-downloads');
    assert.equal(await floating.evaluate(()=>window.__qaRoots[0].querySelectorAll('.menu button').length),13);
    assert.equal(await floating.evaluate(()=>window.__qaRoots[0].querySelector('.article-tools').open),false);
    await floating.keyboard.press('Shift+Tab');
    assert.equal(await floating.evaluate(()=>window.__qaRoots[0].activeElement.tagName),'SUMMARY');
    await floating.evaluate(()=>window.__pushFail=true);
    await floating.evaluate(()=>window.__qaRoots[0].querySelector('.push').click());
    await floating.waitForFunction(()=>window.__qaRoots[0].querySelector('.err-row')?.textContent.includes('主程序未运行'));
    assert.equal(await floating.evaluate(()=>window.__qaRoots[0].querySelector('.push').disabled),false);
    await floating.evaluate(()=>window.__pushFail=false);
    await floating.evaluate(()=>window.__qaRoots[0].querySelector('.push').click());
    await floating.waitForFunction(()=>window.__qaRoots[0].querySelector('.push').textContent.includes('已添加'));
    assert.ok(!(await floating.evaluate(()=>window.__qaRoots[0].querySelector('.push').textContent)).includes('已保存'));
    await floating.keyboard.press('Escape');
    await floating.getByRole('button',{name:'Anything Downloader · 文章',exact:true}).click();
    await floating.evaluate(()=>window.__qaRoots[0].querySelector('.article-tools').open=true);
    await floating.evaluate(()=>[...window.__qaRoots[0].querySelectorAll('.menu button')].find(b=>b.textContent==='文章摘要').click());
    await floating.getByRole('dialog',{name:'文章摘要',exact:true}).waitFor();
    await shot(floating,'article-summary');
    await floating.keyboard.press('Escape');
    await floating.evaluate(()=>{const root=window.__qaRoots[0];root.querySelectorAll('.res-row input').forEach(cb=>{cb.checked=false;cb.dispatchEvent(new Event('change'));});});
    assert.equal(await floating.evaluate(()=>window.__qaRoots[0].querySelector('.push').disabled),true);
    await floating.emulateMedia({colorScheme:'dark'});
    await shot(floating,'page-downloads-dark');
    await floating.keyboard.press('Escape');
    assert.equal(await floating.getByRole('dialog',{name:'Anything Downloader 下载面板'}).count(),0);
    const videoContext = await browser.newContext({viewport:{width:900,height:650},locale:'zh-CN'});
    await mock(videoContext);
    const video = await videoContext.newPage();
    video.on('pageerror',error=>errors.push(error.message));
    await video.setContent('<html><body><h1>视频下载测试</h1><video controls style="position:absolute;left:100px;top:120px;width:640px;height:360px;background:#192638"></video></body></html>');
    await video.evaluate(()=>{
      const attach=Element.prototype.attachShadow; window.__videoRoots=[];
      Element.prototype.attachShadow=function(options){const shadow=attach.call(this,options);window.__videoRoots.push(shadow);return shadow;};
    });
    await video.addScriptTag({path:path.join(root,'video-overlays.js')});
    await video.mouse.move(450,250);
    await video.waitForFunction(()=>document.querySelector('#adl-video-dl')?.style.display==='block');
    await video.evaluate(()=>window.__videoFail=true);
    await video.evaluate(()=>window.__videoRoots[0].querySelector('button').click());
    await video.waitForFunction(()=>window.__videoRoots[0].querySelector('.toast').textContent.includes('主程序未运行'));
    await video.evaluate(()=>window.__videoFail=false);
    await video.evaluate(()=>window.__videoRoots[0].querySelector('button').click());
    await video.waitForFunction(()=>window.__videoRoots[0].querySelector('.toast').textContent.includes('已创建下载任务'));
    await shot(video,'video-download');
    await video.emulateMedia({colorScheme:'dark'});
    await shot(video,'video-download-dark');

    const highlights = await videoContext.newPage();
    highlights.on('pageerror',error=>errors.push(error.message));
    await highlights.setContent('<html><body><p style="margin:100px;font-size:18px">保存这段文章摘录用于离线阅读。</p></body></html>');
    await highlights.evaluate(()=>{window.ADL={};});
    await highlights.addScriptTag({path:path.join(root,'highlights.js')});
    const selectHighlight = async () => {
      await highlights.evaluate(()=>{
        const paragraph=document.querySelector('p');
        const range=document.createRange(); range.selectNodeContents(paragraph);
        const selection=window.getSelection(); selection.removeAllRanges(); selection.addRange(range);
        paragraph.dispatchEvent(new MouseEvent('mouseup',{bubbles:true}));
      });
      await highlights.locator('#adl-hl-btn').waitFor();
    };
    await selectHighlight();
    await highlights.getByRole('button',{name:'✚ 标注',exact:true}).click();
    await highlights.locator('#adl-hl-tip').waitFor();
    assert.ok((await highlights.locator('#adl-hl-tip').textContent()).includes('已保存标注'));
    assert.equal(await highlights.evaluate(()=>ADL.pageHighlights().length),1);
    await highlights.evaluate(()=>document.querySelector('p').textContent='另一段需要保存的摘录');
    await selectHighlight();
    await highlights.evaluate(()=>window.__storageFail=true);
    await highlights.getByRole('button',{name:'✚ 标注',exact:true}).click();
    await highlights.getByRole('alert').waitFor();
    assert.equal(await highlights.evaluate(()=>ADL.pageHighlights().length),1,'failed saves do not enter the in-memory mirror');
    assert.deepEqual(errors,[]);
    console.log('PASS: popup resources/filter/select/push/resolve, tasks/control, settings/focus/storage, takeover, offline state; article extraction/editor/platforms/draft sync/CMS/responsive; closed-shadow download panel/focus/actions/retry/themes; video overlay/download feedback; highlight save/failure; no page errors.');
  } finally { await browser.close(); }
})().catch(error=>{console.error(error);process.exitCode=1;});
