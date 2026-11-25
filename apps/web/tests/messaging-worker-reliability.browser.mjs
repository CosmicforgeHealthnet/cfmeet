import {createServer} from 'node:http';
import {readFile,writeFile,mkdtemp} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {resolve,join,extname} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const [repo,runtime,out] = process.argv.slice(2);
if (!repo || !runtime || !out) throw Error('usage: node worker-repro.mjs REPO PLAYWRIGHT_INSTALL_DIR OUTPUT.json');
const {chromium}=await import(pathToFileURL(join(resolve(runtime),'node_modules/playwright/index.mjs')));
const root=join(resolve(repo),'apps/web/public/messaging-crypto');
const manifest=JSON.parse(await readFile(join(root,'manifest.json')));
// Windows git autocrlf can alter checked-out JavaScript. Serve the canonical LF bytes
// only when they exactly match the committed integrity manifest; never alter WASM.
const assets=new Map();
for(const [file,metadata] of Object.entries(manifest.files)) {
 let bytes=await readFile(join(root,file));
 if(file.endsWith('.js')) bytes=Buffer.from(bytes.toString('utf8').replaceAll('\r\n','\n'));
 if('sha384-'+createHash('sha384').update(bytes).digest('base64')!==metadata.integrity||bytes.length!==metadata.bytes) throw Error('manifest integrity mismatch');
 assets.set(file,bytes);
}
const role=r=>Object.entries(manifest.files).find(([,v])=>v.role===r)[0];
const server=createServer(async(req,res)=>{try{const name=new URL(req.url,'http://local').pathname.slice(1); if(!name){res.end('<html></html>');return;} if(name==='test-fault-worker.js') {
 res.setHeader('Content-Type','text/javascript');
 res.end(`import '/${role('worker')}';
 let armed=false,protectionAbort=false;
 const transaction=IDBDatabase.prototype.transaction;
 IDBDatabase.prototype.transaction=function(stores,mode,...rest){
   const tx=transaction.call(this,stores,mode,...rest);
   const names=Array.from(typeof stores==='string'?[stores]:stores);
   if(armed&&mode==='readwrite'&&names.includes('devices')){armed=false;queueMicrotask(()=>tx.abort());}
   if(protectionAbort&&mode==='readwrite'&&names.includes('devices')&&names.includes('keys')){protectionAbort=false;queueMicrotask(()=>tx.abort());}
   return tx;
 };
 const handler=self.onmessage;
 self.onmessage=e=>{if(e.data?.op==='_abortNextWrite'){armed=true;self.postMessage({id:e.data.id,ok:true,result:true});}else if(e.data?.op==='_abortProtectionCommit'){protectionAbort=true;self.postMessage({id:e.data.id,ok:true,result:true});}else handler(e);};`);return;
 } if(!assets.has(name)) throw Error();res.setHeader('Content-Type',extname(name)==='.wasm'?'application/wasm':'text/javascript');res.end(assets.get(name));}catch{res.writeHead(404).end();}});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
const profile=await mkdtemp(join(tmpdir(),'cfmeet-crypto-repro-'));
let browser=await chromium.launchPersistentContext(profile);
try {
 const context=browser;const page=await context.newPage();const base=`http://127.0.0.1:${server.address().port}`;await page.goto(base);
 const evidence=await page.evaluate(async({worker,glue,wasm})=>{
 const core=await import('/'+glue);await core.default({module_or_path:'/'+wasm});
 const id=n=>`00000000-0000-4000-8000-${String(n).padStart(12,'0')}`;
 const scope={tenantId:id(1),applicationId:id(2),environmentId:id(3)};const account=id(4),receiverId=id(5),senderId=id(6),conv=id(7),gid=new Uint8Array(32).fill(9);
 let w;let serial=0;const calls=new Map();
 function start(){w=new Worker('/test-fault-worker.js',{type:'module'});w.onmessage=({data})=>{const c=calls.get(data.id);calls.delete(data.id);data.ok?c.resolve(data.result):c.reject(Error(data.error));};}
 function call(op,args={}){return new Promise((resolve,reject)=>{const n=++serial;calls.set(n,{resolve,reject});w.postMessage({id:n,op,args});setTimeout(()=>{if(calls.delete(n))reject(Error('TIMEOUT'));},15000);});}
 const events=[];const check=(name,ok,detail={})=>{if(!ok)throw Error(name);events.push({name,result:'CONFIRMED',...detail});};
 const b64=b=>btoa(String.fromCharCode(...b)).replaceAll('+','-').replaceAll('/','_').replace(/=+$/,'');
 const hex=b=>Array.from(b,x=>x.toString(16).padStart(2,'0')).join('');
 start();await call('open',{account,scope});await call('generate',{deviceId:receiverId});await call('adopt');
 const kp=(await call('keyPackages',{count:1}))[0];const sender=new core.WebDevice(senderId);sender.createGroup(gid);
 const packed=new Uint8Array(kp.length+4);new DataView(packed.buffer).setUint32(0,kp.length);packed.set(kp,4);const add=sender.addDevices(gid,packed);
 await call('join',{groupId:gid,welcome:add.welcome});
 const joinedEpoch=await call('epoch',{groupId:gid});
 // A metadata checkpoint while HTTP acceptance is pending must not make the
 // tentative epoch durable. Private KeyPackages must not escape in that window.
 await call('updateKeys',{groupId:gid});
 let pendingKeysRefused=false;try{await call('keyPackages',{count:1});}catch(e){pendingKeysRefused=e.message==='MLS_CHANGE_PENDING';}
 check('unconfirmed group changes block KeyPackage publication',pendingKeysRefused);
 await call('reconcileHistoryMetadata',{conversationId:conv,messages:[]});
 w.terminate();await new Promise(r=>setTimeout(r,100));start();await call('open',{account,scope});
 check('metadata checkpoint never persists an unaccepted epoch',(await call('epoch',{groupId:gid}))===joinedEpoch);
 let seq=0;const encode=(size=20,payload=null)=>{const cmid=id(100+(++seq));const aad=core.message_aad(scope.tenantId,scope.applicationId,scope.environmentId,conv,cmid,senderId);const ciphertext=sender.encrypt(gid,aad,new TextEncoder().encode(JSON.stringify(payload??{v:1,t:'text',text:'x'.repeat(size)})));return {sequence:seq,messageId:id(1000+seq),clientMessageId:cmid,senderDeviceId:senderId,senderPrincipalId:id(8),kind:'APPLICATION',lifecycle:'ACTIVE',createdAt:'2026-11-24T00:00:00Z',epoch:Number(sender.epoch(gid)),ciphertext:b64(ciphertext)};};
 const process=messages=>call('processMessages',{conversationId:conv,groupId:gid,messages});
 const first=encode();check('normal real MLS decrypt',(await process([first])).records[0].status==='DECRYPTED');
 w.terminate();await new Promise(r=>setTimeout(r,100));start();check('worker restart preserves identity',(await call('open',{account,scope})).deviceId===receiverId);check('worker restart preserves encrypted history',(await call('history',{conversationId:conv})).length===1);
 const encryptedReaction=encode(20,{v:1,t:'reaction',target:first.messageId,reaction:':yes:',add:true,senderPrincipalId:id(999)});
 const reactionResult=(await process([encryptedReaction])).records[0];
 check('actual MLS reactions retain authenticated envelope actor and bounded private payload',reactionResult.status==='DECRYPTED'&&reactionResult.payload.t==='reaction'&&reactionResult.payload.add===true&&reactionResult.payload.target===first.messageId&&reactionResult.payload.senderPrincipalId===undefined&&reactionResult.senderPrincipalId===id(8)&&reactionResult.privateControl==='REACTION');
 const removedReaction=(await process([{...encryptedReaction,lifecycle:'DELETED'}])).records[0];
 check('deleted encrypted controls keep hidden classification without content',removedReaction.status==='REMOVED'&&removedReaction.privateControl==='REACTION'&&removedReaction.payload===undefined);
 for(const malformed of [{target:'bad-id',reaction:':yes:',add:true},{target:first.messageId,reaction:'plain-ascii',add:true},{target:first.messageId,reaction:':yes:',add:'true'},{target:first.messageId,reaction:':'+ 'x'.repeat(64)+':',add:true}]){
   const badReaction=encode(20,{v:1,t:'reaction',...malformed});
   const badResult=(await process([badReaction])).records[0];
   checkSilent(badResult.status==='FAILED'&&badResult.error==='PAYLOAD_INVALID');
 }
 check('malformed private reaction UUID boolean and reaction bounds are refused',true);
 function checkSilent(ok){if(!ok)throw Error('malformed reaction accepted');}

 const other=new Worker('/'+worker,{type:'module'});const locked=await new Promise(r=>{other.onmessage=({data})=>r(data.result);other.postMessage({id:1,op:'open',args:{account,scope}});});other.terminate();check('second worker cannot fork device',locked.status==='LOCKED');
 // Independently measure real core retention; do not assume old epochs remain decryptable.
 const rawSender=new core.WebDevice(id(21)),rawReceiver=new core.WebDevice(id(22));rawSender.createGroup(gid);
 const rawKp=rawReceiver.keyPackage(),rawPacked=new Uint8Array(rawKp.length+4);new DataView(rawPacked.buffer).setUint32(0,rawKp.length);rawPacked.set(rawKp,4);
 const rawAdd=rawSender.addDevices(gid,rawPacked);rawReceiver.join(gid,rawAdd.welcome);
 const rawAad=new Uint8Array([1]),rawOld=rawSender.encrypt(gid,rawAad,new Uint8Array([42]));const rawUpdate=rawSender.updateKeys(gid);rawReceiver.process(gid,rawUpdate.commit,undefined,[]);
 let retained=false,retentionError=null;try{retained=rawReceiver.process(gid,rawOld,rawAad,[]).kind==='APPLICATION';}catch(e){retentionError=/^[A-Z_]{3,64}$/.test(e.message)?e.message:'CORE_REJECTED';}
 check('core retains a bounded delayed epoch',retained,{oldEpochDecryptable:retained,error:retentionError});
 const old=encode();
 // Delay the already-member receiver's backlog, advance its own epoch through the real
 // worker commit/confirm API, then deliver the original unmodified old message.
 // This tests the worker contract; production engine reachability requires full-stack evidence.
 const change=await call('updateKeys',{groupId:gid});await call('confirm');sender.process(gid,change.commit,undefined,[]);
 const stateEpoch=await call('epoch',{groupId:gid});
 const oldLate=old;
 const classified=await process([oldLate]);check('delayed prior-member traffic decrypts',classified.records[0].status==='DECRYPTED',{joinedEpoch,currentEpoch:stateEpoch,messageEpoch:old.epoch});
 const lateRetry=await process([oldLate]);check('delayed delivery remains idempotent',lateRetry.records.length===0,{cursor:lateRetry.cursor});
 const outsideWindow=encode();for(let i=0;i<3;i++){const change=await call('updateKeys',{groupId:gid});await call('confirm');sender.process(gid,change.commit,undefined,[]);}
 const unavailable=await process([outsideWindow]);check('expired historical state is not historical non-membership',unavailable.records[0].status==='FAILED'&&unavailable.records[0].error==='HISTORICAL_EPOCH_UNAVAILABLE');
 const bad=encode();const valid={...bad};bad.ciphertext=b64(new Uint8Array([1,2,3]));const failed=await process([bad]);check('invalid ciphertext persists FAILED',failed.records[0].status==='FAILED');w.terminate();await new Promise(r=>setTimeout(r,100));start();await call('open',{account,scope});check('corrected ciphertext recovers at same sequence',(await process([valid])).records[0].status==='DECRYPTED');check('successful delivery is idempotent',(await process([valid])).records.length===0);
 const atomic=encode();await call('_abortNextWrite');let aborted=false;
 try{await process([atomic]);}catch{aborted=true;}
 check('storage abort is surfaced',aborted);
 check('retry after storage abort decrypts with rolled-back ratchet',(await process([atomic])).records[0].status==='DECRYPTED');

 const concurrent=encode();const concurrentResults=await Promise.all([process([concurrent]),process([concurrent])]);check('concurrent duplicate RPCs preserve one decrypted record',concurrentResults.reduce((n,r)=>n+r.records.filter(x=>x.status==='DECRYPTED').length,0)===1&&(await call('history',{conversationId:conv})).find(r=>r.sequence===concurrent.sequence)?.status==='DECRYPTED');
 const aadRetry=encode();
 const aadFailure=await process([{...aadRetry,clientMessageId:id(9000)}]);
 check('AAD mismatch is classified',aadFailure.records[0].error==='AAD_MISMATCH');
 check('correct metadata can retry without consumed ratchet',(await process([aadRetry])).records[0].status==='DECRYPTED');
 const earlier=encode();const later=encode();const gap=await process([later]);check('gap blocks advancement',gap.blocked==='SYNC_GAP'&&gap.records.length===0);check('missing lower sequence is recovered',(await process([earlier])).records[0].status==='DECRYPTED');check('later message succeeds after gap closes',(await process([later])).records[0].status==='DECRYPTED');
 const batchA=encode(),batchB=encode();const batch=await process([batchB,batchA]);check('within-page reversed input sorted',batch.records.length===2&&batch.records.every(x=>x.status==='DECRYPTED'));
 // A newly generated device without the Welcome has no group; supplying it later works.
 const noWelcome=new core.WebDevice(id(25));check('missing Welcome has no group',!noWelcome.hasGroup(gid));
 const manyFailures=Array.from({length:270},()=>encode());
 const failedBatch=await process(manyFailures.map(m=>({...m,ciphertext:'AQID'})));
 check('more than 256 failures remain durable',failedBatch.records.length===270&&failedBatch.records.every(r=>r.status==='FAILED'));
 check('large failure backlog does not stop new delivery',(await process([encode()])).records[0].status==='DECRYPTED');
 check('late failure beyond first 256 can recover',(await process([manyFailures[269]])).records[0].status==='DECRYPTED');
 // Simulate the pre-remediation layout: sealed records/cursors exist, but there
 // are no retry-reference keys. This changes no keys, ciphertext or MLS state.
 await new Promise((resolve,reject)=>{const r=indexedDB.open('cfmeet-messaging');r.onsuccess=()=>{const d=r.result;const tx=d.transaction('pending','readwrite');tx.objectStore('pending').delete(IDBKeyRange.bound([account,'retry:'],[account,'retry;']));tx.oncomplete=()=>{d.close();resolve();};tx.onerror=()=>reject(tx.error);};r.onerror=()=>reject(r.error);});
 for(let i=0;i<4;i++) await call('retryCursor',{conversationId:conv});
 check('paginated migration enrolls legacy failures beyond 256',(await process([manyFailures[268]])).records[0].status==='DECRYPTED');


 // Generate more than 512 KiB of legitimately decrypted synthetic history.
 for(let i=0;i<40;i++){const m=encode(16000);checkSilent((await process([m])).records[0].status==='DECRYPTED');}
 function checkSilent(ok){if(!ok)throw Error('bulk decrypt');}
 const all=await call('history',{conversationId:conv,limit:200});
 const target=new core.WebDevice(id(30));const transferKey=target.beginTransfer();const transferContext=[id(31),scope.tenantId,scope.applicationId,scope.environmentId,account,receiverId,id(30)];
 let overflow=null;try{await call('sealHistory',{context:transferContext,transferKey});}catch(e){overflow=e.message;}
 check('legacy archive never silently truncates',overflow==='HISTORY_REQUIRES_PAGINATION');
 // Exercise actual AES-GCM/HKDF migration with a synthetic 32-byte authenticator
 // output. The separate WebAuthn journey must establish real PRF/UV behavior.
 await call('holdDraft',{conversationId:conv,clientMessageId:id(7000),text:'protected draft'});
 const descriptor={v:1,method:'WEBAUTHN_PRF',account,credentialId:b64(new Uint8Array([1,2,3])),prfSalt:b64(new Uint8Array(32).fill(8)),origin:location.origin,installationId:'web-synthetic-installation',accountDeviceId:id(7001),messagingDeviceId:receiverId,scope};
 const prfSecret=new Uint8Array(32).fill(19);
 await call('_abortProtectionCommit');let migrationAborted=false;
 try{await call('protectLocal',{descriptor,prfSecret});}catch{migrationAborted=true;}
 check('aborted protection commit preserves original readable history',migrationAborted&&(await call('history',{conversationId:conv,limit:200})).length===all.length&&(await call('drafts',{conversationId:conv}))[0].text==='protected draft');
 check('legacy migration commits protected descriptor',(await call('protectLocal',{descriptor,prfSecret})).localProtection.messagingDeviceId===receiverId);
 const keyKind=await new Promise((resolve,reject)=>{const r=indexedDB.open('cfmeet-messaging');r.onsuccess=()=>{const d=r.result;const q=d.transaction('keys').objectStore('keys').get(account);q.onsuccess=()=>{resolve(q.result instanceof CryptoKey?'DIRECT_KEY':q.result.kind);d.close();};q.onerror=()=>reject(q.error);};r.onerror=()=>reject(r.error);});
 check('protected storage retains no directly usable wrapping key',keyKind==='LOCAL_AUTHENTICATOR_V1');
 await call('lock');let lockedRead=false;try{await call('history',{conversationId:conv});}catch{lockedRead=true;}check('locked worker refuses decrypted history',lockedRead);
 w.terminate();await new Promise(r=>setTimeout(r,100));start();
 check('restart requires authenticator output',(await call('open',{account,scope})).status==='AUTHENTICATOR_REQUIRED');
 let wrongSecret=false;try{await call('unlock',{prfSecret:new Uint8Array(32).fill(20)});}catch{wrongSecret=true;}check('wrong authenticator output cannot unlock',wrongSecret);
 check('correct authenticator output restores same identity',(await call('unlock',{prfSecret})).deviceId===receiverId);
 check('migration preserves all history and encrypted drafts',(await call('history',{conversationId:conv,limit:200})).length===all.length&&(await call('drafts',{conversationId:conv}))[0].text==='protected draft');
 await call('wipe',{account});w.terminate();await new Promise(r=>setTimeout(r,100));start();check('logout wipe removes identity',(await call('open',{account,scope})).status==='NONE');
 await call('generate',{deviceId:id(40)});await call('adopt');check('reenrollment has empty history',(await call('history',{conversationId:conv})).length===0);
 const emptyKey=target.beginTransfer();const emptyCtx=[id(32),scope.tenantId,scope.applicationId,scope.environmentId,account,id(40),id(30)];
 let emptyError=null;try{await call('sealHistory',{context:emptyCtx,transferKey:emptyKey});}catch(e){emptyError=e.message;}
 check('empty source is explicit',emptyError==='HISTORY_SOURCE_EMPTY');
 await call('createGroup',{groupId:gid});await call('confirm');
 const selfMessage=await call('encrypt',{conversationId:conv,groupId:gid,clientMessageId:id(5000),payload:{t:'text',text:'synthetic persistence probe'}});
 await process([{sequence:1,messageId:id(5001),clientMessageId:id(5000),senderDeviceId:id(40),senderPrincipalId:account,kind:'APPLICATION',lifecycle:'ACTIVE',epoch:0,ciphertext:b64(selfMessage.ciphertext)}]);
 check('readable history needs no recovery',!(await call('historyRecoveryNeeded')));
 await process([{sequence:2,messageId:id(5002),clientMessageId:id(5003),senderDeviceId:senderId,senderPrincipalId:id(8),kind:'EDIT',targetMessageId:id(5001),lifecycle:'ACTIVE',epoch:0,ciphertext:'AQID'}]);
 check('failed edit controls trigger automatic recovery discovery',await call('historyRecoveryNeeded'));
 const kept=await call('encrypt',{conversationId:conv,groupId:gid,clientMessageId:id(5010),payload:{t:'text',text:'visible after removal'}});
 await process([{sequence:3,messageId:id(5011),clientMessageId:id(5010),senderDeviceId:id(40),senderPrincipalId:account,kind:'APPLICATION',lifecycle:'ACTIVE',epoch:0,ciphertext:b64(kept.ciphertext)}]);
 await call('reconcileHistoryMetadata',{conversationId:conv,messages:[{sequence:1,messageId:id(5001),lifecycle:'DELETED'}]});
 check('removed target stops recovery of its failed hidden edit',!(await call('historyRecoveryNeeded')));
 check('cross-page target removal strips hidden edit payload',(await call('history',{conversationId:conv})).find(r=>r.sequence===2)?.status==='REMOVED');
 const absentConv=id(6000),absentTarget=id(6001),editId=id(6002),editCmid=id(6003);
 const absentEdit=await call('encrypt',{conversationId:absentConv,groupId:gid,clientMessageId:editCmid,payload:{t:'text',text:'deleted edit must not transfer'}});
 await call('processMessages',{conversationId:absentConv,groupId:gid,messages:[{sequence:2,messageId:editId,clientMessageId:editCmid,senderDeviceId:id(40),senderPrincipalId:account,kind:'EDIT',targetMessageId:absentTarget,lifecycle:'ACTIVE',epoch:0,ciphertext:b64(absentEdit.ciphertext)}]});
 await call('reconcileHistoryMetadata',{conversationId:absentConv,messages:[{sequence:1,messageId:absentTarget,lifecycle:'DELETED'}]});
 w.terminate();await new Promise(r=>setTimeout(r,100));start();await call('open',{account,scope});
 await call('reconcileHistoryMetadata',{conversationId:absentConv,messages:[{sequence:2,messageId:editId,targetMessageId:absentTarget,lifecycle:'ACTIVE'}]});
 const absentResult=(await call('history',{conversationId:absentConv}))[0];
 check('removal of an absent target survives restart and redacts later-page edits',absentResult.status==='REMOVED'&&absentResult.payload===undefined);

 w.terminate();return {boundary:'shipped worker and real WASM MLS with real Chromium IndexedDB; no backend or authentication services',events};
 },{worker:role('worker'),glue:role('glue'),wasm:role('wasm')});
 const probe=async p=>p.evaluate(async worker=>{
   const id=n=>`00000000-0000-4000-8000-${String(n).padStart(12,'0')}`;
   const w=new Worker('/'+worker,{type:'module'});let n=0;
   const call=(op,args)=>new Promise((resolve,reject)=>{w.onmessage=({data})=>data.ok?resolve(data.result):reject(Error(data.error));w.postMessage({id:++n,op,args});});
   const opened=await call('open',{account:id(4),scope:{tenantId:id(1),applicationId:id(2),environmentId:id(3)}});
   const history=await call('history',{conversationId:id(7)});w.terminate();return opened.deviceId===id(40)&&history.some(r=>r.status==='SENT');
 },role('worker'));
 await page.reload();if(!await probe(page))throw Error('refresh persistence');evidence.events.push({name:'page refresh preserves identity and sent history',result:'CONFIRMED'});
 await page.close();const reopened=await context.newPage();await reopened.goto(base);if(!await probe(reopened))throw Error('tab closure persistence');evidence.events.push({name:'tab close/reopen preserves identity and sent history',result:'CONFIRMED'});
 await browser.close();browser=await chromium.launchPersistentContext(profile);const restarted=await browser.newPage();await restarted.goto(base);if(!await probe(restarted))throw Error('browser restart persistence');evidence.events.push({name:'browser process restart with same profile preserves identity and sent history',result:'CONFIRMED'});
 // Two real tabs share one serialized runtime without exporting private state.
 const sharedTabs = [await browser.newPage(), await browser.newPage()];
 for (const tab of sharedTabs) {
   await tab.goto(base);
   await tab.evaluate(worker => {
     const shared = new SharedWorker('/' + worker, {type:'module',name:'cfmeet-shared-tab-regression'});
     const waiting = new Map(); let serial=0;
     shared.port.onmessage=({data})=>{const p=waiting.get(data.id);waiting.delete(data.id);data.ok?p.resolve(data.result):p.reject(Error(data.error));};
     shared.port.start();
     window.sharedCall=(op,args={})=>new Promise((resolve,reject)=>{const id=++serial;waiting.set(id,{resolve,reject});shared.port.postMessage({id,op,args});});
     window.sharedIds=n=>`00000000-0000-4000-8000-${String(n).padStart(12,'0')}`;
   },role('worker'));
 }
 const openShared=tab=>tab.evaluate(()=>sharedCall('open',{account:sharedIds(8000),scope:{tenantId:sharedIds(1),applicationId:sharedIds(2),environmentId:sharedIds(3)}}));
 if((await openShared(sharedTabs[0])).status!=='NONE')throw Error('shared initial account');
 await sharedTabs[0].evaluate(async()=>{await sharedCall('generate',{deviceId:sharedIds(8001)});await sharedCall('adopt');await sharedCall('createGroup',{groupId:new Uint8Array(32).fill(81)});await sharedCall('confirm');});
 if((await openShared(sharedTabs[1])).status!=='READY')throw Error('second tab shared identity');
 const sharedSent=await sharedTabs[1].evaluate(async()=>{const r=await sharedCall('encrypt',{conversationId:sharedIds(8002),groupId:new Uint8Array(32).fill(81),clientMessageId:sharedIds(8003),payload:{t:'text',text:'shared tab encrypted send'}});return{epoch:r.epoch,ciphertext:btoa(String.fromCharCode(...r.ciphertext)).replaceAll('+','-').replaceAll('/','_').replace(/=+$/,'')};});
 await sharedTabs[0].evaluate(sent=>sharedCall('processMessages',{conversationId:sharedIds(8002),groupId:new Uint8Array(32).fill(81),messages:[{sequence:1,messageId:sharedIds(8004),senderDeviceId:sharedIds(8001),senderPrincipalId:sharedIds(8000),clientMessageId:sharedIds(8003),kind:'APPLICATION',lifecycle:'ACTIVE',...sent}]}),sharedSent);
 const sharedHistory=await sharedTabs[1].evaluate(()=>sharedCall('history',{conversationId:sharedIds(8002)}));
 if(sharedHistory.length!==1||sharedHistory[0].status!=='SENT')throw Error('shared history');
 await sharedTabs[0].close();
 if((await sharedTabs[1].evaluate(()=>sharedCall('status'))).deviceId!=='00000000-0000-4000-8000-000000008001')throw Error('tab close stopped other tab');
 await sharedTabs[1].evaluate(()=>sharedCall('lock'));
 let sharedLocked=false;try{await sharedTabs[1].evaluate(()=>sharedCall('history',{conversationId:sharedIds(8002)}));}catch{sharedLocked=true;}
 if(!sharedLocked)throw Error('shared lock did not evict key');
 if((await openShared(sharedTabs[1])).status!=='READY')throw Error('shared runtime re-entry');
 await sharedTabs[1].close();
 evidence.events.push({name:'two tabs share one MLS runtime, closing one preserves the other and explicit lock evicts keys',result:'CONFIRMED'});
 await writeFile(out,JSON.stringify({at:new Date().toISOString(),...evidence},null,2));console.log(JSON.stringify(evidence,null,2));
}finally{await browser.close();server.closeAllConnections();await new Promise(r=>server.close(r));}
