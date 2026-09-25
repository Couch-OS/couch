// Browser contract for declared native integration controls. It runs against a
// static bundle and intercepts every API call, so no package, receiver, or
// daemon state is touched.
import assert from 'node:assert/strict';

const { chromium } = await import(
  process.env.COUCH_PLAYWRIGHT ?? '../../build/webui-review/node_modules/playwright/index.mjs',
);

const origin = process.env.COUCH_TEST_URL;
assert(origin, 'Set COUCH_TEST_URL to the static Couch web bundle');
assert(['127.0.0.1', 'localhost'].includes(new URL(origin).hostname), 'Only loopback test servers are allowed');

const v2Actions = [{
  action: 'set_volume_db', min_tenths: -800, max_tenths: 180, step_tenths: 5,
}];
const v2Presentation = [
  {kind: 'status_text', label: 'Current dB', field: 'volume_db'},
  {kind: 'volume_db_control', label: 'Volume'},
];
const v1Presentation = [{kind: 'command_group', title: 'Volume', commands: ['volume-up']}];
const config = {
  schema_version: 1,
  revision: 12,
  areas: [], rooms: [{
    id: 'living-room', name: 'Living room', devices: [{
      id: 'tv', name: 'Living room TV', kind: 'tv',
      integration: {via: 'connection', connection_id: 'webos', resource_id: ''},
    }],
  }], scenes: [], activities: [],
  connections: [
    {
      id: 'receiver-v2', name: 'Denon package v2',
      provider: {
        kind: 'plugin', id: 'denon-v2', label: 'Denon package v2',
        capabilities: [], actions: v2Actions, supports_inputs: false,
        presentation: v2Presentation,
      },
    },
    {
      id: 'receiver-v1', name: 'Denon package v1',
      provider: {
        kind: 'plugin', id: 'denon-v1', label: 'Denon package v1',
        capabilities: [{id: 'volume-up', label: 'Volume up'}], actions: [],
        supports_inputs: false, presentation: v1Presentation,
      },
    },
    {
      id: 'bedroom-tv', name: 'Bedroom TV package',
      provider: {
        kind: 'plugin', id: 'tv-settings', label: 'TV package',
        capabilities: [], actions: [], supports_inputs: false, presentation: [],
      },
    },
    {
      id: 'webos', name: 'LG webOS',
      provider: {
        kind: 'plugin', id: 'webos', label: 'LG webOS',
        capabilities: [{id: 'power-off', label: 'Power off'}], actions: [],
        supports_inputs: true, supports_apps: true, presentation: [],
      },
    },
  ],
};
const tvSettings = [
  {id: 'host', label: 'Device address', kind: 'text', required: true},
  {id: 'port', label: 'Port', kind: 'integer', default: 8080},
];
// What the daemon answers a refused save with. `error` is the sentence every
// client shows; `code` and `reason` are beside it for a client that can do
// better. Only a protocol 3 package gives a reason, and no released Couch
// loads one yet, so the first shape is the only one a remote sends today.
const refusals = [
  {status: 400, body: {error: 'Invalid integration settings or package', code: 'invalid'}},
  {status: 400, body: {
    error: 'The port must not be 0', code: 'invalid',
    reason: {kind: 'invalid_setting', field: 'port', text: 'The port must not be 0'},
  }},
  {status: 400, body: {
    error: 'Pair this TV again', code: 'unpaired',
    reason: {kind: 'message', text: 'Pair this TV again'},
  }},
];
const catalog = {
  integrations: [
    {
      id: 'denon-v2', label: 'Denon package v2', capabilities: [], actions: v2Actions,
      settings: [], supports_inputs: false, presentation: v2Presentation,
    },
    {
      id: 'denon-v1', label: 'Denon package v1',
      capabilities: [{id: 'volume-up', label: 'Volume up'}], actions: [],
      settings: [], supports_inputs: false, presentation: v1Presentation,
    },
    {
      id: 'tv-settings', label: 'TV package', capabilities: [], actions: [],
      settings: tvSettings, supports_inputs: false, presentation: [],
    },
    {
      id: 'webos', label: 'LG webOS',
      capabilities: [{id: 'power-off', label: 'Power off'}], actions: [], settings: [],
      supports_inputs: true, supports_apps: true, presentation: [],
    },
  ],
};
const calls = [];
let volumeState = 'reading';

function status() {
  if (volumeState === 'reading') return {volume_db: {kind: 'reading', tenths: -345}};
  if (volumeState === 'minimum') return {volume_db: {kind: 'minimum'}};
  return {};
}

async function mockApi(page) {
  await page.route('**/api/**', async route => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const body = request.postDataJSON?.() ?? null;
    calls.push({method: request.method(), path, body});
    const json = value => route.fulfill({
      status: 200, contentType: 'application/json', body: JSON.stringify(value),
    });

    if (request.method() === 'GET' && path === '/api/auth/status') {
      return json({authenticated: true, pairing: false, expires_in: 0, tries_left: 0, disabled: true});
    }
    if (request.method() === 'GET' && path === '/api/config') return json(config);
    if (request.method() === 'POST' && path === '/api/updates/check') return json({});
    if (request.method() === 'GET' && path === '/api/updates') {
      return json({installed: 'test', channel: 'preview', available: null, notes: '', phase: 'idle', message: '', can_install: false, automatic_checks: false});
    }
    if (request.method() === 'GET' && path === '/api/integrations') return json(catalog);
    if (request.method() === 'GET' && /^\/api\/connections\/receiver-v[12]\/plugin\/settings$/.test(path)) {
      return json({configured: true, settings: {}, secrets: []});
    }
    if (path === '/api/connections/bedroom-tv/plugin/settings') {
      if (request.method() === 'GET') return json({configured: true, settings: {host: 'tv.local', port: 8080}, secrets: []});
      const refusal = refusals.shift();
      if (!refusal) return json({configured: true, settings: body, secrets: []});
      return route.fulfill({
        status: refusal.status, contentType: 'application/json', body: JSON.stringify(refusal.body),
      });
    }
    if (request.method() === 'GET' && path === '/api/connections/webos/plugin/settings') {
      return json({configured: true, settings: {}, secrets: []});
    }
    if (request.method() === 'GET' && path === '/api/connections/webos/plugin/apps') {
      return json([{id: 'netflix', name: 'Netflix'}, {id: 'youtube', name: 'YouTube'}]);
    }
    if (request.method() === 'POST' && path === '/api/connections/webos/plugin/action') {
      assert.deepEqual(body, {command: 'app:youtube'});
      return json({accepted: true});
    }
    if (request.method() === 'GET' && path === '/api/connections/webos/plugin/status') {
      return json({on: true});
    }
    if (request.method() === 'GET' && path === '/api/rooms/living-room/devices/tv/ir') {
      return json({text: '', codeset: ''});
    }
    if (request.method() === 'GET' && path === '/api/ir/catalog') {
      return json({source: {name: 'Fixture IR library', license: 'MIT'}, codesets: []});
    }
    if (request.method() === 'GET' && path === '/api/connections/receiver-v2/plugin/status') return json(status());
    if (request.method() === 'POST' && path === '/api/connections/receiver-v2/plugin/typed-action') {
      assert.deepEqual(body, {action: 'set_volume_db', tenths: -345});
      volumeState = 'minimum';
      return json({});
    }
    throw new Error(`Unexpected component request: ${request.method()} ${path}`);
  });
}

async function openConnection(page, name) {
  await page.getByRole('navigation').getByRole('button', {name: 'Connections', exact: true}).click();
  await page.getByRole('heading', {name: 'Connections', exact: true}).waitFor();
  await page.locator('.destination').filter({hasText: name}).click();
  await page.getByRole('heading', {name: 'Integration controls', exact: true}).waitFor();
}

const browser = await chromium.launch({headless: true});
const context = await browser.newContext({viewport: {width: 390, height: 844}});
const page = await context.newPage();
const errors = [];
page.on('pageerror', error => errors.push(String(error)));
await mockApi(page);

try {
  await page.goto(origin);
  await openConnection(page, 'Denon package v2');

  const target = page.getByLabel('Target volume (dB)', {exact: true});
  const setVolume = page.getByRole('button', {name: 'Set volume', exact: true});
  assert(await setVolume.isDisabled(), 'an empty dB target is never sent');
  await target.fill('-34.4');
  assert(await setVolume.isDisabled(), 'an off-step dB target is never sent');
  await target.fill('-80.5');
  assert(await setVolume.isDisabled(), 'an out-of-range dB target is never sent');
  await target.fill('-34.5');
  assert.equal(await setVolume.isDisabled(), false, 'a declared in-range dB step is enabled');

  await page.getByRole('button', {name: 'Refresh status', exact: true}).click();
  await page.getByText('-34.5 dB', {exact: true}).first().waitFor();
  await setVolume.click();
  await page.getByText('Command sent. Status refreshed.', {exact: true}).waitFor();
  await page.getByText('Minimum', {exact: true}).first().waitFor();
  const typedAt = calls.findIndex(call => call.path.endsWith('/typed-action'));
  assert(typedAt >= 0, 'the v2 component posts its declared typed action');
  assert.deepEqual(calls[typedAt], {
    method: 'POST', path: '/api/connections/receiver-v2/plugin/typed-action',
    body: {action: 'set_volume_db', tenths: -345},
  });
  assert.deepEqual(calls[typedAt + 1], {
    method: 'GET', path: '/api/connections/receiver-v2/plugin/status', body: null,
  }, 'a successful typed write is followed by one status read');

  volumeState = 'unavailable';
  await page.getByRole('button', {name: 'Refresh status', exact: true}).click();
  await page.getByText('Unavailable', {exact: true}).first().waitFor();
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'dB controls overflow a phone viewport');
  await page.screenshot({path: process.env.COUCH_COMPONENTS_MOBILE ?? 'build/webui-review/plugin-components-mobile.png', fullPage: true});
  await page.setViewportSize({width: 1280, height: 900});
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'dB controls overflow a desktop viewport');
  await page.screenshot({path: process.env.COUCH_COMPONENTS_DESKTOP ?? 'build/webui-review/plugin-components-desktop.png', fullPage: true});

  await openConnection(page, 'Denon package v1');
  assert.equal(await page.getByLabel('Target volume (dB)', {exact: true}).count(), 0, 'v1 manifests do not render a v2 dB target');
  assert.equal(await page.getByRole('button', {name: 'Set volume', exact: true}).count(), 0, 'v1 manifests cannot invoke typed dB actions');
  assert.equal(calls.filter(call => call.path.endsWith('/typed-action')).length, 1, 'only the declared v2 action reached the API');

  await openConnection(page, 'LG webOS');
  await page.getByRole('heading', {name: 'Power control', exact: true}).waitFor();
  await page.getByText('Assign Power toggle below to switch', {exact: false}).waitFor();
  await page.getByRole('button', {name: 'Add IR commands', exact: true}).click();
  await page.getByRole('heading', {name: 'Choose remote codes', exact: true}).waitFor();
  assert(calls.some(call => call.method === 'GET' && call.path === '/api/rooms/living-room/devices/tv/ir'), 'WebOS power setup opens the assigned device IR editor');

  await page.getByRole('button', {name: 'Refresh apps', exact: true}).click();
  const appPicker = page.getByLabel('Integration app', {exact: true});
  await page.getByText('2 apps available.', {exact: true}).waitFor();
  assert.equal(await appPicker.locator('option[value="youtube"]').count(), 1);
  await appPicker.selectOption('youtube');
  await page.getByText('Command sent. Status refreshed.', {exact: true}).waitFor();
  assert(calls.some(call => call.method === 'GET' && call.path === '/api/connections/webos/plugin/apps'), 'the browser asks protocol 5 for apps');
  assert(calls.some(call => call.method === 'POST' && call.path === '/api/connections/webos/plugin/action' && call.body?.command === 'app:youtube'), 'choosing an app launches it through the package');

  // A refused save. The form's own line has always shown the sentence; a
  // reason that names a setting marks that setting and puts the package's
  // words under it.
  await page.setViewportSize({width: 390, height: 844});
  await openConnection(page, 'Bedroom TV package');
  const port = page.getByLabel('Port', {exact: true});
  const host = page.getByLabel('Device address · required', {exact: true});
  const save = page.getByRole('button', {name: 'Save private settings', exact: true});
  const formLine = page.locator('section.card').filter({hasText: 'Integration settings'}).getByRole('status');
  assert.equal(await port.inputValue(), '8080');
  await port.fill('0');
  await save.click();
  await page.getByText('Invalid integration settings or package', {exact: true}).waitFor();
  assert.equal(await page.locator('.field-error').count(), 0, 'a refusal without a reason marks no setting');
  assert.equal(await port.getAttribute('aria-invalid'), null);

  await save.click();
  const complaint = page.locator('#plugin-setting-port-error');
  await complaint.waitFor();
  assert.equal(await complaint.textContent(), 'The port must not be 0');
  assert.equal(await complaint.getAttribute('role'), 'alert');
  assert.equal(await port.getAttribute('aria-invalid'), 'true');
  assert.equal(await port.getAttribute('aria-describedby'), 'plugin-setting-port-error');
  assert.equal(await host.getAttribute('aria-invalid'), null, 'only the blamed setting is marked');
  assert.equal(await page.locator('.field-error').count(), 1);
  assert.equal(await formLine.textContent(), 'Not saved. Check Port.');
  // The words sit directly under the control they are about.
  const [portBox, complaintBox, hostBox] = await Promise.all([port.boundingBox(), complaint.boundingBox(), host.boundingBox()]);
  assert(complaintBox.y >= portBox.y + portBox.height, 'the reason is under the Port control');
  assert(complaintBox.y - (portBox.y + portBox.height) < 24, 'the reason is next to the Port control');
  assert(complaintBox.y > hostBox.y + hostBox.height, 'the reason is not under another setting');
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'a field reason overflows a phone viewport');
  // The card alone, in a viewport tall enough to hold it: a capture that has
  // to scroll puts the sticky bars over the form.
  await page.setViewportSize({width: 390, height: 1400});
  await page.locator('section.card').filter({hasText: 'Integration settings'}).screenshot({path: process.env.COUCH_COMPONENTS_FIELD_ERROR ?? 'build/webui-review/plugin-components-field-error.png'});

  // Editing the setting takes the words away: they were about the old value.
  await port.fill('8081');
  assert.equal(await page.locator('.field-error').count(), 0);
  assert.equal(await port.getAttribute('aria-invalid'), null);

  // A reason about no setting is the form's line, like any other refusal.
  await save.click();
  await page.getByText('Pair this TV again', {exact: true}).waitFor();
  assert.equal(await page.locator('.field-error').count(), 0);

  await save.click();
  await page.getByText('Private settings saved.', {exact: true}).waitFor();
  const saves = calls.filter(call => call.method === 'POST' && call.path === '/api/connections/bedroom-tv/plugin/settings');
  assert.deepEqual(saves.map(call => call.body), [
    {host: 'tv.local', port: 0}, {host: 'tv.local', port: 0},
    {host: 'tv.local', port: 8081}, {host: 'tv.local', port: 8081},
  ], 'a refused save is never retried by the page');

  assert.deepEqual(errors, []);
  console.log('PASS: plugin controls cover typed dB actions, WebOS apps and core IR power setup, v1 compatibility, and field-specific save refusals.');
} finally {
  await browser.close();
}
