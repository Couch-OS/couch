// Real config mutations, mocked device transport. Never contacts a media device.
const {chromium}=require('playwright');
const {spawn}=require('node:child_process');
const fs=require('node:fs'),os=require('node:os'),path=require('node:path'),assert=require('node:assert/strict');
(async()=>{
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'couch-media-ui-'));
 const configPath=path.join(dir,'config.json');
 fs.writeFileSync(configPath,JSON.stringify({schema_version:1,connections:[{id:'core',name:'CoreELEC fixture',provider:{kind:'core-elec',host:'192.0.2.1',port:9090}},{id:'sonos',name:'Sonos fixture',provider:{kind:'sonos',host:'192.0.2.2'}}],rooms:[{id:'office',name:'Office',devices:[]}],areas:[],activities:[],scenes:[]}));
 const server=spawn('daemon/target/release/couch-confd',['--addr','127.0.0.1:18204','--config',configPath,'--no-auth','--www','web/couch-web/dist'],{stdio:'ignore'});
 const browser=await chromium.launch();
 try{
  for(let i=0;i<80;i++){try{if((await fetch('http://127.0.0.1:18204/api/config')).ok)break;}catch{}await new Promise(r=>setTimeout(r,50));}
  const page=await browser.newPage({viewport:{width:1280,height:1100}});const errors=[];page.on('pageerror',e=>errors.push(e.message));let enrolled=false,osActions=0,sonosCommands=0,ownsGroup=true;
  await page.route('**/api/updates**',r=>r.fulfill({json:{installed:'test',channel:'stable',automatic_checks:false,available:null,phase:'idle',message:'',can_install:false}}));
  await page.route('**/api/connections/core/kodi/**',r=>{assert.equal(r.request().method(),'GET');return r.fulfill({json:{web_port:8080,username:'kodi',password_set:false,http_control:false}});});
  await page.route('**/api/connections/core/coreelec/**',r=>{const request=r.request(),url=new URL(request.url());if(url.pathname.endsWith('/connection')&&request.method()==='PUT'){assert.equal(request.postDataJSON().private_key,'PRIVATE KEY FIXTURE');enrolled=true;}if(url.pathname.endsWith('/action')){assert.equal(request.postDataJSON().confirm,true);osActions++;}return r.fulfill({json:url.pathname.endsWith('/status')?{identity:{version:'21.2-Omega',architecture:'Amlogic-ng.arm'},service:{active:'active',sub:'running'}}:{configured:enrolled,user:'root',port:22}});});
  await page.route('**/api/connections/sonos/sonos/**',r=>{if(r.request().method()==='POST'){assert.equal(r.request().postDataJSON().command,'play');sonosCommands++;}return r.fulfill({json:{player:{uuid:'RINCON_FIXTURE',name:'Office Sonos'},coordinator:ownsGroup?'RINCON_FIXTURE':'RINCON_OTHER',transport:'PLAYING',volume:20,muted:false}});});
  await page.goto('http://127.0.0.1:18204/connections');
  await page.getByLabel('Connection type',{exact:true}).selectOption('core-elec');await page.getByRole('button',{name:'Save CoreELEC connection'}).last().waitFor();
  await page.getByLabel('Connection type',{exact:true}).selectOption('sonos');await page.getByRole('button',{name:'Save Sonos connection'}).last().waitFor();
  const core=page.locator('section.saved-connection').filter({has:page.getByRole('heading',{name:'CoreELEC fixture',exact:true})});
  await core.getByText('Enroll SSH access',{exact:true}).click();await core.getByLabel('Private SSH key',{exact:true}).fill('PRIVATE KEY FIXTURE');await core.getByLabel('Verified known_hosts entry',{exact:true}).fill('192.0.2.1 ssh-ed25519 FIXTURE');
  await core.getByRole('button',{name:'Verify & save SSH access'}).click();await core.getByText('SSH verified and saved',{exact:true}).waitFor();assert(enrolled);assert.equal(await core.getByLabel('Private SSH key',{exact:true}).inputValue(),'');
  const run=core.getByRole('button',{name:'Run OS action'});assert(await run.isDisabled());assert.equal(osActions,0);
  await core.getByRole('button',{name:'Refresh OS status'}).click();await core.getByText('CoreELEC 21.2-Omega · Amlogic-ng.arm · Kodi active (running)',{exact:true}).waitFor();
  await core.getByLabel('I understand this interrupts playback; powering off may require a physical power button to turn it on again.').check();await run.click();await core.getByText('Request accepted. Refresh status after the device restarts.',{exact:true}).waitFor();assert.equal(osActions,1);
  const sonos=page.locator('section.saved-connection').filter({has:page.getByRole('heading',{name:'Sonos fixture',exact:true})});assert(await sonos.getByRole('button',{name:'Play',exact:true}).isDisabled());
  await sonos.getByRole('button',{name:'Test connection / refresh'}).click();await sonos.getByText(/Office Sonos · PLAYING/).waitFor();await sonos.getByRole('button',{name:'Play',exact:true}).click();await page.waitForTimeout(100);assert.equal(sonosCommands,1);
  ownsGroup=false;await sonos.getByRole('button',{name:'Test connection / refresh'}).click();await sonos.getByText(/Playback coordinator: RINCON_OTHER/).waitFor();assert(await sonos.getByRole('button',{name:'Play',exact:true}).isDisabled());assert.equal(sonosCommands,1);
  await page.evaluate(()=>window.scrollTo(0,0));
  await page.screenshot({path:'build/media-connections-web.png',fullPage:true});
  await page.goto('http://127.0.0.1:18204/rooms/office');
  await page.getByLabel('From connection',{exact:true}).selectOption('core');
  await page.getByRole('button',{name:'Add to this room',exact:true}).click();
  await page.locator('li.device').filter({has:page.getByRole('heading',{name:'CoreELEC fixture',exact:true})}).waitFor();
  await page.getByLabel('From connection',{exact:true}).selectOption('sonos');
  await page.getByRole('button',{name:'Add to this room',exact:true}).click();
  await page.locator('li.device').filter({has:page.getByRole('heading',{name:'Sonos fixture',exact:true})}).waitFor();
  const saved=JSON.parse(fs.readFileSync(configPath));assert.equal(saved.rooms[0].devices.length,2);assert.equal(saved.rooms[0].devices.find(d=>d.integration.connection_id==='core').kind,'media-player');assert.equal(saved.rooms[0].devices.find(d=>d.integration.connection_id==='sonos').kind,'speaker');
  assert.deepEqual(errors,[]);console.log('CoreELEC enrollment/confirmation and Sonos coordinator control browser flows passed.');
 }finally{await browser.close();server.kill();fs.rmSync(dir,{recursive:true,force:true});}
})().catch(e=>{console.error(e);process.exit(1);});
