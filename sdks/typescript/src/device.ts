// The device side of the CF Meet Platform (ADR 0062 / 0063): one client per signed-in
// partner user, authenticated by a CF Meet user token from YOUR server.
//
//   const device = new CfMeetDeviceClient({
//     baseUrl: 'https://api.cfmeet.com',
//     tokenProvider: () => fetch('/my-api/cfmeet-token').then((r) => r.json()).then((j) => j.token),
//     storage: localStorage,             // persists the installation id (AsyncStorage on React Native)
//   });
//   const chat = await device.chat.connect();          // messages + realtime events
//   const calls = await device.calls.connect();        // incoming calls (server-placed)
//   const meeting = await device.meetings.join(conferenceId); // LiveKit url + token for the RTC SDK
//   await device.push.register({ platform: 'FCM', handle: fcmToken });

import { CfMeetClient, type CfMeetClientConfig } from './client.js';
import { ChatClient } from './chat.js';
import { CallsClient } from './calls.js';
import { isBrowserLike, type CfMeetAuth, type UserTokenAuth, userTokenCache } from './auth.js';
import { CfMeetConfigurationError, CfMeetError } from './errors.js';
import type { Meetings } from './generated/models.js';
import { InstallationIdStore, isValidInstallationId, type InstallationIdSource, type KeyValueStorage } from './installation.js';
import { DEVICE_ROUTES } from './platform-routes.js';
import type { MeetingsRealtimeOptions, RealtimeConnection, RealtimeOptions, ReconnectOptions, WebSocketConstructorLike } from './realtime.js';
import type { ApiResponse, RequestOptions } from './types.js';
import { decodeUserTokenClaims, type UserTokenClaims } from './user-token.js';

export interface CfMeetDeviceClientConfig
  extends Omit<CfMeetClientConfig, 'auth' | 'dangerouslyAllowPlatformCredentialInBrowserLikeRuntime'> {
  /**
   * Returns a fresh CF Meet user token (`cfut_...`) from YOUR server, which mints it for
   * the signed-in user (`client.users.upsertWithToken`). Never put a platform credential
   * in a device app.
   */
  readonly tokenProvider: () => string | Promise<string>;
  /** Ask the provider again this long before the token's `exp` (default 60). */
  readonly refreshBeforeExpirySeconds?: number;
  /** A fixed installation id (otherwise generated once and kept in `storage`). */
  readonly installationId?: string;
  /** Persists the installation id: `localStorage`, React Native AsyncStorage, ... (default in memory). */
  readonly storage?: KeyValueStorage;
  /** Storage key of the installation id (default `cfmeet.installationId`). */
  readonly installationIdKey?: string;
  /** WebSocket implementation for chat and call signaling (default the global one). */
  readonly WebSocket?: WebSocketConstructorLike;
  /** Realtime reconnect tuning (false disables reconnecting). */
  readonly reconnect?: ReconnectOptions | false;
  /** Renew derived credentials (client token, signaling credential) this long before expiry (default 60). */
  readonly renewBeforeExpirySeconds?: number;
  /** Renew derived credentials in the background (default true); otherwise only on use. */
  readonly autoRenew?: boolean;
  /** Clock (ms since the epoch; default Date.now), injectable for tests. */
  readonly now?: () => number;
}

/** A CF Meet device client for one partner user. */
export class CfMeetDeviceClient {
  /** The underlying API client (authenticated with the user token). */
  readonly client: CfMeetClient;
  /** The user-token auth (cached, refreshed before expiry and after a refusal). */
  readonly auth: UserTokenAuth;
  readonly installationIds: InstallationIdStore;
  readonly chat: ChatClient;
  readonly calls: CallsClient;
  readonly meetings: MeetingsDeviceClient;
  readonly push: PushClient;
  readonly #fixedInstallationId: string | null;

  constructor(config: CfMeetDeviceClientConfig) {
    if (!config || typeof config.tokenProvider !== 'function') {
      throw new CfMeetConfigurationError('tokenProvider (a function returning a CF Meet user token from your server) is required');
    }
    if (config.installationId !== undefined && !isValidInstallationId(config.installationId)) {
      throw new CfMeetConfigurationError('installationId must be 8-128 characters of [A-Za-z0-9._-]');
    }
    const now = config.now ?? Date.now;
    this.auth = {
      type: 'userToken',
      tokenProvider: config.tokenProvider,
      refreshBeforeExpirySeconds: config.refreshBeforeExpirySeconds,
      now,
    };
    const { tokenProvider: _t, refreshBeforeExpirySeconds: _r, installationId, storage, installationIdKey, WebSocket, reconnect, renewBeforeExpirySeconds, autoRenew, now: _n, ...clientConfig } =
      config;
    this.client = new CfMeetClient({ ...clientConfig, auth: this.auth });
    this.#fixedInstallationId = installationId ?? null;
    this.installationIds = new InstallationIdStore({ storage, key: installationIdKey });
    const ids: InstallationIdSource = installationId ? { get: () => Promise.resolve(installationId) } : this.installationIds;
    const renewSeconds = renewBeforeExpirySeconds ?? 60;
    if (!(renewSeconds >= 0) || !Number.isFinite(renewSeconds)) throw new CfMeetConfigurationError('renewBeforeExpirySeconds must be >= 0');
    const realtime: Omit<RealtimeOptions, 'signal'> = { WebSocket, reconnect };
    const shared = { installationIds: ids, realtime, renewBeforeMs: renewSeconds * 1000, autoRenew: autoRenew ?? true, now };
    this.chat = new ChatClient(this.client, shared);
    this.calls = new CallsClient(this.client, { ...shared, defaultKind: isBrowserLike() ? 'WEB' : 'MOBILE' });
    this.meetings = new MeetingsDeviceClient(this.client, realtime);
    this.push = new PushClient(this.client, ids);
  }

  /** This installation's id (generated and persisted on first use). */
  installationId(): Promise<string> {
    return this.#fixedInstallationId ? Promise.resolve(this.#fixedInstallationId) : this.installationIds.get();
  }

  /** A valid user token (from the cache, or the provider). */
  userToken(): Promise<string> {
    return userTokenCache(this.auth).get();
  }

  /** The UNVERIFIED claims of the current user token (user id, environment, grants, expiry): display only. */
  async claims(): Promise<UserTokenClaims | null> {
    return decodeUserTokenClaims(await this.userToken());
  }
}

// ---------------------------------------------------------------------------------------
// Meetings

export interface MeetingJoinOptions extends RequestOptions {
  /**
   * Accepted for symmetry with chat and calls; NOT sent (the join body has no installation
   * field): a re-join of the same user keeps the same participant whatever the device.
   */
  readonly installationId?: string;
  /** Explicit consents granted before admission (pre-join contract). */
  readonly consents?: Meetings.JoinConferenceRequest['consents'];
  /** Reconnect: supersede this earlier session of the same participant. */
  readonly previousSessionId?: string;
}

/** `device.meetings`: joins conferences the user token grants (`grt.mtg`). */
export class MeetingsDeviceClient {
  readonly #client: CfMeetClient;
  readonly #realtime: Omit<RealtimeOptions, 'signal'>;

  constructor(client: CfMeetClient, realtime: Omit<RealtimeOptions, 'signal'>) {
    this.#client = client;
    this.#realtime = realtime;
  }

  /**
   * `POST /api/v2/conferences/{id}/join` with the user token. The role comes from the
   * grant and the display name from the user profile. Hand `rtc.url` / `rtc.credential`
   * to the LiveKit client SDK (`room.connect(rtc.url, rtc.credential)`); use the
   * participant token for every other participant operation.
   */
  async join(conferenceId: string, options: MeetingJoinOptions = {}): Promise<MeetingJoin> {
    const { installationId: _i, consents, previousSessionId, ...rest } = options;
    const body: Meetings.JoinConferenceRequest = {};
    if (consents) body.consents = consents;
    if (previousSessionId) body.previousSessionId = previousSessionId;
    const res = await this.#client.request<Meetings.JoinConferenceResponse>(DEVICE_ROUTES.joinConference, { path: { id: conferenceId }, body }, rest);
    if (!res.data || typeof res.data !== 'object' || !res.data.participant) throw new CfMeetError('the join response is malformed');
    return new MeetingJoin(this.#client, conferenceId, res, this.#realtime);
  }
}

/** A joined conference: the RTC credential, the participant token and follow-up helpers. */
export class MeetingJoin {
  readonly conferenceId: string;
  readonly response: ApiResponse<Meetings.JoinConferenceResponse>;
  /** The participant credential for every other participant operation (`X-Participant-Token`). */
  readonly participantToken: string | null;
  /** Auth for participant operations (`client.meetings.*(params, { auth })`), null when no token was issued. */
  readonly participantAuth: CfMeetAuth | null;
  #rtc: Meetings.RtcCredentialResponse | null;
  readonly #client: CfMeetClient;
  readonly #realtime: Omit<RealtimeOptions, 'signal'>;

  constructor(client: CfMeetClient, conferenceId: string, response: ApiResponse<Meetings.JoinConferenceResponse>, realtime: Omit<RealtimeOptions, 'signal'>) {
    this.#client = client;
    this.conferenceId = conferenceId;
    this.response = response;
    this.participantToken = response.data.participantToken ?? null;
    this.participantAuth = this.participantToken ? { type: 'participantToken', token: this.participantToken } : null;
    this.#rtc = response.data.rtc ?? null;
    this.#realtime = realtime;
  }

  get data(): Meetings.JoinConferenceResponse {
    return this.response.data;
  }

  /**
   * The RTC (LiveKit) credential: `{ provider, url, credential, expiresAt }`. Null while the
   * participant waits in the lobby (`data.rtcStatus` `NOT_ISSUED_WAITING`): call
   * `renewMediaCredential()` once admitted.
   */
  get rtc(): Meetings.RtcCredentialResponse | null {
    return this.#rtc;
  }

  /** A fresh media credential for this participant session (before `rtc.expiresAt`, or after admission). */
  async renewMediaCredential(options?: RequestOptions): Promise<Meetings.MediaCredentialResponse> {
    const auth = this.#participant();
    const participantSessionId = this.data.participantSession?.id;
    if (!participantSessionId) throw new CfMeetError('the join returned no participant session');
    const res = await this.#client.meetings.v2MediaCredential({ path: { id: this.conferenceId }, body: { participantSessionId } }, { ...options, auth });
    this.#rtc = res.data.rtc;
    return res.data;
  }

  /** The conference realtime event stream, as this participant. */
  realtime<M = unknown>(options: Omit<MeetingsRealtimeOptions, 'auth'> = {}): RealtimeConnection<M> {
    return this.#client.realtime.meetings<M>(this.conferenceId, { ...this.#realtime, ...options, auth: this.#participant() });
  }

  #participant(): CfMeetAuth {
    if (!this.participantAuth) throw new CfMeetError('the join issued no participant token');
    return this.participantAuth;
  }
}

// ---------------------------------------------------------------------------------------
// Push

export type PushPlatform = 'APNS' | 'APNS_VOIP' | 'FCM';

export interface PushRegistration {
  readonly installationId: string;
  readonly platform: PushPlatform;
  readonly status: 'ACTIVE';
}

/** `device.push`: registers this installation's push handle for the user (pushes sent with YOUR keys). */
export class PushClient {
  readonly #client: CfMeetClient;
  readonly #ids: InstallationIdSource;

  constructor(client: CfMeetClient, ids: InstallationIdSource) {
    this.#client = client;
    this.#ids = ids;
  }

  /**
   * `PUT /v1/messaging/user-push-registrations/{installationId}`. `APNS_VOIP` is the
   * PushKit token (incoming calls), `APNS` the regular token (chat), `FCM` the Firebase
   * registration token. Call again whenever the OS rotates the handle.
   */
  async register(input: { readonly installationId?: string; readonly platform: PushPlatform; readonly handle: string }, options?: RequestOptions): Promise<PushRegistration> {
    if (!['APNS', 'APNS_VOIP', 'FCM'].includes(input?.platform)) throw new CfMeetConfigurationError("platform must be 'APNS', 'APNS_VOIP' or 'FCM'");
    if (typeof input.handle !== 'string' || input.handle.length < 1 || input.handle.length > 4096) {
      throw new CfMeetConfigurationError('handle must be 1-4096 characters');
    }
    const installationId = await this.#installation(input.installationId);
    const res = await this.#client.request<PushRegistration>(
      DEVICE_ROUTES.registerUserPush,
      { path: { installationId }, body: { platform: input.platform, handle: input.handle } },
      options,
    );
    return res.data;
  }

  /** `DELETE …/{installationId}?platform=`: one platform, or every platform when omitted (sign-out). */
  async unregister(input: { readonly installationId?: string; readonly platform?: PushPlatform } = {}, options?: RequestOptions): Promise<void> {
    if (input.platform !== undefined && !['APNS', 'APNS_VOIP', 'FCM'].includes(input.platform)) {
      throw new CfMeetConfigurationError("platform must be 'APNS', 'APNS_VOIP' or 'FCM'");
    }
    const installationId = await this.#installation(input.installationId);
    await this.#client.request(DEVICE_ROUTES.unregisterUserPush, { path: { installationId }, query: { platform: input.platform } }, options);
  }

  async #installation(given: string | undefined): Promise<string> {
    const id = given ?? (await this.#ids.get());
    if (!isValidInstallationId(id)) throw new CfMeetConfigurationError('installationId must be 8-128 characters of [A-Za-z0-9._-]');
    return id;
  }
}
