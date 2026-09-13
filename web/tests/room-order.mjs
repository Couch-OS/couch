// Disposable local daemon only; reorders a room's devices from its page.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,rm} from 'node:fs/promises';
import {resolve} from 'node:path';
import http from 'node:http';
import {chromium} from '../../build/webui-review/node_modules/playwright/index.mjs';
const dir=await mkdtemp(resolve('build/room-order-test-'));
const reserve=http.createServer();await new Promise(r=>reserve.listen(0,'127.0.0.1',r));const port=reserve.address().port;await new Promise(r=>reserve.close(r));
const base=`http://127.0.0.1:${port}`;
const daemon=spawn('daemon/target/release/couch-confd',['--addr',`127.0.0.1:${port}`,'--no-auth','--config',`${dir}/config.json`,'--www','web/couch-web/dist'],{stdio:'ignore'});
const ids=async()=>(await (await fetch(base+'/api/config')).json()).rooms[0].devices.map(d=>d.id);
let browser;
try {
 for(let i=0;i<60;i++){try{if((await fetch(base+'/api/health')).ok)break;}catch{}await new Promise(r=>setTimeout(r,100));}
 const config={schema_version:1,rooms:[{id:'den',name:'Den',devices:[
   {id:'lamp',name:'Lamp',kind:'light'},{id:'telly',name:'Telly',kind:'tv'},{id:'blind',name:'Blind',kind:'blind'}]}],areas:[],scenes:[],activities:[]};
 assert((await fetch(base+'/api/config',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify(config)})).ok);
 browser=await chromium.launch();const page=await browser.newPage({viewport:{width:390,height:844}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto(base+'/rooms/den');
 const cards=page.locator('li.card.device');await cards.nth(2).waitFor();
 assert.deepEqual(await cards.locator('h3').allTextContents(),['Lamp','Telly','Blind']);
 // The first card can only move down, the last only up.
 assert(await cards.nth(0).getByRole('button',{name:'Move up'}).isDisabled());
 assert(await cards.nth(2).getByRole('button',{name:'Move down'}).isDisabled());
 await cards.nth(2).getByRole('button',{name:'Move up'}).click();
 await page.getByRole('status').filter({hasText:/^Saved$/}).waitFor();
 await page.locator('li.card.device').nth(1).getByText('Blind',{exact:true}).waitFor();
 assert.deepEqual(await ids(),['lamp','blind','telly']);
 await page.locator('li.card.device').nth(0).getByRole('button',{name:'Move down'}).click();
 await page.getByRole('status').filter({hasText:/^Saved$/}).waitFor();
 await page.locator('li.card.device').nth(0).getByText('Blind',{exact:true}).waitFor();
 assert.deepEqual(await ids(),['blind','lamp','telly']);
 assert.deepEqual(await page.locator('li.card.device h3').allTextContents(),['Blind','Lamp','Telly']);
 assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
 await page.screenshot({path:'build/room-order-mobile.png',fullPage:true});
 assert.deepEqual(errors,[]);
 console.log('PASS: device cards reorder with the arrows, the order persists, and the mobile layout has no overflow.');
} finally { if(browser)await browser.close();daemon.kill();await rm(dir,{recursive:true,force:true}); }
