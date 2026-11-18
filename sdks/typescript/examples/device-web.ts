// Browser (device) code for a partner web app: chat and incoming calls for the signed-in
// user, with a CF Meet user token from YOUR server. No platform credential here — ever.
//
// The page's origin must be in the environment's allowed websites (Developer Console).

import { CfMeetDeviceClient, browserLocalStorage, type IncomingInvitation } from '@cfmeet/platform-sdk';

const device = new CfMeetDeviceClient({
  baseUrl: 'https://api.cfmeet.com',
  // Your backend mints the token (client.users.upsertWithToken) behind your own login.
  tokenProvider: async () => {
    const res = await fetch('/karrygo/cfmeet-token', { credentials: 'include' });
    if (!res.ok) throw new Error(`token endpoint answered ${res.status}`);
    return ((await res.json()) as { token: string }).token;
  },
  storage: browserLocalStorage() ?? undefined, // keeps the installation id across reloads
});

interface ChatEvent {
  readonly type: string;
  readonly conversationId?: string;
  readonly message?: { readonly text?: string };
}

/** Delivery chat: live events, history, send, read receipts. */
export async function openDeliveryChat(conversationId: string, list: HTMLElement): Promise<{ send(text: string): Promise<void>; close(): void }> {
  const chat = await device.chat.connect<ChatEvent>();
  const show = (text: string) => {
    const li = document.createElement('li');
    li.textContent = text; // text, never markup
    list.append(li);
  };
  const page = await chat.listMessages(conversationId, { limit: 50 });
  for (const m of page.data.messages) show(m.managed?.text ?? '');
  chat.on('message', ({ data }) => {
    if (data.type === 'message.created' && data.conversationId === conversationId) show(data.message?.text ?? '');
  });
  chat.on('closed', (info) => console.info(`chat ended: ${info.cause}`));
  const last = page.data.messages.at(-1);
  if (last) await chat.markRead(conversationId, last.sequence);
  return {
    send: async (text) => void (await chat.sendMessage(conversationId, text)),
    close: () => chat.close(),
  };
}

/** Incoming calls: ring in the page, then accept or decline. */
export async function receiveCalls(ring: (invitation: IncomingInvitation) => Promise<boolean>): Promise<() => void> {
  const calls = await device.calls.connect({ kind: 'WEB' });
  calls.on('invitation', async ({ invitation }) => {
    await calls.acknowledge(invitation.invitationId); // the device is ringing
    if (await ring(invitation)) {
      const accepted = await calls.accept(invitation);
      const media = await calls.mediaCredential(accepted.data.sessionId);
      console.info(`connect media to ${media.data.url}`); // hand url + token to the media client
    } else {
      await calls.decline(invitation);
    }
  });
  calls.on('signal', ({ signal }) => {
    if (signal.type === 'call.cancelled' || signal.type === 'call.answered_elsewhere') console.info('stop ringing');
  });
  return () => calls.close();
}

/** Meetings: join with the user token; LiveKit's own client SDK connects the media. */
export async function joinMeeting(conferenceId: string): Promise<{ url: string; credential: string } | null> {
  const joined = await device.meetings.join(conferenceId);
  // e.g. `await new Room().connect(rtc.url, rtc.credential)` with livekit-client (your dependency, not ours).
  return joined.rtc ? { url: joined.rtc.url, credential: joined.rtc.credential } : null;
}
