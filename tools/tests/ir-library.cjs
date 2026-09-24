// Browser regression uses real revisioned device mutations in an isolated config.
// Catalog/network/transmission endpoints are mocked: no command reaches hardware.
// NODE_PATH=build/webui-review/node_modules node tools/tests/ir-library.cjs
const {chromium}=require('playwright');
const {spawn}=require('node:child_process');
const fs=require('node:fs');
const os=require('node:os');
const path=require('node:path');
const assert=require('node:assert/strict');
(async()=>{
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'couch-device-ir-browser-'));
 const configPath=path.join(dir,'config.json');
 const config={schema_version:1,revision:12,connections:[{id:'tv-network',name:'Network TV',provider:{kind:'android-tv'}},{id:'legacy-blaster',name:'Old infrared connection',provider:{kind:'ir'}}],areas:[],rooms:[{id:'office',name:'Office',devices:[{id:'tv',name:'Office TV',kind:'tv',integration:{via:'connection',connection_id:'tv-network',resource_id:''}},{id:'old-tv',name:'Legacy TV',kind:'tv',integration:{via:'ir',codeset:'shared'}}]}],activities:[],scenes:[]};
 fs.mkdirSync(path.join(dir,'ir'));fs.writeFileSync(path.join(dir,'ir/shared.codeset'),'toggle nec 4 8\n');fs.writeFileSync(configPath,JSON.stringify(config));
 const server=spawn('daemon/target/release/couch-confd',['--addr','127.0.0.1:18196','--config',configPath,'--no-auth','--www','web/couch-web/dist'],{stdio:'ignore'});
 const browser=await chromium.launch();
 try {
  for(let i=0;i<80;i++){try{const r=await fetch('http://127.0.0.1:18196/api/config');if(r.ok)break;}catch{}await new Promise(r=>setTimeout(r,50));}
  const page=await browser.newPage({viewport:{width:1440,height:1000}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
  let catalogReads=0,transmissions=0;const writes=[];
  const extras=Array.from({length:5500},(_,i)=>({id:`extra-${i}`,brand:`Brand ${Math.floor(i/100)}`,device_type:'TV',model:`Model ${i}`,supported_commands:8}));
  await page.route('**/api/ir/**',r=>{
   const pathname=new URL(r.request().url()).pathname;
   if(pathname==='/api/ir/catalog'){catalogReads++;return r.fulfill({json:{source:{name:'Fixture library',license:'CC0'},codesets:[{id:'lg-tv',brand:'LG',device_type:'TV',model:'Example TV',supported_commands:3},...extras]}});}
   if(pathname==='/api/ir/catalog/lg-tv')return r.fulfill({json:{commands:[{name:'Power',supported:true,code:'Power nec 4 8'},{name:'On',supported:true,code:'On nec 4 9'},{name:'Vol_up',supported:true,code:'Vol_up nec 4 2'},{name:'Unsupported',supported:false,reason:'Unsupported protocol fixture'}]}});
   if(pathname==='/api/ir/import')return r.fulfill({json:{commands:[{name:'Imported Off',supported:true,code:'Off raw 38000 9000,4500,560,560'}]}});
   if(pathname.endsWith('/test')){transmissions++;assert.equal(r.request().postDataJSON().command,'volume-up');return r.fulfill({json:{sent:true,physically_verified:false}});}
   return r.continue();
  });
  await page.route('**/api/rooms/**',r=>{if(['PUT','POST','DELETE'].includes(r.request().method())){assert.ok(r.request().headers()['if-match']);writes.push(r.request().postDataJSON());}return r.continue();});
  await page.goto('http://127.0.0.1:18196/connections');
  assert.equal(await page.getByLabel('Connection type',{exact:true}).locator('option[value="ir"]').count(),0);
  assert.equal(await page.getByRole('heading',{name:'Old infrared connection',exact:true}).count(),0);
  await page.goto('http://127.0.0.1:18196/rooms/office');
  let tv=page.locator('li.device').filter({has:page.getByRole('heading',{name:'Office TV',exact:true})});
  await tv.getByRole('button',{name:'Add IR commands',exact:true}).click();
  await tv.getByLabel('IR brand',{exact:true}).selectOption('LG');
  assert.equal(await tv.getByLabel('IR model',{exact:true}).locator('option').count(),2);
  await tv.getByLabel('IR library device type',{exact:true}).selectOption('TV');
  await tv.getByLabel('Search models',{exact:true}).fill('Example');
  await tv.getByLabel('IR model',{exact:true}).selectOption('lg-tv');
  await tv.getByText('Unavailable: Unsupported protocol fixture',{exact:true}).waitFor();
  await tv.getByRole('button',{name:'Assign matching functions',exact:true}).click();
  await tv.getByRole('button',{name:'Test Volume up',exact:true}).waitFor();
  assert.equal(await tv.getByRole('button',{name:'Test Volume up',exact:true}).isDisabled(),true);
  await tv.getByText('Import your own remote codes',{exact:true}).click();
  await tv.getByLabel('IR import contents',{exact:true}).fill('Filetype: IR signals file');
  await tv.getByRole('button',{name:'Preview imported commands',exact:true}).click();
  await tv.getByLabel('Assign Imported Off (1)',{exact:true}).selectOption('power-off');
  assert.equal(writes.length,0);assert.equal(transmissions,0);
  await tv.getByRole('button',{name:'Save IR commands',exact:true}).click();
  await tv.getByRole('button',{name:'Test Volume up',exact:true}).waitFor({state:'visible'});
  await page.waitForFunction(()=>[...document.querySelectorAll('button[aria-label="Test Volume up"]')].some(b=>!b.disabled));
  let saved=JSON.parse(fs.readFileSync(configPath));let device=saved.rooms[0].devices.find(d=>d.id==='tv');
  assert.deepEqual(device.integration,config.rooms[0].devices[0].integration);assert.ok(device.ir.codeset);const first=device.ir.codeset;
  assert.match(fs.readFileSync(path.join(dir,`ir/${first}.codeset`),'utf8'),/power-off raw 38000 9000,4500,560,560/);
  await tv.getByRole('button',{name:'Test Volume up',exact:true}).click();
  await tv.getByText('IR command sent. Check that the device responded.',{exact:true}).waitFor();assert.equal(transmissions,1);
  await tv.getByRole('button',{name:'Remove Power on',exact:true}).click();
  assert.equal(await tv.getByRole('button',{name:'Test Volume up',exact:true}).isDisabled(),true);
  await tv.getByRole('button',{name:'Save IR commands',exact:true}).click();
  await page.waitForFunction(()=>[...document.querySelectorAll('button[aria-label="Test Volume up"]')].some(b=>!b.disabled));
  saved=JSON.parse(fs.readFileSync(configPath));device=saved.rooms[0].devices.find(d=>d.id==='tv');assert.notEqual(device.ir.codeset,first);
  assert.match(fs.readFileSync(path.join(dir,`ir/${first}.codeset`),'utf8'),/power-on/,'Historical/shared snapshot is unchanged');
  await page.setViewportSize({width:390,height:844});
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
  await page.screenshot({path:process.env.SCREENSHOT||'/private/tmp/couch-device-ir-mobile.png',fullPage:true});
  await tv.getByRole('button',{name:'Remove all IR commands',exact:true}).click();
  await tv.getByText('No IR commands assigned. Choose a remote below to get started.',{exact:true}).waitFor();
  saved=JSON.parse(fs.readFileSync(configPath));device=saved.rooms[0].devices.find(d=>d.id==='tv');assert.equal(device.ir,undefined);assert.deepEqual(device.integration,config.rooms[0].devices[0].integration);
  await page.getByLabel('From connection',{exact:true}).selectOption('manual-ir');
  const manual=page.locator('.device-picker');
  await manual.getByLabel('Device name',{exact:true}).fill('Bedroom TV');
  await manual.getByLabel('IR brand',{exact:true}).selectOption('LG');await manual.getByLabel('IR model',{exact:true}).selectOption('lg-tv');
  await manual.getByRole('button',{name:'Assign matching functions',exact:true}).click();
  await manual.getByRole('button',{name:'Add device to room',exact:true}).click();
  await page.getByRole('heading',{name:'Bedroom TV',exact:true}).waitFor();
  saved=JSON.parse(fs.readFileSync(configPath));const added=saved.rooms[0].devices.find(d=>d.name==='Bedroom TV');assert.equal(added.integration.via,'none');assert.ok(added.ir.codeset);assert.equal(saved.connections.length,2,'No new infrared connection');
  const legacy=page.locator('li.device').filter({has:page.getByRole('heading',{name:'Legacy TV',exact:true})});
  await legacy.getByRole('button',{name:'Manage IR commands',exact:true}).click();
  await legacy.getByRole('button',{name:'Test Power toggle',exact:true}).waitFor();
  assert.equal(transmissions,1,'Only the explicit Test click sent anything');assert.equal(catalogReads,1);assert.deepEqual(errors,[]);
  console.log('PASS: real atomic device IR attach/edit/remove/create, network preservation, immutable snapshots, legacy reads, matching assignments, raw import, guarded explicit testing, hidden IR connection, catalog cache and mobile layout.');
 }finally{await browser.close();server.kill();fs.rmSync(dir,{recursive:true,force:true});}
})().catch(e=>{console.error(e);process.exit(1)});
