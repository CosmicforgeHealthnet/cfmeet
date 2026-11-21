// ADR 0068 owner-delivered exports: link parsing (query and fragment), the pending-link stash
// (30 minutes, re-validated), refusal outcomes and the waiting-exports list.
//   node --experimental-strip-types --test tests/owner-exports.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import {
  ACCOUNT_EXPORT_PENDING_KEY,
  PENDING_TTL_MS,
  accountExportLinkFrom,
  asAccountExportLink,
  asOrganizationExportLink,
  canClaim,
  deliveryStateText,
  downloadOutcome,
  downloadOutcomeText,
  exportErrorOf,
  exportFileName,
  forgetLink,
  isFinalOutcome,
  orderedDeliveries,
  organizationExportLinkFrom,
  safeDownloadUrl,
  stashLink,
  stashedLink,
} from "../src/lib/account/owner-exports.ts";
import { safeReturnPath } from "../src/lib/organizations/organization-core.ts";
import { signInNotice } from "../src/lib/account/account-core.ts";

const ID = "0f8fad5b-d9cb-469f-a165-70867728950e";
const ORG = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
const TOKEN = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_abcd";

function memoryStore() {
  const m = new Map();
  return {
    m,
    getItem: (k) => (m.has(k) ? m.get(k) : null),
    setItem: (k, v) => m.set(k, String(v)),
    removeItem: (k) => m.delete(k),
  };
}

test("the account-export link is read from the query string", () => {
  assert.deepEqual(accountExportLinkFrom(`?export=${ID}&token=${TOKEN}`), { exportId: ID, token: TOKEN });
  assert.deepEqual(accountExportLinkFrom(`export=${ID.toUpperCase()}&token=${TOKEN}`), { exportId: ID, token: TOKEN });
  for (const bad of ["", `?export=${ID}`, `?token=${TOKEN}`, `?export=nope&token=${TOKEN}`, `?export=${ID}&token=short`, `?export=${ID}&token=a%20b${TOKEN}`]) {
    assert.equal(accountExportLinkFrom(bad), null, bad);
  }
});

test("the organization-export link is read from the fragment only", () => {
  assert.deepEqual(organizationExportLinkFrom(`#org=${ORG}&export=${ID}&token=${TOKEN}`), {
    organizationId: ORG,
    exportId: ID,
    token: TOKEN,
  });
  for (const bad of ["", "#", `#org=${ORG}&export=${ID}`, `#org=x&export=${ID}&token=${TOKEN}`, `#org=${ORG}&export=${ID}&token=tiny`, "#" + "a".repeat(2000)]) {
    assert.equal(organizationExportLinkFrom(bad), null, bad);
  }
});

test("a pending link waits at most 30 minutes and is re-validated", () => {
  const s = memoryStore();
  const link = { exportId: ID, token: TOKEN };
  stashLink(s, ACCOUNT_EXPORT_PENDING_KEY, link, 1000);
  assert.deepEqual(stashedLink(s, ACCOUNT_EXPORT_PENDING_KEY, 1000 + PENDING_TTL_MS, asAccountExportLink), link);
  assert.equal(stashedLink(s, ACCOUNT_EXPORT_PENDING_KEY, 1001 + PENDING_TTL_MS, asAccountExportLink), null);
  assert.equal(s.m.size, 0, "an expired link is erased");
  s.setItem(ACCOUNT_EXPORT_PENDING_KEY, JSON.stringify({ link: { exportId: ID, token: "x" }, until: Date.now() + 9e6 }));
  assert.equal(stashedLink(s, ACCOUNT_EXPORT_PENDING_KEY, Date.now(), asAccountExportLink), null);
  stashLink(s, "k", { organizationId: ORG, exportId: ID, token: TOKEN }, 0);
  assert.ok(stashedLink(s, "k", 1, asOrganizationExportLink));
  forgetLink(s, "k");
  assert.equal(s.getItem("k"), null);
  assert.equal(stashedLink(null, "k", 0, asOrganizationExportLink), null);
  stashLink({ setItem() { throw new Error("quota"); } }, "k", link, 0);
});

test("refusals map to what the person is told; final ones are forgotten", () => {
  const err = (status, code) => ({ response: { status, data: code ? JSON.stringify({ error: { code } }) : "" } });
  assert.equal(downloadOutcome(err(403, "REAUTHENTICATION_REQUIRED")), "reauthenticate");
  assert.equal(downloadOutcome(err(401, "AUTHENTICATION_REQUIRED")), "sign_in");
  assert.equal(downloadOutcome(err(404, "NOT_FOUND")), "invalid");
  assert.equal(downloadOutcome(err(404, "EXPORT_LINK_INVALID")), "invalid");
  assert.equal(downloadOutcome(err(410, "EXPORT_LINK_EXPIRED")), "expired");
  assert.equal(downloadOutcome(err(409, "EXPORT_ALREADY_DELIVERED")), "already_delivered");
  assert.equal(downloadOutcome(err(409, "EXPORT_EXPIRED")), "expired");
  assert.equal(downloadOutcome(err(503, "SERVICE_UNAVAILABLE")), "unavailable");
  assert.equal(downloadOutcome(new Error("offline")), "unavailable");
  assert.equal(downloadOutcome(err(409, "EXPORT_NOT_READY")), "failed");
  assert.deepEqual(exportErrorOf({ response: { status: 400, data: { error: { code: "VALIDATION_ERROR" } } } }), {
    status: 400,
    code: "VALIDATION_ERROR",
  });
  assert.ok(isFinalOutcome("invalid") && isFinalOutcome("expired") && isFinalOutcome("already_delivered"));
  assert.ok(!isFinalOutcome("reauthenticate") && !isFinalOutcome("unavailable"));
  for (const o of ["reauthenticate", "sign_in", "invalid", "expired", "already_delivered", "unavailable", "failed"]) {
    assert.ok(downloadOutcomeText(o).length > 10);
  }
});

test("the waiting list puts claimable exports first and only follows safe links", () => {
  const now = Date.parse("2026-11-21T12:00:00Z");
  const d = (exportId, exportState, createdAt, expiresAt = "2026-11-28T00:00:00Z") => ({
    exportId, exportState, size: 10, sha256: null, createdAt, completedAt: null, deliveredAt: null, expiresAt,
  });
  const items = [
    d("a", "DELIVERED", "2026-11-21T10:00:00Z"),
    d("b", "READY", "2026-11-19T10:00:00Z"),
    d("c", "READY", "2026-11-20T10:00:00Z", "2026-11-21T11:00:00Z"),
    d("e", "READY", "2026-11-20T11:00:00Z"),
  ];
  assert.deepEqual(orderedDeliveries(items, now).map((x) => x.exportId), ["e", "b", "a", "c"]);
  assert.ok(canClaim(items[1], now) && !canClaim(items[2], now) && !canClaim(items[0], now));
  assert.equal(deliveryStateText(items[2], now), "Expired");
  assert.equal(deliveryStateText(items[0], now), "Downloaded");
  assert.equal(safeDownloadUrl("https://storage.example/x?sig=1"), "https://storage.example/x?sig=1");
  assert.equal(safeDownloadUrl("javascript:alert(1)"), null);
  assert.equal(safeDownloadUrl("http://evil.example/x"), null);
  assert.equal(safeDownloadUrl("http://localhost:9000/x"), "http://localhost:9000/x");
  assert.equal(safeDownloadUrl(42), null);
});

test("pages are valid sign-in return paths; file names carry no token", () => {
  assert.equal(safeReturnPath("/settings/data-export/download"), "/settings/data-export/download");
  assert.equal(safeReturnPath("/organizations/exports/download"), "/organizations/exports/download");
  assert.equal(safeReturnPath("/settings/data-export/download?token=x"), null);
  assert.ok(signInNotice("data_export"));
  const name = exportFileName("account", ID, new Date("2026-11-21T00:00:00Z"));
  assert.equal(name, "cfmeet-account-data-2026-11-21-0f8fad5b.json");
  assert.ok(!name.includes(TOKEN));
});
