/**
 * Structured messages inside a private chat's encrypted content (F4-F7, WhatsApp): link
 * previews, contact cards (a CF Meet ID), locations and polls. Everything here travels
 * end-to-end encrypted; the crypto worker validates every shape on the way in and out.
 * A link preview is built by the sender's device from the link itself — nothing is
 * fetched, by this browser or by the server.
 */

export type LinkPreview = { url: string; title: string | null; description: string | null; siteName: string | null; imageDataUrl?: string | null };
export type PollTally = { counts: number[]; voters: number; myChoices: number[] };

export type RichContent =
  | { kind: "contact"; cfMeetId: string; name: string }
  | { kind: "location"; lat: number; lng: number; label: string | null }
  | { kind: "poll"; question: string; options: string[]; multiple: boolean; tally: PollTally }
  | { kind: "link"; preview: LinkPreview };

/** Payloads the page asks the worker to encrypt (besides text and files). */
export type RichPayload =
  | { t: "text"; text: string; preview?: LinkPreview }
  | { t: "contact"; cfMeetId: string; name: string }
  | { t: "location"; lat: number; lng: number; label: string | null }
  | { t: "poll"; question: string; options: string[]; multiple: boolean }
  | { t: "vote"; poll: string; choices: number[] }
  | { t: "reaction"; target: string; reaction: string; add: boolean };

type StoredPayload = { t?: string; [k: string]: unknown } | null | undefined;

const URL_IN_TEXT = /\bhttps?:\/\/[^\s<>"']{3,2040}/i;

/** The sender-side preview of the first link in a message (no fetch: the link and its site). */
export function linkPreviewFor(text: string): LinkPreview | null {
  const match = URL_IN_TEXT.exec(text);
  if (!match) return null;
  try {
    const url = new URL(match[0].replace(/[).,;:!?]+$/, ""));
    if (url.protocol !== "https:" && url.protocol !== "http:") return null;
    return { url: url.href, title: null, description: null, siteName: url.hostname.replace(/^www\./, "") };
  } catch {
    return null;
  }
}

/** The text a structured message shows in the chat list, search and replies. */
export function richSummary(p: StoredPayload): string | null {
  switch (p?.t) {
    case "contact": return `Contact: ${String(p.name)}`;
    case "location": return p.label ? `Location: ${String(p.label)}` : "Location";
    case "poll": return `Poll: ${String(p.question)}`;
    default: return null;
  }
}

/** A vote is a message of its own; the latest vote of each sender counts (an empty one retracts). */
export type VoteRecord = { poll: string; choices: number[]; sender: string | null; sequence: number };

export function tallyVotes(votes: VoteRecord[], pollMessageId: string, options: number, me: string | null): PollTally {
  const latest = new Map<string, VoteRecord>();
  for (const v of votes) {
    if (v.poll !== pollMessageId || !v.sender) continue;
    const prev = latest.get(v.sender);
    if (!prev || prev.sequence < v.sequence) latest.set(v.sender, v);
  }
  const counts = Array.from({ length: options }, () => 0);
  let voters = 0;
  for (const v of latest.values()) {
    const valid = v.choices.filter((c) => c >= 0 && c < options);
    if (valid.length) voters++;
    for (const c of valid) counts[c]++;
  }
  return { counts, voters, myChoices: me ? (latest.get(me)?.choices ?? []).filter((c) => c < options) : [] };
}

/** The structured content of a stored payload (null: plain text or a file). */
export function richContent(p: StoredPayload, messageId: string | null, votes: VoteRecord[], me: string | null): RichContent | null {
  switch (p?.t) {
    case "contact": return { kind: "contact", cfMeetId: String(p.cfMeetId), name: String(p.name) };
    case "location": return { kind: "location", lat: Number(p.lat), lng: Number(p.lng), label: (p.label as string | null) ?? null };
    case "poll": {
      const options = (p.options as string[]) ?? [];
      return { kind: "poll", question: String(p.question), options, multiple: !!p.multiple, tally: messageId ? tallyVotes(votes, messageId, options.length, me) : { counts: options.map(() => 0), voters: 0, myChoices: [] } };
    }
    case "text": return p.preview ? { kind: "link", preview: p.preview as LinkPreview } : null;
    default: return null;
  }
}

/** A map link for a shared location (opened in a new tab; nothing is loaded inside CF Meet). */
export const mapUrl = (lat: number, lng: number) => `https://www.openstreetmap.org/?mlat=${lat}&mlon=${lng}#map=16/${lat}/${lng}`;

/** Private reactions are authenticated encrypted APPLICATION controls, not server rows. */
export type PrivateReactionRecord = { target: string; reaction: string; add: boolean; sender: string; sequence: number };
export function privateReactionState(controls: PrivateReactionRecord[], targets: ReadonlyMap<string, number>, me: string | null): Map<string, { reaction: string; count: number; reactedByMe: boolean }[]> {
  const latest = new Map<string, PrivateReactionRecord>();
  for (const control of controls) {
    const targetSequence = targets.get(control.target);
    if (targetSequence === undefined || targetSequence >= control.sequence || !control.sender) continue;
    const key = JSON.stringify([control.target, control.sender, control.reaction]);
    const previous = latest.get(key);
    if (!previous || previous.sequence < control.sequence) latest.set(key, control);
  }
  const perActor = new Map<string, number>();
  const grouped = new Map<string, Map<string, Set<string>>>();
  for (const control of [...latest.values()].sort((a, b) => b.sequence - a.sequence)) {
    if (!control.add) continue;
    const actor = JSON.stringify([control.target, control.sender]);
    const count = perActor.get(actor) ?? 0;
    if (count >= 20) continue;
    perActor.set(actor, count + 1);
    const reactions = grouped.get(control.target) ?? new Map<string, Set<string>>();
    const senders = reactions.get(control.reaction) ?? new Set<string>();
    senders.add(control.sender); reactions.set(control.reaction, senders); grouped.set(control.target, reactions);
  }
  return new Map([...grouped].map(([target, reactions]) => [target, [...reactions].sort(([a], [b]) => a.localeCompare(b)).map(([reaction, senders]) => ({ reaction, count: senders.size, reactedByMe: me !== null && senders.has(me) }))]));
}
