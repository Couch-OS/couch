// Generate documentation from the production Slint WASM canvas, then check every guide.
// Run against a local server only; output is generated build data, never device data.
const {chromium}=require('playwright');
const assert=require('node:assert/strict');
const fs=require('node:fs/promises');
const path=require('node:path');
const {execFileSync}=require('node:child_process');
const {createHash}=require('node:crypto');
const base=process.env.SITE_URL||'http://127.0.0.1:8098/';
const generate=!process.argv.includes('--check-only');
const root=path.resolve(__dirname,'../..');
const output=path.join(root,'site/usage/screenshots');
const fixtures={thermostat:{thermostat:true,thermostat_modes:false},"thermostat-modes":{thermostat:true,thermostat_modes:true},home:{room:null,player:false},room:{room:0,player:false},brightness:{room:0,brightness:true,level:61},scenes:{room:0,level:10},kodi:{player:true,panel:0},chapters:{player:true,panel:1},"android-tv":{tv:true,android_tv:true,tv_panel:0},"android-apps":{tv:true,android_tv:true,tv_panel:2},webos:{tv:true,android_tv:false,tv_panel:0},"webos-inputs":{tv:true,android_tv:false,tv_panel:1},"apple-tv":{tv:true,apple_tv:true,tv_panel:0},"apple-apps":{tv:true,apple_tv:true,tv_panel:2},infrared:{tv:true,infrared:true,tv_panel:0},"infrared-commands":{tv:true,infrared:true,tv_panel:2}};
(async()=>{
 const browser=await chromium.launch();
 const errors=[];
 try {
  if(generate){
   assert.ok(['localhost','127.0.0.1'].includes(new URL(base).hostname),'Generate only against a local build');
   await fs.mkdir(output,{recursive:true});
   const images=[];
   for(const [name,expected] of Object.entries(fixtures)){
    const page=await browser.newPage({viewport:{width:480,height:800},deviceScaleFactor:1,reducedMotion:'reduce'});
    page.on('pageerror',e=>errors.push(e.message));
    page.on('request',r=>assert.equal(new URL(r.url()).origin,new URL(base).origin,'Local fixtures only'));
    await page.goto(new URL('preview.html',base).href);
    await page.waitForFunction(()=>!!window.couchDemo?.documentation_screen,null,{timeout:120000});
    await page.evaluate(name=>window.couchDemo.documentation_screen(name),name);
    await page.waitForFunction(expected=>Object.entries(expected).every(([key,value])=>window.couchDemo.state()[key]===value),expected);
    // Finish component entrance animations after the fixture callbacks settle.
    await page.waitForTimeout(500);
    const canvas=page.locator('canvas:not(.page-snapshot)');
    assert.deepEqual(await canvas.evaluate(c=>[c.width,c.height]),[480,800]);
    const file=path.join(output,`${name}.png`);
    await canvas.screenshot({path:file});
    const bytes=await fs.readFile(file);
    assert.ok(bytes.length>15000,`${name} should contain rendered content`);
    images.push({name,width:480,height:800,sha256:createHash('sha256').update(bytes).digest('hex')});
    await page.close();
   }
   assert.equal(new Set(images.map(i=>i.sha256)).size,images.length,'All fixtures must render distinct screens');
   await fs.writeFile(path.join(output,'manifest.json'),JSON.stringify({schema:1,source_commit:execFileSync('git',['rev-parse','HEAD'],{cwd:root,encoding:'utf8'}).trim(),renderer:'Production ui/couch-gui/ui/app.slint via preview/ WASM',fixture_source:'preview/src/demo.rs',images},null,2)+'\n');
  }
  const page=await browser.newPage();
  page.on('pageerror',e=>errors.push(e.message));
  const failures=[];
  page.on('response',r=>{if(r.status()>=400)failures.push(`${r.status()} ${r.url()}`)});
  for(const slug of ['','rooms.html','lights.html','home-assistant.html','kodi.html','android-tv.html','webos.html','apple-tv.html','infrared.html']){
   for(const width of [1440,390,320]){
    await page.setViewportSize({width,height:1000});
    await page.goto(new URL(`usage/${slug}`,base).href);
    await page.locator('footer').scrollIntoViewIfNeeded();
    await page.waitForFunction(()=>[...document.images].every(i=>i.complete&&i.naturalWidth===480));
    assert.equal(await page.locator('h1').count(),1);
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,`${slug} overflow at ${width}`);
    if(slug)assert.ok(await page.locator('tbody tr').count()>=5);
    for(const href of await page.locator('a[href]').evaluateAll(as=>as.map(a=>a.href))){
     const url=new URL(href);
     if(url.origin===new URL(base).origin&&!url.hash){const response=await page.request.get(href);assert.ok(response.ok(),`Broken guide link ${href}`);}
    }
   }
  }
  for(const width of [1440,390,320]){
   await page.setViewportSize({width,height:1000});
   await page.goto(new URL('integrations.html',base).href);
   assert.ok(await page.locator('.integration-cards article').count()>=14,'every shipping provider has an integration card');
   assert.equal(await page.getByRole('navigation',{name:'Main navigation'}).getByRole('link',{name:'Integrations',exact:true}).count(),1);
   assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,`integrations overflow at ${width}`);
   await page.goto(base);
   assert.equal(await page.locator('.integration-inner li').count(),6);
   assert.equal(await page.getByRole('link',{name:'And more',exact:false}).getAttribute('href'),'integrations.html');
   assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,`landing navigation overflow at ${width}`);
  }
  await page.setViewportSize({width:1440,height:1100});
  await page.goto(new URL('usage/lights.html',base).href);
  await page.screenshot({path:path.join(require('node:os').tmpdir(),'couch-usage-lights.png'),fullPage:true});
  assert.deepEqual(errors,[]);assert.deepEqual(failures,[]);
  console.log(`PASS: ${generate?'sixteen production Slint screenshots generated; ':''}nine usage pages, integrations directory, button tables, images, local links and desktop/mobile layouts.`);
 } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exit(1)});
