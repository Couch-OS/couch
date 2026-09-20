// Browser contract for the pairing dialog of a packaged connection: the three
// prompts, what the dialog does with every answer the remote can give, the
// "needs pairing" banner, and forgetting a pairing. Protocol 3 is unreleased -
// no package a shipped build accepts declares `pairing` - so this runs against
// a fixture.
//
// Like plugin-children.mjs it runs against a static bundle and intercepts every
// API call, so no package, device or daemon state is touched.
import assert from 'node:assert/strict';

const { chromium } = await import(
  process.env.COUCH_PLAYWRIGHT ?? '../../build/webui-review/node_modules/playwright/index.mjs',
);

const origin = process.env.COUCH_TEST_URL;
assert(origin, 'Set COUCH_TEST_URL to the static Couch web bundle');
assert(['127.0.0.1', 'localhost'].includes(new URL(origin).hostname), 'Only loopback test servers are allowed');

const shots = process.env.COUCH_PAIRING_SHOTS ?? 'build/webui-review';

// Couch's own headline per prompt. OWNER DECISION: these words are Couch's and
// a package can only add one line under them.
const headlines = {
  press_button: 'Press the button on the device',
  approve_on_device: 'Approve on the device',
  enter_code: 'Enter the code shown on the device',
};

const settings = [
  {id: 'host', label: 'Device address', kind: 'text', required: true},
  {id: 'port', label: 'Port', kind: 'integer', default: 9299},
];

const plugin = (id, label) => ({
  kind: 'plugin', id, label, capabilities: [{id: 'on', label: 'Turn on'}],
  actions: [], supports_inputs: false, presentation: [], children: [],
});

const config = {
  schema_version: 1,
  revision: 7,
  areas: [], rooms: [], scenes: [], activities: [],
  connections: [
    {id: 'lamp', name: 'Hall lamp bridge', provider: plugin('lamp-pkg', 'Hall lamp bridge')},
    {id: 'speaker', name: 'Kitchen speaker', provider: plugin('speaker-pkg', 'Kitchen speaker')},
    {id: 'tv', name: 'Hall television', provider: plugin('tv-pkg', 'Hall television')},
    // A package that does not pair: every package a shipped build runs.
    {id: 'amp', name: 'Amplifier', provider: plugin('amp-pkg', 'Amplifier')},
  ],
};

const pairs = {required: true, max_seconds: 120};
const catalog = {
  integrations: [
    {id: 'lamp-pkg', label: 'Hall lamp bridge', capabilities: [{id: 'on', label: 'Turn on'}], actions: [], settings, supports_inputs: false, presentation: [], pairing: pairs},
    {id: 'speaker-pkg', label: 'Kitchen speaker', capabilities: [{id: 'on', label: 'Turn on'}], actions: [], settings, supports_inputs: false, presentation: [], pairing: pairs},
    {id: 'tv-pkg', label: 'Hall television', capabilities: [{id: 'on', label: 'Turn on'}], actions: [], settings, supports_inputs: false, presentation: [], pairing: pairs},
    {id: 'amp-pkg', label: 'Amplifier', capabilities: [{id: 'on', label: 'Turn on'}], actions: [], settings, supports_inputs: false, presentation: []},
  ],
};

// What each connection's settings route answers. `paired` and `pairing` are
// only there for a package that pairs.
const saved = {
  lamp: {settings: {host: 'lamp.local', port: 9299}, configured: true, secrets: [], paired: false, pairing: {required: true}},
  speaker: {settings: {host: 'speaker.local', port: 9299}, configured: true, secrets: [], paired: false, pairing: {required: true}},
  tv: {settings: {host: 'tv.local', port: 9299}, configured: true, secrets: [], paired: false, pairing: {required: true}},
  amp: {settings: {host: 'amp.local', port: 9299}, configured: true, secrets: []},
};

const waiting = (prompt, poll = 2000) => ({step: 'waiting', prompt, poll_after_ms: poll});
const done = summary => ({step: 'done', summary, settings: {settings: {host: 'tv.local', port: 9299}, configured: true, secrets: []}});

// One scripted conversation per connection. `script` is consulted by
// `…/pair/<session>`; `start` by `…/pair`.
let scripts = {};
const calls = [];
const of = (method, path) => calls.filter(call => call.method === method && call.path === path);
const pairPosts = id => of('POST', `/api/connections/${id}/plugin/pair`);
const deletes = id => calls.filter(call => call.method === 'DELETE' && call.path.startsWith(`/api/connections/${id}/plugin/pair/`));

let sessions = 0;
let settingsRefusal = null;
let statusRefusal = null;

async function mockApi(page) {
  await page.route('**/api/**', async route => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const body = request.postDataJSON?.() ?? null;
    const method = request.method();
    calls.push({method, path, body});
    const json = (value, status = 200) => route.fulfill({
      status, contentType: 'application/json', body: JSON.stringify(value),
    });

    if (method === 'GET' && path === '/api/auth/status') {
      return json({authenticated: true, pairing: false, expires_in: 0, tries_left: 0, disabled: true});
    }
    if (method === 'GET' && path === '/api/config') return json(config);
    if (method === 'POST' && path === '/api/updates/check') return json({});
    if (method === 'GET' && path === '/api/updates') {
      return json({installed: 'test', channel: 'preview', available: null, notes: '', phase: 'idle', message: '', can_install: false, automatic_checks: false});
    }
    if (method === 'GET' && path === '/api/integrations') return json(catalog);
    if (method === 'GET' && path === '/api/remote/device') return json({});

    const settingsRoute = path.match(/^\/api\/connections\/([a-z]+)\/plugin\/settings$/);
    if (settingsRoute) {
      const id = settingsRoute[1];
      if (method === 'GET') return json(saved[id]);
      if (settingsRefusal) {
        const refusal = settingsRefusal;
        settingsRefusal = null;
        return json(refusal.body, refusal.status);
      }
      saved[id] = {...saved[id], settings: {...saved[id].settings, ...body}};
      return json(saved[id]);
    }
    const statusRoute = path.match(/^\/api\/connections\/([a-z]+)\/plugin\/status$/);
    if (statusRoute) {
      if (statusRefusal) {
        const refusal = statusRefusal;
        statusRefusal = null;
        return json(refusal.body, refusal.status);
      }
      return json({on: true});
    }
    const credential = path.match(/^\/api\/connections\/([a-z]+)\/plugin\/credential$/);
    if (credential && method === 'DELETE') {
      const id = credential[1];
      saved[id] = {...saved[id], paired: false, summary: undefined};
      return json({paired: false});
    }

    const start = path.match(/^\/api\/connections\/([a-z]+)\/plugin\/pair$/);
    if (start && method === 'POST') {
      const script = scripts[start[1]];
      assert(script, `no script for ${start[1]}`);
      const answer = script.start();
      if (answer.refuse) return json(answer.refuse.body, answer.refuse.status);
      sessions += 1;
      script.session = `${'0'.repeat(24)}${String(sessions).padStart(8, '0')}`;
      return json({session: script.session, step: answer.step, expires_in: answer.expires_in ?? 120});
    }
    const step = path.match(/^\/api\/connections\/([a-z]+)\/plugin\/pair\/([0-9a-f]+)$/);
    if (step) {
      const script = scripts[step[1]];
      assert(script, `no script for ${step[1]}`);
      if (method === 'DELETE') return json({cancelled: true});
      assert.equal(step[2], script.session, 'a poll always carries the session the remote minted');
      const answer = script.step(body?.input ?? null);
      if (answer.refuse) return json(answer.refuse.body, answer.refuse.status);
      return json({step: answer.step});
    }
    throw new Error(`Unexpected pairing request: ${method} ${path}`);
  });
}

const browser = await chromium.launch({headless: true});
const context = await browser.newContext({viewport: {width: 360, height: 900}});
const page = await context.newPage();
const errors = [];
page.on('pageerror', error => errors.push(String(error)));
await mockApi(page);

const dialog = () => page.getByRole('dialog');
const headline = () => page.locator('#pairing-headline');
const live = () => page.locator('.pairing-live');
const banner = () => page.locator('.needs-pairing');
const failure = () => page.locator('.pairing-failed');
const codeBox = () => page.getByLabel('Code from the device', {exact: true});
const continueButton = () => dialog().getByRole('button', {name: 'Continue', exact: true});
const formLine = () => page.locator('section.card').filter({hasText: 'Integration settings'}).getByRole('status').first();

async function openConnection(name) {
  await page.getByRole('navigation').getByRole('button', {name: 'Connections', exact: true}).click();
  await page.getByRole('heading', {name: 'Connections', exact: true}).waitFor();
  await page.locator('.destination').filter({hasText: name}).click();
  await page.getByRole('heading', {name: 'Integration controls', exact: true}).waitFor();
}

try {
  await page.goto(origin);

  // ------------------------------------------------------------------
  // 1. Press the button. The package adds one line; Couch says the rest.
  // ------------------------------------------------------------------
  let polls = 0;
  scripts.lamp = {
    start: () => ({step: waiting({kind: 'press_button', message: 'The button is on top'})}),
    step: () => {
      polls += 1;
      // Two polls that say nothing new, then the light pairs.
      if (polls < 3) return {step: waiting({kind: 'press_button', message: 'The button is on top'})};
      return {step: done('Paired with the hall lamp bridge')};
    },
  };

  await openConnection('Hall lamp bridge');
  await banner().waitFor();
  assert.equal(await banner().locator('span').textContent(), 'This connection needs pairing');
  assert.equal(await banner().getByRole('button').textContent(), 'Pair');

  // The form's own Pair saves what is typed first: the daemon validates the
  // settings a pairing starts with and does not save them.
  const pairFromForm = page.locator('form').getByRole('button', {name: /^Pair( again)?$/});
  await page.getByLabel('Device address · required', {exact: true}).fill('lamp.local');
  await pairFromForm.click();
  await dialog().waitFor();
  assert.deepEqual(
    calls.slice(-2).map(call => `${call.method} ${call.path}`),
    ['POST /api/connections/lamp/plugin/settings', 'POST /api/connections/lamp/plugin/pair'],
    'what is typed is saved before a pairing is asked for',
  );
  assert.deepEqual(pairPosts('lamp')[0].body, null, 'the pairing itself carries no settings');

  assert.equal(await headline().textContent(), headlines.press_button);
  assert.equal(await dialog().getAttribute('aria-modal'), 'true');
  assert.equal(await dialog().getAttribute('aria-labelledby'), 'pairing-headline');
  assert.equal(await page.locator('.pairing-note').first().textContent(), 'The button is on top');
  assert.equal(await live().textContent(), headlines.press_button);
  await page.getByText(/^\d:\d\d left$/).waitFor();
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    'the pairing dialog overflows a 360-pixel viewport');
  await page.screenshot({path: `${shots}/plugin-pairing-press-button.png`});

  // The dialog takes the focus when it appears, and Tab never leaves it.
  assert(await page.evaluate(() => document.activeElement?.closest('[role=dialog]') !== null),
    'the dialog takes the focus when it appears');
  for (let step = 0; step < 6; step += 1) {
    await page.keyboard.press('Tab');
    assert(await page.evaluate(() => document.activeElement?.closest('[role=dialog]') !== null),
      'Tab left the dialog');
  }
  await page.keyboard.press('Shift+Tab');
  assert(await page.evaluate(() => document.activeElement?.closest('[role=dialog]') !== null),
    'Shift+Tab left the dialog');

  // Three polls later it is paired, and the summary is what the package said.
  await page.getByText('Paired with the hall lamp bridge').first().waitFor({timeout: 15000});
  assert.equal(await headline().textContent(), 'Paired');
  saved.lamp = {...saved.lamp, paired: true, summary: 'Paired with the hall lamp bridge'};
  await dialog().getByRole('button', {name: 'Done', exact: true}).click();
  await dialog().waitFor({state: 'detached'});

  // Closing a finished pairing reads the connection's settings again, and the
  // banner is gone.
  await page.getByText('Paired with the hall lamp bridge').first().waitFor();
  assert.equal(await banner().count(), 0, 'a paired connection asks for nothing');
  assert.equal(await pairFromForm.textContent(), 'Pair again');
  assert(of('GET', '/api/connections/lamp/plugin/settings').length >= 2,
    'a finished pairing reads the settings again rather than guessing');

  // ------------------------------------------------------------------
  // 2. Forget pairing, behind the owner's own sentence.
  // ------------------------------------------------------------------
  const forget = page.locator('.forget-pairing');
  await forget.getByRole('button', {name: 'Forget pairing', exact: true}).click();
  assert.equal(
    await forget.locator('p[role=alert]').textContent(),
    "This removes Couch's copy of the key. The device may still list Couch as paired.",
  );
  assert.equal(of('DELETE', '/api/connections/lamp/plugin/credential').length, 0,
    'asking the question forgets nothing');
  await forget.getByRole('button', {name: 'Keep it', exact: true}).click();
  await forget.getByRole('button', {name: 'Forget pairing', exact: true}).click();
  await forget.getByRole('button', {name: 'Forget pairing', exact: true}).nth(0).click();
  await page.getByText('Couch has forgotten this pairing.').waitFor();
  assert.equal(of('DELETE', '/api/connections/lamp/plugin/credential').length, 1);
  await banner().waitFor();

  // ------------------------------------------------------------------
  // 3. Approve on the device: a slow one, several polls, then cancelled.
  // ------------------------------------------------------------------
  let approvals = 0;
  scripts.speaker = {
    start: () => ({step: waiting({kind: 'approve_on_device'}, 300)}),
    step: () => {
      approvals += 1;
      // Twice busy, and it is retried quietly; then the same prompt over and
      // over, because nobody has touched the speaker.
      if (approvals === 1 || approvals === 2) {
        return {refuse: {status: 503, body: {error: 'The integration request queue is full', code: 'busy'}}};
      }
      return {step: waiting({kind: 'approve_on_device'}, 300)};
    },
  };
  await openConnection('Kitchen speaker');
  await banner().getByRole('button', {name: 'Pair', exact: true}).click();
  await dialog().waitFor();
  assert.equal(await headline().textContent(), headlines.approve_on_device);
  assert.equal(await page.locator('.pairing-note').count(), 0, 'a package that said nothing adds nothing');
  await page.screenshot({path: `${shots}/plugin-pairing-approve.png`});

  // The busy answers are retried quietly: nothing on the page says anything
  // went wrong, and the prompt is still the prompt.
  await page.waitForFunction(() => document.querySelectorAll('[role=dialog]').length === 1);
  await page.waitForTimeout(2000);
  assert.equal(await headline().textContent(), headlines.approve_on_device);
  assert.equal(await dialog().locator('[role=alert]').count(), 0, 'a retried busy poll never shows an error');
  assert(approvals > 3, 'a slow approval keeps being polled');

  // The countdown is shown every second and announced far less often: the
  // live region does not change while three seconds go by.
  const announced = await live().textContent();
  const clock = await page.locator('.pairing-countdown').textContent();
  await page.waitForTimeout(3000);
  assert.notEqual(await page.locator('.pairing-countdown').textContent(), clock, 'the countdown stands still');
  assert.equal(await live().textContent(), announced, 'the countdown is announced every second');

  // Cancel, by the button.
  await dialog().getByRole('button', {name: 'Cancel', exact: true}).click();
  await dialog().waitFor({state: 'detached'});
  assert.equal(deletes('speaker').length, 1, 'Cancel tells the remote to stop');

  // ...by Escape.
  await banner().getByRole('button', {name: 'Pair', exact: true}).click();
  await dialog().waitFor();
  await page.keyboard.press('Escape');
  await dialog().waitFor({state: 'detached'});
  assert.equal(deletes('speaker').length, 2, 'Escape tells the remote to stop');

  // ...by the close control, which also puts the focus back where it was.
  await banner().getByRole('button', {name: 'Pair', exact: true}).focus();
  await page.keyboard.press('Enter');
  await dialog().waitFor();
  await dialog().getByRole('button', {name: 'Close pairing', exact: true}).click();
  await dialog().waitFor({state: 'detached'});
  assert.equal(deletes('speaker').length, 3, 'closing tells the remote to stop');
  assert.equal(
    await page.evaluate(() => document.activeElement?.textContent), 'Pair',
    'closing the dialog puts the focus back on the control that opened it',
  );

  // ...and by the page going away.
  await page.keyboard.press('Enter');
  await dialog().waitFor();
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide')));
  await dialog().waitFor({state: 'detached'});
  assert.equal(deletes('speaker').length, 4, 'leaving the page tells the remote to stop');

  // ------------------------------------------------------------------
  // 4. Enter the code: the box's rules, a wrong code, Try again, done.
  // ------------------------------------------------------------------
  const codePrompt = alphabet => waiting({kind: 'enter_code', message: 'It is on the screen', length: 4, alphabet}, 0);
  let attempt = 0;
  scripts.tv = {
    start: () => {
      attempt += 1;
      if (attempt === 1) return {step: codePrompt('digits')};
      if (attempt === 2) return {step: codePrompt('hex')};
      return {step: codePrompt('digits')};
    },
    step: input => {
      assert(input, 'a code prompt is never polled');
      assert.equal(input.kind, 'code');
      if (input.code === '1234') return {step: done('Paired with the hall television')};
      return {step: {step: 'failed', reason: 'wrong_code', message: 'That was not the code on the screen'}};
    },
  };

  await openConnection('Hall television');
  await banner().getByRole('button', {name: 'Pair', exact: true}).click();
  await dialog().waitFor();
  assert.equal(await headline().textContent(), headlines.enter_code);
  assert.equal(await page.locator('.pairing-note').first().textContent(), 'It is on the screen');
  assert.equal(await codeBox().getAttribute('maxlength'), '4');
  assert.equal(await codeBox().getAttribute('inputmode'), 'numeric');
  assert.equal(await codeBox().getAttribute('autocapitalize'), 'none');
  assert.equal(await codeBox().getAttribute('autocomplete'), 'one-time-code');
  assert(await page.evaluate(() => document.activeElement?.classList.contains('pairing-code')),
    'a code box takes the focus: typing is the only thing there is to do');
  assert(await continueButton().isDisabled(), 'an empty code is never sent');
  await codeBox().pressSequentially('1a2b3');
  assert.equal(await codeBox().inputValue(), '123', 'a digits box keeps only digits');
  assert(await continueButton().isDisabled(), 'half a code is never sent');
  await codeBox().fill('');
  await codeBox().pressSequentially('99999');
  assert.equal(await codeBox().inputValue(), '9999', 'a box never holds more than the device asked for');
  assert.equal(await continueButton().isDisabled(), false);
  await page.screenshot({path: `${shots}/plugin-pairing-enter-code.png`});

  // OWNER DECISION: a wrong code ends the attempt, and "Try again" starts a
  // new pairing rather than letting anybody guess at the same session.
  await continueButton().click();
  await failure().getByText('That was not the code the device is showing.').waitFor();
  assert.equal(await page.locator('.pairing-note').last().textContent(), 'That was not the code on the screen');
  assert.equal(await codeBox().count(), 0, 'a failed attempt offers no second guess');
  await page.screenshot({path: `${shots}/plugin-pairing-wrong-code.png`});

  const before = pairPosts('tv').length;
  await dialog().getByRole('button', {name: 'Try again', exact: true}).click();
  await codeBox().waitFor();
  assert.equal(pairPosts('tv').length, before + 1, 'Try again starts a new pairing');
  // The second attempt is a hexadecimal code: capitals, and a keyboard with
  // letters on it.
  assert.equal(await codeBox().getAttribute('inputmode'), 'text');
  assert.equal(await codeBox().getAttribute('autocapitalize'), 'characters');
  await codeBox().pressSequentially('dezf');
  assert.equal(await codeBox().inputValue(), 'DEF', 'a hexadecimal box keeps hex digits, in capitals');

  // Third time, with the right code.
  await dialog().getByRole('button', {name: 'Cancel', exact: true}).click();
  await dialog().waitFor({state: 'detached'});
  await banner().getByRole('button', {name: 'Pair', exact: true}).click();
  await codeBox().waitFor();
  await codeBox().fill('1234');
  await continueButton().click();
  await page.getByText('Paired with the hall television').first().waitFor();
  saved.tv = {...saved.tv, paired: true, summary: 'Paired with the hall television'};
  await page.screenshot({path: `${shots}/plugin-pairing-done.png`});
  await dialog().getByRole('button', {name: 'Done', exact: true}).click();
  await dialog().waitFor({state: 'detached'});
  await page.getByText('Paired with the hall television').first().waitFor();

  // ------------------------------------------------------------------
  // 5. The endings that are not the device's fault.
  // ------------------------------------------------------------------
  // A window that runs out. The remote says three seconds; the dialog counts
  // them and says so without asking the device anything.
  scripts.tv = {
    start: () => ({step: waiting({kind: 'press_button'}, 60000), expires_in: 3}),
    step: () => ({step: waiting({kind: 'press_button'}, 60000)}),
  };
  await page.locator('form').getByRole('button', {name: 'Pair again', exact: true}).click();
  await dialog().waitFor();
  await failure().getByText('The device did not answer in time.').waitFor({timeout: 15000});
  await dialog().getByRole('button', {name: 'Close', exact: true}).click();
  await dialog().waitFor({state: 'detached'});

  // A session the remote no longer has: its own sentence names a session
  // nobody ever saw, so Couch says what to do instead.
  scripts.tv = {
    start: () => ({step: waiting({kind: 'press_button'}, 200)}),
    step: () => ({refuse: {status: 404, body: {error: 'That pairing is no longer in progress'}}}),
  };
  await page.locator('form').getByRole('button', {name: 'Pair again', exact: true}).click();
  await dialog().waitFor();
  await failure().getByText('Pairing was interrupted. Start again.').waitFor({timeout: 10000});
  await dialog().getByRole('button', {name: 'Close', exact: true}).click();
  await dialog().waitFor({state: 'detached'});

  // Too many at once: the daemon's own sentence, because it is the one that
  // knows how many there are.
  scripts.tv = {
    start: () => ({refuse: {status: 409, body: {error: 'Too many devices are being paired at once; finish one and try again'}}}),
    step: () => assert.fail('a pairing that never started is never polled'),
  };
  const dialogs = await page.locator('[role=dialog]').count();
  await page.locator('form').getByRole('button', {name: 'Pair again', exact: true}).click();
  await page.getByText('Too many devices are being paired at once; finish one and try again').waitFor();
  assert.equal(await page.locator('[role=dialog]').count(), dialogs, 'a pairing that was refused opens no dialog');

  // A refused setting marks the setting its reason names, exactly as a
  // refused save does, and no pairing is started at all.
  const startsBefore = pairPosts('tv').length;
  settingsRefusal = {status: 400, body: {
    error: 'The port must not be 0', code: 'invalid',
    reason: {kind: 'invalid_setting', field: 'port', text: 'The port must not be 0'},
  }};
  await page.getByLabel('Port', {exact: true}).fill('0');
  await page.locator('form').getByRole('button', {name: 'Pair again', exact: true}).click();
  await page.locator('#plugin-setting-port-error').waitFor();
  assert.equal(await page.locator('#plugin-setting-port-error').textContent(), 'The port must not be 0');
  assert.equal(await page.getByLabel('Port', {exact: true}).getAttribute('aria-invalid'), 'true');
  assert.equal(await formLine().textContent(), 'Not saved. Check Port.');
  assert.equal(pairPosts('tv').length, startsBefore, 'settings the package refused never start a pairing');

  // A key the remote stored beside settings it could not: the pairing stands,
  // and the one thing that did not go through is said calmly and stays on
  // screen until it is dismissed.
  scripts.speaker = {
    start: () => ({step: {
      step: 'done', summary: 'Paired with the kitchen speaker',
      settings: {settings: {host: 'speaker.local', port: 9299}, configured: true, secrets: []},
      warning: 'the settings this device corrected were not kept: they were refused (invalid)',
    }}),
    step: () => assert.fail('a pairing that is already done is never polled'),
  };
  await openConnection('Kitchen speaker');
  await banner().getByRole('button', {name: 'Pair', exact: true}).click();
  await dialog().waitFor();
  assert.equal(await headline().textContent(), 'Paired');
  assert.equal(
    await page.locator('.pairing-warning').textContent(),
    'The pairing was saved, but the settings this device corrected were not kept: they were refused (invalid).',
  );
  assert.equal(await page.locator('.pairing-warning[role=alert]').count(), 0,
    'a pairing that worked is not shouted about');
  saved.speaker = {...saved.speaker, paired: true, summary: 'Paired with the kitchen speaker'};
  await page.waitForTimeout(1500);
  assert.equal(await dialog().count(), 1, 'a pairing with a warning is never closed for anybody');
  await page.screenshot({path: `${shots}/plugin-pairing-warning.png`});
  await dialog().getByRole('button', {name: 'Done', exact: true}).click();
  await dialog().waitFor({state: 'detached'});
  await page.getByText('Paired with the kitchen speaker').first().waitFor();

  // ------------------------------------------------------------------
  // 6. A connection that is told it is not paired, by any call at all.
  // ------------------------------------------------------------------
  await openConnection('Amplifier');
  assert.equal(await banner().count(), 0, 'a package that does not pair asks for nothing');
  assert.equal(await page.locator('.pairing-state').count(), 0);
  assert.equal(await page.locator('form').getByRole('button', {name: /^Pair/}).count(), 0,
    'a package that does not pair offers no Pair button');

  await openConnection('Hall television');
  await page.getByText('Paired with the hall television').first().waitFor();
  assert.equal(await banner().count(), 0, 'a paired connection asks for nothing');
  statusRefusal = {status: 409, body: {
    error: 'Pair this television again', code: 'unpaired',
    reason: {kind: 'message', text: 'Pair this television again'},
  }};
  await page.getByRole('button', {name: 'Refresh status', exact: true}).click();
  await banner().waitFor();
  assert.equal(await banner().locator('span').textContent(), 'This connection needs pairing');
  assert.equal(await banner().getByRole('button').textContent(), 'Pair again',
    'a key the device threw away is paired again, not paired for the first time');
  await page.screenshot({path: `${shots}/plugin-pairing-banner.png`, fullPage: true});

  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    'the needs-pairing banner overflows a 360-pixel viewport');
  assert.deepEqual(errors, []);
  console.log('PASS: the three prompts are drawn from the step with Couch\'s own words; a slow approval is polled and a busy poll retried quietly; a wrong code ends the attempt and Try again starts a new one; a key stored beside settings that were not says so and stays up; expiry, an interrupted session and too many pairings each say what happened; every way out sends a cancel; a refused setting is marked and starts nothing; and a call refused for want of a pairing raises the banner.');
} finally {
  await browser.close();
}
