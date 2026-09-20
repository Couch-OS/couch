// The Updates screen's copy, in a browser, against a mocked update service.
// Nothing here touches a real remote: every endpoint is intercepted, so the
// boot states this asserts (a kernel behind the software, an unfinished
// two-step update, a saved boot image to restore) never need a device.
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.COUCH_PLAYWRIGHT ?? '../../build/webui-review/node_modules/playwright/index.mjs');

const origin = process.env.COUCH_TEST_URL;
assert(origin, 'Set COUCH_TEST_URL to a disposable host daemon URL');
assert(['127.0.0.1', 'localhost'].includes(new URL(origin).hostname), 'Only loopback test servers are allowed');

const installed = 'v0.1.0-alpha.20260916.173';
// What `GET /api/updates` reports, as couch-updates fills it in: the kernel on
// the partition came from an earlier release, its commit is the one the boot
// payload's notes named, and the image it replaced is still saved.
const status = {
  installed, channel: 'alpha', available: null, kind: '', notes: '',
  phase: 'idle', message: 'No newer signed build is available on this channel.',
  can_install: false, automatic_checks: true,
  boot_kernel: '81d180fc19ec', boot_previous: true,
  boot_release: 'v0.1.0-alpha.20260916.170', boot_behind: true, boot_pending: false,
  guidance: '',
};
const calls = [];

const browser = await chromium.launch({headless: true});
const context = await browser.newContext({viewport: {width: 390, height: 844}});
const page = await context.newPage();
const errors = [];
page.on('pageerror', error => errors.push(String(error)));
await page.route('**/api/**', async route => {
  const request = route.request();
  const path = new URL(request.url()).pathname;
  const body = request.postDataJSON?.() ?? null;
  calls.push({path, method: request.method(), body});
  const json = value => route.fulfill({status: 200, contentType: 'application/json', body: JSON.stringify(value)});
  if (request.method() === 'GET' && path === '/api/auth/status') return json({authenticated: true, pairing: false, expires_in: 0, tries_left: 0, disabled: true});
  if (request.method() === 'GET' && path === '/api/config') return json({schema_version: 1, revision: 1, areas: [], rooms: [], scenes: [], activities: []});
  if (request.method() === 'GET' && path === '/api/updates') return json(status);
  if (request.method() === 'POST' && path === '/api/updates/check') return json(status);
  if (request.method() === 'POST' && path === '/api/updates/boot-rollback') {
    assert.deepEqual(body, {confirm: true}, 'writing a partition back needs an explicit confirmation');
    // What the service does: the save is consumed, and its record no longer
    // names a boot payload this updater wrote.
    status.boot_previous = false;
    status.boot_kernel = '';
    status.message = 'The previous boot image is back on the boot partition. Restart to run it.';
    return route.fulfill({status: 202, contentType: 'application/json', body: JSON.stringify({accepted: true})});
  }
  throw new Error(`Unexpected update request: ${request.method()} ${path}`);
});

try {
  await page.goto(origin);
  await page.getByRole('navigation').getByRole('button', {name: 'Updates', exact: true}).click();
  await page.getByRole('heading', {name: 'Software updates', exact: true}).waitFor();
  const card = page.locator('section.card').filter({has: page.getByRole('heading', {name: 'What is installed', exact: true})});

  // What is installed: one line each, and the state of the kernel in its own
  // sentence rather than appended to the version.
  await card.getByText(`Software ${installed}`, {exact: true}).waitFor();
  await card.getByText('Kernel and boot image v0.1.0-alpha.20260916.170', {exact: true}).waitFor();
  await card.getByText('This kernel is older than the installed software.', {exact: true}).waitFor();
  // The kernel commit stays available as a labelled detail, never inside the
  // sentence about versions.
  await card.getByText('Kernel source 81d180fc19ec', {exact: true}).waitFor();

  // An ordinary build carries no preview notice.
  const preview = page.getByRole('alert').filter({hasText: 'Protocol 3 preview build. Development remote only.'});
  assert.equal(await preview.count(), 0, 'an ordinary version is not a protocol 3 preview build');

  const copy = async () => (await card.innerText()).replace(/\s+/g, ' ');
  const installedCopy = await copy();
  // The screen cannot know whether a newer kernel has been published, and a
  // status panel is not the place for file paths or recovery mechanics.
  for (const forbidden of ['published', '/opt/couch', 'previous.img', 'recovery on its own', 'holding Back', 'boot partition']) {
    assert(!installedCopy.includes(forbidden), `"What is installed" should not say "${forbidden}": ${installedCopy}`);
  }

  // The restore control lives in its own card, under its own heading: a plain
  // button, a brief warning, and a pointer to the documentation instead of the
  // recovery procedure itself.
  const saved = page.locator('section.card').filter({has: page.getByRole('heading', {name: 'Saved boot image', exact: true})});
  await saved.getByText('Couch saved the boot image that the current one replaced.', {exact: true}).waitFor();
  await saved.getByText('Restoring puts it back without restarting the remote. Use Power afterwards to run it.', {exact: true}).waitFor();
  await saved.getByText('If the remote stops starting up, see the device recovery guide in the Couch documentation.', {exact: true}).waitFor();
  const restore = saved.getByRole('button', {name: 'Restore boot image', exact: true});
  assert(await restore.isDisabled(), 'restoring a boot image needs an explicit confirmation');
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Updates page overflows a phone viewport');
  await page.screenshot({path: process.env.COUCH_SCREENSHOT ?? 'build/webui-review/updates-installed-mobile.png', fullPage: true});
  await saved.getByLabel('Put the saved boot image back', {exact: true}).check();
  await restore.click();
  await page.getByText('The previous boot image is back on the boot partition. Restart to run it.').waitFor();
  assert.equal(calls.filter(call => call.path === '/api/updates/boot-rollback').length, 1);
  // With no saved image left the card goes away with its warning.
  await saved.waitFor({state: 'detached'});

  // An unfinished two-step update names the state, and still says nothing
  // about publishing or paths.
  status.boot_pending = true;
  status.guidance = 'A previous update still needs its boot image checked. Check for updates to finish it.';
  await card.getByText('The kernel that belongs with this software is not installed yet.', {exact: true}).waitFor();
  const pendingCopy = await copy();
  assert(!pendingCopy.includes('published'), pendingCopy);
  assert(!pendingCopy.includes('/opt/couch'), pendingCopy);

  // A kernel that came from the installed release says so in one sentence.
  status.boot_pending = false;
  status.boot_release = installed;
  status.boot_behind = false;
  status.guidance = '';
  await card.getByText('This kernel matches the installed software.', {exact: true}).waitFor();

  // A build with protocol 3 switched on is only ever signed as `.p3.dev`, and
  // the page says what that means in red, above everything else on it.
  status.installed = 'v0.1.0-alpha.20260916.174.p3.dev';
  await preview.waitFor();
  await card.getByText(`Software ${status.installed}`, {exact: true}).waitFor();
  const [red, green, blue] = (await preview.evaluate(node => getComputedStyle(node).color)).match(/\d+/g).map(Number);
  assert(red > 150 && green < 100 && blue < 100, `the preview notice is red, not rgb(${red}, ${green}, ${blue})`);
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'the preview notice overflows a phone viewport');
  await page.evaluate(() => scrollTo(0, 0));
  await page.screenshot({path: process.env.COUCH_PREVIEW_SCREENSHOT ?? 'build/webui-review/updates-preview-build-mobile.png'});
  // The next ordinary dev build takes the notice away again.
  status.installed = 'v0.1.0-alpha.20260916.175.dev';
  await preview.waitFor({state: 'detached'});
  status.installed = installed;

  await page.setViewportSize({width: 1280, height: 900});
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Updates page overflows a desktop viewport');
  await page.screenshot({path: process.env.COUCH_DESKTOP_SCREENSHOT ?? 'build/webui-review/updates-installed-desktop.png', fullPage: true});
  assert.deepEqual(errors, []);
  console.log('PASS: the Updates screen states the installed software and kernel in short sentences, keeps the kernel commit as a labelled detail, offers the boot-image restore with a brief warning and no recovery mechanics, and shows a red notice for a protocol 3 preview build and for nothing else.');
} finally {
  await browser.close();
}
