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
  | {
      kind: "poll"; question: string; options: string[]; multiple: boolean; tally: PollTally;
      /** MANAGED polls (server-held): who chose each option, and whether it was closed (and by whom). */
      voters?: string[][]; closed?: boolean; closedBy?: string | null; managed?: boolean;
    }
  | { kind: "link"; preview: LinkPreview };

/** Payloads the page asks the worker to encrypt (besides text and files). */
export type RichPayload =
  | { t: "text"; text: string; preview?: LinkPreview }
  | { t: "contact"; cfMeetId: string; name: string }
  | { t: "location"; lat: number; lng: number; label: string | null }
  | { t: "poll"; question: string; options: string[]; multiple: boolean }
  | { t: "vote"; poll: string; choices: number[] };

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

/** A MANAGED poll as the server returns it (`Message.managed.poll`). */
export type ServerPoll = {
  options: { index: number; text: string; voteCount: number; voterPrincipalIds: string[] }[];
  allowsMultipleAnswers: boolean;
  voterCount: number;
  closed: boolean;
  closedAt?: string | null;
  closedByPrincipalId?: string | null;
};

/** The poll card of a MANAGED poll message (its text is the question). */
export function managedPoll(question: string, poll: ServerPoll, me: string | null): RichContent {
  const options = [...poll.options].sort((a, b) => a.index - b.index);
  return {
    kind: "poll",
    question,
    options: options.map((o) => o.text),
    multiple: poll.allowsMultipleAnswers,
    tally: {
      counts: options.map((o) => o.voteCount),
      voters: poll.voterCount,
      myChoices: me ? options.filter((o) => o.voterPrincipalIds.includes(me)).map((o) => o.index) : [],
    },
    voters: options.map((o) => [...o.voterPrincipalIds]),
    closed: poll.closed,
    closedBy: poll.closedByPrincipalId ?? null,
    managed: true,
  };
}

/** A stable fingerprint of a server poll (a vote or close changes it). */
export const pollFingerprint = (poll: ServerPoll | null | undefined) =>
  poll ? `${poll.closed ? 1 : 0}|${poll.options.map((o) => `${o.index}:${[...o.voterPrincipalIds].sort().join(",")}`).join(";")}` : "";
