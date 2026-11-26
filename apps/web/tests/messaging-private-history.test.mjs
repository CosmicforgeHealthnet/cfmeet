import assert from 'node:assert/strict';
import test from 'node:test';
import { PrivateHistoryWindow } from '../src/lib/messaging/private-history.ts';
const rows = (first, last, kind = 'APPLICATION') => Array.from({ length: last - first + 1 }, (_, n) => ({ sequence: first + n, messageId: `message-${first+n}`, kind, status: 'DECRYPTED', payload: { t: 'text', text: `text-${first+n}` } }));

test('raw control-only pages retain the cursor and all history stays accessible after new head gaps', () => {
  const history = new PrivateHistoryWindow();
  history.merge({ records: rows(401, 600), nextBeforeSequence: 401 });
  assert.equal(history.cursor, 401);
  history.merge({ records: rows(201, 400, 'EDIT'), nextBeforeSequence: 201 }, 401);
  assert.equal(history.cursor, 201);
  history.merge({ records: rows(1, 200), nextBeforeSequence: null }, 201);
  assert.equal(history.cursor, null);
  assert.equal(history.ordered.length, 600);
  history.merge({ records: rows(801, 1000), nextBeforeSequence: 801 });
  assert.equal(history.cursor, 801, 'a previously complete older window cannot hide a new gap');
  history.merge({ records: rows(601, 800), nextBeforeSequence: 601 }, 801);
  assert.equal(history.cursor, null);
  assert.equal(history.ordered.length, 1000);
});

test('authority boundary changes prune previously cached plaintext and keep pagination within eligibility', () => {
  const history = new PrivateHistoryWindow();
  history.merge({ records: rows(1, 200), nextBeforeSequence: null });
  history.prune(150);
  assert.equal(history.ordered.length, 51);
  assert.equal(history.ordered[0].sequence, 150);
  assert.throws(() => history.prune(0), /HISTORY_VISIBILITY_REQUIRED/);
});

test('late target deletion drops cached originals, edits and private reactions without resurrecting from stale pages', () => {
  const history = new PrivateHistoryWindow();
  history.merge({ records: [rows(1,1)[0], { ...rows(2,2,'EDIT')[0], targetMessageId:'message-1' }, { ...rows(3,3)[0], privateControl:'REACTION',payload:{ t:'reaction',target:'message-1',reaction:':yes:',add:true } }], nextBeforeSequence:null });
  history.reconcile([{ messageId:'message-1',lifecycle:'DELETED' }]);
  assert.equal(history.ordered.every(row => row.status==='REMOVED' && row.payload===undefined), true);
  assert.equal(history.ordered[2].privateControl,'REACTION');
  history.upsert(rows(1,1));
  assert.equal(history.ordered[0].status,'REMOVED');
});

test('fresh recovered records replace cached failures, and a new transfer window rediscovers newly imported older pages', () => {
  const history = new PrivateHistoryWindow();
  history.merge({ records:[{ ...rows(401,401)[0],status:'FAILED',payload:undefined }],nextBeforeSequence:null });
  history.upsert(rows(401,401));
  assert.equal(history.ordered[0].status,'DECRYPTED');
  const afterImport = new PrivateHistoryWindow();
  afterImport.merge({ records:rows(202,401),nextBeforeSequence:202 });
  assert.equal(afterImport.cursor,202);
  afterImport.merge({ records:rows(2,201),nextBeforeSequence:2 },202);
  afterImport.merge({ records:rows(1,1),nextBeforeSequence:null },2);
  assert.equal(afterImport.ordered.length,401);
  assert.equal(afterImport.cursor,null);
});

test('large replay updates only loaded outcomes rather than loading unknown archive records', () => {
  const history = new PrivateHistoryWindow();
  history.updateKnown(rows(1,1000));
  assert.equal(history.ordered.length,0);
  history.merge({ records:rows(801,1000),nextBeforeSequence:801 });
  history.updateKnown([...rows(1,1000),{ ...rows(999,999)[0],status:'REMOVED',payload:undefined }]);
  assert.equal(history.ordered.length,200);
  assert.equal(history.ordered[0].sequence,801);
  assert.equal(history.ordered.find(row=>row.sequence===999).status,'REMOVED');
  assert.equal(history.cursor,801);
});

test('a short tail refresh overlapping the loaded head keeps the cached history and its cursor', () => {
  const history = new PrivateHistoryWindow();
  history.merge({ records: rows(101, 300), nextBeforeSequence: 101 });
  assert.equal(history.head, 300);
  assert.equal(history.cursor, 101);
  // The engine reads only the newest 32 records when they reach the loaded head.
  history.merge({ records: rows(275, 306), nextBeforeSequence: 275 });
  assert.equal(history.head, 306);
  assert.equal(history.ordered.length, 206, 'nothing already loaded is dropped');
  assert.equal(history.cursor, 101, 'no false gap: older paging still starts below the loaded range');
  history.merge({ records: rows(1, 100), nextBeforeSequence: null }, 101);
  assert.equal(history.cursor, null);
  assert.equal(history.ordered.length, 306);
  // A tail that does not reach the head would leave a gap; the engine reads a full page then.
  const gapped = new PrivateHistoryWindow();
  gapped.merge({ records: rows(1, 50), nextBeforeSequence: null });
  gapped.merge({ records: rows(90, 121), nextBeforeSequence: 90 });
  assert.equal(gapped.cursor, 90, 'a gap is visible as a cursor, never silently skipped');
});
