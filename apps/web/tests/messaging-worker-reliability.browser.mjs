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
const server=createServer(async(req,res)=>{try{const name=new URL(req.url,'http://local').pathname.slice(1); if(!name){res.end('<html></html>');return;} if(!assets.has(name)) throw Error();res.setHeader('Content-Type',extname(name)==='.wasm'?'application/wasm':'text/javascript');res.end(assets.get(name));}catch{res.writeHead(404).end();}});
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
 function start(){w=new Worker('/'+worker,{type:'module'});w.onmessage=({data})=>{const c=calls.get(data.id);calls.delete(data.id);data.ok?c.resolve(data.result):c.reject(Error(data.error));};}
 function call(op,args={}){return new Promise((resolve,reject)=>{const n=++serial;calls.set(n,{resolve,reject});w.postMessage({id:n,op,args});setTimeout(()=>{if(calls.delete(n))reject(Error('TIMEOUT'));},15000);});}
 const events=[];const check=(name,ok,detail={})=>{if(!ok)throw Error(name);events.push({name,result:'CONFIRMED',...detail});};
 const b64=b=>btoa(String.fromCharCode(...b)).replaceAll('+','-').replaceAll('/','_').replace(/=+$/,'');
 const hex=b=>Array.from(b,x=>x.toString(16).padStart(2,'0')).join('');
 start();await call('open',{account,scope});await call('generate',{deviceId:receiverId});await call('adopt');
 const kp=(await call('keyPackages',{count:1}))[0];const sender=new core.WebDevice(senderId);sender.createGroup(gid);
 const packed=new Uint8Array(kp.length+4);new DataView(packed.buffer).setUint32(0,kp.length);packed.set(kp,4);const add=sender.addDevices(gid,packed);
 await call('join',{groupId:gid,welcome:add.welcome});
 const joinedEpoch=await call('epoch',{groupId:gid});
 let seq=0;const encode=(size=20)=>{const cmid=id(100+(++seq));const aad=core.message_aad(scope.tenantId,scope.applicationId,scope.environmentId,conv,cmid,senderId);const ciphertext=sender.encrypt(gid,aad,new TextEncoder().encode(JSON.stringify({v:1,t:'text',text:'x'.repeat(size)})));return {sequence:seq,messageId:id(1000+seq),clientMessageId:cmid,senderDeviceId:senderId,senderPrincipalId:id(8),kind:'APPLICATION',lifecycle:'ACTIVE',createdAt:'2026-11-24T00:00:00Z',epoch:Number(sender.epoch(gid)),ciphertext:b64(ciphertext)};};
 const process=messages=>call('processMessages',{conversationId:conv,groupId:gid,messages});
 const first=encode();check('normal real MLS decrypt',(await process([first])).records[0].status==='DECRYPTED');
 w.terminate();await new Promise(r=>setTimeout(r,100));start();check('worker restart preserves identity',(await call('open',{account,scope})).deviceId===receiverId);check('worker restart preserves encrypted history',(await call('history',{conversationId:conv})).length===1);
 const other=new Worker('/'+worker,{type:'module'});const locked=await new Promise(r=>{other.onmessage=({data})=>r(data.result);other.postMessage({id:1,op:'open',args:{account,scope}});});other.terminate();check('second worker cannot fork device',locked.status==='LOCKED');
 // Independently measure real core retention; do not assume old epochs remain decryptable.
 const rawSender=new core.WebDevice(id(21)),rawReceiver=new core.WebDevice(id(22));rawSender.createGroup(gid);
 const rawKp=rawReceiver.keyPackage(),rawPacked=new Uint8Array(rawKp.length+4);new DataView(rawPacked.buffer).setUint32(0,rawKp.length);rawPacked.set(rawKp,4);
 const rawAdd=rawSender.addDevices(gid,rawPacked);rawReceiver.join(gid,rawAdd.welcome);
 const rawAad=new Uint8Array([1]),rawOld=rawSender.encrypt(gid,rawAad,new Uint8Array([42]));const rawUpdate=rawSender.updateKeys(gid);rawReceiver.process(gid,rawUpdate.commit,undefined,[]);
 let retained=false,retentionError=null;try{retained=rawReceiver.process(gid,rawOld,rawAad,[]).kind==='APPLICATION';}catch(e){retentionError=/^[A-Z_]{3,64}$/.test(e.message)?e.message:'CORE_REJECTED';}
 events.push({name:'core old-epoch retention after commit',result:'CONFIRMED',oldEpochDecryptable:retained,error:retentionError});
 const old=encode();
 // Delay the already-member receiver's backlog, advance its own epoch through the real
 // worker commit/confirm API, then deliver the original unmodified old message.
 // This tests the worker contract; production engine reachability requires full-stack evidence.
 const change=await call('updateKeys',{groupId:gid});await call('confirm');sender.process(gid,change.commit,undefined,[]);
 const stateEpoch=await call('epoch',{groupId:gid});
 const oldLate=old;
 const classified=await process([oldLate]);check('prior membership is not labelled BEFORE_JOIN',classified.records[0].status==='FAILED'&&classified.records[0].error==='HISTORICAL_EPOCH_UNAVAILABLE',{joinedEpoch,currentEpoch:stateEpoch,messageEpoch:old.epoch});
 const lateRetry=await process([oldLate]);check('old-epoch failure remains retryable',lateRetry.records.length===1,{cursor:lateRetry.cursor});
 const bad=encode();const valid={...bad};bad.ciphertext=b64(new Uint8Array([1,2,3]));const failed=await process([bad]);check('invalid ciphertext persists FAILED',failed.records[0].status==='FAILED');w.terminate();await new Promise(r=>setTimeout(r,100));start();await call('open',{account,scope});check('corrected ciphertext recovers at same sequence',(await process([valid])).records[0].status==='DECRYPTED');check('successful delivery is idempotent',(await process([valid])).records.length===0);
 const earlier=encode();const later=encode();const gap=await process([later]);check('gap blocks advancement',gap.blocked==='SYNC_GAP'&&gap.records.length===0);check('missing lower sequence is recovered',(await process([earlier])).records[0].status==='DECRYPTED');check('later message succeeds after gap closes',(await process([later])).records[0].status==='DECRYPTED');
 const batchA=encode(),batchB=encode();const batch=await process([batchB,batchA]);check('within-page reversed input sorted',batch.records.length===2&&batch.records.every(x=>x.status==='DECRYPTED'));
 // A newly generated device without the Welcome has no group; supplying it later works.
 const noWelcome=new core.WebDevice(id(25));check('missing Welcome has no group',!noWelcome.hasGroup(gid));
 // Generate more than 512 KiB of legitimately decrypted synthetic history.
 for(let i=0;i<40;i++){const m=encode(16000);checkSilent((await process([m])).records[0].status==='DECRYPTED');}
 function checkSilent(ok){if(!ok)throw Error('bulk decrypt');}
 const all=await call('history',{conversationId:conv,limit:200});
 const target=new core.WebDevice(id(30));const transferKey=target.beginTransfer();const transferContext=[id(31),scope.tenantId,scope.applicationId,scope.environmentId,account,receiverId,id(30)];
 let overflow=null;try{await call('sealHistory',{context:transferContext,transferKey});}catch(e){overflow=e.message;}
 check('legacy archive never silently truncates',overflow==='HISTORY_REQUIRES_PAGINATION');
 await call('wipe',{account});w.terminate();await new Promise(r=>setTimeout(r,100));start();check('logout wipe removes identity',(await call('open',{account,scope})).status==='NONE');
 await call('generate',{deviceId:id(40)});await call('adopt');check('reenrollment has empty history',(await call('history',{conversationId:conv})).length===0);
 const emptyKey=target.beginTransfer();const emptyCtx=[id(32),scope.tenantId,scope.applicationId,scope.environmentId,account,id(40),id(30)];
 let emptyError=null;try{await call('sealHistory',{context:emptyCtx,transferKey:emptyKey});}catch(e){emptyError=e.message;}
 check('empty source is explicit',emptyError==='HISTORY_SOURCE_EMPTY');
 await call('createGroup',{groupId:gid});await call('confirm');
 const selfMessage=await call('encrypt',{conversationId:conv,groupId:gid,clientMessageId:id(5000),payload:{t:'text',text:'synthetic persistence probe'}});
 await process([{sequence:1,messageId:id(5001),clientMessageId:id(5000),senderDeviceId:id(40),senderPrincipalId:account,kind:'APPLICATION',lifecycle:'ACTIVE',epoch:0,ciphertext:b64(selfMessage.ciphertext)}]);
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
 await writeFile(out,JSON.stringify({at:new Date().toISOString(),...evidence},null,2));console.log(JSON.stringify(evidence,null,2));
}finally{await browser.close();server.closeAllConnections();await new Promise(r=>server.close(r));}
