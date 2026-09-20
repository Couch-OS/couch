// Browser contract for the children of one packaged connection: the picker
// that lists a bridge's lamps, "Add all shown" and its confirmation, the
// badge on a child that has stopped being listed, and the light panel over
// the child routes. Protocol 3 is unreleased - no package a shipped build
// accepts declares a kind of child - so this runs against a fixture.
//
// Like plugin-components.mjs it runs against a static bundle and intercepts
// every API call, so no package, bridge or daemon state is touched.
import assert from 'node:assert/strict';

const { chromium } = await import(
  process.env.COUCH_PLAYWRIGHT ?? '../../build/webui-review/node_modules/playwright/index.mjs',
);

const origin = process.env.COUCH_TEST_URL;
assert(origin, 'Set COUCH_TEST_URL to the static Couch web bundle');
assert(['127.0.0.1', 'localhost'].includes(new URL(origin).hostname), 'Only loopback test servers are allowed');

// What the package declares, copied onto the connection: three kinds of child.
const kinds = [
  {
    kind: 'light', label: 'Light', device_kind: 'light', component: 'light',
    capabilities: [{id: 'on', label: 'Turn on'}, {id: 'off', label: 'Turn off'}],
    actions: [{action: 'set_light'}],
  },
  {
    kind: 'blind', label: 'Blind', device_kind: 'blind', component: 'cover',
    capabilities: [{id: 'open', label: 'Open'}, {id: 'close', label: 'Close'}, {id: 'stop', label: 'Stop'}],
    actions: [{action: 'set_cover'}],
  },
  {
    kind: 'scene', label: 'Scene', device_kind: 'other', component: 'scene',
    capabilities: [{id: 'on', label: 'Recall'}],
  },
];

// What the bridge lists. `assigned` is filled in from the configuration on
// every request, exactly as the daemon does it.
const listed = [
  {id: 'lamp/1', kind: 'light', name: 'Desk', room_hint: 'Study', light: {dimmable: true, mirek: [153, 500]}},
  {id: 'lamp/2', kind: 'light', name: 'Reading', room_hint: 'Kitchen', light: {dimmable: true}},
  {id: 'lamp/3', kind: 'light', name: 'Counter', room_hint: 'Kitchen', light: {dimmable: true, mirek: [153, 500]}},
  {id: 'blind/1', kind: 'blind', name: 'Kitchen blind', room_hint: 'Kitchen', cover: {position: true, stop: true}},
  {id: 'scene/1', kind: 'scene', name: 'Relax', room_hint: 'Kitchen'},
  {id: 'lamp/4', kind: 'light', name: 'Hall lamp'},
];

const plugin = (id, label, children) => ({
  kind: 'plugin', id, label, capabilities: [], actions: [],
  supports_inputs: false, presentation: [], children,
});

let config = {
  schema_version: 1,
  revision: 3,
  areas: [], scenes: [], activities: [],
  connections: [
    {id: 'bridge', name: 'Bridge package', provider: plugin('bridge-pkg', 'Bridge package', kinds)},
    // A package that declares children but whose remote answers 404, and one
    // that declares none at all: the picker must behave as it always has for
    // the second.
    {id: 'stale', name: 'Old bridge', provider: plugin('stale-pkg', 'Old bridge', kinds)},
    {id: 'amp', name: 'Amplifier', provider: plugin('amp-pkg', 'Amplifier', [])},
  ],
  rooms: [{
    id: 'kitchen', name: 'Kitchen', devices: [
      {
        id: 'kitchen-counter', name: 'Counter lamp', kind: 'light',
        integration: {
          via: 'connection', connection_id: 'bridge', resource_id: 'lamp/3',
          child: {kind: 'light', light: {dimmable: true, mirek: [153, 500]}},
        },
      },
      // Saved from a child the bridge has stopped listing. Never removed for
      // the person; badged where it lives and in the picker.
      {
        id: 'kitchen-corner', name: 'Corner lamp', kind: 'light',
        integration: {
          via: 'connection', connection_id: 'bridge', resource_id: 'lamp/9',
          child: {kind: 'light', light: {dimmable: true}},
        },
      },
    ],
  }],
};

const calls = [];
const paths = () => calls.map(call => `${call.method} ${call.path}`);
const posts = path => calls.filter(call => call.method === 'POST' && call.path === path);

// What the daemon stamps a saved child with: the snapshot and the device kind
// come from the package's listing, never from the request.
function stamp(resource) {
  const child = listed.find(one => one.id === resource);
  const kind = kinds.find(one => one.kind === child.kind);
  const snapshot = {kind: child.kind};
  if (child.light) snapshot.light = child.light;
  if (child.cover) snapshot.cover = child.cover;
  return {kind: kind.device_kind, child: snapshot};
}

function listing() {
  const assignments = new Map();
  for (const room of config.rooms) {
    for (const device of room.devices) {
      const at = device.integration;
      if (at?.connection_id === 'bridge' && at.resource_id) {
        assignments.set(at.resource_id, {room: room.id, device: device.id});
      }
    }
  }
  for (const scene of config.scenes) {
    if (scene.resource?.connection_id === 'bridge') {
      assignments.set(scene.resource.resource_id, {scene: scene.id});
    }
  }
  const children = listed.map(child => ({...child, assigned: assignments.get(child.id) ?? null}));
  const missing = [...assignments]
    .filter(([id]) => !listed.some(child => child.id === id))
    .map(([id, at]) => ({id, kind: 'light', name: 'Corner lamp', assigned: at}));
  return {kinds, children, missing, fetched_ms: 0};
}

// Refusals the mock hands out once each, in order.
let refuseResource = null;
let busyStatusReads = 0;
let lightState = {light: {on: false, brightness: 10}};

async function mockApi(page) {
  await page.route('**/api/**', async route => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const body = request.postDataJSON?.() ?? null;
    calls.push({method: request.method(), path, body});
    const json = (value, status = 200) => route.fulfill({
      status, contentType: 'application/json', body: JSON.stringify(value),
    });
    const house = () => json(config);
    const refused = (status, error) => json({error, problems: []}, status);

    if (request.method() === 'GET' && path === '/api/auth/status') {
      return json({authenticated: true, pairing: false, expires_in: 0, tries_left: 0, disabled: true});
    }
    if (request.method() === 'GET' && path === '/api/config') return house();
    if (request.method() === 'POST' && path === '/api/updates/check') return json({});
    if (request.method() === 'GET' && path === '/api/updates') {
      return json({installed: 'test', channel: 'preview', available: null, notes: '', phase: 'idle', message: '', can_install: false, automatic_checks: false});
    }
    if (request.method() === 'GET' && path === '/api/integrations') return json({integrations: []});
    if (request.method() === 'GET' && path === '/api/remote/device') return json({});

    // A package whose remote does not list devices at all.
    if (path.startsWith('/api/connections/stale/plugin/children')) {
      return refused(404, 'This integration does not list devices');
    }
    if (path === '/api/connections/bridge/plugin/children'
        || path === '/api/connections/bridge/plugin/children/refresh') {
      return json(listing());
    }
    // One child of the bridge: `…/children/<id…>/<verb>`.
    const child = path.match(/^\/api\/connections\/bridge\/plugin\/children\/(.+)\/([a-z-]+)$/);
    if (child) {
      const [, resource, verb] = child;
      if (verb === 'status') {
        // Busy is the remote reading this connection for somebody else. It is
        // retried quietly: no error may appear on the page.
        if (busyStatusReads > 0) {
          busyStatusReads -= 1;
          return json({error: 'The integration request queue is full', code: 'busy'}, 503);
        }
        return json(lightState);
      }
      if (verb === 'action') {
        assert.deepEqual(Object.keys(body), ['command']);
        lightState = {light: {on: body.command === 'on', brightness: lightState.light.brightness}};
        return json({accepted: true});
      }
      if (verb === 'typed-action') {
        assert.equal(resource, 'lamp/3');
        // A write that answers with the state it left. The page shows that
        // and reads nothing afterwards.
        if (body.action === 'set_light' && body.brightness !== undefined) {
          lightState = {light: {on: body.brightness > 0, brightness: body.brightness}};
        }
        return json(lightState);
      }
    }

    if (request.method() === 'POST' && path === '/api/rooms/kitchen/devices') {
      const resource = body.integration.resource_id;
      if (refuseResource === resource) {
        refuseResource = null;
        return json({
          error: 'The device refused the request', code: 'rejected',
          reason: {kind: 'message', text: 'That blind is not answering'},
        }, 502);
      }
      const {kind, child: snapshot} = stamp(resource);
      config = {
        ...config, revision: config.revision + 1,
        rooms: config.rooms.map(room => room.id !== 'kitchen' ? room : {
          ...room,
          devices: [...room.devices, {
            id: `kitchen-${resource.replace(/[^a-z0-9]+/g, '-')}`,
            name: body.name, kind,
            // The daemon stamps this; the browser never sends it.
            integration: {...body.integration, child: snapshot},
          }],
        }),
      };
      return house();
    }
    if (request.method() === 'POST' && path === '/api/scenes') {
      config = {
        ...config, revision: config.revision + 1,
        scenes: [...config.scenes, {
          id: 'kitchen-relax', name: body.name, steps: [], rooms: body.rooms,
          resource: {...body.resource, kind: 'scene'},
        }],
      };
      return house();
    }
    const removal = path.match(/^\/api\/rooms\/kitchen\/devices\/(.+)$/);
    if (request.method() === 'DELETE' && removal) {
      config = {
        ...config, revision: config.revision + 1,
        rooms: config.rooms.map(room => room.id !== 'kitchen' ? room : {
          ...room, devices: room.devices.filter(device => device.id !== removal[1]),
        }),
      };
      return house();
    }
    throw new Error(`Unexpected children request: ${request.method()} ${path}`);
  });
}

const browser = await chromium.launch({headless: true});
const context = await browser.newContext({viewport: {width: 360, height: 900}});
const page = await context.newPage();
const errors = [];
page.on('pageerror', error => errors.push(String(error)));
await mockApi(page);

const source = () => page.getByLabel('From connection', {exact: true});
const typeBox = () => page.getByLabel('Device type from the integration', {exact: true});
const roomBox = () => page.getByLabel('Room from the integration', {exact: true});
const search = () => page.getByLabel('Search devices', {exact: true});
const cards = () => page.locator('.discovered-devices:not(.gone) .discovered-device strong');
const addAll = () => page.locator('button.add-all');
const dialog = () => page.getByRole('dialog');
const pickerLine = () => page.locator('section.device-picker > p[role=status]');

async function openKitchen() {
  await page.getByRole('navigation').getByRole('button', {name: 'Rooms & devices', exact: true}).click();
  await page.getByRole('button', {name: /Kitchen.*Open/}).click();
  await page.getByRole('heading', {name: 'Add to this room', exact: true}).waitFor();
}

try {
  await page.goto(origin);
  await openKitchen();

  // A device saved from a child the bridge no longer lists is badged where it
  // lives. Nothing was asked of the bridge to know that but the one listing.
  await page.getByText('No longer reported by Bridge package').first().waitFor();
  assert.equal(
    calls.filter(call => call.path === '/api/connections/bridge/plugin/children').length, 1,
    'a room of two lamps of one bridge asks it for one listing',
  );

  // Nothing is listed until a source is chosen.
  assert.equal(await cards().count(), 0);
  await source().selectOption('bridge');
  await page.getByText('6 devices found. Choose the ones that belong in this room.').waitFor();

  // The kinds come from what the package declares; the rooms from what it
  // reported, and the one with this room's name is preselected.
  assert.deepEqual(await typeBox().locator('option').allTextContents(), ['All types', 'Light', 'Blind', 'Scene']);
  assert.deepEqual(await roomBox().locator('option').allTextContents(), ['All rooms', 'Kitchen', 'Study']);
  assert.equal(await roomBox().inputValue(), 'Kitchen', 'the bridge room named like this room is preselected');
  assert.deepEqual(await cards().allTextContents(), ['Reading', 'Counter', 'Kitchen blind', 'Relax']);

  // Already in the house: said where, and never offered twice.
  const counter = page.locator('.discovered-device').filter({hasText: 'Counter'});
  await counter.getByText('Already in Kitchen · Counter lamp').waitFor();
  assert(await counter.getByRole('button').isDisabled(), 'an assigned child cannot be added again');

  // The search covers the name and the room the bridge reports.
  await search().fill('blind');
  assert.deepEqual(await cards().allTextContents(), ['Kitchen blind']);
  await search().fill('read');
  assert.deepEqual(await cards().allTextContents(), ['Reading']);
  await search().fill('');

  // Adding one. The body carries the connection and the resource and nothing
  // else: what the child is, is the package's to say.
  await page.locator('.discovered-device').filter({hasText: 'Reading'})
    .getByRole('button', {name: 'Add to this room', exact: true}).click();
  await page.getByRole('heading', {name: 'Reading', exact: true}).waitFor();
  const [added] = posts('/api/rooms/kitchen/devices');
  assert.deepEqual(added.body, {
    name: 'Reading',
    integration: {via: 'connection', connection_id: 'bridge', resource_id: 'lamp/2'},
  });
  assert.equal('child' in added.body.integration, false, 'the browser never describes a child');
  assert.equal('kind' in added.body, false, 'the browser never picks the device kind of a child');

  // A package scene goes to the room's Scenes button, not to its devices.
  await page.locator('.discovered-device').filter({hasText: 'Relax'})
    .getByRole('button', {name: 'Add to this room', exact: true}).click();
  await page.locator('.room-scenes').getByText('Relax', {exact: true}).waitFor();
  assert.deepEqual(posts('/api/scenes')[0].body, {
    name: 'Relax', rooms: ['kitchen'],
    resource: {connection_id: 'bridge', resource_id: 'scene/1'},
  });

  // OWNER DECISION: "Add all shown" adds exactly what is on screen, minus what
  // is already in the house and minus the scenes, and only after a
  // confirmation that names every one of them.
  await roomBox().selectOption('');
  await page.getByText('Desk', {exact: true}).waitFor();
  assert.deepEqual(
    await cards().allTextContents(),
    ['Desk', 'Reading', 'Counter', 'Kitchen blind', 'Relax', 'Hall lamp'],
  );
  assert.equal(await addAll().textContent(), 'Add all shown (3)');

  const before = posts('/api/rooms/kitchen/devices').length;
  await addAll().click();
  await dialog().waitFor();
  assert.equal(await page.getByRole('heading', {name: 'Add these 3 devices to Kitchen?'}).count(), 1);
  assert.deepEqual(await dialog().locator('.row-title').allTextContents(), ['Desk', 'Kitchen blind', 'Hall lamp']);
  assert.equal(posts('/api/rooms/kitchen/devices').length, before, 'showing the question adds nothing');
  await page.screenshot({path: process.env.COUCH_CHILDREN_CONFIRM ?? 'build/webui-review/plugin-children-confirm.png', fullPage: true});

  // Keyboard only: the question takes the focus, Escape says no, and nothing
  // was added.
  assert(await page.evaluate(() => document.activeElement?.closest('.confirm-add') !== null),
    'the confirmation takes the focus when it appears');
  await page.keyboard.press('Escape');
  await dialog().waitFor({state: 'detached'});
  assert.equal(posts('/api/rooms/kitchen/devices').length, before, 'cancelling adds nothing');

  // ...and Cancel does the same.
  await addAll().click();
  await dialog().waitFor();
  await page.getByRole('button', {name: 'Cancel', exact: true}).click();
  await dialog().waitFor({state: 'detached'});
  assert.equal(posts('/api/rooms/kitchen/devices').length, before, 'Cancel adds nothing');

  // A failure stops the run and says how far it got.
  refuseResource = 'blind/1';
  await addAll().focus();
  await page.keyboard.press('Enter');
  await dialog().waitFor();
  await page.keyboard.press('Tab');
  await page.keyboard.press('Enter');
  await page.getByText(/Added 1 of 3\./).waitFor();
  const stopped = await page.getByText(/Added 1 of 3\./).textContent();
  assert.equal(
    stopped,
    'Added 1 of 3. Kitchen blind was refused: The device refused the request (That blind is not answering). Nothing after it was added.',
  );
  const tried = posts('/api/rooms/kitchen/devices').slice(before);
  assert.deepEqual(tried.map(call => call.body.integration.resource_id), ['lamp/1', 'blind/1'],
    'the saves are sequential and the one after the failure is never sent');

  // ...and running it again adds what is left.
  await page.getByRole('button', {name: 'Add all shown (2)', exact: true}).waitFor();
  await addAll().click();
  await dialog().waitFor();
  await page.getByRole('button', {name: 'Add them', exact: true}).click();
  await page.getByText('Added 2 devices to this room.').waitFor();
  assert.deepEqual(
    posts('/api/rooms/kitchen/devices').slice(before + 2).map(call => call.body.integration.resource_id),
    ['blind/1', 'lamp/4'],
  );
  assert.equal(await addAll().count(), 0, 'nothing left to add, nothing offered');

  // The vanished child is in the picker too, with a Remove of its own.
  const gone = page.locator('.discovered-devices.gone .discovered-device').filter({hasText: 'Corner lamp'});
  await gone.getByText('No longer reported by Bridge package').waitFor();
  await gone.getByText('Already in Kitchen · Corner lamp · Kept until you remove it').waitFor();
  assert.equal(await gone.getByRole('button', {name: 'Remove this device', exact: true}).count(), 1);

  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    'the children picker overflows a 360-pixel viewport');
  await page.screenshot({path: process.env.COUCH_CHILDREN_MOBILE ?? 'build/webui-review/plugin-children-mobile.png', fullPage: true});

  // The panel of one child: on, off, brightness, colour temperature.
  const lamp = page.locator('li.device').filter({hasText: 'Counter lamp'});
  const line = lamp.locator('.child-controls > p').first();
  assert.equal(await line.textContent(), 'State not read yet', 'nothing is claimed before a read');
  assert.deepEqual(
    await lamp.locator('.child-controls .actions button').allTextContents(),
    ['Turn on', 'Turn off', 'Refresh state'],
  );

  // Busy is the remote doing something else with this connection. It is
  // retried quietly: no error reaches the page.
  busyStatusReads = 2;
  const reads = calls.length;
  await lamp.getByRole('button', {name: 'Refresh state', exact: true}).click();
  await line.getByText('Off').waitFor();
  assert.equal(
    calls.slice(reads).filter(call => call.path.endsWith('/lamp/3/status')).length, 3,
    'a busy read is asked again, quietly',
  );
  assert.equal(await lamp.locator('.child-controls > p[role=status]').textContent(), '',
    'a retried busy request never shows an error');

  // A write that answers with the state it left is believed, and nothing is
  // read after it.
  const brightness = lamp.getByLabel('Brightness', {exact: true});
  assert.equal(await lamp.getByLabel('Colour temperature', {exact: true}).getAttribute('min'), '153');
  assert.equal(await lamp.getByLabel('Colour temperature', {exact: true}).getAttribute('max'), '500');
  const wrote = calls.length;
  await brightness.fill('40');
  await line.getByText('On · 40%').waitFor();
  assert.deepEqual(calls.slice(wrote).map(call => `${call.method} ${call.path}`), [
    'POST /api/connections/bridge/plugin/children/lamp/3/typed-action',
  ], 'an acknowledged write is the state, and is not read back');
  assert.deepEqual(calls[wrote].body, {action: 'set_light', brightness: 40});

  // Keyboard only, on a control that sends a command.
  const turnOn = lamp.getByRole('button', {name: 'Turn on', exact: true});
  const pressed = calls.length;
  await turnOn.focus();
  await page.keyboard.press('Enter');
  await line.getByText(/^On/).waitFor();
  assert.deepEqual(calls[pressed].body, {command: 'on'});
  await page.screenshot({path: process.env.COUCH_CHILDREN_CONTROLS ?? 'build/webui-review/plugin-children-controls.png', fullPage: true});

  // A connection whose package lists nothing says so, and offers nothing.
  await source().selectOption('stale');
  await pickerLine().getByText('This integration does not list devices').waitFor();
  assert.equal(await cards().count(), 0);
  assert.equal(await addAll().count(), 0);

  // A package with no kinds of child at all - which is every package a
  // shipped build runs - is the form it has always been.
  await source().selectOption('amp');
  await page.getByPlaceholder('Living room TV').waitFor();
  assert.equal(await typeBox().count(), 0, 'a package without children shows no children picker');
  assert.equal(await roomBox().count(), 0);

  // Removing a vanished device is the ordinary two-tap delete.
  await source().selectOption('bridge');
  const removal = page.locator('.discovered-devices.gone .discovered-device')
    .filter({hasText: 'Corner lamp'}).getByRole('button', {name: 'Remove this device', exact: true});
  await removal.click();
  await page.getByRole('button', {name: 'Confirm delete', exact: true}).first().click();
  await page.getByText('Corner lamp').first().waitFor({state: 'detached'});
  assert(paths().includes('DELETE /api/rooms/kitchen/devices/kitchen-corner'),
    'a vanished child is removed by hand, by the ordinary device delete');

  assert.deepEqual(errors, []);
  console.log('PASS: the children picker lists, filters and preselects; nothing is added without the confirmation that names it; a failed batch stops and says where; a vanished child is badged and only removed by hand; a light panel writes over the child routes and believes the state it is acknowledged with.');
} finally {
  await browser.close();
}
