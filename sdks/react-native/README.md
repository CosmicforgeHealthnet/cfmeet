# @cfmeet/platform-sdk-react-native

CF Meet chat, calls and meetings for the users of YOUR React Native app
([ADR 0062](../../docs/decisions/0062-cf-meet-platform-build-users-tokens-devices.md)).
Private, 0.1.0, not published yet (publishing waits for the owner decision in
[`sdks/README.md`](../README.md)).

* Re-exports the TypeScript SDK ([`@cfmeet/platform-sdk`](../typescript)) unchanged:
  `CfMeetDeviceClient`, chat / calls / meetings / push, user-token handling, typed errors.
* Adds: the installation id persisted in **your** AsyncStorage, a typed
  **`NativeCallBridge`** your native module implements (PushKit / APNs / FCM tokens,
  CallKit / ConnectionService call UI) with `connectNativeCalls` to drive it, and React
  hooks `useCfMeetChat`, `useCfMeetCall`, `useCfMeetMeeting`.
* No runtime dependencies besides the core SDK; `react` (>= 18) is a peer dependency. No
  native code, no AsyncStorage / LiveKit / CallKit library is pulled in: the app chooses
  and injects them.

## Quickstart

Your **server** (Node, with the platform credential — never in the app) mints a user
token for the signed-in user:

```ts
// server: GET /me/cfmeet-token (behind your own login)
const { token } = await cfmeet.users.upsertWithToken({
  externalId: me.id,                 // your opaque user id, never a phone number
  displayName: me.name,
  grants: { msg: ['read', 'send'], call: ['receive'] },
});
res.json({ token });
```

The **app**:

```tsx
import 'react-native-get-random-values'; // Web Crypto for older Hermes builds (ids, idempotency keys)
import AsyncStorage from '@react-native-async-storage/async-storage';
import { createDeviceClient, useCfMeetCall, useCfMeetChat } from '@cfmeet/platform-sdk-react-native';

const device = createDeviceClient({
  baseUrl: 'https://api.cfmeet.com',
  tokenProvider: async () => (await (await fetch(`${API}/me/cfmeet-token`, { headers: myAuth() })).json()).token,
  storage: AsyncStorage, // keeps the installation id across launches
});

function DeliveryChat({ conversationId }: { conversationId: string }) {
  const chat = useCfMeetChat<{ type: string; conversationId?: string }>(device);
  // chat.status: 'connecting' | 'open' | 'reconnecting' | ...; chat.events: realtime frames
  return <Composer disabled={chat.status !== 'open'} onSend={(text) => chat.sendMessage(conversationId, text)} />;
}

function IncomingCall() {
  const call = useCfMeetCall(device, { kind: 'MOBILE' });
  if (!call.incoming) return null;
  return (
    <Ringing
      name={call.incoming.callerDisplayName ?? 'Rider'}
      onAccept={async () => {
        const accepted = await call.accept();
        const media = await call.session!.mediaCredential(accepted.sessionId);
        connectMedia(media.data.url, media.data.token); // your media client
      }}
      onDecline={() => call.decline()}
    />
  );
}
```

Meetings: `const { rtc } = useCfMeetMeeting(device, conferenceId)` joins with the user
token (it must carry an `mtg` grant for that conference); hand `rtc.url` and
`rtc.credential` to LiveKit's React Native SDK (`@livekit/react-native`, your
dependency): `room.connect(rtc.url, rtc.credential)`.

The user token is cached and refreshed ~60 s before expiry and after a
`USER_TOKEN_EXPIRED` / `USER_TOKEN_REVOKED` refusal; the chat client token and the call
signaling credential are renewed before they expire. Sign-out:
`await device.push.unregister()` then close the sessions (unmount the hooks).

## Push and the system call UI (`NativeCallBridge`)

Incoming calls must ring when the app is in the background or killed. That needs native
code this package does not ship: your app implements `NativeCallBridge` (a native module
or an existing CallKit / FCM library behind a small adapter) and calls
`connectNativeCalls`:

```ts
import { connectNativeCalls, type NativeCallBridge } from '@cfmeet/platform-sdk-react-native';

const bridge: NativeCallBridge = MyNativeCallModule; // see "What the native module implements"
const calls = await device.calls.connect({ kind: 'MOBILE' });
const stop = connectNativeCalls({
  device, calls, bridge,
  onAccepted: async (result) => connectMedia(await calls.mediaCredential(result.sessionId)),
});
```

`connectNativeCalls` registers every push token with CF Meet (`device.push.register`,
again on rotation), shows `call.incoming` signals through `displayIncomingCall`,
reconciles call pushes with `recoverInvitations()`, maps answer / decline in the system
UI to `accept` / `decline`, and ends the system call on `call.cancelled`,
`call.answered_elsewhere`, `call.invitation_expired`, ...

### What the native module implements

| Member | iOS | Android |
|---|---|---|
| `getPushTokens()` / `onPushToken` | PushKit VoIP token → `APNS_VOIP`; `registerForRemoteNotifications` token → `APNS` (hex) | `FirebaseMessaging.getToken()` / `onNewToken` → `FCM` |
| `onIncomingCallPush` | `pushRegistry(_:didReceiveIncomingPushWith:)`: **report to CallKit before returning** (`CXProvider.reportNewIncomingCall`, UUID = `invitationId` from the payload), then emit | `FirebaseMessagingService.onMessageReceived` for a high-priority data message: start the call UI (`TelecomManager.addNewIncomingCall` with a self-managed `PhoneAccount`, or a full-screen-intent notification), then emit |
| `displayIncomingCall(call)` | `reportNewIncomingCall` (UUID = `call.invitationId`, `hasVideo`) | `addNewIncomingCall` / notification |
| `onAnswer` / `onDecline` | `CXAnswerCallAction` / `CXEndCallAction` (fulfil them) | `Connection.onAnswer` / `onReject` |
| `reportConnected(id)` | `reportOutgoingCall(with:connectedAt:)` / audio session activation | `Connection.setActive()` |
| `endCall(id, reason)` | `reportCall(with:endedAt:reason:)` (`.remoteEnded`, `.answeredElsewhere`, `.declinedElsewhere`, `.unanswered`, `.failed`) | `Connection.setDisconnected(DisconnectCause)` + `destroy()` |

* Push keys are yours: upload the APNs `.p8` key (with VoIP) and the FCM service account
  in the Developer Console (Push keys). CF Meet sends call pushes as APNs VoIP
  (`apns-push-type: voip`, topic `<bundleId>.voip`) and FCM high-priority data messages;
  chat pushes carry metadata only (never message text).
* Pushes are wake-ups, not state: the JavaScript side always asks CF Meet for the live
  offers (`recoverInvitations`) before answering.

## Development

```bash
cd sdks/react-native
npm install            # @types/react + typescript (dev only); links ../typescript
npm run typecheck      # against ../typescript/src (no SDK build needed)
(cd ../typescript && npm run build) && npm run build   # dist/ against the built SDK
```
