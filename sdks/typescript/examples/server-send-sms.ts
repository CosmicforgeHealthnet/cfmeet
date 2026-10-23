// Server-side: send an SMS with an Idempotency-Key and reconcile an unknown outcome.
//
//   CFMEET_API_CREDENTIAL=cfmk_sbx_... CFMEET_NUMBER_RESOURCE_ID=... node send-sms.js +15550100 "hello"
//
// POST /v1/sms/messages is a paid telecom operation (retry class NEVER): the SDK never
// retries it. If the outcome is unknown (timeout, connection lost, 500/502/504), the SMS
// may already have been accepted — replay deliberately with the SAME key: the server then
// returns the original result (idempotentReplayed: true) instead of sending twice.

import { randomUUID } from 'node:crypto';

import { CfMeetApiError, CfMeetClient, UnknownOutcomeError } from '@cfmeet/platform-sdk';

const client = new CfMeetClient({
  baseUrl: process.env.CFMEET_API_URL ?? 'https://api.cfmeet.com',
  // Machine credential: server-side only, from your secret store / environment.
  auth: { type: 'platformCredential', credential: process.env.CFMEET_API_CREDENTIAL ?? '' },
  timeoutMs: 15_000,
});

export async function sendSms(to: string, text: string): Promise<string> {
  // Persist the key with your own record BEFORE sending, so a crash can be reconciled too.
  const idempotencyKey = randomUUID();
  const body = { from: { numberResourceId: process.env.CFMEET_NUMBER_RESOURCE_ID ?? '' }, to, text };
  for (let attempt = 1; ; attempt++) {
    try {
      const res = await client.sms.sendMessage({ body, idempotencyKey });
      console.log(`message ${res.data.messageId} ${res.data.status}${res.idempotentReplayed ? ' (replayed)' : ''} request ${res.requestId}`);
      return res.data.messageId;
    } catch (e) {
      if (e instanceof UnknownOutcomeError && attempt < 3) {
        console.warn(`outcome unknown (${e.reason}); replaying with the same Idempotency-Key ${e.idempotencyKey}`);
        continue;
      }
      if (e instanceof CfMeetApiError) console.error(`refused: ${e.code} (${e.status}) request ${e.requestId}`);
      throw e;
    }
  }
}

if (process.argv[2]) {
  await sendSms(process.argv[2], process.argv[3] ?? 'Hello from CF Meet');
}
