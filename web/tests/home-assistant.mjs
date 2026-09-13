// Entire integration test is loopback-only; it never contacts real lights.
import assert from 'node:assert/strict';
import http from 'node:http';
import {spawn} from 'node:child_process';
import {mkdtemp,readFile,stat,rm} from 'node:fs/promises';
import {resolve} from 'node:path';
import {chromium} from '../../build/webui-review/node_modules/playwright/index.mjs';
const directory = await mkdtemp(resolve('build/ha-test-'));
const settings = `${directory}/connection.json`;
let power='on',brightness=128;
const commands=[];
const light=()=>({entity_id:'light.test',state:power,attributes:{friendly_name:'Test light',brightness,supported_color_modes:['brightness']}});
const mock=http.createServer(async(req,res)=>{
  if(req.headers.authorization!=='Bearer test-secret'){res.writeHead(401);res.end('{}');return;}
  let data='';for await(const chunk of req)data+=chunk;
  res.setHeader('Content-Type','application/json');
  if(req.url==='/api/states'){res.end(JSON.stringify([light()]));return;}
  if(req.url==='/api/states/light.test'){res.end(JSON.stringify(light()));return;}
  if(req.method==='POST' && req.url.startsWith('/api/services/light/')){
    const body=JSON.parse(data);assert.equal(body.entity_id,'light.test');commands.push([req.url,body]);
    power=req.url.endsWith('turn_off')?'off':'on';if(body.brightness_pct!==undefined)brightness=Math.round(body.brightness_pct*255/100);
    res.end('[]');return;
  }
  res.writeHead(404);res.end('{}');
});
await new Promise(r=>mock.listen(0,'127.0.0.1',r));
const ha=`http://127.0.0.1:${mock.address().port}`;
const portServer=http.createServer();await new Promise(r=>portServer.listen(0,'127.0.0.1',r));const port=portServer.address().port;await new Promise(r=>portServer.close(r));
const base=`http://127.0.0.1:${port}`;
const daemon=spawn('daemon/target/release/couch-confd',['--addr',`127.0.0.1:${port}`,'--no-auth','--config',`${directory}/config.json`,'--www','web/couch-web/dist'],{env:{...process.env,COUCH_HA_CONNECTION:settings},stdio:'ignore'});
let browser;
try{
  for(let i=0;i<50;i++){try{if((await fetch(`${base}/api/health`)).ok)break;}catch{}await new Promise(r=>setTimeout(r,100));}
  browser=await chromium.launch();const page=await browser.newPage({viewport:{width:390,height:844}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await fetch(`${base}/api/rooms`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name:'Test room'})});
  await page.goto(base);await page.getByRole('navigation').getByRole('button',{name:'Connections',exact:true}).click();
  await page.getByLabel('Connection type',{exact:true}).selectOption('home-assistant');
  await page.getByLabel('Connection name',{exact:true}).fill('Home Assistant');
  await page.getByRole('button',{name:'Create connection',exact:true}).click();
  await page.getByRole('heading',{name:'Home Assistant',exact:true}).first().waitFor();
  await page.getByLabel('Server URL',{exact:true}).fill(ha);
  await page.getByLabel('Long-lived access token').fill('test-secret');
  await page.getByRole('button',{name:'Test & save connection'}).click();
  await page.locator('.ha-connection').getByRole('status').filter({hasText:'Connected and saved'}).waitFor();
  assert.equal(await page.getByRole('button',{name:'Add to this room',exact:true}).count(),0);
  assert.equal(await page.getByLabel('Long-lived access token').inputValue(),'');
  // A named connection keeps its credentials beside the config, per connection.
  const connectionId=(await(await fetch(`${base}/api/config`)).json()).connections[0].id;
  const saved=`${directory}/connections/${connectionId}/ha-connection.json`;
  assert.equal((await stat(saved)).mode & 0o777,0o600);
  const publicSettings=await(await fetch(`${base}/api/connections/${connectionId}/ha/connection`)).json();assert(!JSON.stringify(publicSettings).includes('test-secret'));
  const before=await readFile(saved,'utf8');
  const rejected=await fetch(`${base}/api/connections/${connectionId}/ha/connection`,{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({url:ha,token:'wrong'})});assert.equal(rejected.status,502);assert.equal(await readFile(saved,'utf8'),before);
  await page.getByRole('navigation').getByRole('button',{name:'Rooms & devices',exact:true}).click();await page.getByRole('button',{name:/^Test room/}).click();
  await page.getByRole('button',{name:'Add to this room',exact:true}).click();await page.locator('.device').getByRole('heading',{name:'Test light',exact:true}).waitFor();
  await page.getByRole('button',{name:'Show light controls',exact:true}).click();
  await page.getByRole('button',{name:'Turn off',exact:true}).click();await page.getByText('Off',{exact:true}).waitFor();
  await page.getByLabel('Brightness (%)',{exact:true}).fill('37');await page.getByRole('button',{name:'Apply brightness'}).click();await page.getByText('On · 37%',{exact:true}).waitFor();
  assert(commands.some(([url,body])=>url.endsWith('turn_off')));assert(commands.some(([,body])=>body.brightness_pct===37));
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  await page.screenshot({path:'build/webui-review/home-assistant-mobile.png',fullPage:true});
  const config=await(await fetch(`${base}/api/config`)).json();assert(config.rooms.some(r=>r.devices.some(d=>d.kind==='light' && d.integration.resource_id==='light.test')));
  assert.deepEqual(errors,[]);console.log('PASS: connection test/save, private credentials, rejected replacement, discovery, service control, brightness, room import, mobile layout');
}finally{if(browser)await browser.close();daemon.kill();await new Promise(r=>mock.close(r));await rm(directory,{recursive:true,force:true});}
