// Wires a CF Meet call session to the app's native call UI and push tokens.

import type { CallSession, CallSignal, CfMeetDeviceClient, IncomingInvitation, Calling } from '@cfmeet/platform-sdk';

import type { NativeCallBridge, NativeCallEndReason, Unsubscribe } from './native-bridge.js';

export interface ConnectNativeCallsOptions {
  readonly device: CfMeetDeviceClient;
  /** From `device.calls.connect({ kind: 'MOBILE' })`. */
  readonly calls: CallSession;
  readonly bridge: NativeCallBridge;
  /** Register the push tokens with CF Meet (default true). */
  readonly registerPush?: boolean;
  /** The call was answered: connect media with `calls.mediaCredential(result.sessionId)`. */
  readonly onAccepted?: (result: Calling.AcceptResult, invitation: IncomingInvitation) => void;
  readonly onError?: (error: unknown) => void;
}

const END_REASONS: Readonly<Record<string, NativeCallEndReason>> = {
  'call.cancelled': 'REMOTE_ENDED',
  'call.ring_cancelled': 'REMOTE_ENDED',
  'call.ended': 'REMOTE_ENDED',
  'call.answered_elsewhere': 'ANSWERED_ELSEWHERE',
  'call.declined': 'DECLINED_ELSEWHERE',
  'call.invitation_expired': 'UNANSWERED',
  'call.no_answer': 'UNANSWERED',
  'call.failed': 'FAILED',
};

/**
 * Keeps the native side and CF Meet in step:
 *   push tokens → `device.push.register` (and on rotation);
 *   `call.incoming` over signaling → `bridge.displayIncomingCall`;
 *   a call push → `calls.recoverInvitations()` (ends a system call with no live offer);
 *   answer / decline in the system UI → `calls.accept` / `calls.decline`;
 *   cancelled / answered elsewhere / expired → `bridge.endCall`.
 * Returns a function that removes every listener.
 */
export function connectNativeCalls(options: ConnectNativeCallsOptions): Unsubscribe {
  const { device, calls, bridge } = options;
  const report = (e: unknown) => options.onError?.(e);
  const invitations = new Map<string, IncomingInvitation>();
  const displayed = new Set<string>();
  const byCall = new Map<string, string>();
  const subs: Unsubscribe[] = [];

  const remember = (inv: IncomingInvitation) => {
    invitations.set(inv.invitationId, inv);
    byCall.set(inv.callId, inv.invitationId);
  };
  const display = (inv: IncomingInvitation) => {
    remember(inv);
    if (displayed.has(inv.invitationId)) return;
    displayed.add(inv.invitationId);
    bridge
      .displayIncomingCall({ invitationId: inv.invitationId, callId: inv.callId, callerDisplayName: inv.callerDisplayName, hasVideo: inv.callKind === 'VIDEO' })
      .catch(report);
    calls.acknowledge(inv.invitationId).catch(report);
  };

  if (options.registerPush !== false) {
    const register = (platform: 'APNS' | 'APNS_VOIP' | 'FCM', handle: string) =>
      device.push.register({ platform, handle }).catch(report);
    bridge
      .getPushTokens()
      .then((tokens) => tokens.forEach((t) => void register(t.platform, t.token)))
      .catch(report);
    subs.push(bridge.onPushToken((t) => void register(t.platform, t.token)));
  }

  subs.push(calls.on('invitation', ({ invitation }) => display(invitation)));

  subs.push(
    calls.on('signal', ({ signal }: { signal: CallSignal }) => {
      const reason = END_REASONS[signal.type];
      const invitationId = byCall.get(signal.callId);
      if (!reason || !invitationId) return;
      byCall.delete(signal.callId);
      invitations.delete(invitationId);
      bridge.endCall(invitationId, reason).catch(report);
    }),
  );

  subs.push(
    bridge.onIncomingCallPush((payload) => {
      if (payload.invitationId) displayed.add(payload.invitationId); // already shown natively
      calls
        .recoverInvitations()
        .then((live) => {
          live.forEach(display);
          const id = payload.invitationId;
          if (id && !live.some((i) => i.invitationId === id)) {
            displayed.delete(id);
            void bridge.endCall(id, 'UNANSWERED').catch(report); // no longer actionable
          }
        })
        .catch(report);
    }),
  );

  subs.push(
    bridge.onAnswer((invitationId) => {
      const inv = invitations.get(invitationId);
      const accept = inv ? calls.accept(inv) : calls.accept(invitationId);
      accept
        .then(async (res) => {
          await bridge.reportConnected(invitationId);
          if (inv) options.onAccepted?.(res.data, inv);
        })
        .catch((e: unknown) => {
          report(e);
          void bridge.endCall(invitationId, 'FAILED').catch(report);
        });
    }),
  );

  subs.push(
    bridge.onDecline((invitationId) => {
      const inv = invitations.get(invitationId);
      (inv ? calls.decline(inv) : calls.decline(invitationId)).catch(report);
    }),
  );

  return () => {
    for (const s of subs.splice(0)) s();
  };
}
