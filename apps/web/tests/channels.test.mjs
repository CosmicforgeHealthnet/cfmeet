// ADR 0072: the channel screens' pure helpers mirror the server's rules (usernames, reserved
// words, invite tokens only in URL fragments, poll percentages, scheduling window, media
// kinds), and the web relay allowlist permits exactly the new channel operations.
// Run: node --experimental-strip-types --test tests/channels.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import {
  compactCount, firstLink, handleProblem, inviteLink, inviteToken, mediaKindOf, normalizeHandle, percentText,
  pollPercents, presentationsFor, rangeFor, scheduleBounds, sparkPath, toLocalInput, channelLink, postLink, COUNTRIES, CATEGORIES,
} from "../src/lib/channels.ts";
import { isMessagingRelayRoute } from "../src/lib/messaging/routes.ts";

const ID = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

test("usernames are normalized and validated like the server", () => {
  assert.equal(normalizeHandle("  @Daily_News "), "daily_news");
  assert.equal(handleProblem("daily_news"), null);
  for (const bad of ["abc", "9news", "Daily", "daily-news", "daily_", "da__ily", "a".repeat(33)]) assert.equal(handleProblem(bad), "INVALID", bad);
  for (const reserved of ["admin", "support", "cfmeet_news", "the_official_one", "verifiedme"]) assert.equal(handleProblem(reserved), "RESERVED", reserved);
});

test("invite tokens live only in the URL fragment and are strictly shaped", () => {
  const token = `mci_${"A".repeat(43)}`;
  const link = inviteLink("https://cfmeet.com/", token);
  assert.equal(link, `https://cfmeet.com/dashboard/updates/channels/join#${token}`);
  assert.equal(new URL(link).search, "");
  assert.equal(inviteToken(new URL(link).hash), token);
  for (const bad of ["", "#", "#mci_short", `#mil_${"A".repeat(43)}`, `#mci_${"A".repeat(42)}!`]) assert.equal(inviteToken(bad), null, bad);
  assert.equal(channelLink("https://cfmeet.com", ID), `https://cfmeet.com/dashboard/updates/channels/${ID}`);
  assert.equal(postLink("https://cfmeet.com", ID, ID), `https://cfmeet.com/dashboard/updates/channels/${ID}?post=${ID}`);
});

test("poll percentages add up to 100 and are zero without votes", () => {
  assert.deepEqual(pollPercents([0, 0, 0]), [0, 0, 0]);
  assert.deepEqual(pollPercents([1, 1, 1]), [34, 33, 33]);
  assert.deepEqual(pollPercents([2, 1]), [67, 33]);
  for (const votes of [[3, 7, 11], [1, 0, 0, 2], [5, 5, 5, 5, 5, 5, 1]]) assert.equal(pollPercents(votes).reduce((a, b) => a + b, 0), 100);
});

test("scheduling stays inside the server's window (1 minute to 30 days)", () => {
  const now = new Date(2026, 10, 28, 10, 0, 0);
  const { min, max } = scheduleBounds(now);
  assert.equal(min, toLocalInput(new Date(2026, 10, 28, 10, 2)));
  assert.ok(new Date(max).getTime() < now.getTime() + 30 * 86_400_000);
  assert.ok(new Date(min).getTime() > now.getTime() + 60_000);
});

test("picked files map to the kinds and presentations the server accepts", () => {
  assert.equal(mediaKindOf("image/png"), "IMAGE");
  assert.equal(mediaKindOf("video/mp4"), "VIDEO");
  assert.equal(mediaKindOf("audio/webm;codecs=opus"), "AUDIO");
  assert.equal(mediaKindOf("application/pdf"), "DOCUMENT");
  assert.equal(mediaKindOf("application/vnd.openxmlformats-officedocument.wordprocessingml.document"), "DOCUMENT");
  for (const refused of ["image/svg+xml", "text/html", "application/x-msdownload", "application/javascript"]) assert.equal(mediaKindOf(refused), null, refused);
  assert.deepEqual(presentationsFor("audio/ogg", 1000), ["VOICE"]);
  assert.deepEqual(presentationsFor("image/gif", 1000), ["GIF"]);
  assert.deepEqual(presentationsFor("image/webp", 900_000), ["STICKER"]);
  assert.deepEqual(presentationsFor("image/webp", 2_000_000), []);
});

test("link previews start from the first link in the text (never fetched)", () => {
  assert.equal(firstLink("read https://example.com/a?b=1, then"), "https://example.com/a?b=1");
  assert.equal(firstLink("no links"), null);
  assert.equal(firstLink("javascript:alert(1)"), null);
});

test("analytics helpers format counts and draw real series only", () => {
  assert.equal(compactCount(999), "999");
  assert.equal(compactCount(1234), "1.2K");
  assert.equal(compactCount(2_500_000), "2.5M");
  assert.equal(percentText(null), "—");
  assert.equal(percentText(0.5), "50%");
  assert.equal(sparkPath([], 100, 10), "");
  assert.equal(sparkPath([null, null], 100, 10), "");
  assert.match(sparkPath([0, 5, 10], 100, 10), /^M0\.0,10\.0 L50\.0,5\.0 L100\.0,0\.0$/);
  assert.deepEqual(rangeFor(7, new Date(Date.UTC(2026, 10, 28, 23))), { from: "2026-11-22", to: "2026-11-28" });
});

test("pickers offer real codes", () => {
  assert.ok(COUNTRIES.includes("NG") && COUNTRIES.includes("GH") && COUNTRIES.every((c) => /^[A-Z]{2}$/.test(c)));
  assert.equal(new Set(COUNTRIES).size, COUNTRIES.length);
  assert.ok(CATEGORIES.some((c) => c.code === "NEWS"));
});

test("the relay allowlist permits the channel operations and their query keys only", () => {
  const ch = "v1/messaging/channels";
  for (const [m, p] of [
    ["GET", `${ch}?q=news&category=NEWS&country=NG&language=en&sort=TRENDING&cursor=t.30&limit=20`],
    ["GET", `${ch}/suggestions?q=da`], ["GET", `${ch}/lookup?handle=daily`], ["GET", `${ch}/starred?cursor=x`],
    ["POST", `${ch}/invites/preview`], ["POST", `${ch}/invites/join`],
    ["PUT", `${ch}/${ID}/mute`], ["DELETE", `${ch}/${ID}/mute`], ["GET", `${ch}/${ID}/admins`],
    ["PUT", `${ch}/${ID}/admins/${ID}`], ["DELETE", `${ch}/${ID}/admins/${ID}`], ["POST", `${ch}/${ID}/owner`],
    ["GET", `${ch}/${ID}/invites`], ["POST", `${ch}/${ID}/invites`], ["DELETE", `${ch}/${ID}/invites/${ID}`],
    ["POST", `${ch}/${ID}/posts/views`], ["PATCH", `${ch}/${ID}/posts/${ID}`],
    ["PUT", `${ch}/${ID}/posts/${ID}/pin`], ["DELETE", `${ch}/${ID}/posts/${ID}/vote`], ["PUT", `${ch}/${ID}/posts/${ID}/star`],
    ["POST", `${ch}/${ID}/posts/${ID}/forwards`], ["GET", `${ch}/${ID}/scheduled`], ["DELETE", `${ch}/${ID}/scheduled/${ID}`],
    ["POST", `${ch}/${ID}/reports`], ["GET", `${ch}/${ID}/verification`], ["POST", `${ch}/${ID}/verification`],
    ["GET", `${ch}/${ID}/moderation`], ["GET", `${ch}/${ID}/audit?cursor=x`], ["GET", `${ch}/${ID}/analytics?from=2026-11-01&to=2026-11-28`],
  ]) assert.equal(isMessagingRelayRoute(m, p), true, `${m} ${p}`);
  // Nothing an owner could use to verify or moderate themselves, no foreign query keys.
  for (const [m, p] of [
    ["POST", `${ch}/${ID}/verification/approve`], ["POST", `${ch}/${ID}/moderate`], ["GET", `${ch}/${ID}/followers`],
    ["GET", `${ch}/${ID}/analytics?principalId=${ID}`], ["GET", `${ch}/lookup?handle=a&q=b`], ["PUT", `${ch}/${ID}/posts/${ID}/views`],
  ]) assert.equal(isMessagingRelayRoute(m, p), false, `${m} ${p}`);
});
