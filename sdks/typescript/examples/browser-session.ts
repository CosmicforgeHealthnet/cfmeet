// Browser code: use the signed-in user's CF Meet session — NEVER a platform credential.
// (The SDK refuses a cfmk_ credential when window and document exist.)
//
// This file uses only browser APIs; bundle it with your web app.

import { CfMeetAuthenticationError, CfMeetClient } from '@cfmeet/platform-sdk';

/** Your app's own session accessor (e.g. from its auth library); returns a fresh JWT. */
declare function currentSessionToken(): Promise<string>;

const client = new CfMeetClient({
  baseUrl: 'https://api.cfmeet.com',
  auth: { type: 'userSession', token: () => currentSessionToken() },
});

export async function showOrganizations(list: HTMLElement): Promise<void> {
  try {
    for await (const org of client.developerPlatform.listOrganizations.all()) {
      const li = document.createElement('li');
      li.textContent = org.displayName;
      list.append(li);
    }
  } catch (e) {
    if (e instanceof CfMeetAuthenticationError) window.location.assign('/login');
    else throw e;
  }
}

/** Live conference events: a fresh one-shot ticket per (re)connect; resumes after the last event. */
export function followConference(conferenceId: string, onEvent: (event: unknown) => void): () => void {
  const conn = client.realtime.meetings<{ type: string; event?: unknown }>(conferenceId);
  conn.on('message', ({ data }) => {
    if (data.type === 'event') onEvent(data.event);
  });
  conn.on('closed', (info) => console.info(`realtime ended: ${info.cause} (${info.code})`));
  return () => conn.close();
}
