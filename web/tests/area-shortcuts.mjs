// Disposable local daemon only; assigns every kind of quick-access key on an area page.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,rm} from 'node:fs/promises';
import {resolve} from 'node:path';
import http from 'node:http';
import {chromium} from '../../build/webui-review/node_modules/playwright/index.mjs';
const dir=await mkdtemp(resolve('build/area-shortcuts-test-'));
const reserve=http.createServer();await new Promise(r=>reserve.listen(0,'127.0.0.1',r));const port=reserve.address().port;await new Promise(r=>reserve.close(r));
const base=`http://127.0.0.1:${port}`;
const daemon=spawn('daemon/target/release/couch-confd',['--addr',`127.0.0.1:${port}`,'--no-auth','--config',`${dir}/config.json`,'--www','web/couch-web/dist'],{stdio:'ignore'});
const json=async (method,path,body)=>{const r=await fetch(base+path,{method,headers:{'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});return {ok:r.ok,status:r.status,body:await r.json().catch(()=>null)};};
let browser;
try {
 for(let i=0;i<60;i++){try{if((await fetch(base+'/api/health')).ok)break;}catch{}await new Promise(r=>setTimeout(r,100));}
 const config={schema_version:1,
  rooms:[{id:'living',name:'Living room',devices:[
    {id:'lamp',name:'Lamp',kind:'light',integration:{via:'home-assistant',entity_id:'light.lamp'}},
    {id:'heat',name:'Heating',kind:'thermostat',integration:{via:'home-assistant',entity_id:'climate.living'}},
    {id:'tv',name:'Telly',kind:'tv'}]}],
  areas:[{id:'living-area',name:'Living',rooms:['living']},{id:'basement',name:'Basement',rooms:[]}],
  scenes:[],activities:[{id:'reading',name:'Reading',room:'living'}]};
 assert((await json('PUT','/api/config',config)).ok);
 // The API refuses keys that are not quick-access keys and targets that do not exist.
 assert.equal((await json('PUT','/api/areas/living-area/shortcuts',[{button:'ok',action:{kind:'area',area:'basement'}}])).status,422);
 assert.equal((await json('PUT','/api/areas/living-area/shortcuts',[{button:'red',action:{kind:'activity',activity:'ghost'}}])).status,422);
 assert.equal((await json('PUT','/api/areas/living-area/shortcuts',[{button:'red',action:{kind:'toggle',device:'tv'}}])).status,422);
 browser=await chromium.launch();const page=await browser.newPage({viewport:{width:390,height:844}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto(base+'/areas/living-area');
 const editor=page.getByRole('region',{name:'Quick-access keys'});
 await editor.waitFor();
 assert.equal(await editor.getByRole('button',{name:/quick access$/}).count(),8);
 const assign=async (key,option)=>{
  await editor.getByRole('button',{name:`${key}, quick access`}).click();
  const picker=page.locator('dialog.command-picker[open]');
  await picker.getByRole('heading',{name:key,exact:true}).waitFor();
  await picker.getByRole('button',{name:option,exact:true}).click();
  await page.getByRole('status').filter({hasText:/^Saved$/}).waitFor();
  await picker.waitFor({state:'hidden'});
 };
 await assign('Red','Areas · show the page: Basement');
 await assign('Light key','Lights and covers · switch on / off: Lamp');
 await assign('Media key','Activities · open: Reading');
 await assign('Climate key','Devices · open controls: Heating');
 // A search narrows the picker to matching targets across every group.
 await editor.getByRole('button',{name:'Green, quick access'}).click();
 const picker=page.locator('dialog.command-picker[open]');
 await picker.getByRole('searchbox',{name:'Search targets'}).fill('heat');
 assert.deepEqual(await picker.getByRole('button',{name:/^(Areas|Activities|Lights|Devices)/}).allTextContents(),['Heating · Living room · thermostat＋']);
 await picker.getByRole('searchbox',{name:'Search targets'}).fill('nothing here');
 await picker.getByText('Nothing matches',{exact:false}).waitFor();
 await picker.getByRole('button',{name:'Close key picker'}).click();
 await picker.waitFor({state:'hidden'});
 let saved=(await json('GET','/api/config')).body.areas[0].shortcuts;
 assert.deepEqual(saved,[
  {button:'lights',action:{kind:'toggle',device:'lamp'}},
  {button:'music',action:{kind:'activity',activity:'reading'}},
  {button:'tv',action:{kind:'device',device:'heat'}},
  {button:'red',action:{kind:'area',area:'basement'}}]);
 // The slots show what was saved after a reload, and a key can be cleared.
 await page.reload();
 await editor.getByRole('button',{name:'Red, quick access'}).getByText('Basement',{exact:true}).waitFor();
 await editor.getByRole('button',{name:'Light key, quick access'}).getByText('Switch on / off',{exact:true}).waitFor();
 await editor.getByRole('button',{name:'Climate key, quick access'}).getByText('Heating · Living room',{exact:true}).waitFor();
 await editor.getByRole('button',{name:'Media key, quick access'}).click();
 await page.locator('dialog.command-picker[open]').getByRole('button',{name:'Clear this key'}).click();
 await page.getByRole('status').filter({hasText:/^Saved$/}).waitFor();
 await editor.getByRole('button',{name:'Media key, quick access'}).getByText('Not assigned',{exact:true}).waitFor();
 saved=(await json('GET','/api/config')).body.areas[0].shortcuts;
 assert.deepEqual(saved.map(s=>s.button),['lights','tv','red']);
 // Deleting the target area drops the key that reached it.
 assert((await json('DELETE','/api/areas/basement')).ok);
 saved=(await json('GET','/api/config')).body.areas[0].shortcuts;
 assert.deepEqual(saved.map(s=>s.button),['lights','tv']);
 await page.reload();
 await editor.getByRole('button',{name:'Red, quick access'}).getByText('Not assigned',{exact:true}).waitFor();
 assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
 await page.screenshot({path:'build/area-shortcuts-mobile.png',fullPage:true});
 await page.setViewportSize({width:1280,height:900});
 await page.screenshot({path:'build/area-shortcuts-desktop.png',fullPage:true});
 assert.deepEqual(errors,[]);
 console.log('PASS: quick-access keys assign an area, a light toggle, an activity and a device; search, clear, reload and target removal behave; mobile layout has no overflow.');
} finally { if(browser)await browser.close();daemon.kill();await rm(dir,{recursive:true,force:true}); }
