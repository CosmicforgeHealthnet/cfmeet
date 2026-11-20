// The SHIPPED browser crypto core (public/messaging-crypto, as the manifest names it) lets a
// member's new device join a private chat BY ITSELF from the GroupInfo a member published
// (MLS external commit), while no other member is online; members merge that commit only
// for a joining device they verified, and the new device reads and writes at once.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, join } from "node:path";

const dir = join(dirname(fileURLToPath(import.meta.url)), "..", "public", "messaging-crypto");
const manifest = JSON.parse(readFileSync(join(dir, "manifest.json"), "utf8"));
const file = (role) => Object.entries(manifest.files).find(([, f]) => f.role === role)[0];
const core = await import(pathToFileURL(join(dir, file("glue"))).href);
core.initSync({ module: readFileSync(join(dir, file("wasm"))) });

const gid = new Uint8Array(32).fill(7);
const id = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const hex = (b) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const text = (r) => new TextDecoder().decode(r.plaintext);

test("a new device joins by itself from the GroupInfo and members merge it only once verified", () => {
  const alice = new core.WebDevice(id(1));
  const bobPhone = new core.WebDevice(id(2));
  alice.createGroup(gid);
  // Alice publishes the group's GroupInfo with a key-update commit (nobody else has a device yet).
  const update = alice.updateKeys(gid);
  assert.equal(Number(update.epoch), 0);
  const info = alice.groupInfo(gid);
  assert.ok(info.length > 0);

  // Bob's phone joins on its own; Alice is offline.
  const join = bobPhone.externalJoin(gid, info);
  assert.equal(Number(join.epoch), 1, "created in the GroupInfo's epoch");
  assert.equal(join.welcome, undefined);
  assert.equal(Number(bobPhone.epoch(gid)), 2);
  assert.ok(bobPhone.groupInfo(gid).length > 0, "the joiner publishes the next GroupInfo itself");
  const aad = new TextEncoder().encode("aad");
  const hello = bobPhone.encrypt(gid, aad, new TextEncoder().encode("hi from the phone"));
  assert.throws(() => bobPhone.externalJoin(gid, info), /GROUP_EXISTS/);

  // Alice comes back: unverified joiner → nothing merged, the joiner is named.
  const pending = alice.process(gid, join.commit, undefined, []);
  assert.equal(pending.kind, "JOINER_UNVERIFIED");
  assert.equal(pending.joiner, id(2));
  assert.equal(Number(alice.epoch(gid)), 1);
  // Another device's entry, or the right device with another key, does not admit it.
  assert.equal(alice.process(gid, join.commit, undefined, [`${id(3)}:${hex(bobPhone.signaturePublicKey)}`]).kind, "JOINER_UNVERIFIED");
  assert.equal(alice.process(gid, join.commit, undefined, [`${id(2)}:${hex(alice.signaturePublicKey)}`]).kind, "JOINER_UNVERIFIED");
  // Verified (registered device, its directory key): merged.
  const merged = alice.process(gid, join.commit, undefined, [`${id(2)}:${hex(bobPhone.signaturePublicKey)}`]);
  assert.equal(merged.kind, "COMMIT");
  assert.equal(merged.joiner, id(2));
  assert.equal(hex(merged.joinerKey), hex(bobPhone.signaturePublicKey));
  assert.deepEqual([...alice.members(gid)].sort(), [id(1), id(2)]);
  const got = alice.process(gid, hello, aad, []);
  assert.equal(got.kind, "APPLICATION");
  assert.equal(text(got), "hi from the phone");
  const back = alice.encrypt(gid, aad, new TextEncoder().encode("got it"));
  assert.equal(text(bobPhone.process(gid, back, aad, [])), "got it");
});

test("a GroupInfo of another group is refused", () => {
  const alice = new core.WebDevice(id(11));
  alice.createGroup(gid);
  const info = alice.groupInfo(gid);
  const other = new core.WebDevice(id(12));
  assert.throws(() => other.externalJoin(new Uint8Array(32).fill(8), info), /GROUP_INFO_GROUP_MISMATCH/);
  assert.throws(() => other.externalJoin(gid, new Uint8Array([1, 2, 3])), /GROUP_INFO_INVALID/);
});
