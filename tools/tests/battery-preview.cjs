// Exercise battery states and the Power setting in the production Slint canvas.
// Build with tools/build-preview.sh, serve site/, and set NODE_PATH to Playwright.
const {chromium}=require('playwright');
const assert=require('node:assert/strict');
const base=process.env.SITE_URL||'http://127.0.0.1:8098/';
const cases={
  'battery-low':{battery:19,battery_known:true,battery_charging:false,battery_percentage:false,settings:false},
  'battery-medium':{battery:20,battery_known:true,battery_charging:false,battery_percentage:false,settings:false},
  'battery-full':{battery:60,battery_known:true,battery_charging:false,battery_percentage:false,settings:false},
  'battery-charging':{battery:42,battery_known:true,battery_charging:true,battery_percentage:false,settings:false},
  'battery-unknown':{battery_known:false,battery_charging:false,battery_percentage:false,settings:false},
};
(async()=>{
  assert.ok(['localhost','127.0.0.1'].includes(new URL(base).hostname),'Use local fixtures only');
  const browser=await chromium.launch();
  try {
    const page=await browser.newPage({viewport:{width:480,height:800},deviceScaleFactor:1});
    const errors=[];page.on('pageerror',e=>errors.push(e.message));
    page.on('request',r=>assert.equal(new URL(r.url()).origin,new URL(base).origin,'Do not contact real devices'));
    await page.goto(new URL('preview.html',base).href);
    await page.waitForFunction(()=>!!window.couchDemo?.documentation_screen);
    const wait=expected=>page.waitForFunction(e=>Object.entries(e).every(([k,v])=>window.couchDemo.state()[k]===v),expected);
    for(const [name,expected] of Object.entries(cases)) {
      await page.evaluate(n=>window.couchDemo.documentation_screen(n),name);
      await wait(expected);
      if(process.env.SCREENSHOT_DIR) {
        const fs=require('node:fs');fs.mkdirSync(process.env.SCREENSHOT_DIR,{recursive:true});
        await page.locator('canvas').screenshot({path:`${process.env.SCREENSHOT_DIR}/${name}.png`});
      }
    }
    await page.evaluate(()=>window.couchDemo.documentation_screen('battery-percentage-setting'));
    await wait({battery:42,battery_known:true,battery_charging:false,battery_percentage:false,settings:true,settings_panel:6});
    await page.evaluate(()=>window.couchDemo.remote_button('right'));
    await wait({battery_percentage:true});
    await page.evaluate(()=>window.couchDemo.remote_button('left'));
    await wait({battery_percentage:false});
    assert.deepEqual(errors,[]);
    console.log('PASS: battery low/medium/full/charging/unknown states and Power percentage toggle');
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exit(1)});
