// Exercise the real Slint canvas through on-page physical controls and touch.
const {chromium}=require('playwright');
const assert=require('node:assert/strict');
const tmp=require('node:os').tmpdir();
// The 12.8 MB WASM demo is not fetched on page load. It arrives when the poster
// button is pressed, or when the device scrolls into view on a wide viewport
// without reduced motion or data saving. Wait for whichever applies.
async function startDemo(page,{click=false}={}){
 if(click) await page.locator('#demo-load').click();
 let frame;
 for(let attempt=0;attempt<200 && !frame;attempt++){
  frame=page.frames().find(f=>f.url().includes('preview.html'));
  if(!frame) await page.waitForTimeout(50);
 }
 assert.ok(frame,'the demo frame loads once the demo is requested');
 await frame.waitForFunction(()=>!!window.couchDemo,{},{timeout:120000});
 return frame;
}
async function verifyNativeDisplayScale() {
 // Playwright's deviceScaleFactor emulation does not reproduce native
 // ResizeObserver physical sizes. Set Chromium's display scale instead.
 for (const density of [1, 1.5, 2]) {
  const browser = await chromium.launch({args:[`--force-device-scale-factor=${density}`]});
  try {
   const page = await browser.newPage({viewport:null,reducedMotion:'reduce'});
   await page.goto(process.env.SITE_URL || 'http://127.0.0.1:8098/');
   assert.equal(await page.locator('#demo-poster').isVisible(),true,'the demo waits behind its poster');
   const frame = await startDemo(page,{click:true});
   await page.waitForTimeout(300);
   assert.equal(await page.evaluate(()=>devicePixelRatio),density,'real parent display density');
   const framebuffer = () => frame.locator('#canvas').evaluate(c=>({width:c.width,height:c.height}));
   assert.deepEqual(await framebuffer(),{width:480,height:800},`framebuffer at native DPR ${density}`);
   await page.locator('#slint-demo').scrollIntoViewIfNeeded();
   const box = await page.locator('#slint-demo').boundingBox();
   await page.mouse.click(box.x + 200*box.width/480,box.y + 200*box.height/800);
   await page.waitForTimeout(550);
   assert.equal(await frame.evaluate(()=>couchDemo.state().room),0,'touch coordinates match displayed room');
   await page.locator('[data-remote="back"]').click();
   await page.waitForTimeout(550);
   assert.equal(await frame.evaluate(()=>couchDemo.state().room),null);
   assert.deepEqual(await framebuffer(),{width:480,height:800},'transitions retain framebuffer dimensions');
  } finally { await browser.close(); }
 }
}
(async()=>{await verifyNativeDisplayScale();const browser=await chromium.launch();try{
 const page=await browser.newPage({viewport:{width:1440,height:1100}}),errors=[],requests=[];
 page.on('pageerror',e=>errors.push(e.message));page.on('request',r=>requests.push(r.url()));
 await page.goto(process.env.SITE_URL||'http://127.0.0.1:8098/');
 const frame=await startDemo(page);
 await page.locator('#demo-loading').waitFor({state:'hidden'});await page.waitForTimeout(300);
 assert.equal(await page.locator('#demo-poster').isVisible(),false,'a wide viewport loads the demo on its own');
 const state=()=>frame.evaluate(()=>window.couchDemo.state());
 const button=async(name)=>{const control=page.locator(`[data-remote="${name}"]`);if(await control.count()){await control.click();}else{await frame.locator('canvas').focus();await page.keyboard.press({ok:'Enter',up:'ArrowUp',down:'ArrowDown','volume-up':'+','channel-up':'PageUp'}[name]);}await page.waitForTimeout(550);};
 const touch=async(x,y)=>{const box=await page.locator('#slint-demo').boundingBox();await page.mouse.click(box.x+x*box.width/480,box.y+y*box.height/800);await page.waitForTimeout(550);};
 assert.equal((await state()).room,null);
 await touch(200,200);assert.equal((await state()).room,0,'tap opens a room without a physical OK key');assert.equal(await page.locator('.device-shell').getAttribute('data-autoplay'),'stopped');
 const initial=(await state()).level;
 await touch(200,120);assert.equal((await state()).level,0,'tap toggles a light in the cropped browser preview');
 await button('ok');assert.equal((await state()).level,56);
 await button('volume-up');assert.equal((await state()).level,61);assert.equal((await state()).brightness,true);
 await page.screenshot({path:`${tmp}/couch-slint-brightness.png`});
 await button('back');assert.equal((await state()).room,null);assert.equal((await state()).brightness,false,'navigation clears toast');
 await button('ok');await button('channel-up');assert.equal((await state()).level,100,'channel recalls next scene');
 await button('down');await button('down');await button('ok');assert.equal((await state()).player,true);
 await touch(240,600);assert.equal((await state()).paused,true,'touch pauses movie');
 await touch(95,705);assert.equal((await state()).panel,1,'chapter sheet');
 await touch(160,320);assert.equal((await state()).panel,0,'chapter selection');
 await page.screenshot({path:`${tmp}/couch-slint-cinema.png`});
 const back=page.locator('[data-remote="back"]');await back.hover();await page.mouse.down();await page.waitForTimeout(650);await page.mouse.up();await page.waitForTimeout(250);
 assert.equal((await state()).player,false,'long Back exits media');assert.equal((await state()).room,0);
 await button('back');
 for(let i=0;i<5;i++)await button('down');
 await button('ok');assert.equal((await state()).room,5,'D-pad scroll reaches offscreen room');
 await button('home');assert.equal((await state()).room,null);
 await button('power');assert.equal(await page.locator('#demo-sleep').isVisible(),true);await button('power');assert.equal(await page.locator('#demo-sleep').isVisible(),false);
 for(const width of [1440,390,320]){await page.setViewportSize({width,height:1100});await page.waitForTimeout(250);assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,'page overflow '+width);const size=await frame.locator('canvas').evaluate(e=>({width:e.clientWidth,height:e.clientHeight}));assert.deepEqual(size,{width:480,height:800});await page.screenshot({path:`${tmp}/couch-slint-${width}.png`,fullPage:true});}
 const autoPage=await browser.newPage({viewport:{width:1440,height:1100}});
 autoPage.on('pageerror',e=>errors.push(e.message));
 await autoPage.goto(process.env.SITE_URL||'http://127.0.0.1:8098/');
 const autoFrame=await startDemo(autoPage);
 await autoFrame.waitForFunction(()=>window.couchDemo?.state().player,{},{timeout:45000});
 assert.equal(await autoPage.locator('.device-shell').getAttribute('data-autoplay'),'running');
 const autoBox=await autoPage.locator('#slint-demo').boundingBox();
 await autoPage.mouse.click(autoBox.x+20,autoBox.y+20);
 await autoPage.waitForTimeout(300);
 assert.equal(await autoPage.locator('.device-shell').getAttribute('data-autoplay'),'stopped');
 const stopped=await autoFrame.evaluate(()=>window.couchDemo.state());
 await autoPage.waitForTimeout(3000);
 assert.deepEqual(await autoFrame.evaluate(()=>window.couchDemo.state()),stopped,'tour remains stopped after touch');
 await autoPage.close();
 const quiet=await browser.newPage({viewport:{width:1440,height:1100},reducedMotion:'reduce'});
 const quietRequests=[];
 quiet.on('request',r=>quietRequests.push(r.url()));
 await quiet.goto(process.env.SITE_URL||'http://127.0.0.1:8098/');
 await quiet.waitForTimeout(1500);
 assert.equal(await quiet.locator('#demo-poster').isVisible(),true,'reduced motion never loads the demo uninvited');
 assert.deepEqual(quietRequests.filter(url=>/preview\.html|couch_preview/.test(url)),[],'no WASM before the demo is requested');
 assert.equal(await quiet.locator('.copy').first().isVisible(),true,'the page works with the demo unloaded');
 await startDemo(quiet,{click:true});
 await quiet.locator('#demo-loading').waitFor({state:'hidden',timeout:120000});await quiet.waitForTimeout(3000);
 assert.notEqual(await quiet.locator('.device-shell').getAttribute('data-autoplay'),'running');
 await quiet.close();
 assert.deepEqual(errors,[]);assert.ok(requests.every(url=>url.startsWith(new URL(page.url()).origin)),'demo contacts no external devices');
 console.log('PASS: production Slint canvas, room selection/scroll, light toggles, brightness toast, scenes, cinema/pause/chapters, long Back, Home, sleep/wake, responsive sizing, automatic tour/cancel, reduced motion holds the demo behind its poster, click to load, no external requests.');
}finally{await browser.close()}})().catch(e=>{console.error(e);process.exit(1)});
