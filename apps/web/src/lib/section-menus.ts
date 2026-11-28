/**
 * The ⋮ menu of each section (WhatsApp, brief §20): it changes with the section and lists
 * only what the signed-in person may do there. Pure (no imports, no icons): each screen maps
 * an item's `key` to its icon and its action; node tests run this file directly
 * (tests/section-menus.test.mjs).
 *
 * Sections: Updates, Chats, My status, a channel and Communities (`communitiesMenu` returns
 * nothing while the Communities section is not deployed).
 */

export type MenuItem = {
  key: string;
  label: string;
  /** A screen to open (a link), when the action is navigation. */
  href?: string;
  /** Destructive actions are shown in red and confirmed by the screen. */
  danger?: boolean;
};

/** Where the menus lead (existing screens and the Starred screen). */
export const ROUTES = {
  starred: "/dashboard/starred",
  statusPrivacy: "/dashboard/updates/privacy",
  myStatus: "/dashboard/updates/mine",
  settings: "/dashboard/settings",
  events: "/dashboard/events",
} as const;

/**
 * Sections that exist on this build: a section's menu entries stay hidden until its screens
 * are deployed, so a menu never leads to a page that isn't there.
 */
export type Sections = { events: boolean; communities: boolean };

/** This build: Events (ADR 0071) and Communities (ADR 0073) are deployed. */
export const DEPLOYED: Sections = { events: true, communities: true };

/**
 * Channel features on this build (ADR 0072): per-follower notifications (mute), reports
 * (reviewed by HQ through the admin bridge) and the management screen.
 */
export const CHANNEL_FEATURES = { notifications: true, report: true, manage: true } as const;

// ------------------------------------------------------------------ Updates

/** Updates: Create channel, Discover channels, Status privacy, Starred, Events, Settings. */
export function updatesMenu(can: { signedIn: boolean; createChannel: boolean }, sections: Sections): MenuItem[] {
  if (!can.signedIn) return [];
  const items: MenuItem[] = [];
  if (can.createChannel) items.push({ key: "create-channel", label: "Create channel" });
  items.push({ key: "discover-channels", label: "Discover channels" });
  items.push({ key: "status-privacy", label: "Status privacy", href: ROUTES.statusPrivacy });
  items.push({ key: "starred", label: "Starred", href: ROUTES.starred });
  if (sections.events) items.push({ key: "events", label: "Events", href: ROUTES.events });
  items.push({ key: "settings", label: "Settings", href: ROUTES.settings });
  return items;
}

// ------------------------------------------------------------------ Chats

/** Chats: New group, New conversation, Custom lists, Starred messages, Events, Settings. */
export function chatsMenu(can: { messaging: boolean; lists: boolean }, sections: Sections): MenuItem[] {
  const items: MenuItem[] = [];
  if (can.messaging) {
    items.push({ key: "new-group", label: "New group" });
    items.push({ key: "new-chat", label: "New conversation" });
  }
  if (can.lists) items.push({ key: "lists", label: "Custom lists" });
  items.push({ key: "starred", label: "Starred messages", href: ROUTES.starred });
  if (sections.events) items.push({ key: "events", label: "Events", href: ROUTES.events });
  items.push({ key: "settings", label: "Settings" });
  return items;
}

// ------------------------------------------------------------------ Communities

/** Communities: New community, Events, Settings — only once the Communities section exists. */
export function communitiesMenu(can: { createCommunity: boolean }, sections: Sections): MenuItem[] {
  if (!sections.communities) return [];
  const items: MenuItem[] = [];
  if (can.createCommunity) items.push({ key: "new-community", label: "New community" });
  if (sections.events) items.push({ key: "events", label: "Events", href: ROUTES.events });
  items.push({ key: "settings", label: "Settings", href: ROUTES.settings });
  return items;
}

// ------------------------------------------------------------------ My status

/** One of my statuses as the menu sees it: live on the server, or still in this browser's queue. */
export type MyStatusState =
  | { kind: "live"; readReceipts: boolean; allowSharing: boolean }
  | { kind: "pending"; failed: boolean; fatal: boolean };

/**
 * My status: Status privacy, Viewers, Sharing, Delete; for an upload still being added,
 * Retry (failed and retryable) and Cancel. Viewers only while read receipts are on (the
 * server lists none otherwise).
 */
export function myStatusMenu(s: MyStatusState): MenuItem[] {
  if (s.kind === "pending") {
    const items: MenuItem[] = [];
    if (s.failed && !s.fatal) items.push({ key: "retry", label: "Retry" });
    items.push({ key: "cancel", label: "Cancel upload", danger: true });
    return items;
  }
  const items: MenuItem[] = [{ key: "privacy", label: "Status privacy", href: ROUTES.statusPrivacy }];
  if (s.readReceipts) items.push({ key: "viewers", label: "Viewers" });
  items.push({ key: "sharing", label: s.allowSharing ? "Turn off sharing" : "Allow sharing" });
  items.push({ key: "replies", label: "Replies" });
  items.push({ key: "delete", label: "Delete", danger: true });
  return items;
}

// ------------------------------------------------------------------ a channel

/** What the caller may do in one channel (from the channel the server returned). */
export type ChannelAbilities = {
  isOwner: boolean;
  following: boolean;
  /** Per-follower channel notifications exist on this build. */
  notifications: boolean;
  /** Reporting a channel exists on this build (HQ reviews reports through the admin bridge). */
  report: boolean;
  /** An admin (not the owner) with management rights. */
  admin?: boolean;
  /** The channel can be shared (the Web Share API or the clipboard is available). */
  share: boolean;
  /** A channel management screen exists on this build (admins, invites, analytics). */
  manage: boolean;
};

/**
 * A channel: Channel info, Notifications, Share, Report, Unfollow, and the management
 * options of its owner / admins. Owners never report or unfollow their own channel.
 */
export function channelMenu(c: ChannelAbilities): MenuItem[] {
  const items: MenuItem[] = [{ key: "info", label: "Channel info" }];
  if (c.following && c.notifications && !c.isOwner) items.push({ key: "notifications", label: "Notifications" });
  if (c.share) items.push({ key: "share", label: "Share" });
  if (!c.isOwner && c.report) items.push({ key: "report", label: "Report", danger: true });
  if (!c.isOwner && c.following) items.push({ key: "unfollow", label: "Unfollow" });
  if ((c.isOwner || c.admin) && c.manage) items.push({ key: "manage", label: "Manage channel" });
  if (c.isOwner) items.push({ key: "delete", label: "Delete channel", danger: true });
  return items;
}
