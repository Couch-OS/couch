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
  areas: [], rooms: [], scenes: [], activities: [],
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
  ],
};
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
  ],
};
const calls = [];
let volumeState = 'reading';
let migrationHasV2Parity = false;

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
    if (request.method() === 'GET' && path === '/api/connections/receiver-v2/plugin/status') return json(status());
    if (request.method() === 'POST' && path === '/api/connections/receiver-v2/plugin/typed-action') {
      assert.deepEqual(body, {action: 'set_volume_db', tenths: -345});
      volumeState = 'minimum';
      return json({});
    }
    if (request.method() === 'GET' && path === '/api/integrations/catalog') {
      return json({installed: [], available: [], repositories: []});
    }
    if (request.method() === 'GET' && path === '/api/integrations/recovery') {
      return json({recovery: null});
    }
    if (request.method() === 'GET' && path === '/api/integrations/operations/current') {
      return json({operation: null});
    }
    if (request.method() === 'GET' && path === '/api/integrations/migrations/denon') {
      return json({
        revision: 12, package_available: true, connections: [],
        supports_volume_db: migrationHasV2Parity,
        supports_absolute_volume: migrationHasV2Parity,
      });
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

  await page.getByRole('navigation').getByRole('button', {name: 'Integrations', exact: true}).click();
  const warning = 'Preview limitation: this package does not provide full dB reading and absolute-volume support. Keep built-in control if you need either feature.';
  await page.getByText(warning, {exact: true}).waitFor();

  migrationHasV2Parity = true;
  const parityPage = await context.newPage();
  const parityErrors = [];
  parityPage.on('pageerror', error => parityErrors.push(String(error)));
  await mockApi(parityPage);
  await parityPage.goto(origin);
  await parityPage.getByRole('navigation').getByRole('button', {name: 'Integrations', exact: true}).click();
  await parityPage.getByRole('heading', {name: 'Denon migration pilot', exact: true}).waitFor();
  assert.equal(await parityPage.getByText(warning, {exact: true}).count(), 0, 'the migration warning clears only when both v2 parity flags are true');
  await parityPage.close();
  assert.deepEqual(parityErrors, []);
  assert.deepEqual(errors, []);
  console.log('PASS: v2 dB controls validate declared bounds, post one typed action then refresh status, preserve v1 controls, and gate the Denon migration warning on v2 parity.');
} finally {
  await browser.close();
}
