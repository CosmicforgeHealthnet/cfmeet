// Partner calls on a device (ADR 0062; build contract §4, §7). Calls stay server-placed:
// your server starts them with `POST /v1/calls`; the device receives and answers them.
//
//   user token ──POST /v1/calling/user-sessions──▶ endpoint (inst:<installationId>) + cfsig_ credential
//   cfsig_ ──WebSocket /v1/calling/signaling (subprotocol bearer.<cfsig>)──▶ call.incoming, ...
//   cfsig_ ──REST──▶ accept / decline / acknowledge invitations, media credentials
//
// The signaling credential never outlives the user token; it is exchanged again (same
// endpoint) before it expires and whenever the server closes the channel with 4001.

import type { CfMeetAuth } from './auth.js';
import { CfMeetClient } from './client.js';
import { Emitter, RenewingCredential, expiryMs, isAuthRefusal, type Expiring } from './device-shared.js';
import { CfMeetConfigurationError, CfMeetError } from './errors.js';
import type { Calling } from './generated/models.js';
import type { InstallationIdSource } from './installation.js';
import { isValidInstallationId } from './installation.js';
import { DEVICE_ROUTES } from './platform-routes.js';
import type { RealtimeCloseInfo, RealtimeConnection, RealtimeOptions } from './realtime.js';
import type { ApiResponse, RequestOptions, RequestParams } from './types.js';

/** `POST /v1/calling/user-sessions` answer. */
export interface CallingUserSession {
  readonly principalId: string;
  readonly endpointId: string;
  readonly signalingCredential: string;
  readonly signalingCredentialExpiresAt: string;
}

interface SignalingCredentialState extends Expiring {
  readonly session: CallingUserSession;
}

/** A realtime call signal (`cfmeet.calling.realtime-signal`). */
export interface CallSignal {
  readonly schema?: 'cfmeet.calling.realtime-signal';
  readonly type: string;
  readonly callId: string;
  readonly callKind: string;
  readonly callState: string;
  readonly callVersion: number;
  readonly sequence: number;
  readonly issuedAt: string;
  readonly invitation?: {
    readonly invitationId: string;
    readonly participantId: string;
    readonly status: string;
    readonly purpose: string;
    readonly issuedAt: string;
    readonly expiresAt: string;
    readonly callerDisplayName?: string | null;
  };
  readonly target?: { readonly endpointId: string; readonly principalId: string };
  readonly data?: unknown;
  readonly [field: string]: unknown;
}

/** An incoming call offer (from `call.incoming`, or `recoverInvitations()`). */
export interface IncomingInvitation {
  readonly invitationId: string;
  readonly callId: string;
  readonly callKind: string;
  /** The Call version to answer on (`expectedVersion`). */
  readonly callVersion: number;
  readonly expiresAt: string;
  readonly callerDisplayName: string | null;
}

export interface CallEvents {
  /** An incoming call: alert the user (CallKit / ConnectionService / your UI), then accept or decline. */
  invitation: { readonly invitation: IncomingInvitation; readonly signal: CallSignal };
  /** Every call signal (`call.cancelled`, `call.answered_elsewhere`, `call.ended`, ...). */
  signal: { readonly signal: CallSignal };
  /** A non-signal frame (`signaling.ready`, `signaling.pong`, `signaling.error`, ...). */
  frame: { readonly data: unknown };
  open: { readonly attempt: number; readonly protocol: string };
  close: { readonly code: number; readonly reason: string; readonly willReconnect: boolean };
  reconnecting: { readonly attempt: number; readonly delayMs: number };
  renewed: { readonly session: CallingUserSession };
  error: { readonly error: unknown };
  closed: RealtimeCloseInfo;
}

export interface CallsConnectOptions extends Omit<RealtimeOptions, 'signal'> {
  /** Defaults to the device client's persisted installation id. */
  readonly installationId?: string;
  /** The endpoint kind (default `WEB` in a browser-like runtime, `MOBILE` otherwise). */
  readonly kind?: 'WEB' | 'MOBILE';
  /** Open the signaling channel (default true); false when push wakes the app instead. */
  readonly signaling?: boolean;
  readonly signal?: AbortSignal;
}

export interface CallsDefaults {
  readonly installationIds: InstallationIdSource;
  readonly realtime: Omit<RealtimeOptions, 'signal'>;
  readonly renewBeforeMs: number;
  readonly autoRenew: boolean;
  readonly now: () => number;
  readonly defaultKind: 'WEB' | 'MOBILE';
}

/** `device.calls`: registers this installation as a call endpoint of the user. */
export class CallsClient {
  readonly #client: CfMeetClient;
  readonly #defaults: CallsDefaults;

  constructor(client: CfMeetClient, defaults: CallsDefaults) {
    this.#client = client;
    this.#defaults = defaults;
  }

  /** Exchanges the user token for the endpoint and its signaling credential, then opens signaling. */
  async connect(options: CallsConnectOptions = {}): Promise<CallSession> {
    const installationId = options.installationId ?? (await this.#defaults.installationIds.get());
    if (!isValidInstallationId(installationId)) throw new CfMeetConfigurationError('installationId must be 8-128 characters of [A-Za-z0-9._-]');
    const kind = options.kind ?? this.#defaults.defaultKind;
    if (kind !== 'WEB' && kind !== 'MOBILE') throw new CfMeetConfigurationError("kind must be 'WEB' or 'MOBILE'");
    const session = new CallSession(this.#client, installationId, kind, options, this.#defaults);
    await session.start();
    return session;
  }
}

function toInvitation(signal: CallSignal): IncomingInvitation | null {
  const inv = signal.invitation;
  if (!inv || typeof inv.invitationId !== 'string') return null;
  return {
    invitationId: inv.invitationId,
    callId: signal.callId,
    callKind: signal.callKind,
    callVersion: signal.callVersion,
    expiresAt: inv.expiresAt,
    callerDisplayName: inv.callerDisplayName ?? null,
  };
}

/** One device's call endpoint: signaling events and the invitation commands. */
export class CallSession {
  readonly installationId: string;
  readonly kind: 'WEB' | 'MOBILE';
  readonly #client: CfMeetClient;
  readonly #options: CallsConnectOptions;
  readonly #defaults: CallsDefaults;
  readonly #events = new Emitter<CallEvents>();
  readonly #credential: RenewingCredential<SignalingCredentialState>;
  /** Signaling-credential auth for any SIGNALING_CREDENTIAL operation. */
  readonly signalingAuth: CfMeetAuth;
  /** invitationId → callId, callId → latest callVersion (from signals). */
  readonly #invitationCall = new Map<string, string>();
  readonly #callVersion = new Map<string, number>();
  #conn: RealtimeConnection<unknown> | null = null;
  #closed = false;
  #reopened = 0;
  #resolveClosed!: (info: RealtimeCloseInfo) => void;
  readonly closed: Promise<RealtimeCloseInfo>;

  constructor(client: CfMeetClient, installationId: string, kind: 'WEB' | 'MOBILE', options: CallsConnectOptions, defaults: CallsDefaults) {
    this.#client = client;
    this.installationId = installationId;
    this.kind = kind;
    this.#options = options;
    this.#defaults = defaults;
    this.closed = new Promise((r) => (this.#resolveClosed = r));
    this.#credential = new RenewingCredential<SignalingCredentialState>(() => this.#exchange(), {
      renewBeforeMs: defaults.renewBeforeMs,
      autoRenew: defaults.autoRenew,
      now: defaults.now,
      onRenewed: () => this.#events.emit('renewed', { session: this.#credential.current!.session }),
      onError: (error) => this.#events.emit('error', { error }),
    });
    this.signalingAuth = { type: 'signalingCredential', credential: () => this.signalingCredential() };
    options.signal?.addEventListener('abort', () => this.close(), { once: true });
  }

  /** @internal Called by `CallsClient.connect`. */
  async start(): Promise<void> {
    await this.#credential.get();
    if (this.#options.signaling !== false && !this.#closed) this.#openSignaling();
  }

  get principalId(): string {
    return this.#session().principalId;
  }

  get endpointId(): string {
    return this.#session().endpointId;
  }

  get realtime(): RealtimeConnection<unknown> | null {
    return this.#conn;
  }

  on<K extends keyof CallEvents>(event: K, handler: (ev: CallEvents[K]) => void): () => void {
    return this.#events.on(event, handler);
  }

  /** A valid signaling credential (exchanged again when it is about to expire). */
  async signalingCredential(): Promise<string> {
    return (await this.#credential.get()).session.signalingCredential;
  }

  /** Tells CF Meet the device is ringing (PENDING → RINGING). */
  acknowledge(invitationId: string, options?: RequestOptions): Promise<ApiResponse<Calling.AcknowledgeResult>> {
    return this.request('acknowledgeInvitation', { path: { invitation_id: invitationId } }, options);
  }

  /** Answers the call. `expectedVersion` defaults to the latest call version seen in signals. */
  async accept(
    invitation: IncomingInvitation | string,
    options: RequestOptions & { readonly expectedVersion?: number } = {},
  ): Promise<ApiResponse<Calling.AcceptResult>> {
    const { expectedVersion, ...rest } = options;
    const [id, version] = this.#command(invitation, expectedVersion);
    return this.request('acceptInvitation', { path: { invitation_id: id }, body: { expectedVersion: version } }, rest);
  }

  /** Declines the call. */
  async decline(
    invitation: IncomingInvitation | string,
    options: RequestOptions & { readonly expectedVersion?: number } = {},
  ): Promise<ApiResponse<Calling.DeclineResult>> {
    const { expectedVersion, ...rest } = options;
    const [id, version] = this.#command(invitation, expectedVersion);
    return this.request('declineInvitation', { path: { invitation_id: id }, body: { expectedVersion: version } }, rest);
  }

  /**
   * The media credential of an accepted call's session (`AcceptResult.sessionId`): the
   * media gateway `url` and join `token` for the media connection.
   */
  mediaCredential(sessionId: string, options?: RequestOptions): Promise<ApiResponse<Calling.MediaCredential>> {
    return this.request('issueOwnMediaCredential', { path: { session_id: sessionId }, body: {} }, options);
  }

  /** The endpoint's live and recent offers (e.g. after a push woke the app). */
  async recoverInvitations(options?: RequestOptions): Promise<IncomingInvitation[]> {
    const res = await this.request<Calling.RecoveredInvitationList>('recoverInvitations', {}, options);
    const out: IncomingInvitation[] = [];
    for (const r of res.data?.data ?? []) {
      this.#remember(r.invitationId, r.callId, r.callVersion);
      if (r.actionable) {
        out.push({
          invitationId: r.invitationId,
          callId: r.callId,
          callKind: r.callKind,
          callVersion: r.callVersion,
          expiresAt: r.expiresAt,
          callerDisplayName: r.callerDisplayName,
        });
      }
    }
    return out;
  }

  /** Any SIGNALING_CREDENTIAL calling operation; a refused credential is exchanged again and the call retried once. */
  async request<T = unknown>(name: string, params: RequestParams = {}, options: RequestOptions = {}): Promise<ApiResponse<T>> {
    const spec = CfMeetClient.operation('calling', name);
    if (!spec.auth.includes('SIGNALING_CREDENTIAL')) throw new CfMeetConfigurationError(`calling.${name} does not accept a signaling credential`);
    if (this.#closed) throw new CfMeetError('the call session is closed');
    const call = async () => {
      const credential = await this.signalingCredential();
      return this.#client.request<T>(spec, params, { ...options, auth: { type: 'signalingCredential', credential } });
    };
    try {
      return await call();
    } catch (e) {
      if (!isAuthRefusal(e)) throw e;
      this.#credential.markStale();
      return call();
    }
  }

  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#credential.close();
    const conn = this.#conn;
    this.#conn = null;
    conn?.close();
    this.#finish({ code: 1000, reason: 'client closed', cause: 'client' });
  }

  #finish(info: RealtimeCloseInfo): void {
    this.#closed = true;
    this.#credential.close();
    this.#events.emit('closed', info);
    this.#resolveClosed(info);
  }

  #session(): CallingUserSession {
    const c = this.#credential.current;
    if (!c) throw new CfMeetError('the call session is not connected');
    return c.session;
  }

  #remember(invitationId: string | undefined, callId: string, version: number): void {
    if (invitationId) this.#invitationCall.set(invitationId, callId);
    if (Number.isInteger(version) && version > (this.#callVersion.get(callId) ?? -1)) this.#callVersion.set(callId, version);
  }

  #command(invitation: IncomingInvitation | string, expectedVersion: number | undefined): [string, number] {
    const id = typeof invitation === 'string' ? invitation : invitation.invitationId;
    if (typeof id !== 'string' || id === '') throw new CfMeetConfigurationError('an invitation id is required');
    const callId = typeof invitation === 'string' ? this.#invitationCall.get(id) : invitation.callId;
    const version =
      expectedVersion ??
      (callId !== undefined ? this.#callVersion.get(callId) : undefined) ??
      (typeof invitation === 'string' ? undefined : invitation.callVersion);
    if (typeof version !== 'number') {
      throw new CfMeetConfigurationError(`unknown call version for invitation ${id}: pass expectedVersion (or the IncomingInvitation)`);
    }
    return [id, version];
  }

  async #exchange(): Promise<SignalingCredentialState> {
    const res = await this.#client.request<CallingUserSession>(DEVICE_ROUTES.callingUserSession, {
      body: { installationId: this.installationId, kind: this.kind },
    });
    const s = res.data;
    if (!s || typeof s.signalingCredential !== 'string' || typeof s.endpointId !== 'string' || typeof s.principalId !== 'string') {
      throw new CfMeetError('the calling user session response is malformed');
    }
    return { session: s, expiresAtMs: expiryMs(s.signalingCredentialExpiresAt, 'signaling credential') };
  }

  #onFrame(data: unknown): void {
    const f = data as Partial<CallSignal> | null;
    if (f && typeof f === 'object' && typeof f.type === 'string' && f.type.startsWith('call.') && typeof f.callId === 'string') {
      const signal = f as CallSignal;
      this.#remember(signal.invitation?.invitationId, signal.callId, signal.callVersion);
      this.#events.emit('signal', { signal });
      if (signal.type === 'call.incoming') {
        const invitation = toInvitation(signal);
        if (invitation) this.#events.emit('invitation', { invitation, signal });
      }
      return;
    }
    this.#events.emit('frame', { data });
  }

  #openSignaling(): void {
    const { realtime } = this.#defaults;
    // A provider credential: the realtime helper reconnects after 4001 with a fresh one.
    const conn = this.#client.realtime.callingSignaling<unknown>({
      ...realtime,
      ...this.#options,
      signal: undefined,
      credential: () => this.signalingCredential(),
    });
    this.#conn = conn;
    conn.on('message', ({ data }) => this.#onFrame(data));
    conn.on('open', (e) => {
      this.#reopened = 0;
      this.#events.emit('open', e);
    });
    conn.on('close', (e) => {
      // 4001: the credential expired or was revoked server-side: exchange again.
      if (e.code === 4001) this.#credential.markStale();
      this.#events.emit('close', e);
    });
    conn.on('reconnecting', (e) => this.#events.emit('reconnecting', e));
    conn.on('error', (e) => this.#events.emit('error', e));
    conn.on('closed', (info) => {
      if (this.#closed || this.#conn !== conn) return;
      this.#conn = null;
      if (info.cause === 'refused' && isAuthRefusal(info.error) && this.#reopened < 1) {
        this.#reopened++;
        this.#credential.markStale();
        this.#credential.get().then(
          () => {
            if (!this.#closed) this.#openSignaling();
          },
          (error: unknown) => {
            this.#events.emit('error', { error });
            this.#finish({ ...info, error });
          },
        );
        return;
      }
      this.#finish(info);
    });
  }
}
