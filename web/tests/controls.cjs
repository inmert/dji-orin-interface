// Offline regression checks for command handlers and connection lifecycle.
// React hooks are modeled here; no sockets or USB endpoints are opened.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require('typescript');

function harness() {
  const states = [];
  const effects = [];
  const react = {
    useRef: value => ({ current: value }),
    useState: value => {
      const state = { value };
      states.push(state);
      return [value, next => { state.value = next; }];
    },
    useCallback: fn => fn,
    useMemo: fn => fn(),
    useEffect: fn => effects.push(fn),
  };
  return { react, states, effects };
}
function load(file, react, globals = {}) {
  const exports = {};
  const source = ts.transpileModule(fs.readFileSync(file, 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const jsx = (type, props) => ({ type, props });
  // Components import helpers from src/api.ts; load it the same way.
  const api = name => name.endsWith('/api') && !file.endsWith('api.ts')
    ? load('src/api.ts', react, globals) : undefined;
  vm.runInNewContext(source, {
    exports, require: name => name === 'react' ? react : api(name) ?? { jsx, jsxs: jsx }, ...globals,
  });
  return exports;
}
function buttons(tree) {
  const found = [];
  function walk(node) {
    if (!node || typeof node !== 'object') return;
    if (Array.isArray(node)) return node.forEach(walk);
    if (node.type === 'button') found.push(node);
    walk(node.props?.children);
  }
  walk(tree);
  return found;
}
const h = harness();
const sockets = [];
let tick;
let now = 10000;
class Socket {
  static OPEN = 1;
  readyState = 1;
  sent = [];
  constructor(url) { this.url = url; sockets.push(this); }
  close() {}
  send(value) { this.sent.push(value); }
}
const { useLink } = load('src/api.ts', h.react, {
  WebSocket: Socket, URLSearchParams,
  location: { search: '', protocol: 'https:', host: 'orin:8443' },
  Date: { now: () => now },
  window: { setInterval: fn => { tick = fn; }, setTimeout: () => { throw Error('unexpected retry'); } },
  clearTimeout() {}, clearInterval() {},
});
const link = useLink();
const cleanup = h.effects[0]();
cleanup();
h.effects[0](); // StrictMode effect replay retains the ref.
sockets[0].onclose();
sockets[0].onopen();
link.send({ cmd: 'stop' });
assert.equal(sockets[1].sent.length, 1);
assert.equal(sockets[1].url, 'wss://orin:8443/ws');
const snapshot = frames => ({ data: JSON.stringify({ frames, streams: {}, gimbals: {}, link_up: true, linked: false }) });
sockets[1].onmessage(snapshot(100));
assert.equal(h.states[2].value, true, 'cached initial snapshot is stale');
sockets[1].onmessage(snapshot(101));
assert.equal(h.states[2].value, false, 'advancing frame count is fresh');
now += 2500;
sockets[1].onmessage(snapshot(101));
tick();
assert.equal(h.states[2].value, true, 'repeated cached snapshots become stale');
sockets[0].onmessage(snapshot(999));
assert.equal(h.states[1].value.frames, 101);

sockets[1].onmessage(snapshot(1));
assert.equal(h.states[2].value, true, 'counter reset must not enable movement');
tick();
assert.equal(h.states[2].value, true, 'freshness timer must preserve reset state');
sockets[1].onmessage(snapshot(2));
assert.equal(h.states[2].value, false, 'counter must advance again after reset');

// Malformed nested rows previously passed parsing and crashed the render path.
const good = JSON.parse(snapshot(3).data);
const flightWith = data => ({ port: '/dev/ttyTHS1', open: true, frames: 5, rejected: 0, data: {
  time_ms: 1, attitude: null, acceleration: null, velocity: null, angular_rate: null, position: null,
  obstacles: null, gps: null, mag: null, rc: null, gimbal: null, status: null, battery: null,
  compass: null, last_flags: 1, ...data } });
const usbOsd = { pitch: 1.3, roll: 2.3, yaw: 143, mode: 6, mode_name: 'P-GPS', satellites: 0,
  unverified: { latitude: 0, longitude: 0, height: 0, velocity: [0, 0, 0] } };
const usbLink = { port: '/dev/ttyACM0', open: true, frames: 4, osd: usbOsd, gimbals: {} };
const malformed = [
  { ...good, frames: -1 }, { ...good, frames: 3.5 },
  { ...good, frames: 1e100 }, { ...good, link_up: "true" },
  { ...good, linked: null }, { ...good, streams: [] },
  { ...good, streams: { bad: null } },
  { ...good, streams: { bad: { from: 'Gimbal', to: 'App', count: '1' } } },
  { ...good, gimbals: { 0: null } },
  { ...good, gimbals: { 0: { index: 0, pitch: {}, roll: 0, yaw: 0 } } },
  { ...good, motors: { busy: 'no', last: null, ok: null, message: '' } },
  { ...good, motors: { busy: false, last: true, ok: null, message: '' } },
  { ...good, motors: { busy: false, last: null, ok: null } },
  { ...good, motors: { busy: false, last: null, ok: null, message: '', armed: 'yes' } },
  { ...good, motors: { busy: false, last: null, ok: null, message: '', usb_flight: 1 } },
];
now += 2500;
tick();
for (const value of malformed) {
  sockets[1].onmessage({ data: JSON.stringify(value) });
  assert.equal(h.states[1].value.frames, 2, 'malformed snapshot preserves last valid data');
  assert.equal(h.states[2].value, true, 'malformed snapshot cannot restore freshness');
}
// A malformed panel section (a non-finite reading arrives as null) is
// dropped on its own: the rest of the snapshot, motors and plan included,
// still arrives. Same frame count, so freshness is untouched.
const detached = [
  { ...good, frames: 2, flight: null },
  { ...good, frames: 2, flight: { port: '/dev/ttyTHS1', open: true, frames: 1, rejected: 0 } },
  { ...good, frames: 2, flight: flightWith({ attitude: { roll: 0, pitch: 'x', yaw: 0 } }) },
  { ...good, frames: 2, flight: flightWith({ status: { flight: 0, mode: 6, gear: 0, error: 0, flight_name: 'Stopped' } }) },
  { ...good, frames: 2, flight: flightWith({ gps: { date: 1, time: 1, latitude: 0, longitude: 0, hfsl: 0, hdop: 0, pdop: 0, fix: 0, satellites: 0, velocity_ned: [0, 0] } }) },
  { ...good, frames: 2, flight: flightWith({ mag: [1, 2] }) },
  { ...good, frames: 2, usb: null },
  { ...good, frames: 2, usb: { ...usbLink, gimbals: { 0: { index: 0, pitch: 0 } } } },
  { ...good, frames: 2, usb: { ...usbLink, osd: { ...usbOsd, yaw: '143' } } },
  { ...good, frames: 2, usb: { ...usbLink, osd: { ...usbOsd, unverified: { latitude: 0, longitude: 0, height: 0 } } } },
  { ...good, frames: 2, flight: flightWith({ attitude: { roll: null, pitch: 0, yaw: 0 } }) },
  { ...good, frames: 2, usb: { ...usbLink, osd: { ...usbOsd, unverified: { latitude: null, longitude: 0, height: 0, velocity: [0, 0, 0] } } } },
  { ...good, frames: 2, video: { cam1: { state: 'playing' } } },
];
for (const value of detached) {
  const marked = { ...value, motors: { busy: false, last: 'stop', ok: true, message: 'kept', armed: false } };
  sockets[1].onmessage({ data: JSON.stringify(marked) });
  const t = h.states[1].value;
  assert.equal(t.motors.message, 'kept', 'the rest of the snapshot arrives');
  assert.ok(t.flight === undefined || value.flight === undefined, 'a malformed flight section is dropped');
  assert.ok(t.usb === undefined || value.usb === undefined, 'a malformed USB section is dropped');
  assert.ok(t.video === undefined || value.video === undefined, 'a malformed video section is dropped');
  assert.equal(h.states[2].value, true, 'a dropped section cannot restore freshness');
}
sockets[1].onmessage({ data: JSON.stringify({ ...good,
  streams: { route: { from: 'Gimbal', to: 'App', cmd_set: 4, cmd_id: 5, count: 3, last_len: 12 } },
  gimbals: { 0: { index: 0, pitch: -10, roll: 0, yaw: 20, raw: 'f6 ff', provisional: true } },
  flight: flightWith({ attitude: { roll: 3.9, pitch: -0.3, yaw: -97.5 },
    status: { flight: 0, flight_name: 'Stopped', mode: 6, mode_name: 'P-GPS', gear: 1, gear_name: 'Down', error: 200 },
    mag: [101, 582, 1366] }),
  usb: { ...usbLink, gimbals: { 0: { index: 0, pitch: 0, roll: 0, yaw: 1425, raw: '', provisional: true } } },
  motors: { busy: false, last: 'start', ok: false, message: 'FC refused', armed: true, armed_from: 'RC link', usb_flight: 'allowed' },
}) });
assert.equal(h.states[1].value.frames, 3, 'valid populated telemetry still accepted');
assert.equal(h.states[2].value, false);

for (const disabled of [false, true]) {
  const pad = harness();
  const handlers = {};
  const events = { addEventListener: (name, fn) => { handlers[name] = fn; }, removeEventListener: name => { delete handlers[name]; } };
  const document = { ...events, hidden: false };
  const { GimbalPad } = load('src/gimbal/GimbalPad.tsx', pad.react, { window: events, document });
  const sent = [];
  const tree = GimbalPad({ camera: 1, linked: false, disabled, send: c => sent.push(c) });
  const unmount = pad.effects[0]();
  pad.effects[1]();
  const controls = buttons(tree);
  const up = controls.find(b => b.props['aria-label'] === 'Camera 1 joystick');
  const stop = controls.find(b => b.props['aria-label'] === 'Stop camera 1');
  stop.props.onClick();
  assert.equal(sent.at(-1).active, false, 'stop releases while idle');
  const recenter = controls.find(b => b.props['aria-label'] === 'Recenter camera 1 yaw');
  assert.equal(recenter.props.disabled, disabled, 'recenter follows the movement gate');
  if (!disabled) {
    recenter.props.onClick();
    assert.deepEqual(sent.slice(-2).map(c => c.cmd), ['gimbal', 'reset'], 'recenter releases, then resets');
    assert.equal(sent.at(-2).active, false);
    assert.equal(sent.at(-1).camera, 1);
  }
  const press = () => up.props.onKeyDown({ key: 'ArrowUp', repeat: false, preventDefault() {} });
  press();
  assert.equal(sent.at(-1).active, !disabled);
  if (!disabled) {
    up.props.onKeyUp({ key: 'ArrowUp', preventDefault() {} });
    assert.equal(sent.at(-1).active, false);
    const target = { focus() {}, setPointerCapture() {}, getBoundingClientRect: () => ({ width: 140 }) };
    const event = (x, y, id = 1) => ({ button: 0, pointerId: id, clientX: x, clientY: y, currentTarget: target, preventDefault() {} });
    up.props.onPointerDown(event(70, 70));
    assert.equal(sent.at(-1).active, false, 'press starts centered');
    up.props.onPointerMove(event(94, 46));
    assert.equal(sent.at(-1).pitch, 100);
    assert.equal(sent.at(-1).yaw, 100, 'diagonal drag drives both axes proportionally');
    up.props.onPointerMove(event(550, 70));
    assert.equal(sent.at(-1).yaw, 200, 'outside drag clamps to speed limit');
    up.props.onPointerUp(event(550, 70, 2));
    assert.equal(sent.at(-1).active, true, 'another pointer cannot end the gesture');
    up.props.onPointerMove(event(71, 71));
    assert.equal(sent.at(-1).active, false, 'center dead zone releases movement');
    up.props.onPointerMove(event(70, 22));
    up.props.onPointerUp(event(70, 22));
    assert.equal(sent.at(-1).active, false, 'release outside stops movement');
    const count = sent.length;
    up.props.onPointerMove(event(550, 70));
    assert.equal(sent.length, count, 'movement after release is ignored');
    for (const handler of ['onPointerCancel', 'onLostPointerCapture']) {
      up.props.onPointerDown(event(70, 70));
      up.props.onPointerMove(event(118, 70));
      up.props[handler](event(118, 70));
      assert.equal(sent.at(-1).active, false, handler);
    }
    up.props.onPointerDown(event(70, 70));
    up.props.onPointerMove(event(118, 70));
    pad.effects[2](); // Global stop generation changed.
    assert.equal(sent.at(-1).active, false, 'global stop cancels a held gesture');
    const afterStop = sent.length;
    up.props.onPointerMove(event(118, 22));
    assert.equal(sent.length, afterStop, 'drag after global stop cannot restart movement');
    press(); handlers.blur(); assert.equal(sent.at(-1).active, false);
    press(); document.hidden = true; handlers.visibilitychange(); assert.equal(sent.at(-1).active, false);
    press(); unmount(); assert.equal(sent.at(-1).active, false);
  } else {
    assert.equal(up.props.disabled, true);
    unmount();
  }
}
const { modeSwitch } = load('src/telemetry/FlightPanel.tsx', harness().react,
  { window: { confirm: () => true, isSecureContext: true }, navigator: {} });
assert.deepEqual([8000, 0, -8000].map(modeSwitch), ['P', 'S', 'A'], 'RC mode switch positions');

// The command bar checks a sentence first and only runs what the server showed back.
{
  const bar = harness();
  // The text box starts filled; every other piece of state keeps its initial value.
  bar.react.useState = value => [value === '' ? 'arm for 2 seconds then disarm' : value, () => {}];
  const { CommandBar } = load('src/telemetry/FlightPanel.tsx', bar.react,
    { window: { confirm: () => true, isSecureContext: false }, navigator: {} });
  const sent = [];
  const idle = { running: false, plan: null, text: '', step: 0, message: '', ok: null, preview: null, preview_error: null, parsing: false, source: '' };
  let tree = CommandBar({ plan: idle, enabled: true, send: c => sent.push(c) });
  let [talk, check, run, cancel] = buttons(tree);
  assert.equal(talk.props.disabled, true, 'no microphone without a secure context');
  assert.equal(run.props.disabled, true, 'run needs a checked plan');
  assert.equal(cancel.props.disabled, true);
  check.props.onClick();
  assert.equal(JSON.stringify(sent.at(-1)), JSON.stringify({ cmd: 'plan', text: 'arm for 2 seconds then disarm' }));
  const checked = { ...idle, text: 'arm for 2 seconds then disarm',
    preview: { steps: [{ cmd: 'arm' }, { cmd: 'wait', seconds: 2 }, { cmd: 'disarm' }] } };
  tree = CommandBar({ plan: checked, enabled: true, send: c => sent.push(c) });
  [talk, check, run, cancel] = buttons(tree);
  assert.equal(run.props.disabled, false);
  run.props.onClick();
  assert.equal(JSON.stringify(sent.at(-1)), JSON.stringify({ cmd: 'plan', text: 'arm for 2 seconds then disarm', run: true, plan: checked.preview }),
    'run sends the previewed steps, not the sentence alone');
  tree = CommandBar({ plan: { ...checked, running: true, plan: checked.preview, preview: null }, enabled: true, send: c => sent.push(c) });
  [talk, check, run, cancel] = buttons(tree);
  assert.equal(run.props.disabled, true, 'no second run while one runs');
  cancel.props.onClick();
  assert.equal(JSON.stringify(sent.at(-1)), JSON.stringify({ cmd: 'cancel' }));
}

console.log('PASS: reconnect isolation, stale telemetry, disabled movement, joystick drag/clamp/dead zone/pointer ownership, keyboard release, stop, blur, hidden tab, unmount, mode switch');

// Search must accept exactly the hex IDs shown in the table.
const tableHooks = harness();
let stateSlot = 0;
tableHooks.react.useState = value => [stateSlot++ === 0 ? '0x04 / 0x05' : value, () => {}];
const { StreamsTable } = load('src/telemetry/StreamsTable.tsx', tableHooks.react);
const table = StreamsTable({ stale: false, telemetry: { frames: 1, streams: {
  route: { from: 'Gimbal', to: 'App', cmd_set: 4, cmd_id: 5, count: 1, last_len: 12 },
}, gimbals: {} } });
assert.ok(JSON.stringify(table).includes('1 of 1') || JSON.stringify(table).includes('[1," of ",1," streams"]'), 'displayed command IDs match search');

// Disarm stays available while a motor request is in progress (a start over
// the UART can take many seconds); Arm waits for it.
{
  const { Motors } = load('src/telemetry/FlightPanel.tsx', harness().react,
    { window: { confirm: () => true, isSecureContext: false }, navigator: {} });
  const sent = [];
  const busy = { busy: true, last: 'start', ok: null, message: 'starting motors…', armed: null };
  const [, arm, disarm] = buttons(Motors({ motors: busy, enabled: true, uartOpen: false, send: c => sent.push(c) }));
  assert.equal(arm.props.disabled, true, 'no second start while one runs');
  assert.equal(disarm.props.disabled, false, 'a stop can always be sent');
  disarm.props.onClick();
  assert.equal(JSON.stringify(sent.at(-1)), JSON.stringify({ cmd: 'motors', on: false }));
  console.log('PASS: disarm available during a motor request');
}

// A tap on Talk shorter than the microphone takes to open must not leave it
// recording.
(async () => {
  let opened;
  let recorders = 0;
  let stopped = 0;
  class MediaRecorder {
    static isTypeSupported() { return true; }
    constructor() { recorders++; }
    start() {}
  }
  const navigator = { mediaDevices: { getUserMedia: () => new Promise(resolve => { opened = resolve; }) } };
  const { useVoice } = load('src/telemetry/FlightPanel.tsx', harness().react,
    { window: { isSecureContext: true }, navigator, MediaRecorder });
  const voice = useVoice(() => {});
  const pressed = voice.start();
  voice.stop(); // released before the microphone opened
  opened({ getTracks: () => [{ stop: () => { stopped++; } }] });
  await pressed;
  assert.equal(recorders, 0, 'no recording starts after the release');
  assert.equal(stopped, 1, 'the microphone is closed again');
  // A normal hold still records.
  const held = voice.start();
  opened({ getTracks: () => [{ stop: () => { stopped++; } }] });
  await held;
  assert.equal(recorders, 1);
  console.log('PASS: a quick tap on Talk does not leave the microphone recording');
})().catch(e => { console.error(e); process.exit(1); });
