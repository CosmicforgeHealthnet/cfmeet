/**
 * Status privacy and management: the pure rules behind the Updates screens (ADR 0069).
 *
 * The server decides who may see a status (snapshots, one gate); nothing here filters what
 * anyone can see. These helpers only turn the server's answers into labels, counts and
 * picker lists, so the screens and the unit tests agree.
 *
 * Type-only imports: unit-tested under `node --experimental-strip-types`.
 */

export type AudienceMode = "CONTACTS" | "CONTACTS_EXCEPT" | "ONLY" | "CLOSE_FRIENDS" | "CUSTOM";

export interface AudienceSelection {
  mode: AudienceMode;
  /** CONTACTS_EXCEPT: who is left out; ONLY: who is in. */
  principalIds?: string[];
  /** CUSTOM: the named list. */
  listId?: string | null;
}

export interface NamedList {
  listId: string;
  name: string;
  memberPrincipalIds: string[];
}

export interface Named {
  principalId: string;
  name: string;
}

export const MODE_TITLES: Record<AudienceMode, string> = {
  CONTACTS: "My contacts",
  CONTACTS_EXCEPT: "My contacts except…",
  ONLY: "Only share with…",
  CLOSE_FRIENDS: "Close friends",
  CUSTOM: "Custom list",
};

/** The short audience text: "My contacts", "8 excluded", "12 selected", "Close friends", a list's name. */
export function audienceShortLabel(sel: AudienceSelection | null | undefined, lists: NamedList[] = []): string {
  if (!sel) return "My contacts";
  const n = sel.principalIds?.length ?? 0;
  switch (sel.mode) {
    case "CONTACTS_EXCEPT":
      return n ? `${n} excluded` : "My contacts";
    case "ONLY":
      return `${n} selected`;
    case "CLOSE_FRIENDS":
      return "Close friends";
    case "CUSTOM":
      return lists.find((l) => l.listId === sel.listId)?.name ?? "Custom list";
    default:
      return "My contacts";
  }
}

/** The audience button beside Publish: "Status (My contacts)", "Status (8 excluded)", "Status (12 selected)", "Close friends", a list's name. */
export function audienceButtonLabel(sel: AudienceSelection | null | undefined, lists: NamedList[] = []): string {
  const short = audienceShortLabel(sel, lists);
  if (sel?.mode === "CLOSE_FRIENDS" || sel?.mode === "CUSTOM") return short;
  return `Status (${short})`;
}

/** The label of a published status's stored audience snapshot (owner view). */
export function snapshotLabel(a: { mode: AudienceMode; label: string | null; includedCount: number; excludedCount: number } | null | undefined): string {
  if (!a) return "My contacts";
  switch (a.mode) {
    case "CONTACTS_EXCEPT":
      return a.excludedCount ? `${a.excludedCount} excluded` : "My contacts";
    case "ONLY":
      return `${a.includedCount} selected`;
    case "CLOSE_FRIENDS":
      return "Close friends";
    case "CUSTOM":
      return a.label || "Custom list";
    default:
      return "My contacts";
  }
}

/**
 * A choice the server will accept: ONLY needs at least one person, CUSTOM a list that exists.
 * Ids no longer eligible are dropped (the server would drop them anyway).
 */
export function normalizeSelection(sel: AudienceSelection, eligible: string[], lists: NamedList[] = []): AudienceSelection | null {
  const ok = new Set(eligible);
  switch (sel.mode) {
    case "CONTACTS":
    case "CLOSE_FRIENDS":
      return { mode: sel.mode };
    case "CONTACTS_EXCEPT":
    case "ONLY": {
      const ids = [...new Set(sel.principalIds ?? [])].filter((p) => ok.has(p));
      if (sel.mode === "ONLY" && !ids.length) return null;
      return { mode: sel.mode, principalIds: ids };
    }
    case "CUSTOM":
      return sel.listId && lists.some((l) => l.listId === sel.listId) ? { mode: "CUSTOM", listId: sel.listId } : null;
  }
}

/** The wire shape for a status create / reshare / the saved default. */
export function selectionToChoice(sel: AudienceSelection): { mode: AudienceMode; principalIds?: string[]; listId?: string } {
  if (sel.mode === "CUSTOM") return { mode: "CUSTOM", listId: sel.listId ?? undefined };
  if (sel.mode === "CONTACTS_EXCEPT" || sel.mode === "ONLY") return { mode: sel.mode, principalIds: [...(sel.principalIds ?? [])] };
  return { mode: sel.mode };
}

/**
 * Who a choice reaches among the eligible contacts right now (for counts and for limiting the
 * @mention picker). The server resolves the real snapshot at publish time.
 */
export function audienceMembers(sel: AudienceSelection, eligible: string[], closeFriends: string[] = [], lists: NamedList[] = []): string[] {
  const pick = (ids: string[]) => {
    const want = new Set(ids);
    return eligible.filter((p) => want.has(p));
  };
  switch (sel.mode) {
    case "CONTACTS":
      return [...eligible];
    case "CONTACTS_EXCEPT": {
      const out = new Set(sel.principalIds ?? []);
      return eligible.filter((p) => !out.has(p));
    }
    case "ONLY":
      return pick(sel.principalIds ?? []);
    case "CLOSE_FRIENDS":
      return pick(closeFriends);
    case "CUSTOM":
      return pick(lists.find((l) => l.listId === sel.listId)?.memberPrincipalIds ?? []);
  }
}

/** Case- and accent-insensitive search over names. */
export function fold(s: string): string {
  return s.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase().trim();
}

export function searchPeople<T extends { name: string }>(people: T[], q: string): T[] {
  const f = fold(q);
  if (!f) return people;
  return people.filter((p) => fold(p.name).includes(f));
}

export function toggle(ids: string[], id: string): string[] {
  return ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id];
}

/** Select every one of `visible` (keeps earlier picks outside it). */
export function selectAll(ids: string[], visible: string[]): string[] {
  return [...new Set([...ids, ...visible])];
}

/** Deselect every one of `visible` (keeps picks outside it). */
export function deselectAll(ids: string[], visible: string[]): string[] {
  const drop = new Set(visible);
  return ids.filter((x) => !drop.has(x));
}

// ---------------------------------------------------------------------------------- mentions

/**
 * The @mention being typed at `caret`: the text after the last "@" that starts a word, while it
 * has no line break and at most 40 characters. null when the caret is not in one.
 */
export function mentionQuery(text: string, caret: number): { start: number; query: string } | null {
  const before = text.slice(0, caret);
  const at = before.lastIndexOf("@");
  if (at < 0) return null;
  if (at > 0 && !/\s/.test(before[at - 1])) return null;
  const query = before.slice(at + 1);
  if (query.length > 40 || /[\n@]/.test(query)) return null;
  return { start: at, query };
}

/** Inserts "@Name " for the picked person in place of the query being typed. */
export function insertMention(text: string, caret: number, name: string): { text: string; caret: number } {
  const q = mentionQuery(text, caret);
  const start = q ? q.start : caret;
  const token = `@${name} `;
  return { text: text.slice(0, start) + token + text.slice(caret), caret: start + token.length };
}

/** The picked people whose "@Name" is still in the text (deleting the name drops the mention). */
export function mentionsInText(text: string, picked: Named[]): string[] {
  const out: string[] = [];
  for (const p of picked) if (text.includes(`@${p.name}`) && !out.includes(p.principalId)) out.push(p.principalId);
  return out;
}

/** Mentions are kept only for people the chosen audience reaches (the server enforces the same). */
export function mentionsWithinAudience(mentions: string[], members: string[]): string[] {
  const ok = new Set(members);
  return mentions.filter((m) => ok.has(m));
}

// ---------------------------------------------------------------------------------- my status

/** "23h left", "45m left", "Expiring" for a status expiring at `expiresAt`. */
export function timeLeft(expiresAt: string, now: number): string {
  const ms = Date.parse(expiresAt) - now;
  if (!Number.isFinite(ms) || ms <= 60_000) return "Expiring";
  const h = Math.floor(ms / 3_600_000);
  if (h >= 1) return `${h}h left`;
  return `${Math.floor(ms / 60_000)}m left`;
}

export interface ViewerLike {
  principalId: string;
  viewedAt: string;
  reaction?: string | null;
  liked?: boolean;
}

/** One entry per person (the latest view), newest first. */
export function uniqueViewers<T extends ViewerLike>(viewers: T[]): T[] {
  const by = new Map<string, T>();
  for (const v of viewers) {
    const had = by.get(v.principalId);
    if (!had || Date.parse(v.viewedAt) > Date.parse(had.viewedAt)) by.set(v.principalId, v);
  }
  return [...by.values()].sort((a, b) => Date.parse(b.viewedAt) - Date.parse(a.viewedAt));
}

/** Reactions of the viewers, most frequent first ("❤️ 3"); a like without an emoji counts as ❤️. */
export function reactionSummary(viewers: ViewerLike[]): { emoji: string; count: number }[] {
  const counts = new Map<string, number>();
  for (const v of viewers) {
    const e = v.reaction || (v.liked ? "❤️" : null);
    if (e) counts.set(e, (counts.get(e) ?? 0) + 1);
  }
  return [...counts.entries()].map(([emoji, count]) => ({ emoji, count })).sort((a, b) => b.count - a.count || a.emoji.localeCompare(b.emoji));
}

// ---------------------------------------------------------------------------------- rings

export interface RingLike {
  principalId: string;
  hasActiveStatus: boolean;
  count: number;
  unseenCount: number;
  closeFriends: boolean;
}

export type RingLook = { kind: "none" } | { kind: "ring"; unseen: boolean; closeFriends: boolean; segments: number; seenSegments: number };

/** How an avatar's ring looks: none when nothing is visible, segmented by status count. */
export function ringLook(ring: RingLike | null | undefined): RingLook {
  if (!ring || !ring.hasActiveStatus || ring.count <= 0) return { kind: "none" };
  const segments = Math.min(ring.count, 30);
  const unseen = Math.min(ring.unseenCount, ring.count);
  return { kind: "ring", unseen: unseen > 0, closeFriends: ring.closeFriends, segments, seenSegments: Math.max(0, segments - Math.min(unseen, segments)) };
}

/**
 * SVG arcs of a segmented ring (WhatsApp): `segments` arcs with a small gap, the first
 * `seenSegments` drawn as seen. Angles in degrees, clockwise from the top.
 */
export function ringArcs(segments: number, seenSegments: number, gapDeg = 6): { from: number; to: number; seen: boolean }[] {
  if (segments <= 1) return [{ from: 0, to: 360, seen: seenSegments >= 1 }];
  const each = 360 / segments;
  const gap = Math.min(gapDeg, each / 3);
  return Array.from({ length: segments }, (_, i) => ({ from: i * each + gap / 2, to: (i + 1) * each - gap / 2, seen: i < seenSegments }));
}

/** Unique principals, at most 200 per request (the rings API limit), stable order. */
export function ringBatches(ids: (string | null | undefined)[], size = 200): string[][] {
  const unique = [...new Set(ids.filter((x): x is string => !!x))].sort();
  const out: string[][] = [];
  for (let i = 0; i < unique.length; i += size) out.push(unique.slice(i, i + size));
  return out;
}

/**
 * Contacts (known by CF Meet ID) to Messaging principals (known by the names API), display-only:
 * a contact is matched only when exactly one principal carries the same name (and nickname),
 * and that principal matches no other contact. Ambiguous names get no ring rather than a wrong one.
 */
export function matchContacts(
  contacts: { cfMeetId: string; displayName: string; alias: string | null }[],
  principals: { principalId: string; displayName: string; alias?: string | null }[],
): Map<string, string> {
  const key = (name: string, alias: string | null | undefined) => `${fold(name)}\u0000${fold(alias ?? "")}`;
  const byKey = new Map<string, string[]>();
  for (const p of principals) {
    const k = key(p.displayName, p.alias);
    byKey.set(k, [...(byKey.get(k) ?? []), p.principalId]);
  }
  const contactKeys = new Map<string, number>();
  for (const c of contacts) contactKeys.set(key(c.displayName, c.alias), (contactKeys.get(key(c.displayName, c.alias)) ?? 0) + 1);
  const out = new Map<string, string>();
  for (const c of contacts) {
    const k = key(c.displayName, c.alias);
    const hits = byKey.get(k) ?? [];
    if (hits.length === 1 && contactKeys.get(k) === 1) out.set(c.cfMeetId, hits[0]);
  }
  return out;
}

// ---------------------------------------------------------------------------------- updates

/** Status search on the Updates page: authors by name, and text / caption of their statuses. */
export function searchStatusAuthors<A extends { person: { name: string }; posts: { text: string | null; caption: string | null }[] }>(authors: A[], q: string): A[] {
  const f = fold(q);
  if (!f) return authors;
  return authors.filter((a) => fold(a.person.name).includes(f) || a.posts.some((p) => fold(p.text ?? "").includes(f) || fold(p.caption ?? "").includes(f)));
}

/** Whether "Forward" / "Reshare" may be offered: the status (and a reshared original) allows sharing. */
export function canShare(post: { allowSharing?: boolean; origin?: unknown }, mine: boolean): boolean {
  return !mine && post.allowSharing !== false;
}
