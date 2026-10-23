// Cross-SDK interop peer (parity scenario `messaging.e2ee.crossSdk`): ONE Messaging device
// run by the TypeScript SDK (the wasm crypto core through the SDK adapter, every request
// through the SDK's typed accessors), driven over JSON lines by another SDK's suite.
//
//   node build-test/test/support/interop-peer.js <stack-env.json> <principalId>
//
// stdout, first line: {"event":"ready","sdk":"typescript","deviceId":...} once the device
// is registered and has published one KeyPackage. Then one JSON answer per stdin command:
//   {"cmd":"join","conversationId"}                    -> {"ok":true,"epoch"}
//   {"cmd":"receive","conversationId","afterSequence"} -> {"ok":true,"messages":[{sequence,messageId,plaintext,senderDeviceId}]}
//   {"cmd":"send","conversationId","text"}             -> {"ok":true,"messageId","sequence","ciphertext"}
//   {"cmd":"exit"}                                     -> exits 0
// Failures answer {"ok":false,"error":...}. The Dart peer (sdks/dart/test/support/
// interop_peer.dart) speaks the same protocol.

import { createInterface } from 'node:readline';

import {
  cleanupTmp,
  credentialAuth,
  joinFromWelcome,
  loadShippedCrypto,
  readStackEnv,
  receiveDecrypted,
  registerDevice,
  sendEncrypted,
  stackClient,
} from './stack.js';

const [envFile, principalId] = process.argv.slice(2);
if (!envFile || !principalId) {
  process.stderr.write('usage: interop-peer.js <stack-env.json> <principalId>\n');
  process.exit(2);
}

const say = (o: unknown) => process.stdout.write(`${JSON.stringify(o)}\n`);

async function main(): Promise<void> {
  const env = readStackEnv(envFile!);
  const tenant = env.tenants.sandbox;
  const c = stackClient(env, credentialAuth(env.credentials.sandbox));
  const crypto = await loadShippedCrypto();
  const me = await registerDevice(c, crypto, principalId!, 'sdk-parity interop peer (typescript)');
  const groups = new Map<string, Uint8Array>();
  say({ event: 'ready', sdk: 'typescript', deviceId: me.deviceId });

  const rl = createInterface({ input: process.stdin, crlfDelay: Infinity });
  for await (const line of rl) {
    if (!line.trim()) continue;
    try {
      const cmd = JSON.parse(line) as { cmd: string; conversationId?: string; afterSequence?: number; text?: string };
      const cid = cmd.conversationId ?? '';
      if (cmd.cmd === 'exit') break;
      if (cmd.cmd === 'join') {
        const { groupId, epoch } = await joinFromWelcome(c, me, cid);
        groups.set(cid, groupId);
        say({ ok: true, epoch: Number(epoch) });
      } else if (cmd.cmd === 'receive') {
        const groupId = groups.get(cid);
        if (!groupId) throw new Error('not joined');
        const messages = await receiveDecrypted(c, crypto, tenant, cid, me, groupId, cmd.afterSequence ?? 0);
        say({ ok: true, messages: messages.map(({ sequence, messageId, plaintext, senderDeviceId }) => ({ sequence, messageId, plaintext, senderDeviceId })) });
      } else if (cmd.cmd === 'send') {
        const groupId = groups.get(cid);
        if (!groupId) throw new Error('not joined');
        const { sent, ciphertext } = await sendEncrypted(c, crypto, tenant, cid, me, groupId, cmd.text ?? '');
        say({ ok: true, messageId: sent.data.messageId, sequence: sent.data.sequence, ciphertext });
      } else {
        throw new Error(`unknown command ${cmd.cmd}`);
      }
    } catch (e) {
      say({ ok: false, error: e instanceof Error ? `${e.name}: ${e.message}` : String(e) });
    }
  }
  rl.close();
}

main()
  .then(() => {
    cleanupTmp();
    process.exit(0);
  })
  .catch((e: unknown) => {
    cleanupTmp();
    say({ event: 'failed', error: e instanceof Error ? `${e.name}: ${e.message}` : String(e) });
    process.exit(1);
  });
