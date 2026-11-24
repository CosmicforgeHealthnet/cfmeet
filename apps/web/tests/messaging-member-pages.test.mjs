import assert from 'node:assert/strict';
import test from 'node:test';
import { allMemberPages } from '../src/lib/messaging/member-pages.ts';

test('device coverage reads later member pages and deduplicates a member seen while pages change', async () => {
  const requested = [];
  const members = await allMemberPages(async (cursor) => {
    requested.push(cursor);
    if (cursor === null) return { members: [{ principalId: 'A' }, { principalId: 'B' }], nextCursor: 'second' };
    return { members: [{ principalId: 'B' }, { principalId: 'C' }], nextCursor: null };
  });
  assert.deepEqual(requested, [null, 'second']);
  assert.deepEqual(members.map((member) => member.principalId), ['A', 'B', 'C']);
});

test('broken or unavailable pagination never becomes a successful partial device directory', async () => {
  await assert.rejects(allMemberPages(async () => ({ members: [{ principalId: 'A' }], nextCursor: 'loop' })), /MEMBER_DIRECTORY_INCOMPLETE/);
  await assert.rejects(allMemberPages(async () => ({ members: [], nextCursor: 'missing' })), /MEMBER_DIRECTORY_INCOMPLETE/);
  await assert.rejects(allMemberPages(async (cursor) => {
    if (cursor) throw new Error('directory unavailable');
    return { members: [{ principalId: 'A' }], nextCursor: 'later' };
  }), /directory unavailable/);
});
