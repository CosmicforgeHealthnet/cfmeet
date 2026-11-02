import { chromium } from 'playwright';
import fs from 'node:fs';
import assert from 'node:assert/strict';

const users=JSON.parse(fs.readFileSync('/result/accounts.private.json','utf8'));
const report={scope:'deployed-web-real-CF-Meet-accounts',checks:{},phase:'login'};
const browser=await chromium.launch({headless:true,args:['--use-fake-device-for-media-stream','--disable-dev-shm-usage']});
const pages=[],contexts=[],tokens=[];
const calls=[];
async function request(index,path,body) {
  return pages[index].evaluate(async ({path,body,token})=>{
    const response=await fetch(path,{method:body===undefined?'GET':'POST',headers:{Authorization:'Bearer '+token,
      'Content-Type':'application/json','Idempotency-Key':crypto.randomUUID()},body:body===undefined?undefined:JSON.stringify(body)});
    return {status:response.status,body:await response.json()};
  },{path,body,token:tokens[index]});
}
try {
  for(let i=0;i<2;i++) {
    const context=await browser.newContext({permissions:['microphone','camera']});contexts.push(context);
    const page=await context.newPage();pages.push(page);page.setDefaultTimeout(25000);
    page.on('response',async response=>{
      const path=new URL(response.url()).pathname;
      if(path.startsWith('/api/calling')&&response.status()>=400) {
        let code='NON_JSON';try{code=(await response.json()).error?.code||'UNKNOWN';}catch{}
        (report.failed_requests??=[]).push({path,status:response.status(),code});
      }
    });
    page.on('websocket',socket=>socket.on('socketerror',()=>{report.signaling_socket_error=true;}));
    page.on('response',async response=>{if(response.url().includes('/auth/login')&&response.ok()) {
      const value=await response.json();tokens[i]=value.access_token;
    }});
    await page.goto('https://cfmeet.com/login');
    await page.locator('input[type=email]').fill(users[i].email);
    await page.locator('input[type=password]').fill(users[i].password);
    await page.getByRole('button',{name:'Log In',exact:true}).click();
    await page.waitForURL(/\/dashboard/);
    await page.goto('https://cfmeet.com/dashboard/calls');
    await page.getByRole('heading',{name:'Calls',exact:true}).waitFor();
    const identity=await request(i,'/api/calling/identity');
    report.checks['account_'+i+'_web_identity']=identity.status===200?'PASS':'FAIL';
    if(identity.status!==200) {report.failure_code=identity.body.error?.code;throw new Error('Real account Calling identity failed');}
    report.phase='account_'+i+'_endpoint_and_signaling';
    await page.getByRole('button',{name:'Enable this browser',exact:true}).click();
    await page.getByText('Ready to receive calls',{exact:true}).waitFor();
  }
  report.checks.two_account_web_login='PASS';
  for(const kind of ['VOICE','VIDEO']) {
    report.phase=kind.toLowerCase()+'_call';
    const a=pages[0],b=pages[1];
    await a.locator('#call-person').selectOption(users[1].userId);
    await a.locator('#call-kind').selectOption(kind);
    const created=a.waitForResponse(r=>r.url().endsWith('/api/calling/v1/calls')&&r.request().method()==='POST');
    await a.getByRole('button',{name:'Call',exact:true}).click();
    const response=await created,body=await response.json();
    assert(response.ok(), 'Call creation rejected');calls.push(body.call.callId);
    await b.getByRole('heading',{name:'Incoming '+kind.toLowerCase()+' call'}).waitFor();
    report.checks[kind.toLowerCase()+'_ringing']='PASS';
    await b.getByRole('button',{name:'Accept',exact:true}).click();
    await a.getByRole('button',{name:'Microphone',exact:true}).waitFor();
    await b.getByRole('button',{name:'Microphone',exact:true}).waitFor();
    await a.waitForTimeout(5000);
    if(kind==='VIDEO') {
      await a.waitForFunction(()=>[...document.querySelectorAll('video')].filter(v=>v.readyState>=2&&v.videoWidth>0).length>=2);
      await b.waitForFunction(()=>[...document.querySelectorAll('video')].filter(v=>v.readyState>=2&&v.videoWidth>0).length>=2);
    }
    const call=await request(0,'/api/calling/v1/calls/'+body.call.callId);
    report.last_call_state=call.body.state;
    assert.equal(call.body.state,'CONNECTED');
    report.checks[kind.toLowerCase()+'_connected']='PASS';
    await a.getByRole('button',{name:'End',exact:true}).click();
    await a.getByRole('button',{name:'Microphone',exact:true}).waitFor({state:'hidden'});
    await b.getByRole('button',{name:'Microphone',exact:true}).waitFor({state:'hidden'});
    report.checks[kind.toLowerCase()+'_end_cleanup']='PASS';
  }
  report.phase='decline';
  await pages[0].getByRole('button',{name:'Call',exact:true}).click();
  await pages[1].getByRole('button',{name:'Decline',exact:true}).click();
  await pages[1].getByRole('button',{name:'Decline',exact:true}).waitFor({state:'hidden'});
  report.checks.decline='PASS';
} catch(error) {
  report.error_type=error.name;
  report.failure_message=String(error.message).split('\n')[0].slice(0,200);
  report.alerts=[];
  for(const page of pages) {try{report.alerts.push(...await page.getByRole('alert').allTextContents());}catch{}}
  report.pages=[];
  for(const page of pages) {try{report.pages.push({origin:new URL(page.url()).origin,path:new URL(page.url()).pathname,
    title:await page.title(),text:(await page.locator('body').innerText()).slice(0,700)});}catch{}}
  process.exitCode=1;
} finally {
  for(const call of calls) {
    try {const current=await request(0,'/api/calling/v1/calls/'+call);
      if(!['ENDED','CANCELLED','FAILED','DECLINED','NO_ANSWER'].includes(current.body.state))
        await request(0,'/api/calling/v1/calls/'+call+'/end',{expectedVersion:current.body.version});
    } catch {}
  }
  await browser.close();
  fs.writeFileSync('/result/web-result.json',JSON.stringify(report,null,2));
}
