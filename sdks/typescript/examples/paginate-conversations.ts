// Server-side: iterate every conversation of the environment (cursor pagination).
import { CfMeetClient, type Messaging } from '@cfmeet/platform-sdk';

const client = new CfMeetClient({
  baseUrl: process.env.CFMEET_API_URL ?? 'https://api.cfmeet.com',
  auth: { type: 'platformCredential', credential: process.env.CFMEET_API_CREDENTIAL ?? '' },
});

export async function activeConversations(): Promise<Messaging.Conversation[]> {
  const out: Messaging.Conversation[] = [];
  // .all() follows nextCursor until the last page; each page request is a SAFE GET
  // (retried on 429 / 5xx / network errors with backoff).
  for await (const conversation of client.messaging.listConversations.all({ query: { lifecycleState: 'ACTIVE', limit: 100 } })) {
    out.push(conversation);
  }
  return out;
}

// Page by page, with the response metadata:
export async function pageSizes(): Promise<number[]> {
  const sizes: number[] = [];
  for await (const page of client.messaging.listConversations.pages({ query: { limit: 50 } })) {
    sizes.push(page.data.conversations.length);
    if (page.quota?.remainingThisMinute === 0) break; // be a good citizen
  }
  return sizes;
}
