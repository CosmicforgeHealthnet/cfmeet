// Hand-written operation specs of the CF Meet Platform "Build" routes (ADR 0062 / 0063,
// docs/platform/build-internal-contracts.md §3, §4, §8).
//
// The users helpers and the device clients use THESE specs with `client.request`, so they
// work whether or not the generated catalogue (src/generated/operations.ts) already holds
// the routes. Names match the contracts' operationIds, so after regeneration
// `client.developerPlatform.createUser` & co. are the same operations.

import type { OperationSpec, PaginationSpec, RetryClass } from './generated/operations.js';

interface RouteInit {
  readonly query?: readonly string[];
  readonly body?: OperationSpec['body'];
  readonly idempotency?: OperationSpec['idempotency'];
  readonly retry: RetryClass;
  readonly success: readonly number[];
  readonly scope?: string | null;
  readonly pagination?: PaginationSpec | null;
}

function route(
  domain: string,
  name: string,
  method: OperationSpec['method'],
  path: string,
  auth: readonly string[],
  init: RouteInit,
): OperationSpec {
  return Object.freeze({
    domain,
    name,
    operationId: name,
    method,
    path,
    protocol: 'REST',
    auth,
    scope: init.scope ?? null,
    pathParams: [...path.matchAll(/\{([^}]+)\}/g)].map((m) => m[1]!),
    query: (init.query ?? []).map((q) => ({ name: q, required: false })),
    headers: [],
    body: init.body ?? 'NONE',
    idempotency: init.idempotency ?? 'NONE',
    retry: init.retry,
    paidTelecom: false,
    pagination: init.pagination ?? null,
    success: init.success,
  });
}

const PLATFORM = ['PLATFORM_CREDENTIAL'] as const;
const USER = ['USER_TOKEN'] as const;
const PAGE: PaginationSpec = { cursorParam: 'cursor', nextField: 'nextCursor', itemsField: 'items' };

/** `/v1/users*` (Developer Console public API, platform credential; build contract §3). */
export const USER_ROUTES = {
  /** Idempotent on externalId (201 new, 200 existing): safe to retry. */
  createUser: route('developer-platform', 'createUser', 'POST', '/v1/users', PLATFORM, {
    body: 'REQUIRED', idempotency: 'OPTIONAL', retry: 'IDEMPOTENT', success: [200, 201], scope: 'principals.write',
  }),
  listUsers: route('developer-platform', 'listUsers', 'GET', '/v1/users', PLATFORM, {
    query: ['externalId', 'limit', 'cursor'], retry: 'SAFE', success: [200], scope: 'principals.read', pagination: PAGE,
  }),
  getUser: route('developer-platform', 'getUser', 'GET', '/v1/users/{userId}', PLATFORM, {
    retry: 'SAFE', success: [200], scope: 'principals.read',
  }),
  updateUser: route('developer-platform', 'updateUser', 'PATCH', '/v1/users/{userId}', PLATFORM, {
    body: 'REQUIRED', retry: 'IDEMPOTENT', success: [200], scope: 'principals.write',
  }),
  deleteUser: route('developer-platform', 'deleteUser', 'DELETE', '/v1/users/{userId}', PLATFORM, {
    retry: 'IDEMPOTENT', success: [204], scope: 'principals.write',
  }),
  revokeUserSessions: route('developer-platform', 'revokeUserSessions', 'POST', '/v1/users/{userId}/revoke-sessions', PLATFORM, {
    retry: 'IDEMPOTENT', success: [200], scope: 'principals.write',
  }),
  /**
   * NO_RETRY: an Idempotency-Key replay returns the metadata WITHOUT the token
   * (`tokenRedacted: true`), so a lost answer is better re-minted than replayed.
   */
  issueUserToken: route('developer-platform', 'issueUserToken', 'POST', '/v1/users/{userId}/tokens', PLATFORM, {
    body: 'REQUIRED', idempotency: 'OPTIONAL', retry: 'NO_RETRY', success: [201], scope: 'user_tokens.issue',
  }),
  upsertUserToken: route('developer-platform', 'upsertUserToken', 'POST', '/v1/users/tokens', PLATFORM, {
    body: 'REQUIRED', idempotency: 'OPTIONAL', retry: 'NO_RETRY', success: [201], scope: 'user_tokens.issue',
  }),
} as const;

/** Device exchange routes (user token; build contract §4). */
export const DEVICE_ROUTES = {
  messagingUserSession: route('messaging', 'exchangeUserSession', 'POST', '/v1/messaging/user-sessions', USER, {
    body: 'REQUIRED', retry: 'NO_RETRY', success: [200, 201],
  }),
  registerUserPush: route('messaging', 'registerUserPush', 'PUT', '/v1/messaging/user-push-registrations/{installationId}', USER, {
    body: 'REQUIRED', retry: 'IDEMPOTENT', success: [200],
  }),
  unregisterUserPush: route('messaging', 'unregisterUserPush', 'DELETE', '/v1/messaging/user-push-registrations/{installationId}', USER, {
    query: ['platform'], retry: 'IDEMPOTENT', success: [204],
  }),
  /** Creates (201) or reuses (200) the installation's endpoint: safe to retry. */
  callingUserSession: route('calling', 'exchangeUserSession', 'POST', '/v1/calling/user-sessions', USER, {
    body: 'REQUIRED', retry: 'IDEMPOTENT', success: [200, 201],
  }),
  /** The existing join route, which also accepts a user token (§8). */
  joinConference: route('meetings', 'v2JoinConference', 'POST', '/api/v2/conferences/{id}/join', USER, {
    body: 'OPTIONAL', retry: 'NO_RETRY', success: [200],
  }),
} as const;
