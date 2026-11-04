import assert from 'node:assert/strict';
import test from 'node:test';
import { callingOrigin } from '../src/lib/calling/origin.mjs';

test('production signaling uses the real Meeting API origin when Calling has no separate public setting',()=>{
  assert.equal(callingOrigin('', 'https://api.cfmeet.com/api/v1', true),'https://api.cfmeet.com');
  assert.equal(new URL('/v1/calling/signaling', callingOrigin('', 'https://api.cfmeet.com/api/v1',true)).href,
    'https://api.cfmeet.com/v1/calling/signaling');
});
test('production never silently selects a localhost signaling server',()=>{
  assert.equal(callingOrigin('','',true),'https://api.cfmeet.com');
  assert.equal(callingOrigin('','',false),'http://localhost:8090');
  assert.equal(callingOrigin('https://calling.example/prefix','https://api.example/api/v1',true),'https://calling.example');
});
