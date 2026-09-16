// Browser contract fixture; intercept every API and never contact a device.
const {chromium} = require('playwright');
const assert = require('node:assert/strict');
(async () => {
 const browser = await chromium.launch();
 try {
  const page = await browser.newPage({viewport:{width:1024,height:900}});
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  let state={installed:'v0.1.0-alpha.1',channel:'stable',automatic_checks:true,available:null,notes:'',phase:'idle',message:'No newer signed build is available on this channel.',can_install:false};
  let installed=false,restarted=false;
  await page.route('**/api/**',async route=>{
   const req=route.request(),path=new URL(req.url()).pathname;
   let result={};
   if(path==='/api/auth/status') result={authenticated:true,pairing:false,expires_in:0,tries_left:5,disabled:false};
   else if(path==='/api/config')result={schema_version:1};
   else if(path==='/api/updates')result=state;
   else if(path==='/api/updates/settings'){Object.assign(state,req.postDataJSON());}
   else if(path==='/api/updates/check'&&!req.postDataJSON().automatic){Object.assign(state,{available:'v0.1.0',kind:'combined',steps:1,can_install:true,message:'An update is available.',guidance:'Couch software and kernel install together with one restart.'});}
   else if(path==='/api/updates/install'){assert.equal(req.postDataJSON().version,'v0.1.0');installed=true;Object.assign(state,{can_install:false,phase:'ready',message:'Update verified and staged. Restart to apply it.'});}
   else if(path==='/api/updates/restart'){assert.equal(req.postDataJSON().confirm,true);restarted=true;}
   await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(result)});
  });
  await page.goto(process.env.COUCH_TEST_WEB_URL || 'http://127.0.0.1:8097/');
  await page.getByRole('button',{name:'Updates',exact:true}).click();
  await page.getByRole('heading',{name:'Software updates'}).waitFor();
  await page.getByLabel('Release channel').selectOption('alpha');
  await page.getByRole('button',{name:'Check now'}).click();
  await page.getByRole('heading',{name:'Couch software and kernel v0.1.0'}).waitFor();
  assert.equal(await page.getByText(/step [12] of 2/i).count(),0);
  await page.getByRole('button',{name:'Download & verify update'}).click();
  const restart=page.getByRole('button',{name:'Install & restart'});
  await restart.waitFor();assert(await restart.isDisabled());assert(installed);assert(!restarted);
  await page.getByText(/Both install together with one restart/).waitFor();
  await page.getByLabel('Restart the remote and apply this update').check();
  await page.screenshot({path:'build/updates-web-preview.png',fullPage:true});
  await restart.click();await page.waitForTimeout(100);assert(restarted);assert.deepEqual(errors,[]);
  Object.assign(state,{kind:'',steps:0,phase:'idle',available:null,boot_pending:true,message:''});
  await page.getByRole('heading',{name:'This update is not finished'}).waitFor();
  await page.getByRole('button',{name:'Find the rest of this update'}).waitFor();
  Object.assign(state,{boot_pending:false,boot_release:state.installed});
  await page.getByRole('heading',{name:'Update status',exact:true}).waitFor();
  assert.equal(await page.getByRole('button',{name:'Find the rest of this update'}).count(),0);
  console.log('Updates browser flow passed: combined update, one restart, legacy warning reconciliation.');
 } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exit(1);});
