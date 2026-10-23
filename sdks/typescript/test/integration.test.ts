// Real-stack integration (gated). Set CFMEET_STACK_ENV to the JSON file written by the
// stack launcher (scripts/certification/stack.py), at least:
//
//   { "gatewayUrl": "http://127.0.0.1:8080", "credentials": { "sandbox": "cfmk_sbx_..." } }
//
// Optional: "quotaHeaders": false when the stack runs without platform quotas.
// Without CFMEET_STACK_ENV the suite prints SKIPPED and passes.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, test } from 'node:test';

import {
  CfMeetAuthenticationError,
  CfMeetClient,
  CfMeetNotFoundError,
  type OperationSpec,
} from '../src/index.js';

interface StackEnv {
  readonly gatewayUrl: string;
  readonly credentials: { readonly sandbox: string; readonly [env: string]: string };
  readonly quotaHeaders?: boolean;
}

const file = process.env.CFMEET_STACK_ENV;
const skip = file ? false : 'SKIPPED: CFMEET_STACK_ENV is not set (no real stack)';
if (skip) console.log(`# real-stack integration: ${skip}`);

function stack(): StackEnv {
  const env = JSON.parse(readFileSync(file!, 'utf8')) as StackEnv;
  assert.equal(typeof env.gatewayUrl, 'string', 'stack env: gatewayUrl');
  assert.match(env.credentials?.sandbox ?? '', /^cfmk_sbx_/, 'stack env: credentials.sandbox');
  return env;
}

const UNROUTED: OperationSpec = {
  domain: 'gateway',
  name: 'unrouted',
  operationId: 'unrouted',
  method: 'GET',
  path: '/v1/definitely-not-a-route',
  protocol: 'REST',
  auth: ['PLATFORM_CREDENTIAL'],
  scope: null,
  pathParams: [],
  query: [],
  headers: [],
  body: 'NONE',
  idempotency: 'NONE',
  retry: 'SAFE',
  paidTelecom: false,
  pagination: null,
  success: [200],
};

describe('real stack (CFMEET_STACK_ENV)', { skip }, () => {
  test('GET /discovery (anonymous)', async () => {
    const env = stack();
    const c = new CfMeetClient({ baseUrl: env.gatewayUrl });
    const r = await c.discovery();
    assert.equal(r.status, 200);
    assert.equal(typeof r.data, 'object');
    assert.ok(r.requestId, 'x-request-id is surfaced');
  });

  test('GET /v1/developer-platform/context with the sandbox credential', async () => {
    const env = stack();
    const c = new CfMeetClient({ baseUrl: env.gatewayUrl, auth: { type: 'platformCredential', credential: env.credentials.sandbox } });
    const r = await c.gateway.getDeveloperPlatformContext();
    assert.equal(r.status, 200);
    for (const k of ['tenantId', 'applicationId', 'environmentId', 'serviceAccountId'] as const) {
      assert.match(r.data[k], /^[0-9a-f-]{36}$/, k);
    }
    assert.equal(r.data.authority, 'PLATFORM_ACCESS');
    assert.equal(r.data.forwardedBy, 'cfmeet.api-gateway');
    assert.ok(r.requestId);
    assert.ok(r.correlationId);
    if (env.quotaHeaders !== false) {
      assert.ok(r.quota, 'x-cf-quota-* headers are surfaced');
      assert.equal(typeof r.quota.limitPerMinute, 'number');
      assert.equal(typeof r.quota.remainingThisMinute, 'number');
    }
  });

  test('an unrouted path is a typed 404 (default deny)', async () => {
    const env = stack();
    const c = new CfMeetClient({ baseUrl: env.gatewayUrl, auth: { type: 'platformCredential', credential: env.credentials.sandbox } });
    const e = await c.request(UNROUTED).catch((x: unknown) => x);
    assert.ok(e instanceof CfMeetNotFoundError, String(e));
    assert.equal(e.code, 'NOT_FOUND');
    assert.ok(e.requestId);
  });

  test('an invalid credential is a typed 401', async () => {
    const env = stack();
    const c = new CfMeetClient({
      baseUrl: env.gatewayUrl,
      auth: { type: 'platformCredential', credential: 'cfmk_sbx_00000000000000000000000000000000_invalid-secret' },
      maxRetries: 0,
    });
    const e = await c.gateway.getDeveloperPlatformContext().catch((x: unknown) => x);
    assert.ok(e instanceof CfMeetAuthenticationError, String(e));
    assert.equal(e.status, 401);
    assert.equal(e.code, 'AUTHENTICATION_FAILED');
    assert.ok(e.requestId);
  });
});
