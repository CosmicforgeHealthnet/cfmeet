// Credentials and how each one is presented.
//
// | auth type             | operations' auth mode               | presented as                              |
// |-----------------------|-------------------------------------|-------------------------------------------|
// | platformCredential    | PLATFORM_CREDENTIAL                 | Authorization: Bearer cfmk_<env>_<id>_<s> |
// | userSession           | DEVELOPER_SESSION / MEETING_SESSION | Authorization: Bearer <session JWT>       |
// | participantToken      | PARTICIPANT_TOKEN                   | X-Participant-Token: <token>              |
// | clientToken           | CLIENT_TOKEN                        | Authorization: CF-Client-Token <token>    |
// | signalingCredential   | SIGNALING_CREDENTIAL                | Authorization: Bearer cfsig_...           |
// | userToken             | USER_TOKEN                          | Authorization: Bearer cfut_<env>_<JWS>    |
//
// A user token (ADR 0062) is a DEVICE credential: the partner's server mints it for one
// of its users and the device obtains it through `tokenProvider`. The SDK caches it,
// asks again ~60 s before `exp` and after a 401 USER_TOKEN_EXPIRED / USER_TOKEN_REVOKED /
// AUTHENTICATION_FAILED (the request is then retried once).
//
// A platform credential (cfmk_...) is a MACHINE credential: server-side only. The SDK
// refuses it in a browser-like runtime unless the loudly-named escape hatch is set.

import { CfMeetConfigurationError } from './errors.js';
import { USER_TOKEN_PREFIX, UserTokenCache, isUserToken } from './user-token.js';

/** A token, or a function returning a fresh one (called for every request / reconnect). */
export type TokenSource = string | (() => string | Promise<string>);

export type CfMeetAuth =
  | { readonly type: 'platformCredential'; readonly credential: TokenSource }
  | { readonly type: 'userSession'; readonly token: TokenSource }
  | { readonly type: 'participantToken'; readonly token: TokenSource }
  | { readonly type: 'clientToken'; readonly token: TokenSource }
  | { readonly type: 'signalingCredential'; readonly credential: TokenSource }
  | UserTokenAuth;

/** A CF Meet user token (`cfut_...`), obtained from the partner's server by `tokenProvider`. */
export interface UserTokenAuth {
  readonly type: 'userToken';
  /**
   * Returns a fresh user token from YOUR server (which mints it with
   * `POST /v1/users/tokens`). Called on first use, ~60 s before the cached token's `exp`
   * and after the token is refused; concurrent callers share one call.
   */
  readonly tokenProvider: () => string | Promise<string>;
  /** Ask for a new token this long before `exp` (default 60). */
  readonly refreshBeforeExpirySeconds?: number;
  /** Clock (ms since the epoch; default Date.now), injectable for tests. */
  readonly now?: () => number;
}

export type AuthType = CfMeetAuth['type'];

/** The operation auth modes (OperationSpec.auth) each auth type satisfies. */
export const AUTH_MODES: Readonly<Record<AuthType, readonly string[]>> = {
  platformCredential: ['PLATFORM_CREDENTIAL'],
  userSession: ['DEVELOPER_SESSION', 'MEETING_SESSION'],
  participantToken: ['PARTICIPANT_TOKEN'],
  clientToken: ['CLIENT_TOKEN'],
  signalingCredential: ['SIGNALING_CREDENTIAL'],
  userToken: ['USER_TOKEN'],
};

export const PLATFORM_CREDENTIAL_PREFIX = 'cfmk_';

/** True in a browser-like runtime (a window and a document exist). */
export function isBrowserLike(): boolean {
  const g = globalThis as { window?: unknown; document?: unknown };
  return typeof g.window !== 'undefined' && typeof g.document !== 'undefined';
}

/** Escape-hatch flag name (kept in one place so messages and checks agree). */
export const BROWSER_ESCAPE_HATCH = 'dangerouslyAllowPlatformCredentialInBrowserLikeRuntime';

/**
 * Refuses a platform credential in a browser-like runtime. `allow` is the escape hatch for
 * trusted SERVER-SIDE runtimes that happen to define window and document (some edge or
 * test runtimes): never set it in code that ships to end users' browsers.
 */
export function assertPlatformCredentialAllowed(allow: boolean): void {
  if (isBrowserLike() && !allow) {
    throw new CfMeetConfigurationError(
      'refusing to use a CF Meet platform credential (cfmk_...) in a browser: machine credentials are server-side only. ' +
        'Use a user session, participant token or client token in browser code. ' +
        `(Trusted server-side runtimes that define window/document may set ${BROWSER_ESCAPE_HATCH}: true.)`,
    );
  }
}

const userTokenCaches = new WeakMap<UserTokenAuth, UserTokenCache>();

/** The token cache of a userToken auth object (one per object, shared by every client using it). */
export function userTokenCache(auth: UserTokenAuth): UserTokenCache {
  let c = userTokenCaches.get(auth);
  if (!c) {
    c = new UserTokenCache(auth.tokenProvider, { refreshBeforeExpirySeconds: auth.refreshBeforeExpirySeconds, now: auth.now });
    userTokenCaches.set(auth, c);
  }
  return c;
}

function source(auth: CfMeetAuth): TokenSource {
  switch (auth.type) {
    case 'platformCredential':
    case 'signalingCredential':
      return auth.credential;
    case 'userToken':
      if (typeof auth.tokenProvider !== 'function') return undefined as unknown as TokenSource;
      return () => userTokenCache(auth).get();
    default:
      return auth.token;
  }
}

/** Static checks at client construction. */
export function validateAuth(auth: CfMeetAuth, allowPlatformInBrowser: boolean): void {
  if (!(auth.type in AUTH_MODES)) {
    throw new CfMeetConfigurationError(`unknown auth type ${String((auth as { type: unknown }).type)}`);
  }
  const s = source(auth);
  if (typeof s !== 'string' && typeof s !== 'function') {
    throw new CfMeetConfigurationError(`auth ${auth.type}: a token string or a token provider function is required`);
  }
  if (auth.type === 'platformCredential') assertPlatformCredentialAllowed(allowPlatformInBrowser);
  if (auth.type === 'userToken') userTokenCache(auth); // checks the options once
  if (typeof s === 'string') checkToken(auth.type, s, allowPlatformInBrowser);
}

function checkToken(type: AuthType, token: string, allowPlatformInBrowser: boolean): void {
  if (!token || /[\s\u0000-\u001f\u007f]/.test(token) || token.length > 8192) {
    throw new CfMeetConfigurationError(`auth ${type}: the token is empty or contains whitespace / control characters`);
  }
  const machine = token.startsWith(PLATFORM_CREDENTIAL_PREFIX);
  if (machine && type !== 'platformCredential') {
    throw new CfMeetConfigurationError(
      `auth ${type}: a platform credential (cfmk_...) is not a ${type}; use { type: 'platformCredential' } (server-side only)`,
    );
  }
  if (machine) assertPlatformCredentialAllowed(allowPlatformInBrowser);
  if (type === 'userToken' && !isUserToken(token)) {
    throw new CfMeetConfigurationError(`auth userToken: not a CF Meet user token (expected ${USER_TOKEN_PREFIX}<sbx|test|live>_<JWS>)`);
  }
}

/** Resolves the token (calling a provider) and returns the headers that carry it. */
export async function authHeaders(auth: CfMeetAuth, allowPlatformInBrowser: boolean): Promise<Record<string, string>> {
  const s = source(auth);
  const token = typeof s === 'function' ? await s() : s;
  if (typeof token !== 'string') {
    throw new CfMeetConfigurationError(`auth ${auth.type}: the token provider did not return a string`);
  }
  checkToken(auth.type, token, allowPlatformInBrowser);
  return headerFor(auth.type, token);
}

/** Resolves only the token value (used by realtime helpers). */
export async function resolveToken(auth: CfMeetAuth, allowPlatformInBrowser: boolean): Promise<string> {
  const s = source(auth);
  const token = typeof s === 'function' ? await s() : s;
  if (typeof token !== 'string') {
    throw new CfMeetConfigurationError(`auth ${auth.type}: the token provider did not return a string`);
  }
  checkToken(auth.type, token, allowPlatformInBrowser);
  return token;
}

function headerFor(type: AuthType, token: string): Record<string, string> {
  switch (type) {
    case 'participantToken':
      return { 'x-participant-token': token };
    case 'clientToken':
      return { authorization: `CF-Client-Token ${token}` };
    default:
      return { authorization: `Bearer ${token}` };
  }
}
