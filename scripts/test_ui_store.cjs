const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join, dirname, resolve } = require('node:path');
const { createRequire } = require('node:module');
const ts = require('../ui/node_modules/typescript');
const sourceDir = resolve(__dirname, '../ui/src');
const nativeRequire = createRequire(join(sourceDir, '../package.json'));

// Load the actual store and API through the project's TypeScript compiler.
// Only the async task source is replaced; no Tauri window is needed.
function fixture(tasks) {
  const modules = new Map();
  function load(file) {
    if (modules.has(file)) return modules.get(file).exports;
    const module = { exports: {} };
    modules.set(file, module);
    const { outputText } = ts.transpileModule(readFileSync(file, 'utf8'), {
      compilerOptions: { target: ts.ScriptTarget.ES2021, module: ts.ModuleKind.CommonJS },
      fileName: file,
    });
    const requireFile = request => request.startsWith('.')
      ? load(resolve(dirname(file), request + '.ts')) : nativeRequire(request);
    new Function('require', 'module', 'exports', outputText)(requireFile, module, module.exports);
    return module.exports;
  }
  const { api } = load(join(sourceDir, 'api.ts'));
  let updates = [];
  api.listTasks = async () => tasks;
  api.tasksUpdatedSince = async () => updates;
  return { store: load(join(sourceDir, 'store.ts')).store, setUpdates: rows => { updates = rows; } };
}

const task = (id, extra = {}) => ({
  id, status: 'downloading', filename: id + '.bin', url: 'https://example.com/' + id,
  total_bytes: 1000, downloaded_bytes: 0, created_at: 1, updated_at: 1, ...extra,
});
const progress = (id, downloaded, extra = {}) => ({
  type: 'progress', task_id: id, downloaded, bps: 500, total: 1000, segments: null, ...extra,
});

test('100 progress events keep a 10,000-task list stable while live values update', async () => {
  const { store } = fixture(Array.from({ length: 10000 }, (_, i) => task(String(i))));
  await store.refresh();
  const snapshot = store.getSnapshot();
  let notifications = 0;
  const unsubscribe = store.subscribe(() => { notifications++; });
  for (let i = 1; i <= 100; i++) store.applyEvent(progress('0', i));
  assert.equal(store.getSnapshot(), snapshot);
  assert.equal(store.liveOf('0').downloaded, 100);
  assert.equal(notifications, 100);
  unsubscribe();
});

test('new total size reaches the task snapshot and preserves live progress', async () => {
  const { store } = fixture([task('a', { total_bytes: null })]);
  await store.refresh();
  const snapshot = store.getSnapshot();
  store.applyEvent(progress('a', 200));
  assert.notEqual(store.getSnapshot(), snapshot);
  assert.equal(store.getSnapshot()[0].total_bytes, 1000);
  assert.equal(store.liveOf('a').downloaded, 200);
});

test('pausing preserves the latest progress and clears the live speed', async () => {
  const { store } = fixture([task('a')]);
  await store.refresh();
  store.applyEvent(progress('a', 650));
  store.applyEvent({ type: 'state_changed', task_id: 'a', status: 'paused' });
  assert.equal(store.getSnapshot()[0].status, 'paused');
  assert.equal(store.getSnapshot()[0].downloaded_bytes, 650);
  assert.equal(store.liveOf('a'), undefined);
});

test('incremental terminal states clear live data when no state event arrives', async () => {
  const { store, setUpdates } = fixture([task('a')]);
  await store.refresh();
  store.applyEvent(progress('a', 650));
  setUpdates([task('a', { status: 'done', downloaded_bytes: 1000, updated_at: Date.now() })]);
  await store.pollIncremental();
  assert.equal(store.getSnapshot()[0].status, 'done');
  assert.equal(store.liveOf('a'), undefined);
});

test('late progress for a removed task does not recreate it', async () => {
  const { store } = fixture([]);
  await store.refresh();
  const snapshot = store.getSnapshot();
  store.applyEvent(progress('gone', 100));
  assert.equal(store.getSnapshot(), snapshot);
  assert.equal(store.liveOf('gone'), undefined);
});

test('queued tasks sort by priority DESC then queue_order ASC ahead of paused and done', async () => {
  const { store } = fixture([
    task('done1', { status: 'done', created_at: 100 }),
    task('paused1', { status: 'paused', created_at: 200 }),
    task('q_low', { status: 'queued', priority: 0, queue_order: 30, created_at: 30 }),
    task('q_first', { status: 'queued', priority: 0, queue_order: 10, created_at: 50 }),
    task('q_high', { status: 'queued', priority: 30, queue_order: 99, created_at: 10 }),
    task('active1', { status: 'downloading', created_at: 5 }),
  ]);
  await store.refresh();
  assert.deepEqual(
    store.getSnapshot().map((t) => t.id),
    ['active1', 'q_high', 'q_first', 'q_low', 'paused1', 'done1']
  );
});

test('segment speeds track segments by start offset across dynamic splits', async () => {
  const { store } = fixture([task('a', { total_bytes: 1000 })]);
  await store.refresh();
  store.applyEvent(
    progress('a', 200, {
      segments: [
        { start: 0, written: 100, end: 500 },
        { start: 500, written: 100, end: 1000 },
      ],
    })
  );
  // Wait > 200ms so segSpeeds delta computes
  await new Promise((r) => setTimeout(r, 230));
  store.applyEvent(
    progress('a', 550, {
      segments: [
        { start: 0, written: 200, end: 300 },
        { start: 300, written: 50, end: 500 }, // dynamically split new segment
        { start: 500, written: 300, end: 1000 },
      ],
    })
  );
  const live = store.liveOf('a');
  assert.equal(live.segSpeeds.length, 3);
  assert.ok(live.segSpeeds[0] > 0, 'seg 0 speed computed');
  assert.equal(live.segSpeeds[1], 0, 'newly split seg starts at 0 without false delta');
  assert.ok(live.segSpeeds[2] > live.segSpeeds[0], 'seg at start=500 kept its baseline');
});

test('totalBps equals exact sum of downloading tasks and ignores paused/verifying/done', async () => {
  const { store } = fixture([
    task('d1', { status: 'downloading' }),
    task('d2', { status: 'downloading' }),
    task('p1', { status: 'paused' }),
  ]);
  await store.refresh();
  assert.equal(store.getTotalBps(), 0);

  store.applyEvent(progress('d1', 100, { bps: 3000 }));
  store.applyEvent(progress('d2', 200, { bps: 7000 }));
  store.applyEvent(progress('p1', 50, { bps: 9999 }));
  assert.equal(store.getTotalBps(), 10000);

  // Transitioning d1 to verifying immediately removes its speed from totalBps
  store.applyEvent({ type: 'state_changed', task_id: 'd1', status: 'verifying' });
  assert.equal(store.getTotalBps(), 7000);
  assert.equal(store.liveOf('d1').bps, 0);

  // Pausing d2 drops totalBps to 0
  store.applyEvent({ type: 'state_changed', task_id: 'd2', status: 'paused' });
  assert.equal(store.getTotalBps(), 0);
});

test('pruneStaleSpeeds decays old speeds without breaking list snapshot reference', async () => {
  const { store } = fixture([task('d1', { status: 'downloading' })]);
  await store.refresh();
  const snap = store.getSnapshot();
  store.applyEvent(progress('d1', 300, { bps: 4500 }));
  assert.equal(store.getTotalBps(), 4500);
  await new Promise((r) => setTimeout(r, 20));
  store.pruneStaleSpeeds(5);
  assert.equal(store.getTotalBps(), 0);
  assert.equal(store.liveOf('d1').bps, 0);
  assert.equal(store.getSnapshot(), snap);
});
