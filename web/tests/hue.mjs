// Isolated HTTPS bridge fixture. Never sends commands to household lights.
import assert from 'node:assert/strict';
import https from 'node:https';
import http from 'node:http';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,readFile,writeFile,stat,rm} from 'node:fs/promises';
import {resolve} from 'node:path';
import {chromium} from '../../build/webui-review/node_modules/playwright/index.mjs';
const dir=await mkdtemp(resolve('build/hue-test-'));const settings=`${dir}/connection.json`;
execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',`${dir}/key.pem`,'-out',`${dir}/cert.pem`,'-days','1','-subj','/CN=fixture-bridge'],{stdio:'ignore'});
const id='00000000-0000-0000-0000-000000000001';let pressed=false,power=true,brightness=50,connected=true,reject=false;const commands=[];
const mock=https.createServer({key:await readFile(`${dir}/key.pem`),cert:await readFile(`${dir}/cert.pem`)},async(req,res)=>{
 let text='';for await(const c of req)text+=c;res.setHeader('Content-Type','application/json');
 const send=v=>res.end(JSON.stringify(v));
 if(req.url==='/api' && req.method==='POST'){assert.equal(JSON.parse(text).devicetype,'couch#remote');send(pressed?[{success:{username:'test-secret'}}]:[{error:{type:101,description:'link button not pressed'}}]);return;}
 if(req.headers['hue-application-key']!=='test-secret'){res.writeHead(403);send({});return;}
 if(req.url==='/clip/v2/resource'){send({errors:[],data:[{type:'room',id:'bridge-office',metadata:{name:'Bridge Office'},services:[{rtype:'grouped_light',rid:id}]},{type:'grouped_light',id,on:{on:power}},{type:'scene',id,metadata:{name:'Relax'},group:{rid:'bridge-office'}},{type:'light',id,owner:{rid:'device'},metadata:{name:'Test Hue light'},on:{on:power},dimming:{brightness}},{type:'zigbee_connectivity',owner:{rid:'device'},status:connected?'connected':'disconnected'}]});return;}
 if(req.method==='PUT' && req.url===`/clip/v2/resource/light/${id}`){
  if(reject){send({errors:[{description:'rejected'}],data:[]});return;}
  const body=JSON.parse(text);commands.push(body);power=body.on.on;if(body.dimming)brightness=body.dimming.brightness;
  send({errors:[],data:[{rid:id,rtype:'light'}]});return;
 }
 res.writeHead(404);send({});
});
await new Promise(r=>mock.listen(0,'127.0.0.1',r));const bridge=`https://127.0.0.1:${mock.address().port}`;
const reserve=http.createServer();await new Promise(r=>reserve.listen(0,'127.0.0.1',r));const port=reserve.address().port;await new Promise(r=>reserve.close(r));const base=`http://127.0.0.1:${port}`;
const daemon=spawn('daemon/target/release/couch-confd',['--addr',`127.0.0.1:${port}`,'--no-auth','--config',`${dir}/config.json`,'--www','web/couch-web/dist'],{env:{...process.env,COUCH_HUE_CONNECTION:settings},stdio:'ignore'});
let browser;
try{
 for(let i=0;i<60;i++){try{if((await fetch(`${base}/api/health`)).ok)break;}catch{}await new Promise(r=>setTimeout(r,100));}
 const api=(path,method='GET',data)=>fetch(`${base}/api/hue/${path}`,{method,headers:{'Content-Type':'application/json'},body:data?JSON.stringify(data):undefined});
 assert.equal((await api('connection','PUT',{url:bridge})).status,502);
 browser=await chromium.launch();const page=await browser.newPage({viewport:{width:390,height:844}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await fetch(`${base}/api/rooms`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name:'Test room'})});
 await page.goto(base);assert.equal(await page.getByRole('navigation').getByRole('button',{name:'Scenes',exact:true}).count(),0);await page.getByRole('navigation').getByRole('button',{name:'Connections',exact:true}).click();
 await page.getByLabel('Connection type',{exact:true}).selectOption('hue');await page.getByLabel('Connection name',{exact:true}).fill('Philips Hue');await page.getByRole('button',{name:'Create connection',exact:true}).click();
 const card=page.locator('.hue-connection');await card.getByLabel('Hue bridge address').fill(bridge);
 pressed=true;await card.getByRole('button',{name:'Pair bridge',exact:true}).click();await card.getByRole('status').filter({hasText:'Connected and saved'}).waitFor();
 assert.equal(await page.getByRole('button',{name:'Add to this room',exact:true}).count(),0);
 // A named connection keeps its credentials beside the config, per connection.
 const connectionId=(await(await fetch(`${base}/api/config`)).json()).connections[0].id;const saved=`${dir}/connections/${connectionId}/hue-connection.json`;
 const named=(path,method='GET',data)=>fetch(`${base}/api/connections/${connectionId}/hue/${path}`,{method,headers:{'Content-Type':'application/json'},body:data?JSON.stringify(data):undefined});
 assert.equal((await stat(saved)).mode & 0o777,0o600);assert(!JSON.stringify(await(await named('connection')).json()).includes('test-secret'));
 const before=await readFile(saved,'utf8');pressed=false;assert.equal((await named('connection','PUT',{url:bridge})).status,502);assert.equal(await readFile(saved,'utf8'),before);
 await page.getByRole('navigation').getByRole('button',{name:'Rooms & devices',exact:true}).click();await page.getByRole('button',{name:/^Test room/}).click();
 await page.getByRole('button',{name:'Add to this room',exact:true}).click();await page.locator('.device').getByRole('heading',{name:'Test Hue light',exact:true}).waitFor();
 await page.getByRole('button',{name:'Show light controls',exact:true}).click();
 await page.locator('.device').getByRole('button',{name:'Turn off',exact:true}).click();await page.locator('.device').getByText('Off',{exact:true}).waitFor();
 await page.locator('.device').getByLabel('Brightness (%)',{exact:true}).fill('37');await page.locator('.device').getByRole('button',{name:'Apply brightness'}).click();await page.locator('.device').getByText('On · 37%',{exact:true}).waitFor();
 assert(commands.some(b=>b.on.on===false));assert(commands.some(b=>b.dimming?.brightness===37));
 reject=true;assert.equal((await api(`lights/${id}/command`,'POST',{action:'on'})).status,502);reject=false;
 connected=false;assert.equal((await(await api(`lights/${id}`)).json()).on,null);const n=commands.length;assert.equal((await api(`lights/${id}/command`,'POST',{action:'on'})).status,502);assert.equal(commands.length,n);connected=true;
 assert.equal((await api(`lights/${id}/command`,'POST',{action:'brightness',brightness:101})).status,400);
 const cfg=await(await fetch(`${base}/api/config`)).json();assert(cfg.rooms.some(r=>r.devices.some(d=>d.integration.via==='connection' && d.integration.resource_id===id)));
 await page.getByLabel('Hue controls',{exact:true}).selectOption('rooms');
 await page.locator('.discovered-device').getByText('Bridge Office',{exact:true}).waitFor();
 await page.locator('.discovered-device').getByRole('button',{name:'Add to this room',exact:true}).click();
 await page.locator('.device').getByRole('heading',{name:'Bridge Office',exact:true}).waitFor();
 await page.getByLabel('Hue controls',{exact:true}).selectOption('scenes');
 await page.locator('.discovered-device').getByText('Relax',{exact:true}).waitFor();
 await page.getByLabel('Search devices',{exact:true}).fill('no-match');
 assert.equal(await page.locator('.discovered-device').count(),0);
 await page.getByLabel('Search devices',{exact:true}).fill('relax');
 await page.getByLabel('Hue room or zone',{exact:true}).selectOption('Bridge Office');
 await page.locator('.discovered-device').getByRole('button',{name:'Add to this room',exact:true}).click();
 await page.locator('.room-scenes').getByText('Relax',{exact:true}).waitFor();
 assert.equal(await page.getByLabel('Hue controls',{exact:true}).inputValue(),'scenes');
 await page.locator('.discovered-device').getByRole('button',{name:'Added',exact:true}).waitFor();
 let cfg2=await (await fetch(`${base}/api/config`)).json();
 assert.equal(cfg2.scenes.find(s=>s.hue?.scene_id===id).rooms[0],'test-room');
 assert(!cfg2.rooms.find(r=>r.id==='test-room').devices.some(d=>d.integration.resource_id==='scene:'+id));
 await fetch(`${base}/api/rooms`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name:'Second room'})});
 await page.goto(`${base}/rooms/second-room`);
 await page.getByLabel('From connection',{exact:true}).selectOption(cfg2.connections.find(c=>c.provider.kind==='hue').id);
 await page.getByLabel('Hue controls',{exact:true}).selectOption('scenes');
 await page.locator('.discovered-device').getByRole('button',{name:'Add to this room',exact:true}).click();
 await page.locator('.room-scenes').getByText('Relax',{exact:true}).waitFor();
 cfg2=await (await fetch(`${base}/api/config`)).json();
 assert.equal(cfg2.scenes.filter(s=>s.hue?.scene_id===id).length,1);
 assert.deepEqual(cfg2.scenes.find(s=>s.hue?.scene_id===id).rooms,['test-room','second-room']);
 await page.locator('.room-scenes').getByRole('button',{name:'Remove from room',exact:true}).click();
 await page.locator('.room-scenes').getByText('Relax',{exact:true}).waitFor({state:'hidden'});
 cfg2=await (await fetch(`${base}/api/config`)).json();
 assert.deepEqual(cfg2.scenes.find(s=>s.hue?.scene_id===id).rooms,['test-room']);
 await page.goto(`${base}/rooms/test-room`);
 await page.locator('.room-scenes').getByText('Relax',{exact:true}).waitFor();
 await page.screenshot({path:'build/hue-scenes-in-room.png',fullPage:true});
 const hueConnection=cfg2.connections.find(c=>c.provider.kind==='hue').id;
 assert.equal((await fetch(`${base}/api/connections/${hueConnection}`,{method:'DELETE'})).status,422);
 await page.screenshot({path:'build/hue-scenes-web.png',fullPage:true});
 assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await page.screenshot({path:'build/webui-review/hue-mobile.png',fullPage:true});
 const corrupt=JSON.parse(before);corrupt.certificate[0]^=1;await writeFile(settings,JSON.stringify(corrupt));assert.equal((await api('lights')).status,502);await writeFile(settings,before);
 assert.deepEqual(errors,[]);console.log('PASS: HTTPS pairing, link-button errors, pinned certificate rejection, private settings, discovery, on/off/brightness, unreachable lights, Hue error envelopes, room controls, scene import/search/filter/assignment and mobile UI');
}finally{if(browser)await browser.close();daemon.kill();mock.closeAllConnections();await new Promise(r=>mock.close(r));await rm(dir,{recursive:true,force:true});}
