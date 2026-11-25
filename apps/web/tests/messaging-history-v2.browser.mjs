import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile, writeFile, mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, extname } from 'node:path';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';

const repo = resolve(process.argv[2] ?? '.');
const output = process.argv[3];
const require = createRequire(join(repo, 'apps/web/package.json'));
const { chromium } = require('playwright');
const publicDir = join(repo, 'apps/web/public/messaging-crypto');
const manifest = JSON.parse(await readFile(join(publicDir, 'manifest.json'), 'utf8'));
const assets = new Map();
for (const [file, metadata] of Object.entries(manifest.files)) {
  let bytes = await readFile(join(publicDir, file));
  if (file.endsWith('.js')) bytes = Buffer.from(bytes.toString('utf8').replaceAll('\r\n', '\n'));
  assert.equal(bytes.length, metadata.bytes);
  assert.equal('sha384-' + createHash('sha384').update(bytes).digest('base64'), metadata.integrity);
  assets.set(file, bytes);
}
const worker = Object.entries(manifest.files).find(([, value]) => value.role === 'worker')[0];
const server = createServer((req, res) => {
  const name = new URL(req.url, 'http://local').pathname.slice(1);
  if (name === '') { res.end('<html></html>'); return; }
  res.setHeader('Content-Type', extname(name) === '.wasm' ? 'application/wasm' : 'text/javascript');
  if (name === 'test-worker.js') {
    res.end(`import '/${worker}';
      let armed=false;
      const original=IDBDatabase.prototype.transaction;
      IDBDatabase.prototype.transaction=function(stores,mode,...rest){
        const tx=original.call(this,stores,mode,...rest);
        if(armed&&mode==='readwrite'&&Array.from(typeof stores==='string'?[stores]:stores).includes('devices')){armed=false;queueMicrotask(()=>tx.abort());}
        return tx;
      };
      const handler=self.onmessage;
      self.onmessage=e=>{if(e.data?.op==='_abortNextWrite'){armed=true;self.postMessage({id:e.data.id,ok:true,result:true});}else handler(e);};`);
    return;
  }
  if (!assets.has(name)) { res.writeHead(404).end(); return; }
  res.end(assets.get(name));
});
await new Promise((done) => server.listen(0, '127.0.0.1', done));
const origin = `http://127.0.0.1:${server.address().port}`;
const profiles = await Promise.all([mkdtemp(join(tmpdir(), 'cfmeet-v2-source-')), mkdtemp(join(tmpdir(), 'cfmeet-v2-target-'))]);
let sourceBrowser, targetBrowser;
const wire = (value) => {
  if (value instanceof Uint8Array) return { bytes: Buffer.from(value).toString('base64url'), wireBytes: true };
  if (Array.isArray(value)) return value.map(wire);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, wire(item)]));
  return value;
};
const unwire = (value) => {
  if (value?.wireBytes === true) return new Uint8Array(Buffer.from(value.bytes, 'base64url'));
  if (Array.isArray(value)) return value.map(unwire);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, unwire(item)]));
  return value;
};
async function pageFor(browser) {
  const page = await browser.newPage();
  await page.goto(origin);
  await page.evaluate(() => {
    let worker, serial = 0;
    const waiting = new Map();
    window.startWorker = () => {
      worker = new Worker('/test-worker.js', { type: 'module' });
      worker.onmessage = ({ data }) => {
        const promise = waiting.get(data.id); waiting.delete(data.id);
        data.ok ? promise.resolve(data.result) : promise.reject(Error(data.error));
      };
    };
    window.stopWorker = () => worker.terminate();
    window.callWorker = (op, args) => new Promise((resolve, reject) => {
      const id = ++serial;
      waiting.set(id, { resolve, reject }); worker.postMessage({ id, op, args });
      setTimeout(() => { if (waiting.delete(id)) reject(Error('WORKER_TIMEOUT')); }, 30_000);
    });
    window.startWorker();
  });
  return page;
}
async function call(page, op, args = {}) {
  return unwire(await page.evaluate(async ({ op, args }) => {
    const decode = (v) => {
      if (v?.wireBytes === true) return Uint8Array.from(atob(v.bytes.replaceAll('-', '+').replaceAll('_', '/')), (c) => c.charCodeAt(0));
      if (Array.isArray(v)) return v.map(decode);
      if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, item]) => [k, decode(item)]));
      return v;
    };
    const encode = (v) => {
      if (v instanceof Uint8Array) {
        let s = ''; for (let i = 0; i < v.length; i += 0x8000) s += String.fromCharCode(...v.subarray(i, i + 0x8000));
        return { bytes: btoa(s).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, ''), wireBytes: true };
      }
      if (Array.isArray(v)) return v.map(encode);
      if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, item]) => [k, encode(item)]));
      return v;
    };
    return encode(await window.callWorker(op, decode(args)));
  }, { op, args: wire(args) }));
}
const id = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
const b64 = (bytes) => Buffer.from(bytes).toString('base64url');
const scope = { tenantId: id(1), applicationId: id(2), environmentId: id(3) }, account = id(4), sourceId = id(5), targetId = id(6);
const conversations = [id(7), id(8)], groups = [new Uint8Array(32).fill(7), new Uint8Array(32).fill(8)], sequences = [0, 0];
const events = [];
const confirmed = (name) => events.push({ name, result: 'CONFIRMED' });
try {
  sourceBrowser = await chromium.launchPersistentContext(profiles[0]);
  targetBrowser = await chromium.launchPersistentContext(profiles[1]);
  let source = await pageFor(sourceBrowser), target = await pageFor(targetBrowser);
  for (const [page, deviceId] of [[source, sourceId], [target, targetId]]) {
    await call(page, 'open', { account, scope }); await call(page, 'generate', { deviceId }); await call(page, 'adopt');
  }
  for (const groupId of groups) { await call(source, 'createGroup', { groupId }); await call(source, 'confirm'); }
  async function send(which, payload, extra = {}) {
    const sequence = ++sequences[which], clientMessageId = id(100 + which * 100 + sequence);
    const encrypted = await call(source, 'encrypt', { conversationId: conversations[which], groupId: groups[which], clientMessageId, payload });
    const processed = await call(source, 'processMessages', { conversationId: conversations[which], groupId: groups[which], messages: [{
      sequence, messageId: id(1000 + which * 100 + sequence), clientMessageId, senderDeviceId: sourceId, senderPrincipalId: account,
      kind: extra.kind ?? 'APPLICATION', targetMessageId: extra.targetMessageId, lifecycle: 'ACTIVE', createdAt: '2026-11-24T00:00:00Z', epoch: encrypted.epoch, ciphertext: b64(encrypted.ciphertext),
    }] });
    assert.equal(processed.records[0].status, 'SENT');
  }
  for (let i = 0; i < 80; i++) await send(i % 2, { t: 'text', text: `${i}:` + '\u00e9'.repeat(7990) });
  const filePlaintext = new Uint8Array([12, 34, 56, 78]);
  const attachment = await call(source, 'encryptFile', { data: filePlaintext, mediaType: 'application/octet-stream', fileName: 'history.bin' });
  await send(0, { t: 'file', handle: attachment.handle, attachmentId: id(9000) });
  const removedBaseId = id(1001), deletedEditText = 'deleted edit must never reach recovered plaintext';
  await send(0, { t: 'text', text: deletedEditText }, { kind: 'EDIT', targetMessageId: removedBaseId });
  await send(0, { t: 'reaction', target: id(1041), reaction: ':visible:', add: true });
  await send(0, { t: 'reaction', target: removedBaseId, reaction: ':deleted:', add: true });

  const eligibleConversations = conversations.map((conversationId, i) => ({ conversationId, firstSequence: 1, lastSequence: sequences[i] }));
  const context = [id(20), scope.tenantId, scope.applicationId, scope.environmentId, account, sourceId, targetId];
  const expiresAt = Math.floor(Date.now() / 1000) + 600;
  const transferKey = await call(target, 'beginTransferV2', { context, expiresAt });
  const signature = await call(target, 'signTransferRequestV2', { context, expiresAt });
  const targetPublic = (await call(target, 'status')).signaturePublicKey;
  assert.equal(await call(source, 'verifyTransferRequestV2', { targetKey: targetPublic, context, transferKey, expiresAt, signature }), true);
  const snapshot = await call(source, 'prepareHistoryV2', { context, transferKey, expiresAt, eligibleConversations });
  assert.ok(snapshot.descriptor.totalPlaintextBytes > 1024 * 1024); assert.ok(snapshot.descriptor.chunkCount > 2);
  assert.equal(snapshot.coverage, 'SOURCE_AVAILABLE'); confirmed('more than 1 MiB and two conversations form a complete bounded snapshot');
  // Freeze a valid archive, then remove its target in a separate authoritative page.
  // The target has no local base record: it must still reject the stale EDIT payload.
  const removalPage = { conversationId: conversations[0], messages: [{ sequence: 1, messageId: removedBaseId, lifecycle: 'DELETED' }] };
  await call(source, 'reconcileHistoryMetadata', removalPage);
  await call(target, 'reconcileHistoryMetadata', removalPage);
  const exported = await call(source, 'exportLocalHistory', { account });
  assert.equal(exported.records.find(r => r.conversationId === conversations[0] && r.sequence === sequences[0]).status, 'REMOVED');
  assert.equal(JSON.stringify(exported).includes(deletedEditText), false);
  confirmed('authoritative target tombstones redact source plaintext exports across page boundaries');

  const sourceKey = (await call(source, 'status')).signaturePublicKey;
  const sealedManifest = await call(source, 'historyObjectV2', { transferId: context[0], index: 0xffffffff });
  // These attacker-controlled lengths overflowed unchecked usize arithmetic on
  // wasm32. Refusal must leave the runtime and the reusable target key functional.
  const hostileLength = new Uint8Array(73); hostileLength[0] = 2; hostileLength.set([255,255,255,255],1);
  await assert.rejects(call(target, 'acceptHistoryManifestV2', { context, descriptor: snapshot.descriptor, sourceKey, sealed: hostileLength }));
  assert.equal((await call(target, 'historyProgressV2', { transferId: context[0] })).nextIndex, 0);
  confirmed('malformed wasm32 frame lengths are refused without losing the runtime');
  const damagedManifest = sealedManifest.slice(); damagedManifest[damagedManifest.length - 1] ^= 1;
  await assert.rejects(call(target, 'acceptHistoryManifestV2', { context, descriptor: snapshot.descriptor, sourceKey, sealed: damagedManifest }));
  assert.equal((await call(target, 'historyProgressV2', { transferId: context[0] })).nextIndex, 0);
  confirmed('tampered manifest does not consume target transfer keys');
  await call(target, 'acceptHistoryManifestV2', { context, descriptor: snapshot.descriptor, sourceKey, sealed: sealedManifest });
  const sealedFirst = await call(source, 'historyObjectV2', { transferId: context[0], index: 0 });
  const damagedFirst = sealedFirst.slice(); damagedFirst[damagedFirst.length - 1] ^= 1;
  await assert.rejects(call(target, 'importHistoryChunkV2', { transferId: context[0], index: 0, sealed: damagedFirst, eligibleConversations }));
  confirmed('corrupt chunk does not advance durable progress');
  await assert.rejects(call(target, 'importHistoryChunkV2', { transferId: context[0], index: 1, sealed: sealedFirst, eligibleConversations }));
  const first = await call(target, 'importHistoryChunkV2', { transferId: context[0], index: 0, sealed: sealedFirst, eligibleConversations });
  const duplicate = await call(target, 'importHistoryChunkV2', { transferId: context[0], index: 0, sealed: sealedFirst, eligibleConversations });
  assert.equal(duplicate.records, first.records); confirmed('duplicate chunks import once and out-of-order chunks are refused');
  await sourceBrowser.close(); sourceBrowser = await chromium.launchPersistentContext(profiles[0]); source = await pageFor(sourceBrowser);
  assert.equal((await call(source, 'open', { account, scope })).deviceId, sourceId);
  assert.deepEqual(await call(source, 'prepareHistoryV2', { context, transferKey, expiresAt, eligibleConversations }), snapshot);
  assert.deepEqual(await call(source, 'historyObjectV2', { transferId: context[0], index: 0 }), sealedFirst);
  confirmed('source process restart preserves immutable snapshot and byte-identical sealed chunks');
  await send(1, { t: 'text', text: 'after frozen snapshot' });
  await targetBrowser.close(); targetBrowser = await chromium.launchPersistentContext(profiles[1]); target = await pageFor(targetBrowser);
  assert.equal((await call(target, 'open', { account, scope })).deviceId, targetId);
  assert.equal((await call(target, 'historyProgressV2', { transferId: context[0] })).nextIndex, 1);
  const second = await call(source, 'historyObjectV2', { transferId: context[0], index: 1 });
  await call(target, '_abortNextWrite');
  await assert.rejects(call(target, 'importHistoryChunkV2', { transferId: context[0], index: 1, sealed: second, eligibleConversations }));
  assert.equal((await call(target, 'historyProgressV2', { transferId: context[0] })).nextIndex, 1);
  confirmed('target process restart restores private transfer key and storage abort preserves checkpoint');
  await assert.rejects(call(target, 'importHistoryChunkV2', { transferId: context[0], index: 1, sealed: second, eligibleConversations: [] }));
  confirmed('membership visibility limits are enforced before import');
  let last;
  for (let index = 1; index < snapshot.descriptor.chunkCount; index++) {
    const sealed = index === 1 ? second : await call(source, 'historyObjectV2', { transferId: context[0], index });
    last = await call(target, 'importHistoryChunkV2', { transferId: context[0], index, sealed, eligibleConversations });
  }
  assert.equal(last.records, 84); assert.equal(last.nextIndex, snapshot.descriptor.chunkCount); assert.ok(last.signature.length > 0);
  const history = await Promise.all(conversations.map((conversationId) => call(target, 'history', { conversationId })));
  assert.equal(history.flat().length, 84); assert.equal(history[1].some((r) => r.payload?.text === 'after frozen snapshot'), false);
  assert.equal(history[0].find(r => r.messageId === removedBaseId).status, 'REMOVED');
  assert.equal(history[0].find(r => r.kind === 'EDIT').status, 'REMOVED');
  assert.equal(JSON.stringify(history).includes(deletedEditText), false);
  const liveReaction = history[0].find(record => record.payload?.t === 'reaction');
  assert.deepEqual(liveReaction.payload, { t: 'reaction', target: id(1041), reaction: ':visible:', add: true });
  assert.equal(liveReaction.privateControl, 'REACTION');
  assert.equal(history[0].some(record => record.privateControl === 'REACTION' && record.status === 'REMOVED' && record.payload === undefined), true);
  assert.equal(JSON.stringify(history).includes(':deleted:'), false);
  confirmed('encrypted reaction controls survive history recovery while deleted-target reactions stay redacted');

  confirmed('stale frozen archives cannot resurrect deleted EDIT plaintext on a new device');
  const file = history[0].find((r) => r.payload?.t === 'file'); assert.ok(file);
  const decrypted = await call(target, 'decryptAttachment', { conversationId: conversations[0], sequence: file.sequence, ciphertext: attachment.ciphertext });
  assert.deepEqual(decrypted.data, filePlaintext);
  for (const groupId of groups) assert.equal(await call(target, 'hasGroup', { groupId }), false);
  confirmed('complete resumed history includes attachment keys and never imports MLS group secrets');
  await call(target, 'forgetHistoryTransferV2', { transferId: context[0] });
  await assert.rejects(call(target, 'historyProgressV2', { transferId: context[0] }));
  assert.equal((await call(target, 'history', { conversationId: conversations[0] })).length, 44);
  confirmed('transfer cleanup removes checkpoint secrets while retaining authorized history');
  const expiringContext = [...context]; expiringContext[0] = id(99);
  await call(target, 'beginTransferV2', { context: expiringContext, expiresAt: Math.floor(Date.now() / 1000) + 2 });
  await new Promise(resolve => setTimeout(resolve, 2200));
  const remaining = await call(target, 'listHistoryTransfersV2');
  assert.equal(remaining.some(t => t.transferId === expiringContext[0]), false);
  await assert.rejects(call(target, 'historyProgressV2', { transferId: expiringContext[0] }), /NO_TRANSFER/);
  assert.equal((await call(target, 'history', { conversationId: conversations[0] })).length, 44);
  confirmed('runtime expiry removes private transfer checkpoints without deleting recovered history');
  const evidence = { at: new Date().toISOString(), boundary: 'real shipped worker/WASM, two independent Chromium profiles, IndexedDB; server relay and authentication tested separately', events };
  if (output) await writeFile(output, JSON.stringify(evidence, null, 2));
  process.stdout.write(JSON.stringify(evidence, null, 2) + '\n');
} finally {
  if (sourceBrowser) await sourceBrowser.close(); if (targetBrowser) await targetBrowser.close();
  server.closeAllConnections(); await new Promise((done) => server.close(done));
}
