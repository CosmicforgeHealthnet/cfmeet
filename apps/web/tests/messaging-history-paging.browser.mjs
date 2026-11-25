import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile, writeFile, mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join, extname } from 'node:path';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';

// Uses only the packaged worker/WASM and Chromium IndexedDB, never a fake MLS store.
const repo = resolve(process.argv[2] ?? '.');
const output = process.argv[3];
const { chromium } = createRequire(join(repo, 'apps/web/package.json'))('playwright');
const root = join(repo, 'apps/web/public/messaging-crypto');
const manifest = JSON.parse(await readFile(join(root, 'manifest.json'), 'utf8'));
const assets = new Map();
for (const [name, metadata] of Object.entries(manifest.files)) {
  let bytes = await readFile(join(root, name));
  if (name.endsWith('.js')) bytes = Buffer.from(bytes.toString('utf8').replaceAll('\r\n', '\n'));
  assert.equal(bytes.length, metadata.bytes);
  assert.equal('sha384-' + createHash('sha384').update(bytes).digest('base64'), metadata.integrity);
  assets.set(name, bytes);
}
const role = value => Object.entries(manifest.files).find(([, metadata]) => metadata.role === value)[0];
const server = createServer((req, res) => {
  const name = new URL(req.url, 'http://local').pathname.slice(1);
  if (!name) { res.end('<html></html>'); return; }
  if (name === 'bounded-worker.js') {
    res.setHeader('Content-Type', 'text/javascript');
    res.end(`import '/${role('worker')}';
      let paging=null;
      for(const method of ['getAll','getAllKeys']) {
        const original=IDBObjectStore.prototype[method];
        IDBObjectStore.prototype[method]=function(...args) {
          if(paging!==null&&this.name==='records') throw Error('UNBOUNDED_RECORD_SCAN');
          return original.apply(this,args);
        };
      }
      const respond=self.postMessage.bind(self);
      self.postMessage=(data,...args)=>{if(data.id===paging)paging=null;return respond(data,...args);};
      const handle=self.onmessage;
      self.onmessage=event=>{if(event.data.op==='historyPage')paging=event.data.id;handle(event);};`);
    return;
  }
  if (!assets.has(name)) { res.writeHead(404).end(); return; }
  res.setHeader('Content-Type', extname(name) === '.wasm' ? 'application/wasm' : 'text/javascript');
  res.end(assets.get(name));
});
await new Promise(done => server.listen(0, '127.0.0.1', done));
const browser = await chromium.launchPersistentContext(await mkdtemp(join(tmpdir(), 'cfmeet-history-paging-')));
try {
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  const evidence = await page.evaluate(async ({ worker, glue, wasm }) => {
    const core = await import('/' + glue); await core.default({ module_or_path: '/' + wasm });
    const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
    const account = id(4), deviceId = id(5), senderId = id(6), conversationId = id(7), groupId = new Uint8Array(32).fill(9);
    const scope = { tenantId: id(1), applicationId: id(2), environmentId: id(3) };
    let w, serial = 0; const waiting = new Map();
    const start = () => {
      w = new Worker('/bounded-worker.js', { type: 'module' });
      w.onmessage = ({ data }) => { const pending = waiting.get(data.id); waiting.delete(data.id); data.ok ? pending.resolve(data.result) : pending.reject(Error(data.error)); };
    };
    const call = (op, args = {}) => new Promise((resolve, reject) => {
      const number = ++serial;
      const timer = setTimeout(() => { waiting.delete(number); reject(Error('WORKER_TIMEOUT')); }, 30_000);
      waiting.set(number, { resolve: value => { clearTimeout(timer); resolve(value); }, reject: error => { clearTimeout(timer); reject(error); } });
      w.postMessage({ id: number, op, args });
    });
    const checks = [];
    const check = (name, condition) => { if (!condition) throw Error(name); checks.push({ name, result: 'CONFIRMED' }); };
    const b64 = bytes => btoa(String.fromCharCode(...bytes)).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
    const read = args => call('historyPage', { conversationId, ...args });
    start(); await call('open', { account, scope }); await call('generate', { deviceId }); await call('adopt');
    const kp = (await call('keyPackages', { count: 1 }))[0];
    const sender = new core.WebDevice(senderId); sender.createGroup(groupId);
    const packed = new Uint8Array(kp.length + 4); new DataView(packed.buffer).setUint32(0, kp.length); packed.set(kp, 4);
    await call('join', { groupId, welcome: sender.addDevices(groupId, packed).welcome });
    for (let first = 1; first <= 600; first += 50) {
      const messages = [];
      for (let sequence = first; sequence < first + 50; sequence++) {
        const clientMessageId = id(1000 + sequence), control = sequence >= 201 && sequence <= 400;
        const aad = core.message_aad(scope.tenantId, scope.applicationId, scope.environmentId, conversationId, clientMessageId, senderId);
        const ciphertext = sender.encrypt(groupId, aad, new TextEncoder().encode(JSON.stringify({ v: 1, t: 'text', text: `private ${sequence}` })));
        messages.push({ sequence, messageId: id(10000 + sequence), clientMessageId, senderDeviceId: senderId, senderPrincipalId: id(8),
          kind: control ? 'EDIT' : 'APPLICATION', ...(control ? { targetMessageId: id(10001) } : {}), lifecycle: 'ACTIVE',
          createdAt: '2026-11-25T00:00:00Z', epoch: Number(sender.epoch(groupId)), ciphertext: b64(ciphertext) });
      }
      const result = await call('processMessages', { conversationId, groupId, messages });
      check(`real MLS decrypt batch ${first}`, result.records.length === 50 && result.records.every(record => record.status === 'DECRYPTED'));
    }
    const newest = await read({});
    check('newest page bounded at 200 ascending records', newest.records.length === 200 && newest.records[0].sequence === 401 && newest.records[199].sequence === 600 && newest.nextBeforeSequence === 401);
    const controls = await read({ beforeSequence: newest.nextBeforeSequence });
    check('control-only middle page retains a raw continuation', controls.records.length === 200 && controls.records.every(record => record.kind === 'EDIT') && controls.nextBeforeSequence === 201);
    const oldest = await read({ beforeSequence: controls.nextBeforeSequence });
    check('old messages beyond 200 remain navigable without overlap', oldest.records.length === 200 && oldest.records[0].sequence === 1 && oldest.records[199].sequence === 200 && oldest.nextBeforeSequence === null && new Set([...oldest.records, ...controls.records, ...newest.records].map(record => record.sequence)).size === 600);
    const one = await read({ limit: 1 });
    check('limit one uses an exclusive cursor', one.records[0].sequence === 600 && one.nextBeforeSequence === 600 && (await read({ limit: 1, beforeSequence: 600 })).records[0].sequence === 599);
    check('empty conversation terminates', (await call('historyPage', { conversationId: id(99) })).nextBeforeSequence === null);
    const below = await read({ beforeSequence: 1 });
    check('empty exclusive lower boundary terminates', below.records.length === 0 && below.nextBeforeSequence === null);
    for (const args of [{ limit: 0 }, { limit: 201 }, { limit: 1.5 }, { beforeSequence: 0 }, { beforeSequence: 1.5 }]) {
      let refused = false; try { await read(args); } catch (error) { refused = ['BAD_LIMIT', 'BAD_CURSOR'].includes(error.message); }
      check(`invalid bounds refused ${JSON.stringify(args)}`, refused);
    }
    await call('reconcileHistoryMetadata', { conversationId, messages: [{ sequence: 1, messageId: id(10001), lifecycle: 'DELETED' }] });
    const removed = await read({ beforeSequence: 201 });
    check('older tombstone redacts plaintext', removed.records[0].status === 'REMOVED' && removed.records[0].payload === undefined);
    const removedControls = await read({ beforeSequence: 401 });
    check('control records obey target tombstones during paging', removedControls.records.every(record => record.status === 'REMOVED' && record.payload === undefined));
    w.terminate(); await new Promise(done => setTimeout(done, 100)); start();
    check('restart reopens the same durable identity', (await call('open', { account, scope })).deviceId === deviceId);
    const restarted = await read({ beforeSequence: 201 });
    check('restart preserves older page and terminal redaction', restarted.records.length === 200 && restarted.records[0].status === 'REMOVED' && restarted.records[0].payload === undefined);
    await call('processMessages', { conversationId, groupId, messages: [], visibleFromSequence: 125 });
    const bounded = await read({ beforeSequence: 201 });
    check('visibility lower bound excludes retained older records', bounded.records.length === 76 && bounded.records[0].sequence === 125 && bounded.nextBeforeSequence === null);
    check('cursor below visibility bound returns empty', (await read({ beforeSequence: 125 })).records.length === 0);
    w.terminate(); sender.free();
    return { boundary: 'packaged worker, real WASM MLS, real Chromium IndexedDB; no backend or authentication service', checks };
  }, { worker: role('worker'), glue: role('glue'), wasm: role('wasm') });
  if (output) await writeFile(output, JSON.stringify({ at: new Date().toISOString(), ...evidence }, null, 2));
  console.log(JSON.stringify(evidence, null, 2));
} finally {
  await browser.close(); server.closeAllConnections(); await new Promise(done => server.close(done));
}
