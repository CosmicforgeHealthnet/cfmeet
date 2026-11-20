// The native side of incoming calls and push on React Native.
//
// CF Meet ships no native code in this package (no dependency on a CallKit / FCM
// library). YOUR app provides a native module implementing `NativeCallBridge`:
//
//   iOS      PushKit (PKPushRegistry, VoIP token → platform APNS_VOIP), CallKit
//            (CXProvider reportNewIncomingCall / CXCallController), and the regular APNs
//            token (UNUserNotificationCenter → platform APNS) for chat.
//   Android  Firebase Messaging (FCM token → platform FCM; high-priority DATA messages for
//            calls) and ConnectionService / Telecom (TelecomManager.addNewIncomingCall,
//            a self-managed PhoneAccount) or a full-screen-intent notification.
//
// Rules the native module MUST follow (enforced by the OS, not by CF Meet):
//   * iOS: every VoIP push must be reported to CallKit BEFORE the PushKit delegate
//     returns, or iOS terminates the app and eventually stops VoIP delivery. Report it
//     natively at once (caller name from the payload, if any), then emit
//     `incomingCallPush` to JavaScript; `connectNativeCalls` reconciles it with the
//     endpoint's live invitations and ends the CallKit call if none is actionable.
//   * Android: start the incoming-call UI from the FCM service (`onMessageReceived`) for a
//     call data message, then emit `incomingCallPush`.
//   * Identify every system call by the CF Meet `invitationId` (CallKit needs a UUID:
//     invitation ids are UUIDs, use them as the CXCall UUID).
//
// Pushes carry metadata only (never message content or credentials). The push keys are
// YOUR keys, uploaded in the Developer Console (APNs .p8 with the VoIP topic, FCM service
// account); CF Meet sends with them to the handles registered through `device.push`.

/** Unsubscribes a listener. */
export type Unsubscribe = () => void;

export type NativePushPlatform = 'APNS' | 'APNS_VOIP' | 'FCM';

export interface NativePushToken {
  readonly platform: NativePushPlatform;
  /** The device token (hex for APNs) or FCM registration token. */
  readonly token: string;
}

/** An incoming call to show in the system UI. */
export interface NativeIncomingCall {
  /** The CF Meet invitation id: use it as the system call's UUID. */
  readonly invitationId: string;
  readonly callId: string;
  readonly callerDisplayName: string | null;
  readonly hasVideo: boolean;
}

/** Why a system call ends (map to CXCallEndedReason / DisconnectCause). */
export type NativeCallEndReason = 'REMOTE_ENDED' | 'ANSWERED_ELSEWHERE' | 'DECLINED_ELSEWHERE' | 'UNANSWERED' | 'FAILED';

/** What the app's native module implements (iOS: PushKit + CallKit; Android: FCM + ConnectionService). */
export interface NativeCallBridge {
  /** The current push tokens (any of APNS_VOIP, APNS, FCM the platform provides). */
  getPushTokens(): Promise<readonly NativePushToken[]>;
  /** A token was issued or rotated. */
  onPushToken(listener: (token: NativePushToken) => void): Unsubscribe;
  /**
   * A call push arrived (VoIP push on iOS, FCM data message on Android) and the native
   * side has ALREADY shown the system call UI for `invitationId` when the payload had one.
   * `payload` is the push's custom data as received.
   */
  onIncomingCallPush(listener: (payload: { readonly invitationId?: string; readonly [key: string]: unknown }) => void): Unsubscribe;
  /** Shows the system incoming-call UI for a call that arrived over signaling (app in the foreground). */
  displayIncomingCall(call: NativeIncomingCall): Promise<void>;
  /** The user answered in the system UI. */
  onAnswer(listener: (invitationId: string) => void): Unsubscribe;
  /** The user declined (or hung up before answering) in the system UI. */
  onDecline(listener: (invitationId: string) => void): Unsubscribe;
  /** The call is connected (CXProvider reportOutgoingCall / Connection.setActive). */
  reportConnected(invitationId: string): Promise<void>;
  /** Ends the system call (cancelled by the caller, answered elsewhere, expired, failed). */
  endCall(invitationId: string, reason: NativeCallEndReason): Promise<void>;
}
