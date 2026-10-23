// Compile-time checks of the typed public API (tsc fails the test build if they break).

import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { ApiResponse, CfMeetClient, ParamsOf, ResponseOf, Sms } from '../src/index.js';

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
const ok = <T extends true>(_: T): void => {};

// Never executed: type checking only.
async function typed(c: CfMeetClient): Promise<void> {
  const m = await c.sms.getMessage({ path: { messageId: 'm' } });
  ok<Equal<typeof m, ApiResponse<Sms.Message>>>(true);
  const id: string = m.data.messageId;
  void id;

  // @ts-expect-error path parameters are required
  await c.sms.getMessage();
  // @ts-expect-error unknown path parameter name
  await c.sms.getMessage({ path: { id: 'm' } });
  // @ts-expect-error the body is required
  await c.sms.sendMessage({});
  // @ts-expect-error sendMessage needs `to` or `threadId` (oneOf)
  await c.sms.sendMessage({ body: { from: { numberResourceId: 'n' }, text: 'x' } });
  await c.sms.sendMessage({ body: { from: { numberResourceId: 'n' }, to: '+15550100', text: 'x' }, idempotencyKey: 'k' });
  // @ts-expect-error getMessage takes no idempotency key
  await c.sms.getMessage({ path: { messageId: 'm' }, idempotencyKey: 'k' });
  // @ts-expect-error unknown operation
  void c.sms.deleteEverything;

  // operations without required params may be called without arguments
  await c.sms.listMessages();
  for await (const item of c.sms.listMessages.all({ query: { limit: 10 } })) {
    ok<Equal<typeof item, Sms.Message>>(true);
  }
  // @ts-expect-error non-paginated operations have no .all
  void c.sms.getMessage.all;

  // documented headers are typed
  await c.messaging.sendMessage({
    path: { conversationId: 'c' },
    headers: { 'cf-acting-principal': 'p' },
    body: { clientMessageId: 'x', text: 'hi' },
  });

  ok<Equal<ParamsOf<'sms', 'getMessage'>, { path: { messageId: string } }>>(true);
  ok<Equal<ResponseOf<'sms', 'getMessage'>, Sms.Message>>(true);
}

test('typed API compiles', () => {
  assert.equal(typeof typed, 'function');
});
