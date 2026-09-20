// Run against a disposable --no-auth host daemon after `tools/build-webui.sh --host`.
// Package endpoints are intercepted so this remains a browser contract test and
// never installs or trusts anything on the machine running it.
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.COUCH_PLAYWRIGHT ?? '../../build/webui-review/node_modules/playwright/index.mjs');

const origin = process.env.COUCH_TEST_URL;
assert(origin, 'Set COUCH_TEST_URL to a disposable host daemon URL');
assert(['127.0.0.1', 'localhost'].includes(new URL(origin).hostname), 'Only loopback test servers are allowed');

const catalog = {
  installed: [{
    id: 'denon', name: 'Denon AVR', version: '1.4.0',
    available_version: '1.5.0', status: {kind: 'ready'},
    description: 'Controls a network receiver.', repository: 'official-preview',
    connection_configured: true, can_rollback: true,
  }, {
    // The feed's signed metadata says the newer version needs a newer Couch.
    id: 'kodi', name: 'Kodi', version: '0.1.0',
    available_version: '0.2.0', update_installable: false, update_reason: 'Needs a newer Couch',
    status: {kind: 'installed'}, repository: 'official-preview',
    connection_configured: false, can_rollback: false,
  }],
  available: [{
    id: 'example-tv', name: 'Example TV', version: '0.3.0',
    description: 'A test integration from a signed catalog.', repository: 'official-preview',
  }, {
    id: 'future-tv', name: 'Future TV', version: '1.0.0',
    description: 'Built for the next Couch.', repository: 'official-preview',
    installable: false, reason: 'Needs a newer Couch',
  }],
  repositories: [{
    id: 'official-preview', name: 'Couch preview', url: 'https://packages.couch.example/preview',
    fingerprint: 'A1:B2:C3', trusted: true, official: true,
  }],
};
const calls = [];
const operations = new Map([['resume', {polls: 0, message: 'Resuming package operation…'}]]);
let sequence = 0;
let catalogReads = 0;
// A connection saved while Denon was built into the OS. The remote converts it
// by itself; here it has failed once (no internet) and waits for its backoff.
const waitingReason = "The package feed could not be read (Couch preview: cannot download repository index over HTTPS). Check the remote's internet connection.";
let legacy = 'waiting';
let configReads = 0;
// What GET /api/updates says is installed; the last step makes it a preview build.
let installedVersion = 'test';
function operation(message) {
  const id = `op-${++sequence}`;
  operations.set(id, {polls: 0, message});
  return {operation_id: id};
}

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
  if (request.method() === 'GET' && path === '/api/config') {
    configReads += 1;
    return json({schema_version: 1, revision: 7, areas: [], rooms: [], scenes: [], activities: []});
  }
  if (request.method() === 'POST' && path === '/api/updates/check') return json({});
  if (request.method() === 'GET' && path === '/api/updates') return json({installed: installedVersion, channel: 'stable', available: null, notes: '', phase: 'idle', message: '', can_install: false, automatic_checks: false});
  if (request.method() === 'GET' && path === '/api/integrations/catalog') {
    catalogReads += 1;
    if (catalogReads === 1) return route.fulfill({status: 409, contentType: 'application/json', body: JSON.stringify({error: 'package store is busy'})});
    return json(catalog);
  }
  if (request.method() === 'GET' && path === '/api/integrations/recovery') return json({recovery: {integrations_active: false, revision: 4, path: '/opt/couch/legacy-config.json', pending_path: null}});
  if (request.method() === 'GET' && path === '/api/integrations/legacy') {
    if (legacy === 'converted') return json({connections: [], working: false, retry_in_seconds: null});
    const working = legacy === 'retrying';
    if (working) legacy = 'converted';
    return json({working, retry_in_seconds: working ? null : 240, connections: [{
      id: 'living-avr', name: 'Living room receiver', kind: 'denon', package: 'denon', package_name: 'Denon',
      message: 'Needs the Denon package', reason: waitingReason,
    }]});
  }
  if (request.method() === 'POST' && path === '/api/integrations/legacy/retry') {
    assert.equal(body, null, 'trying again names nothing: the remote knows what is waiting');
    legacy = 'retrying';
    return route.fulfill({status: 202, contentType: 'application/json', body: JSON.stringify({retrying: true})});
  }
  if (request.method() === 'GET' && path === '/api/integrations/operations/current') return json({operation: {id: 'resume', state: 'running', phase: 'download', message: 'Resuming package operation…'}});
  if (request.method() === 'POST' && path === '/api/integrations/refresh') return json({operation_id: 'expired'});
  if (request.method() === 'POST' && /^\/api\/integrations\/(install|update|remove|rollback)$/.test(path)) return json(operation('Verifying package signature…'));
  if (request.method() === 'GET' && path.startsWith('/api/integrations/operations/')) {
    const id = path.split('/').pop();
    if (id === 'expired') return route.fulfill({status: 404, contentType: 'application/json', body: JSON.stringify({error: 'operation unavailable'})});
    const current = operations.get(id);
    current.polls += 1;
    return json(current.polls === 1 ? {state: 'running', phase: 'download', message: current.message} : {state: 'succeeded', message: 'Package operation complete.'});
  }
  if (request.method() === 'POST' && path === '/api/integrations/repositories') {
    assert.equal(body.public_key.includes('BEGIN PUBLIC KEY'), true);
    return json({pending_confirmation: {id: body.id, name: body.name, url: body.url, fingerprint: 'SHA256:TEST-KEY', algorithm: 'SHA-256 (PEM)'}});
  }
  if (request.method() === 'POST' && path === '/api/integrations/repositories/living-room/confirm') {
    assert.deepEqual(body, {fingerprint: 'SHA256:TEST-KEY'});
    catalog.repositories.push({id: 'living-room', name: 'Living room', url: 'https://packages.example.test/couch', fingerprint: 'SHA256:TEST-KEY', trusted: true, official: false});
    return json({trusted: true});
  }
  if (request.method() === 'DELETE' && path === '/api/integrations/repositories/living-room') {
    catalog.repositories = catalog.repositories.filter(repo => repo.id !== 'living-room');
    return json({removed: true});
  }
  throw new Error(`Unexpected integration request: ${request.method()} ${path}`);
});

const previewNotice = page.getByRole('alert').filter({hasText: 'Protocol 3 preview build. Development remote only.'});
try {
  await page.goto(origin);
  await page.getByRole('navigation').getByRole('button', {name: 'Integrations', exact: true}).click();
  await page.getByRole('heading', {name: 'Integrations', exact: true}).waitFor();
  await page.getByText('Saved integration configuration found').waitFor();
  assert.equal(await previewNotice.count(), 0, 'an ordinary build shows no protocol 3 preview notice');
  await page.getByRole('link', {name: 'Download saved integration configuration', exact: true}).waitFor();
  await page.getByText('Package operation complete.').waitFor();
  assert(catalogReads >= 2, 'a transient catalog lock reloads after the resumed operation completes');
  await page.getByRole('heading', {name: 'Denon AVR', exact: true}).waitFor();
  await page.getByText('Saved connection settings are retained.').waitFor();
  // The reversible pilot is gone: nothing offers to switch a connection by
  // hand, in either direction.
  assert.equal(await page.getByRole('heading', {name: 'Denon migration pilot', exact: true}).count(), 0);
  assert.equal(await page.getByRole('button', {name: 'Switch to Denon package', exact: true}).count(), 0);
  assert.equal(await page.getByRole('button', {name: 'Restore built-in control', exact: true}).count(), 0);
  // A connection from before the package says what it needs and why it is
  // still waiting, and the remote can be told to try now.
  await page.getByRole('heading', {name: 'Connections waiting for a package', exact: true}).waitFor();
  await page.getByRole('heading', {name: 'Living room receiver', exact: true}).waitFor();
  await page.getByText('Needs the Denon package', {exact: true}).waitFor();
  await page.getByText(`Last attempt: ${waitingReason}`, {exact: true}).waitFor();
  await page.getByText('The remote tries again by itself in about 4 min.', {exact: true}).waitFor();
  const readsBefore = configReads;
  await page.getByRole('button', {name: 'Try again', exact: true}).click();
  await page.getByRole('heading', {name: 'Connections waiting for a package', exact: true}).waitFor({state: 'detached'});
  assert.equal(calls.filter(c => c.method === 'POST' && c.path === '/api/integrations/legacy/retry').length, 1);
  assert(configReads > readsBefore, 'a converted connection reloads the configuration the page holds');
  // A package this Couch cannot run says so and cannot be asked for; the
  // daemon would refuse it before downloading anything. The rows beside it
  // are untouched.
  const card = name => page.locator('article.integration-card').filter({has: page.getByRole('heading', {name, exact: true})});
  await card('Future TV').getByText('Needs a newer Couch.', {exact: true}).waitFor();
  assert(await card('Future TV').getByRole('button', {name: 'Install', exact: true}).isDisabled(), 'a package that needs a newer Couch cannot be installed');
  assert(await card('Example TV').getByRole('button', {name: 'Install', exact: true}).isEnabled(), 'an installable package beside it still can');
  assert.equal(await card('Example TV').getByText('Needs a newer Couch').count(), 0);
  await card('Kodi').getByText('Version 0.2.0 is available. Needs a newer Couch.', {exact: true}).waitFor();
  assert(await card('Kodi').getByRole('button', {name: 'Update to 0.2.0', exact: true}).isDisabled(), 'an update that needs a newer Couch cannot be asked for');
  assert(await card('Kodi').getByRole('button', {name: 'Remove package', exact: true}).isEnabled(), 'the installed version can still be removed');
  await page.getByRole('button', {name: 'Update to 1.5.0', exact: true}).click();
  await page.getByText('Package operation complete.').waitFor();
  const update = calls.find(call => call.path === '/api/integrations/update');
  assert.deepEqual(update.body, {id: 'denon', repository: 'official-preview', preserve_connection_config: true});
  assert.equal('url' in update.body, false, 'package actions must use catalog IDs, never raw package URLs');

  await page.getByLabel('Repository ID').fill('living-room');
  await page.getByLabel('Repository name').fill('Living room');
  await page.getByLabel('Repository URL').fill('https://packages.example.test/couch');
  await page.getByLabel('Repository public key').fill('-----BEGIN PUBLIC KEY-----\nTEST\n-----END PUBLIC KEY-----');
  await page.getByRole('button', {name: 'Check public-key fingerprint', exact: true}).click();
  await page.getByText('SHA-256 (PEM): SHA256:TEST-KEY').waitFor();
  const trust = page.getByRole('button', {name: 'Trust repository', exact: true});
  assert(await trust.isDisabled(), 'trust requires an explicit fingerprint comparison');
  await page.getByLabel(/I compared this fingerprint/).check();
  await trust.click();
  await page.getByRole('heading', {name: 'Living room', exact: true}).waitFor();
  await page.getByText('Repository trusted. Refresh packages to load its catalog.').waitFor();
  await page.getByRole('button', {name: 'Remove repository', exact: true}).click();
  await page.getByRole('button', {name: 'Confirm delete', exact: true}).click();
  await page.getByRole('heading', {name: 'Living room', exact: true}).waitFor({state: 'detached'});

  await page.getByRole('button', {name: 'Refresh packages', exact: true}).click();
  await page.getByRole('alert').filter({hasText: 'The package operation is no longer available after the remote restarted.'}).waitFor();
  assert(calls.some(call => call.path === '/api/integrations/operations/current'), 'opening the page restores a daemon-owned operation');

  await card('Denon AVR').getByRole('button', {name: 'Remove package', exact: true}).click();
  await page.getByRole('button', {name: 'Confirm delete', exact: true}).click();
  await page.getByText('Package operation complete.').waitFor();
  assert.equal(calls.some(call => ['kodi', 'future-tv'].includes(call.body?.id)), false, 'nothing was asked of a package that cannot be installed');
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Integrations page overflows a phone viewport');
  await page.screenshot({path: process.env.COUCH_SCREENSHOT ?? 'build/webui-review/integrations-mobile.png', fullPage: true});
  await page.setViewportSize({width: 1280, height: 900});
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Integrations page overflows a desktop viewport');
  await page.screenshot({path: process.env.COUCH_DESKTOP_SCREENSHOT ?? 'build/webui-review/integrations-desktop.png', fullPage: true});
  // A build with protocol 3 switched on is only ever signed as `.p3.dev`; on
  // this page, where its packages are installed, it says so in red.
  installedVersion = 'v0.1.0-alpha.20260916.174.p3.dev';
  await page.getByRole('navigation').getByRole('button', {name: 'Updates', exact: true}).click();
  await page.getByRole('heading', {name: 'Software updates', exact: true}).waitFor();
  await page.getByRole('navigation').getByRole('button', {name: 'Integrations', exact: true}).click();
  await page.getByRole('heading', {name: 'Integrations', exact: true}).waitFor();
  await previewNotice.waitFor();
  assert.deepEqual(errors, []);
  console.log('PASS: integration catalog actions use IDs, a package that needs a newer Couch says so and cannot be installed or updated, package operations report progress, connection settings survive removal, a connection waiting for its package says why and can be retried, and custom repositories require key fingerprint confirmation.');
} finally {
  await browser.close();
}
