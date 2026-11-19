// Partner users and CF Meet user tokens (ADR 0062; build contract §3): SERVER-SIDE helpers
// over `/v1/users*`, authenticated with the platform credential (`cfmk_...`, refused in
// browsers). Your server creates a user per person in your app (keyed by your own opaque
// `externalId`, never a phone number) and mints short-lived user tokens for their devices.

import type { CfMeetClient } from './client.js';
import { CfMeetError } from './errors.js';
import { USER_ROUTES } from './platform-routes.js';
import type { ApiResponse, RequestOptions } from './types.js';
import type { UserTokenGrants } from './user-token.js';

export type PartnerUserStatus = 'ACTIVE' | 'SUSPENDED' | 'DEACTIVATED';

/** A partner user (a Platform Access principal of your environment). */
export interface PartnerUser {
  readonly id: string;
  readonly externalId: string;
  readonly displayName: string | null;
  readonly imageUrl: string | null;
  readonly status: PartnerUserStatus;
  readonly createdAt: string | null;
  readonly updatedAt: string | null;
}

export interface CreatePartnerUser {
  /** Your opaque user id: 1–128 printable ASCII, no spaces, never a phone number or SIP address. */
  readonly externalId: string;
  /** Up to 64 characters. */
  readonly displayName?: string;
  /** `https://` only, up to 512 characters. */
  readonly imageUrl?: string;
}

export interface UpdatePartnerUser {
  readonly displayName?: string | null;
  readonly imageUrl?: string | null;
  readonly status?: 'ACTIVE' | 'SUSPENDED';
}

/** Meeting grant in the request form (the token carries the compact `{c, r}`). */
export interface MeetingGrant {
  readonly conferenceId: string;
  readonly role: 'HOST' | 'CO_HOST' | 'PRESENTER' | 'PARTICIPANT' | 'VIEWER';
}

/** What a user token lets the device do (each part optional; a subset of your key's scopes). */
export interface UserTokenGrantsRequest {
  /** Chat: `read` (realtime, history) and / or `send`. Needs messages.read / messages.send. */
  readonly msg?: readonly ('read' | 'send')[];
  /** Calls: `receive` (signaling, accept / decline). Needs calls.write. */
  readonly call?: readonly 'receive'[];
  /** Meetings: at most 10 conferences. Needs meetings.write. */
  readonly mtg?: readonly MeetingGrant[];
}

export interface IssueUserTokenRequest {
  readonly grants: UserTokenGrantsRequest;
  /** 60–86400 s (default 3600). */
  readonly ttlSeconds?: number;
}

export interface UpsertUserTokenRequest extends CreatePartnerUser, IssueUserTokenRequest {}

/** A minted user token: hand `token` to the device (it is returned once, never stored by CF Meet). */
export interface IssuedUserToken {
  /** `cfut_<sbx|test|live>_...`; absent (`tokenRedacted: true`) on an Idempotency-Key replay. */
  readonly token: string;
  readonly tokenId: string;
  readonly expiresAt: string;
  readonly tokenRedacted?: boolean;
}

export interface UpsertedUserToken extends IssuedUserToken {
  readonly user: PartnerUser;
}

export interface ListPartnerUsersQuery {
  readonly externalId?: string;
  /** 1–100. */
  readonly limit?: number;
  readonly cursor?: string;
}

export interface PartnerUserPage {
  readonly items: readonly PartnerUser[];
  readonly nextCursor: string | null;
}

/** Options of the mutating helpers. */
export interface UserRequestOptions extends RequestOptions {
  /** Idempotency-Key (generated when omitted). */
  readonly idempotencyKey?: string;
}

function split(options: UserRequestOptions = {}): [string | undefined, RequestOptions] {
  const { idempotencyKey, ...rest } = options;
  return [idempotencyKey, rest];
}

/** `client.users`: partner users and user tokens (server-side, platform credential). */
export class UsersClient {
  readonly #client: CfMeetClient;

  constructor(client: CfMeetClient) {
    this.#client = client;
  }

  /** `POST /v1/users`: creates the user, or returns the existing one with that externalId. */
  create(body: CreatePartnerUser, options?: UserRequestOptions): Promise<ApiResponse<PartnerUser>> {
    const [idempotencyKey, rest] = split(options);
    return this.#client.request<PartnerUser>(USER_ROUTES.createUser, { body, idempotencyKey }, rest);
  }

  /** Like `create`, telling whether the user was created (201) or already existed (200). */
  async upsert(body: CreatePartnerUser, options?: UserRequestOptions): Promise<{ readonly user: PartnerUser; readonly created: boolean }> {
    const res = await this.create(body, options);
    return { user: res.data, created: res.status === 201 };
  }

  /** `GET /v1/users/{userId}`. */
  get(userId: string, options?: RequestOptions): Promise<ApiResponse<PartnerUser>> {
    return this.#client.request<PartnerUser>(USER_ROUTES.getUser, { path: { userId } }, options);
  }

  /** The user with this externalId, or null. */
  async findByExternalId(externalId: string, options?: RequestOptions): Promise<PartnerUser | null> {
    const res = await this.list({ externalId, limit: 1 }, options);
    return res.data.items[0] ?? null;
  }

  /** `GET /v1/users` (one page). */
  list(query: ListPartnerUsersQuery = {}, options?: RequestOptions): Promise<ApiResponse<PartnerUserPage>> {
    return this.#client.request<PartnerUserPage>(USER_ROUTES.listUsers, { query: { ...query } }, options);
  }

  /** Every user of every page (follows nextCursor). */
  all(query: Omit<ListPartnerUsersQuery, 'cursor'> = {}, options?: RequestOptions): AsyncGenerator<PartnerUser, void, undefined> {
    return this.#client.paginate<PartnerUser>(USER_ROUTES.listUsers, { query: { ...query } }, options);
  }

  /** `PATCH /v1/users/{userId}`: profile, or `status: SUSPENDED` (ends every device credential). */
  update(userId: string, body: UpdatePartnerUser, options?: RequestOptions): Promise<ApiResponse<PartnerUser>> {
    return this.#client.request<PartnerUser>(USER_ROUTES.updateUser, { path: { userId }, body }, options);
  }

  /** `DELETE /v1/users/{userId}`: tombstones the user (profile erased, ids kept). */
  async delete(userId: string, options?: RequestOptions): Promise<void> {
    await this.#client.request(USER_ROUTES.deleteUser, { path: { userId } }, options);
  }

  /** `POST /v1/users/{userId}/revoke-sessions`: every token minted before now stops working. */
  revokeSessions(userId: string, options?: RequestOptions): Promise<ApiResponse<{ readonly tokensNotBefore: string }>> {
    return this.#client.request(USER_ROUTES.revokeUserSessions, { path: { userId } }, options);
  }

  /** `POST /v1/users/{userId}/tokens`: mints a user token for the device. */
  async createToken(userId: string, body: IssueUserTokenRequest, options?: UserRequestOptions): Promise<IssuedUserToken> {
    const [idempotencyKey, rest] = split(options);
    const res = await this.#client.request<IssuedUserToken>(USER_ROUTES.issueUserToken, { path: { userId }, body, idempotencyKey }, rest);
    return checkToken(res.data);
  }

  /**
   * `POST /v1/users/tokens`: creates or updates the user (by externalId) and mints a token
   * in one call — the usual "my user signed in on a device" path.
   */
  async upsertWithToken(body: UpsertUserTokenRequest, options?: UserRequestOptions): Promise<UpsertedUserToken> {
    const [idempotencyKey, rest] = split(options);
    const res = await this.#client.request<UpsertedUserToken>(USER_ROUTES.upsertUserToken, { body, idempotencyKey }, rest);
    return checkToken(res.data);
  }
}

function checkToken<T extends IssuedUserToken>(data: T): T {
  if (!data || typeof data !== 'object') throw new CfMeetError('the user token response is malformed');
  if (data.tokenRedacted === true) {
    throw new CfMeetError('the user token response was an idempotent replay without the token (tokenRedacted): mint a new token with a new Idempotency-Key');
  }
  if (typeof data.token !== 'string' || !data.token.startsWith('cfut_')) throw new CfMeetError('the user token response is malformed');
  return data;
}

export type { UserTokenGrants };
