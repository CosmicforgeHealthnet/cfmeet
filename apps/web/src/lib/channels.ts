/**
 * Channels (ADR 0072): pure helpers shared by the channel screens. No network, no React:
 * the server (Messaging) enforces every rule; these only mirror them for instant feedback.
 */

export const CATEGORIES: readonly { code: string; label: string }[] = [
  { code: "NEWS", label: "News" }, { code: "ENTERTAINMENT", label: "Entertainment" }, { code: "SPORTS", label: "Sports" },
  { code: "BUSINESS", label: "Business" }, { code: "TECHNOLOGY", label: "Technology" }, { code: "EDUCATION", label: "Education" },
  { code: "HEALTH", label: "Health" }, { code: "LIFESTYLE", label: "Lifestyle" }, { code: "MUSIC", label: "Music" },
  { code: "GAMING", label: "Gaming" }, { code: "SCIENCE", label: "Science" }, { code: "POLITICS", label: "Politics" },
  { code: "RELIGION", label: "Religion" }, { code: "TRAVEL", label: "Travel" }, { code: "FOOD", label: "Food" },
  { code: "FASHION", label: "Fashion" }, { code: "ART", label: "Art" }, { code: "COMMUNITY", label: "Community" },
  { code: "GOVERNMENT", label: "Government" }, { code: "ORGANIZATION", label: "Organization" }, { code: "OTHER", label: "Other" },
];

export const REPORT_CATEGORIES: readonly { code: string; label: string }[] = [
  { code: "SPAM", label: "Spam" }, { code: "SCAM", label: "Scam or fraud" }, { code: "HARASSMENT", label: "Harassment or bullying" },
  { code: "HATE", label: "Hate speech" }, { code: "VIOLENCE", label: "Violence or threats" },
  { code: "SEXUAL_CONTENT", label: "Sexual content" }, { code: "CHILD_SAFETY", label: "Child safety" },
  { code: "SELF_HARM", label: "Self-harm" }, { code: "IMPERSONATION", label: "Impersonation" },
  { code: "MISINFORMATION", label: "False information" }, { code: "COPYRIGHT", label: "Copyright" },
  { code: "ILLEGAL_CONTENT", label: "Illegal content" }, { code: "OTHER", label: "Something else" },
];

export const VERIFICATION_BASES: readonly { code: string; label: string }[] = [
  { code: "PUBLIC_FIGURE", label: "Public figure" }, { code: "BRAND", label: "Brand or business" },
  { code: "ORGANIZATION", label: "Organization" }, { code: "MEDIA", label: "News or media" },
  { code: "GOVERNMENT", label: "Government" }, { code: "CREATOR", label: "Creator" }, { code: "OTHER", label: "Other" },
];

/** Common BCP 47 languages offered in the picker (any valid code is accepted by the server). */
export const LANGUAGES: readonly string[] = [
  "en", "fr", "es", "pt", "ar", "ha", "yo", "ig", "sw", "am", "zu", "af", "de", "it", "nl", "ru", "tr", "fa", "ur", "hi", "bn",
  "zh", "ja", "ko", "id", "ms", "vi", "th", "pl", "uk", "ro", "el", "he", "sv", "no", "da", "fi",
];

/** ISO 3166-1 alpha-2 codes. */
export const COUNTRIES: readonly string[] = (
  "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BL BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ " +
  "DE DJ DK DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT GU GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT " +
  "JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA NC NE NF NG NI " +
  "NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL " +
  "TM TN TO TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW"
).split(" ");

const RESERVED = new Set([
  "admin", "administrator", "support", "help", "helpdesk", "system", "root", "moderator", "moderation", "staff", "security",
  "settings", "account", "accounts", "billing", "api", "www", "about", "privacy", "terms", "legal", "channel", "channels",
  "status", "updates", "meet", "meeting", "meetings", "null", "undefined", "everyone", "here", "owner", "team", "trust",
  "safety", "abuse", "report", "reports", "notifications",
]);
const RESERVED_WORDS = ["cfmeet", "official", "verified"];

/** The canonical spelling of a typed username (`@Daily_News` → `daily_news`). */
export function normalizeHandle(raw: string): string {
  return raw.trim().replace(/^@+/, "").toLowerCase();
}

/** Why a username can't be used, mirroring the server (`null`: fine if nobody holds it). */
export function handleProblem(handle: string): "INVALID" | "RESERVED" | null {
  if (!/^[a-z][a-z0-9_]{3,30}[a-z0-9]$/.test(handle) || handle.includes("__")) return "INVALID";
  if (RESERVED.has(handle) || RESERVED_WORDS.some((w) => handle.includes(w))) return "RESERVED";
  return null;
}

export function handleHint(problem: "INVALID" | "RESERVED" | "TAKEN" | null): string | null {
  switch (problem) {
    case "INVALID": return "5–32 lower-case letters, digits or _, starting with a letter.";
    case "RESERVED": return "This username is reserved.";
    case "TAKEN": return "This username is taken.";
    default: return null;
  }
}

const TOKEN = /^mci_[A-Za-z0-9_-]{43}$/;

/** The shareable invite link: the token travels in the URL fragment (never sent to a server). */
export function inviteLink(origin: string, token: string): string {
  return `${origin.replace(/\/+$/, "")}/dashboard/updates/channels/join#${token}`;
}

/** The invite token in a location hash (`#mci_…`), or null. */
export function inviteToken(hash: string): string | null {
  const t = hash.replace(/^#/, "").trim();
  return TOKEN.test(t) ? t : null;
}

export function channelLink(origin: string, channelId: string): string {
  return `${origin.replace(/\/+$/, "")}/dashboard/updates/channels/${channelId}`;
}

export function postLink(origin: string, channelId: string, postId: string): string {
  return `${channelLink(origin, channelId)}?post=${postId}`;
}

/** Whole-number percentages of a poll's options that add up to 100 (largest remainder). */
export function pollPercents(votes: readonly number[]): number[] {
  const total = votes.reduce((a, b) => a + b, 0);
  if (total === 0) return votes.map(() => 0);
  const exact = votes.map((v) => (v * 100) / total);
  const floors = exact.map(Math.floor);
  let left = 100 - floors.reduce((a, b) => a + b, 0);
  const order = exact.map((v, i) => [v - Math.floor(v), i] as const).sort((a, b) => b[0] - a[0] || a[1] - b[1]);
  for (const [, i] of order) {
    if (left <= 0) break;
    floors[i] += 1;
    left -= 1;
  }
  return floors;
}

/** 1234 → "1.2K". */
export function compactCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0).replace(/\.0$/, "")}K`;
  return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, "")}M`;
}

export function followersText(n: number): string {
  return `${compactCount(n)} ${n === 1 ? "follower" : "followers"}`;
}

/** 0.1234 → "12.3%"; null → "—". */
export function percentText(rate: number | null | undefined): string {
  if (rate === null || rate === undefined) return "—";
  return `${(rate * 100).toFixed(1).replace(/\.0$/, "")}%`;
}

/** The value of a `datetime-local` input for a Date (local time, minutes). */
export function toLocalInput(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** The scheduling window the server accepts: 1 minute to 30 days ahead (min rounded up). */
export function scheduleBounds(now: Date): { min: string; max: string } {
  return {
    min: toLocalInput(new Date(now.getTime() + 2 * 60_000)),
    max: toLocalInput(new Date(now.getTime() + 30 * 86_400_000 - 60_000)),
  };
}

export type ChannelMediaKind = "IMAGE" | "VIDEO" | "AUDIO" | "DOCUMENT";

const DOCUMENT_TYPES = new Set([
  "application/pdf", "application/msword", "application/vnd.ms-excel", "application/vnd.ms-powerpoint",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  "application/vnd.oasis.opendocument.text", "application/vnd.oasis.opendocument.spreadsheet",
  "application/epub+zip", "application/zip", "text/plain", "text/csv",
]);

export const MEDIA_LIMITS: Record<ChannelMediaKind, number> = {
  IMAGE: 8 * 1024 * 1024, AUDIO: 8 * 1024 * 1024, VIDEO: 64 * 1024 * 1024, DOCUMENT: 32 * 1024 * 1024,
};

/** The media kind of a picked file (documents by their type), or null when not supported. */
export function mediaKindOf(type: string): ChannelMediaKind | null {
  const t = type.split(";")[0].trim().toLowerCase();
  if (["image/jpeg", "image/png", "image/webp", "image/gif"].includes(t)) return "IMAGE";
  if (["video/mp4", "video/webm", "video/quicktime"].includes(t)) return "VIDEO";
  if (t.startsWith("audio/")) return "AUDIO";
  if (DOCUMENT_TYPES.has(t)) return "DOCUMENT";
  return null;
}

/** The presentations a file can be posted as (beyond its plain kind). */
export function presentationsFor(type: string, size: number): ("VOICE" | "GIF" | "STICKER")[] {
  const t = type.split(";")[0].trim().toLowerCase();
  const out: ("VOICE" | "GIF" | "STICKER")[] = [];
  if (t.startsWith("audio/")) out.push("VOICE");
  if (t === "image/gif" || t.startsWith("video/")) out.push("GIF");
  if ((t === "image/webp" || t === "image/png") && size <= 1024 * 1024) out.push("STICKER");
  return out;
}

/** The first http(s) link in a text, if any (the device builds the preview from it). */
export function firstLink(text: string): string | null {
  const m = text.match(/https?:\/\/[^\s<>"']+/i);
  return m ? m[0].replace(/[).,!?;:]+$/, "") : null;
}

/** An SVG path of a series scaled into width × height (empty for no data). */
export function sparkPath(values: readonly (number | null)[], width: number, height: number): string {
  const pts = values.map((v, i) => [i, v] as const).filter((p): p is readonly [number, number] => p[1] !== null);
  if (pts.length === 0) return "";
  const max = Math.max(1, ...pts.map((p) => p[1]));
  const min = Math.min(0, ...pts.map((p) => p[1]));
  const span = max - min || 1;
  const step = values.length > 1 ? width / (values.length - 1) : 0;
  return pts
    .map(([i, v], n) => `${n === 0 ? "M" : "L"}${(i * step).toFixed(1)},${(height - ((v - min) / span) * height).toFixed(1)}`)
    .join(" ");
}

/** The analytics range presets (UTC days, inclusive). */
export function rangeFor(days: number, today: Date): { from: string; to: string } {
  const to = new Date(Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), today.getUTCDate()));
  const from = new Date(to.getTime() - (days - 1) * 86_400_000);
  const iso = (d: Date) => d.toISOString().slice(0, 10);
  return { from: iso(from), to: iso(to) };
}
