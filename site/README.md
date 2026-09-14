# Couch landing page

GitHub Pages publishes this directory after building the production Slint UI for WebAssembly. Run `tools/build-preview.sh`, then `python3 -m http.server 8098 --directory site` to preview it locally.

`preview.html` hosts the exact `ui/couch-gui/ui/app.slint` component tree at 480×800 inside a scaled iframe. That isolated frame fixes its pixel ratio to 1 to reproduce the remote’s physical framebuffer and avoid a Winit software-renderer startup mismatch on Retina screens; the rest of the page retains the browser’s normal pixel ratio. `demo.js` supplies the HA100-inspired bezel controls, loading state, simulated display sleep, and an automatic walkthrough. The frame carries no `src` until the demo is asked for: a poster with a **Load the demo** button stands in its place, and the page fetches the 12.8 MB WebAssembly build by itself only when the bezel scrolls into view on a viewport at least 850 px wide that has not asked for reduced motion or data saving. The rest of the page works with the demo unloaded. The walkthrough sends the same commands as the buttons, pauses offscreen/in background tabs, and stops permanently on the first interaction with the device. Reduced-motion users get a static, usable screen. `preview/src/demo.rs` supplies example rooms, lights, scenes, and a Kodi movie without linking any device clients or making network requests to devices. UI edits therefore flow into both the remote and website builds.

The interface, fonts, icons, list animations, overlays, and media screen are shared. The bezel ends at Back/Home/Power. In this browser adapter, tapping a row selects and activates it; the physical remote still uses selection plus OK. Mouse wheel, vertical swipes, and arrow keys navigate lists, and Enter remains available as OK. The tap adapter’s hit bounds match the fixed local fixture and are covered by browser tests. Page slides use browser canvas snapshots in place of the host’s framebuffer copies and honor reduced motion. Hardware input drivers, standby timing, and integration latency are not reproduced by the browser backend. This is a UI demonstration, not a hardware performance test. The movie fixture does not stream video; playback controls update local state. Canvas accessibility is limited; HTML controls and a changing screen summary provide a partial alternative.

Build requirements and asset attribution are in `preview/README.md`. Generated `wasm/` files stay out of Git. GitHub Actions builds and deploys them on changes to the site, preview, or production Slint sources/assets.

Browser regression check (Playwright installed in the existing review environment):

```sh
NODE_PATH=build/webui-review/node_modules node tools/tests/site-demo.cjs
# Or verify the published site:
SITE_URL=https://dangerouslaser.github.io/couch/ NODE_PATH=build/webui-review/node_modules node tools/tests/site-demo.cjs
```
