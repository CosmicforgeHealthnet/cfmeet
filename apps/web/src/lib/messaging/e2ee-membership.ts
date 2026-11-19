/**
 * Pure rules of the web client's private-chat (MLS) membership, shared by the engine and
 * the UI and tested on their own (tests/messaging-e2ee-membership.test.mjs):
 *
 * * which members' devices a sender adds before it encrypts (WhatsApp: a message reaches
 *   every device the others registered, from the KeyPackages they published in advance);
 * * how many KeyPackages a device publishes to keep its stock;
 * * how pending removals are committed (also of devices that never entered the tree);
 * * what a chat shows for messages this device cannot read (yet): never nothing.
 *
 * No key material and no network here.
 */

/** Below this many published KeyPackages a device publishes more (the server's low watermark)… */
export const KEY_PACKAGE_STOCK = 10;
/** …up to this many, so others can add it to new chats while it is offline. */
export const KEY_PACKAGE_TARGET = 20;
/** The most KeyPackages one publication may carry (Messaging `MAX_KEY_PACKAGES_PER_REQUEST`). */
const MAX_PER_REQUEST = 20;

/** Shown in a chat for a message this device cannot read (yet). */
export const WAITING_NOTE = "Waiting for this message. This may take a while.";
/** Chat-list preview of such a message (short, never a blank bar). */
export const WAITING_PREVIEW = "Waiting for this message";

export type DeviceLike = { deviceId: string; status: string; keyPackagesAvailable?: number };
export type MemberLike = { principalId: string; state?: string };

/**
 * How many KeyPackages this device should publish now: none while the stock is at least
 * KEY_PACKAGE_STOCK (or the server does not report it), otherwise up to KEY_PACKAGE_TARGET.
 */
export function keyPackagesToPublish(available: number | undefined | null): number {
  if (typeof available !== "number" || !Number.isFinite(available) || available >= KEY_PACKAGE_STOCK) return 0;
  return Math.min(MAX_PER_REQUEST, KEY_PACKAGE_TARGET - Math.max(0, Math.floor(available)));
}

/**
 * The ACTIVE devices of ACTIVE members that are not leaves of the group's tree (this
 * device excluded): what a member device adds, and the principals to claim them from.
 */
export function missingDevices(
  members: MemberLike[],
  devicesOf: (principalId: string) => DeviceLike[],
  inTree: ReadonlySet<string>,
  self: string | null,
): { principals: string[]; deviceIds: string[]; activeInTree: string[] } {
  const principals: string[] = [];
  const deviceIds: string[] = [];
  const activeInTree: string[] = [];
  for (const m of members) {
    if (m.state && m.state !== "ACTIVE") continue;
    const fresh: string[] = [];
    for (const d of devicesOf(m.principalId)) {
      if (d.status !== "ACTIVE") continue;
      if (inTree.has(d.deviceId)) activeInTree.push(d.deviceId);
      else if (d.deviceId !== self) fresh.push(d.deviceId);
    }
    if (fresh.length) {
      principals.push(m.principalId);
      deviceIds.push(...fresh);
    }
  }
  return { principals, deviceIds, activeInTree };
}

/**
 * The commit that clears the server's pending removals: leaves still in the tree are
 * removed; every pending device is attested (one that never entered the tree needs no
 * leaf removal, so a key-update commit carries the attestation). `null`: nothing to do,
 * or this device itself is to be removed.
 */
export function removalPlan(pending: string[], inTree: ReadonlySet<string>, self: string | null): { remove: string[]; attest: string[] } | null {
  if (!pending.length || (self && pending.includes(self))) return null;
  return { remove: pending.filter((d) => inTree.has(d)), attest: [...pending] };
}

/** The worker's joiner-policy entry for a verified device: `"<device id>:<hex key>"`. */
export function joinerEntry(deviceId: string, signatureKey: Uint8Array): string {
  return `${deviceId}:${Array.from(signatureKey, (b) => b.toString(16).padStart(2, "0")).join("")}`;
}

/** The note on a message sent before this device was linked (older history). */
export function beforeJoinNote(hasOtherDevice: boolean): string {
  return hasOtherDevice
    ? "Sent before this device was linked. Open CF Meet on your other device to transfer older messages."
    : "Sent before this device was linked, so it can't be shown here.";
}

/** What the chat list shows under a chat's title. */
export type PreviewInput = {
  e2ee: boolean;
  /** This device holds the chat's group. */
  groupReady: boolean;
  /** The chat's messages were rendered at least once on this device. */
  loaded: boolean;
  latest: { text: string | null; file: { name: string } | null; note: string | null; state: string } | null | undefined;
  summary: { lifecycle: string; previewText: string | null; attachmentCount: number } | null | undefined;
};

/**
 * The preview text, or `null` while a readable private chat is still being loaded (a
 * brief placeholder bar). A private message this device cannot read (yet) is shown as
 * WAITING_PREVIEW, never as an empty or endless placeholder.
 */
export function chatPreview({ e2ee, groupReady, loaded, latest, summary }: PreviewInput): string | null {
  if (latest) {
    if (latest.text) return latest.text;
    if (latest.file) return `📎 ${latest.file.name}`;
    if (latest.state === "unreadable" || latest.state === "before-join") return WAITING_PREVIEW;
    return latest.note ?? "";
  }
  if (!summary) return "No messages yet";
  if (summary.lifecycle !== "ACTIVE") return "Message deleted";
  if (!e2ee) return summary.previewText ?? (summary.attachmentCount ? "📎 Attachment" : "");
  return groupReady && !loaded ? null : WAITING_PREVIEW;
}
