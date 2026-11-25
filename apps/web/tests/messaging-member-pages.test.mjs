import assert from 'node:assert/strict';
import test from 'node:test';
import { allDevicePages, allMemberPages } from '../src/lib/messaging/member-pages.ts';

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

const directoryDevices = (from, count) => Array.from({ length: count }, (_, n) => ({ deviceId: `device-${from + n}` }));

test('frontend-first rollout reads a complete small legacy directory without query parameters', async () => {
  const queries = [];
  const result = await allDevicePages(async query => { queries.push(query); return { devices: directoryDevices(0, 99) }; });
  assert.deepEqual(queries, ['']);
  assert.equal(result.length, 99);
});

test('a full legacy directory never masquerades as an unlimited complete device set', async () => {
  await assert.rejects(allDevicePages(async () => ({ devices: directoryDevices(0, 100) })), /DEVICE_DIRECTORY_PAGINATION_REQUIRED/);
});

test('new backends enumerate more than 100 devices through explicit page boundaries', async () => {
  const queries = [];
  const result = await allDevicePages(async query => {
    queries.push(query);
    return query === '' ? { devices: directoryDevices(0, 100), nextCursor: 'next-page' } : { devices: directoryDevices(100, 23), nextCursor: null };
  });
  assert.deepEqual(queries, ['', '?limit=500&cursor=next-page']);
  assert.equal(result.length, 123);
});

test('mixed relay versions and missing later boundaries fail without returning a partial directory', async () => {
  const oldRelay = Object.assign(new Error('BAD_PATH'), { code: 'BAD_PATH' });
  await assert.rejects(allDevicePages(async query => {
    if (query) throw oldRelay;
    return { devices: directoryDevices(0, 100), nextCursor: 'next' };
  }), /DEVICE_DIRECTORY_PAGINATION_REQUIRED/);
  await assert.rejects(allDevicePages(async query => query ? { devices: directoryDevices(100, 1) } : { devices: directoryDevices(0, 100), nextCursor: 'next' }), /DEVICE_DIRECTORY_PAGINATION_REQUIRED/);
  await assert.rejects(allDevicePages(async () => ({ devices: directoryDevices(0, 100), nextCursor: 'loop' })), /DEVICE_DIRECTORY_PAGE_INVALID/);
});
