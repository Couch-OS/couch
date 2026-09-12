// Browser UI fixture: all Apple TV requests are mocked before navigation.
const {chromium}=require('playwright');
const {spawn}=require('node:child_process');
const fs=require('node:fs'),os=require('node:os'),path=require('node:path'),assert=require('node:assert/strict');
(async()=>{
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'couch-apple-metadata-'));
 const config=path.join(dir,'config.json');
 fs.writeFileSync(config,JSON.stringify({schema_version:1,connections:[{id:'apple',name:'Living room Apple TV',provider:{kind:'apple-tv'}}],rooms:[],areas:[],activities:[],scenes:[]}));
 const server=spawn('daemon/target/release/couch-confd',['--addr','127.0.0.1:18205','--config',config,'--no-auth','--www','web/couch-web/dist'],{stdio:'ignore'});
 const browser=await chromium.launch();
 try{
  for(let i=0;i<100;i++){try{if((await fetch('http://127.0.0.1:18205/api/config')).ok)break;}catch{}await new Promise(r=>setTimeout(r,50));}
  const page=await browser.newPage({viewport:{width:1280,height:1100}});const errors=[];page.on('pageerror',e=>errors.push(e.message));let paired=false,begins=0,finishes=0,controls=0;
  await page.route('**/api/updates**',r=>r.fulfill({json:{phase:'idle'}}));
  await page.route('**/api/connections/apple/appletv/**',r=>{
   const q=r.request(),url=new URL(q.url()),op=url.pathname.split('/').at(-1),metadata=url.pathname.includes('/metadata/');
   if(!metadata){assert.equal(q.method(),'GET');return r.fulfill({json:{paired:true,address:'192.0.2.1',port:49152}});}
   let value={};
   if(op==='connection'){if(q.method()==='DELETE')paired=false;value={paired,address:'192.0.2.1',port:7000};}
   else if(op==='discover')value=[{name:'Living room AirPlay',address:'192.0.2.1',port:7000}];
   else if(op==='pair-start'){assert.deepEqual(q.postDataJSON(),{address:'192.0.2.1',port:7000});begins++;value={token:'fixture-session',code_length:4};}
   else if(op==='pair-finish'){assert.deepEqual(q.postDataJSON(),{token:'fixture-session',code:'0123'});finishes++;paired=true;value={paired:true};}
   else if(op==='status')value={now_playing:{title:'The Long Way Home',state:'paused'}};
   else if(op==='pairing'){assert.equal(q.method(),'DELETE');value={cancelled:true};}
   else {controls++;throw new Error('Unexpected metadata control request');}
   return r.fulfill({json:value});
  });
  await page.goto('http://127.0.0.1:18205/connections');
  const section=page.locator('.apple-metadata');await section.getByRole('heading',{name:'Optional now playing'}).waitFor();
  await section.getByRole('button',{name:'Find TVs on this network'}).click();await section.getByRole('button',{name:'Living room AirPlay',exact:true}).click();
  assert.equal(await section.getByLabel('AirPlay port',{exact:true}).inputValue(),'7000');
  await section.getByRole('button',{name:'Show pairing code on TV'}).click();await section.getByLabel('TV pairing code',{exact:true}).fill('0123');
  await section.getByRole('button',{name:'Verify & save pairing'}).click();await section.getByText('Metadata paired. Reopen this TV on the remote to see now playing.',{exact:true}).waitFor();
  assert.equal(await section.getByLabel('TV pairing code',{exact:true}).count(),0);
  await section.getByRole('button',{name:'Test connection',exact:true}).click();await section.getByText('The Long Way Home · paused',{exact:true}).waitFor();
  await page.evaluate(()=>window.scrollTo(0,0));await page.screenshot({path:'build/apple-metadata-web.png',fullPage:true});
  await section.getByRole('button',{name:'Remove metadata pairing'}).click();await section.getByText('Metadata pairing removed. Companion controls are unchanged.',{exact:true}).waitFor();assert(!paired);
  await section.getByRole('button',{name:'Show pairing code on TV'}).click();await section.getByRole('button',{name:'Cancel pairing'}).click();await section.getByText('Pairing cancelled.',{exact:true}).waitFor();
  assert.equal(begins,2);assert.equal(finishes,1);assert.equal(controls,0);assert.deepEqual(errors,[]);console.log('Separate AirPlay discovery/PIN/status/remove/cancel browser flow passed.');
 }finally{await browser.close();server.kill();fs.rmSync(dir,{recursive:true,force:true});}
})().catch(e=>{console.error(e);process.exit(1);});
