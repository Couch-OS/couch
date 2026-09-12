// Exercise the actual thermostat Slint component using deterministic local data.
const {chromium}=require('playwright');
const assert=require('node:assert/strict');
const base=process.env.SITE_URL||'http://127.0.0.1:8098/';
(async()=>{
 assert.ok(['localhost','127.0.0.1'].includes(new URL(base).hostname),'Local fixtures only');
 const browser=await chromium.launch();
 try {
  const page=await browser.newPage({viewport:{width:480,height:800},deviceScaleFactor:1});
  const errors=[]; page.on('pageerror',e=>errors.push(e.message));
  page.on('request',r=>assert.equal(new URL(r.url()).origin,new URL(base).origin,'Do not contact real devices'));
  await page.goto(new URL('preview.html',base).href);
  await page.waitForFunction(()=>!!window.couchDemo?.documentation_screen);
  const wait=async expected=>page.waitForFunction(e=>Object.entries(e).every(([k,v])=>window.couchDemo.state()[k]===v),expected);
  const key=async name=>page.evaluate(n=>window.couchDemo.remote_button(n),name);
  await page.evaluate(()=>window.couchDemo.documentation_screen('thermostat-feedback'));
  await page.waitForTimeout(500);
  if(process.env.SCREENSHOTS){const fs=require('node:fs');fs.mkdirSync(process.env.SCREENSHOTS,{recursive:true});await page.screenshot({path:`${process.env.SCREENSHOTS}/thermostat-feedback.png`});}
  for(const name of ['thermostat','thermostat-modes','thermostat-range','thermostat-unavailable']){
   await page.evaluate(n=>window.couchDemo.documentation_screen(n),name);
   await wait({thermostat:true,thermostat_modes:name==='thermostat-modes',thermostat_range:name==='thermostat-range',thermostat_adjustable:name!=='thermostat-unavailable'});
   await page.waitForTimeout(500);
   if(process.env.SCREENSHOT_DIR) await page.locator('canvas:not(.page-snapshot)').screenshot({path:`${process.env.SCREENSHOT_DIR}/${name}.png`});
  }
  await key('volume-up'); await wait({thermostat_target:'—'});
  await page.evaluate(()=>window.couchDemo.documentation_screen('thermostat'));
  await wait({thermostat_target:'21°C'});
  await key('volume-up'); await wait({thermostat_target:'21.5°C'});
  await key('volume-down'); await wait({thermostat_target:'21°C'});
  await key('ok'); await wait({thermostat_modes:true});
  await key('down'); await key('down'); await key('ok');
  await wait({thermostat_mode:'Cool',thermostat_modes:false});
  // BottomTray retains its input shield for the 200 ms exit animation.
  // Logical state changes before the tray finishes moving; interact only once settled.
  await page.waitForTimeout(250);
  await page.mouse.click(240,600); await wait({thermostat_modes:true});
  await page.waitForTimeout(250);
  await page.mouse.click(240,120); await wait({thermostat_modes:false});
  await page.waitForTimeout(250);
  await key('back'); await wait({thermostat:false,room:0});
  await page.evaluate(()=>window.couchDemo.documentation_screen('thermostat-range'));
  await wait({thermostat_range:true});
  await key('volume-up'); await wait({thermostat_target:'20.5°C – 24.5°C'});
  await key('home'); await wait({thermostat:false,room:null});
  assert.deepEqual(errors,[]);
  console.log('PASS: thermostat target/range steps, unavailable state, physical mode selection, touch tray dismissal and navigation');
 } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exit(1)});
