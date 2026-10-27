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
  vm.runInNewContext(source, {
    exports, require: name => name === 'react' ? react : { jsx, jsxs: jsx }, ...globals,
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
  location: { search: '', protocol: 'https:', hostname: 'orin' },
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
assert.equal(sockets[1].url, 'wss://orin:8081');
const snapshot = frames => ({ data: JSON.stringify({ frames, streams: {}, gimbals: {}, link_up: true }) });
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
    press(); handlers.blur(); assert.equal(sent.at(-1).active, false);
    press(); document.hidden = true; handlers.visibilitychange(); assert.equal(sent.at(-1).active, false);
    press(); unmount(); assert.equal(sent.at(-1).active, false);
  } else {
    assert.equal(up.props.disabled, true);
    unmount();
  }
}
console.log('PASS: reconnect isolation, stale telemetry, disabled movement, joystick drag/clamp/dead zone/pointer ownership, keyboard release, stop, blur, hidden tab, unmount');
