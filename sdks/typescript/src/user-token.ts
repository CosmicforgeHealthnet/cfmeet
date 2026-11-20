// The CF Meet user token (ADR 0062): `cfut_<sbx|test|live>_<compact JWS>`, minted for one
// partner user by the partner's server and handed to the device through a token provider.
//
// The SDK never verifies the token (CF Meet services do, live); it only decodes the JWS
// payload to learn `exp`, so it can ask the provider for a fresh token ~60 s before expiry
// instead of waiting for a 401. A token whose payload cannot be decoded is used until the
// first refusal.

import { CfMeetConfigurationError } from './errors.js';

export const USER_TOKEN_PREFIX = 'cfut_';
const USER_TOKEN_RE = /^cfut_(sbx|test|live)_([A-Za-z0-9_-]+)\.([A-Za-z0-9_-]+)\.([A-Za-z0-9_-]*)$/;
/** Maximum wire length of a user token (build contract §1). */
export const USER_TOKEN_MAX_LENGTH = 2048;

/**
 * 401 error codes after which a request made with a user token is retried ONCE with a
 * fresh token from the provider (expired, revoked by `revoke-sessions`, or refused).
 */
export const USER_TOKEN_REFRESH_CODES: ReadonlySet<string> = new Set([
  'USER_TOKEN_EXPIRED',
  'USER_TOKEN_REVOKED',
  'USER_TOKEN_INVALID',
  'AUTHENTICATION_FAILED',
  'AUTHENTICATION_REQUIRED',
]);

/** The grants of a user token (`grt`). */
export interface UserTokenGrants {
  readonly msg?: readonly ('read' | 'send')[];
  readonly call?: readonly 'receive'[];
  readonly mtg?: readonly { readonly c: string; readonly r: string }[];
}

/** The (UNVERIFIED) claims of a user token, for display and scheduling only. */
export interface UserTokenClaims {
  readonly iss?: string;
  readonly aud?: string;
  /** The partner user's principal id. */
  readonly sub?: string;
  readonly tid?: string;
  readonly aid?: string;
  readonly eid?: string;
  readonly ekd?: 'SANDBOX' | 'TEST' | 'PRODUCTION';
  readonly jti?: string;
  readonly iat?: number;
  readonly nbf?: number;
  /** Expiry, seconds since the epoch. */
  readonly exp?: number;
  readonly grt?: UserTokenGrants;
  readonly [claim: string]: unknown;
}

/** True when `token` has the shape of a CF Meet user token (not a verification). */
export function isUserToken(token: string): boolean {
  return typeof token === 'string' && token.length <= USER_TOKEN_MAX_LENGTH && USER_TOKEN_RE.test(token);
}

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';

function base64UrlDecode(s: string): Uint8Array | null {
  const out: number[] = [];
  let buffer = 0;
  let bits = 0;
  for (const ch of s) {
    const v = B64.indexOf(ch);
    if (v < 0) return null;
    buffer = (buffer << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push((buffer >> bits) & 0xff);
    }
  }
  return Uint8Array.from(out);
}

function utf8(bytes: Uint8Array): string {
  if (typeof TextDecoder !== 'undefined') return new TextDecoder().decode(bytes);
  // Runtimes without TextDecoder: the claims are ASCII JSON.
  return String.fromCharCode(...bytes);
}

/**
 * Decodes the payload of a user token WITHOUT verifying it (never use the result for an
 * access decision). Null when the token is not a well-formed `cfut_` JWS.
 */
export function decodeUserTokenClaims(token: string): UserTokenClaims | null {
  if (typeof token !== 'string' || token.length > USER_TOKEN_MAX_LENGTH) return null;
  const m = USER_TOKEN_RE.exec(token);
  if (!m) return null;
  const bytes = base64UrlDecode(m[3]!);
  if (!bytes) return null;
  try {
    const claims = JSON.parse(utf8(bytes)) as unknown;
    return claims && typeof claims === 'object' && !Array.isArray(claims) ? (claims as UserTokenClaims) : null;
  } catch {
    return null;
  }
}

export interface UserTokenCacheOptions {
  /** Ask the provider again this long before `exp` (default 60 s). */
  readonly refreshBeforeExpirySeconds?: number;
  /** Clock (ms since the epoch; default Date.now). Tests inject a fake one. */
  readonly now?: () => number;
}

/**
 * Caches the user token returned by a provider: one provider call at a time, a fresh
 * token ~60 s before `exp`, and `invalidate()` after a refusal.
 */
export class UserTokenCache {
  readonly #provider: () => string | Promise<string>;
  readonly #refreshMs: number;
  readonly #now: () => number;
  #token: string | null = null;
  #expiresAtMs: number | null = null;
  #inflight: Promise<string> | null = null;

  constructor(provider: () => string | Promise<string>, options: UserTokenCacheOptions = {}) {
    if (typeof provider !== 'function') throw new CfMeetConfigurationError('userToken: tokenProvider must be a function');
    const s = options.refreshBeforeExpirySeconds ?? 60;
    if (!(s >= 0) || !Number.isFinite(s)) throw new CfMeetConfigurationError('userToken: refreshBeforeExpirySeconds must be >= 0');
    this.#provider = provider;
    this.#refreshMs = s * 1000;
    this.#now = options.now ?? Date.now;
  }

  /** Expiry of the cached token (ms since the epoch), null when unknown or none is cached. */
  get expiresAtMs(): number | null {
    return this.#token ? this.#expiresAtMs : null;
  }

  /** The cached token, or a fresh one from the provider when none is cached or it expires soon. */
  get(): Promise<string> {
    const t = this.#token;
    if (t !== null && (this.#expiresAtMs === null || this.#now() < this.#expiresAtMs - this.#refreshMs)) return Promise.resolve(t);
    return this.refresh();
  }

  /** Asks the provider for a new token (concurrent callers share one provider call). */
  refresh(): Promise<string> {
    if (this.#inflight) return this.#inflight;
    const p = (async () => {
      const token = await this.#provider();
      if (typeof token !== 'string' || !isUserToken(token)) {
        throw new CfMeetConfigurationError('userToken: the token provider did not return a CF Meet user token (cfut_...)');
      }
      const exp = decodeUserTokenClaims(token)?.exp;
      this.#token = token;
      this.#expiresAtMs = typeof exp === 'number' && Number.isFinite(exp) ? exp * 1000 : null;
      return token;
    })();
    this.#inflight = p;
    const clear = () => {
      if (this.#inflight === p) this.#inflight = null;
    };
    p.then(clear, clear);
    return p;
  }

  /** Forgets the cached token (only `token`, when given, so a newer token is kept). */
  invalidate(token?: string): void {
    if (token === undefined || token === this.#token) {
      this.#token = null;
      this.#expiresAtMs = null;
    }
  }
}
