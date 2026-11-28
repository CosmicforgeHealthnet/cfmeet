/**
 * The bell's inbox rules (Notification Service U1, relayed under /api/v2/notifications):
 * where each item opens, and how the pages of the inbox are merged. Pure, so it is tested
 * without a browser (tests/notification-inbox.test.mjs).
 */

/** The parts of a `NotificationItem` these rules read. */
export type InboxItem = {
  id: string;
  action: { kind: string; ref?: string | null };
  dismissedAt?: string | null;
};

/** One U1 page. */
export type InboxPage<T extends InboxItem> = { items: T[]; nextCursor: string | null };

/** U1 page size: the contract allows 1-100. */
export const INBOX_PAGE_SIZE = 30;

/** Where an item opens (the server names the target and its id); `null`: nowhere. */
export function inboxHref(item: InboxItem): string | null {
  const ref = item.action.ref ? encodeURIComponent(item.action.ref) : "";
  switch (item.action.kind) {
    // Added to a group / a new message: that conversation.
    case "OPEN_CONVERSATION": return ref ? `/dashboard/messages?conversation=${ref}` : "/dashboard/messages";
    // Missed call: that call in the call history.
    case "OPEN_CALL_HISTORY": return ref ? `/calling?call=${ref}` : "/calling";
    case "OPEN_VOICEMAIL": return "/calling?tab=voicemail";
    case "OPEN_RECORDING": return "/calling";
    // Meeting reminder / rescheduled: the reference is the conference (its join page).
    case "OPEN_MEETING": return ref ? `/conferences/${ref}` : "/dashboard/meetings";
    case "OPEN_ORGANIZATION": case "OPEN_ORGANIZATION_INVITATION": return "/dashboard/organizations";
    // A contact request waits in Contacts > Requests; an accepted / direct contact in Contacts.
    case "OPEN_CONTACT_REQUEST": return "/dashboard/contacts?tab=requests";
    case "OPEN_CONTACT": return "/dashboard/contacts";
    case "OPEN_SECURITY_SETTINGS": return "/dashboard/settings?tab=security";
    case "OPEN_DEVELOPER_APPLICATION": return "/developers";
    // An event invitation, cancellation or reminder (Messaging, ADR 0071): the event page.
    case "OPEN_EVENT": return ref ? `/dashboard/events/${ref}` : "/dashboard/events";
    // Mentioned in a status / your status was shared (Messaging, ADR 0069): that status in
    // Updates (it opens only while the reader may still see it).
    case "OPEN_STATUS": return ref ? `/dashboard/updates?status=${ref}` : "/dashboard/updates";
    // A community invitation, join request or approval (Messaging, ADR 0073): the community
    // (an invitee who is not a member yet is pointed to Communities, where invitations wait).
    case "OPEN_COMMUNITY": return ref ? `/dashboard/communities/${ref}` : "/dashboard/communities";
    default: return null;
  }
}

/** Every loaded page in order, without dismissed items or duplicates (an item grouped
 *  between two page loads moves to the top and could otherwise appear twice). */
export function mergeInboxPages<T extends InboxItem>(pages: readonly InboxPage<T>[] | undefined): T[] {
  const seen = new Set<string>();
  const items: T[] = [];
  for (const page of pages ?? []) {
    for (const item of page.items) {
      if (item.dismissedAt || seen.has(item.id)) continue;
      seen.add(item.id);
      items.push(item);
    }
  }
  return items;
}
