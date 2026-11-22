/** Each status recipient has a separate expiring encryption group. Broadcasts use
 * ordinary direct conversations: recipients never see the author's audience list. */
const STATUS_PREFIX = "\u2063cfmeet:status:v1:";
export const STATUS_LIFETIME_MS = 24 * 60 * 60 * 1000;

export function statusGroupTitle(postId: string): string {
  if (!/^[a-f0-9-]{36}$/i.test(postId)) throw new Error("Invalid status id");
  return `${STATUS_PREFIX}${postId}`;
}

export function statusPostId(title: string | null | undefined): string | null {
  if (!title?.startsWith(STATUS_PREFIX)) return null;
  const id = title.slice(STATUS_PREFIX.length);
  return /^[a-f0-9-]{36}$/i.test(id) ? id : null;
}

export function isLiveStatus(c: { groupTitle?: string | null; expiresAt?: string | null; securityProfile: string; contextClass: string; announcementOnly: boolean }, now = Date.now()): boolean {
  const expires = Date.parse(c.expiresAt ?? "");
  return c.contextClass === "GROUP" && c.securityProfile === "PRIVATE_E2EE" && c.announcementOnly
    && !!statusPostId(c.groupTitle) && Number.isFinite(expires) && expires > now;
}

export type ShareRecipient = { cfMeetId: string; label: string };
export type ShareResult = { sent: string[]; unconfirmed: ShareRecipient[] };
export interface PrivateShareTransport {
  createConversation(users: string[], profile: "PRIVATE_E2EE", options: { cfMeetIds: string[]; title?: string; expiresAt?: string; retentionSeconds?: number; announcementOnly?: boolean }): Promise<string>;
  sendRich(conversation: string, payload: { t: "text"; text: string }): Promise<void>;
}

export async function sharePrivately(transport: PrivateShareTransport, recipients: ShareRecipient[], text: string,
  status: { postId: string; createdAt: number } | null = null): Promise<ShareResult> {
  const message = text.trim();
  if (!message || message.length > (status ? 700 : 4000)) throw new Error("Message is empty or too long");
  const unique = [...new Map(recipients.map((r) => [r.cfMeetId, r])).values()];
  if (!unique.length || unique.length > 500) throw new Error("Choose between 1 and 500 contacts");
  const expiresAt = status ? new Date(status.createdAt + STATUS_LIFETIME_MS).toISOString() : undefined;
  if (status && status.createdAt + STATUS_LIFETIME_MS <= Date.now()) throw new Error("This status has expired");
  const result: ShareResult = { sent: [], unconfirmed: [] };
  for (const recipient of unique) {
    try {
      const id = await transport.createConversation([], "PRIVATE_E2EE", {
        cfMeetIds: [recipient.cfMeetId],
        ...(status ? { title: statusGroupTitle(status.postId), expiresAt, retentionSeconds: 86400, announcementOnly: true } : {}),
      });
      await transport.sendRich(id, { t: "text", text: message });
      result.sent.push(recipient.cfMeetId);
    } catch {
      // A timeout can follow a committed send. Never retry it automatically or report
      // a definite failure, which could encourage the author to send a duplicate.
      result.unconfirmed.push(recipient);
    }
  }
  return result;
}
