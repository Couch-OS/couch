// Disposable local daemon only; a write must not rebuild the screen it was made
// from. Everything here failed before the screens read the config through the
// per-slice memos: an accepted write replaced the whole subtree, which threw
// away open <details>, half-typed fields and the scroll position.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,rm} from 'node:fs/promises';
import {resolve} from 'node:path';
import http from 'node:http';
import {chromium} from '../../build/webui-review/node_modules/playwright/index.mjs';
const dir=await mkdtemp(resolve('build/in-place-test-'));
const reserve=http.createServer();await new Promise(r=>reserve.listen(0,'127.0.0.1',r));const port=reserve.address().port;await new Promise(r=>reserve.close(r));
const base=`http://127.0.0.1:${port}`;
const daemon=spawn('daemon/target/release/couch-confd',['--addr',`127.0.0.1:${port}`,'--no-auth','--config',`${dir}/config.json`,'--www','web/couch-web/dist'],{stdio:'ignore'});
const saved=async()=>(await (await fetch(base+'/api/config')).json());
let browser;
try {
 for(let i=0;i<60;i++){try{if((await fetch(base+'/api/health')).ok)break;}catch{}await new Promise(r=>setTimeout(r,100));}
 const config={schema_version:1,areas:[],scenes:[],rooms:[{id:'office',name:'Office',devices:[
   {id:'office-lamp',name:'Lamp',kind:'light'},{id:'office-fan',name:'Fan',kind:'other'}]}],
   activities:[{id:'watch',name:'Watch',room:'office',kind:'video',setup:{devices:['office-lamp']},buttons:[],steps:[]}]};
 assert((await fetch(base+'/api/config',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify(config)})).ok);
 browser=await chromium.launch();const page=await browser.newPage({viewport:{width:390,height:844}});
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 const card=name=>page.locator('li.card.device').filter({has:page.getByRole('heading',{name,exact:true})});
 const isSaved=()=>page.getByRole('status').filter({hasText:/^Saved$/}).waitFor();

 await page.goto(base+'/rooms/office');
 // Open both device editors and half-type a new name into the second one.
 await card('Lamp').getByText('Edit device',{exact:true}).click();
 await card('Fan').getByText('Edit device',{exact:true}).click();
 await card('Fan').getByLabel('Device name',{exact:true}).fill('Ceiling fan draft');
 const fan=await card('Fan').elementHandle();
 // Save the other row.
 await card('Lamp').getByLabel('Device name',{exact:true}).fill('Desk lamp');
 await card('Lamp').getByRole('button',{name:'Save device',exact:true}).click();
 await isSaved();
 assert.equal((await saved()).rooms[0].devices[0].name,'Desk lamp');
 // The neighbour kept its draft and its open editor, and was never replaced.
 assert.equal(await card('Fan').getByLabel('Device name',{exact:true}).inputValue(),'Ceiling fan draft');
 assert(await card('Fan').locator('details').first().evaluate(d=>d.open),'the neighbour\'s editor closed');
 assert(await fan.evaluate(n=>n.isConnected),'the neighbour\'s row was rebuilt');
 // The saved row updated in place: same list, new heading.
 assert(await card('Desk lamp').locator('details').first().evaluate(d=>d.open),'the saved row\'s editor closed');
 assert.deepEqual(await page.locator('li.card.device h3').allTextContents(),['Desk lamp','Fan']);

 // A write from another screen does not disturb this one either: the room's
 // own name is saved while the device draft is still open below it.
 await page.getByLabel('Name',{exact:true}).first().fill('Back office');
 await page.getByLabel('Name',{exact:true}).first().press('Enter');
 await isSaved();
 await page.getByRole('heading',{name:'Back office',exact:true}).waitFor();
 assert.equal(await card('Fan').getByLabel('Device name',{exact:true}).inputValue(),'Ceiling fan draft');
 assert(await fan.evaluate(n=>n.isConnected),'renaming the room rebuilt the device list');

 // The same on an activity: ticking one device leaves the list it is in.
 await page.goto(base+'/activities/watch');
 const included=await page.locator('.activity-device-list').first().elementHandle();
 await page.locator('.activity-device-choice').filter({hasText:'Fan'}).getByRole('checkbox').check();
 await isSaved();
 assert.deepEqual((await saved()).activities[0].setup.devices.sort(),['office-fan','office-lamp']);
 assert(await included.evaluate(n=>n.isConnected),'ticking a device rebuilt the included list');

 assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
 assert.deepEqual(errors,[]);
 console.log('PASS: a save leaves a neighbouring row\'s draft, its open editor and the row itself alone, on a room page and in an activity.');
} finally { if(browser)await browser.close();daemon.kill();await rm(dir,{recursive:true,force:true}); }
