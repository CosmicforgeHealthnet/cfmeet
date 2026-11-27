/**
 * What a chat is called (chat list, chat header, chat info) and how a private group's name
 * travels.
 *
 * - A direct chat is named after the OTHER person: your saved contact alias for them, else
 *   their display name. It is never a list of members and never includes you; the generic
 *   "CF Meet user" is used only while nobody's name is known.
 * - A group shows its name: the server-held title of a MANAGED group, or, for a PRIVATE_E2EE
 *   group, the name its members' devices exchanged inside the end-to-end encrypted group.
 *   The server never holds a private group's name (Messaging refuses `title` for
 *   PRIVATE_E2EE; ADR 0052, AD-08 spirit: no plaintext the server could read). Without a
 *   name, a group is named after its other members.
 *
 * The private group name is carried as an ordinary encrypted text payload with a reserved,
 * versioned envelope (the browser crypto core accepts only text and file payloads), so it is
 * encrypted exactly like a message and only member devices can read it.
 *
 * No runtime imports: node tests run this file directly (tests/messaging-titles.test.mjs).
 */

export const UNKNOWN_PERSON = "CF Meet user";
export const MAX_TITLE = 120;

/** U+2063 (invisible separator) cannot be typed by accident: a message is never mistaken for a name. */
const GROUP_TITLE_ENVELOPE = "⁣cfmeet:group-title:v1⁣";

/** The same rule Messaging applies to a MANAGED title: 1-120 characters, no control characters. */
export function normalizeTitle(title: unknown): string | null {
  if (typeof title !== "string") return null;
  const t = title.trim();
  if (!t || [...t].length > MAX_TITLE || /[\u0000-\u001f\u007f-\u009f]/.test(t)) return null;
  return t;
}

/** The encrypted text that names a private group (sent like a message, read only by member devices). */
export function encodeGroupTitle(title: string): string {
  const t = normalizeTitle(title);
  if (!t) throw new Error("TITLE_INVALID");
  return `${GROUP_TITLE_ENVELOPE}${JSON.stringify({ title: t })}`;
}

/** The group name carried by a decrypted text, or null when the text is an ordinary message. */
export function decodeGroupTitle(text: unknown): string | null {
  if (typeof text !== "string" || !text.startsWith(GROUP_TITLE_ENVELOPE)) return null;
  try {
    const v: unknown = JSON.parse(text.slice(GROUP_TITLE_ENVELOPE.length));
    return v && typeof v === "object" ? normalizeTitle((v as { title?: unknown }).title) : null;
  } catch {
    return null;
  }
}

/** Whether a decrypted text is a private-group name control message (never shown as a message). */
export function isGroupTitleText(text: unknown): boolean {
  return typeof text === "string" && text.startsWith(GROUP_TITLE_ENVELOPE);
}

export type TitleRecord = { sequence: number; senderPrincipalId: string | null; text: string | null };

/**
 * The name of a private group from its decrypted history (records in sequence order): the
 * latest valid name wins. `changes` lists the records that changed the name (each later
 * repetition of the same name, e.g. re-shared for a newly added device, changes nothing).
 */
export function groupTitleFromRecords(records: readonly TitleRecord[], initial: string | null = null): { title: string | null; changes: Map<number, string> } {
  let title = initial;
  const changes = new Map<number, string>();
  for (const r of records) {
    const t = decodeGroupTitle(r.text);
    if (!t) continue;
    if (t !== title) changes.set(r.sequence, t);
    title = t;
  }
  return { title, changes };
}

export type TitleInput = {
  contextClass: string;
  /** MANAGED title held by Messaging. */
  serverTitle?: string | null;
  /** PRIVATE_E2EE group name read from the encrypted group on this device. */
  groupTitle?: string | null;
  members: readonly string[];
  me: string | null | undefined;
  /** principalId -> the name to show (contact alias first, then display name). */
  names: Readonly<Record<string, string | undefined>>;
  /** The Hub's view of a direct chat's other person, when known. */
  counterpart?: { self?: boolean; alias?: string | null; displayName?: string | null } | null;
};

/** The person a direct chat is with, as a label (alias, then display name), or null when unknown. */
export function directCounterpartLabel(input: Pick<TitleInput, "members" | "me" | "names" | "counterpart">): string | null {
  const c = input.counterpart;
  if (c && !c.self) {
    const label = c.alias?.trim() || c.displayName?.trim();
    if (label) return label;
  }
  const others = input.members.filter((m) => m !== input.me);
  for (const m of others) {
    const name = input.names[m]?.trim();
    if (name) return name;
  }
  return null;
}

export function conversationTitle(input: TitleInput): string {
  if (input.contextClass === "DIRECT") {
    return directCounterpartLabel(input) ?? UNKNOWN_PERSON;
  }
  const explicit = normalizeTitle(input.serverTitle) ?? normalizeTitle(input.groupTitle);
  if (explicit) return explicit;
  const others = input.members.filter((m) => m !== input.me);
  if (others.length) return others.map((m) => input.names[m]?.trim() || UNKNOWN_PERSON).join(", ");
  return input.contextClass === "GROUP" ? "Group conversation" : "Conversation";
}
