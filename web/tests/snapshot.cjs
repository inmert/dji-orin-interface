// Offline contract check: the snapshot the server sends (snapshot.json,
// written by the_web_fixture_is_the_snapshot_the_server_sends in
// apps/server/src/web/ws.rs) passes the UI's validators, and a malformed
// copy of any section does not.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require('typescript');

const api = {};
const react = { useCallback: f => f, useEffect: () => {}, useRef: v => ({ current: v }), useState: v => [v, () => {}] };
vm.runInNewContext(ts.transpileModule(fs.readFileSync('src/api.ts', 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
}).outputText, { exports: api, require: name => name === 'react' ? react : {}, URLSearchParams, location: { search: '' } });

const fixture = JSON.parse(fs.readFileSync('tests/snapshot.json', 'utf8'));
const copy = () => JSON.parse(JSON.stringify(fixture));

assert.ok(api.isTelemetry(fixture), 'the server snapshot passes validation');
assert.ok(api.isFlightLink(fixture.flight) && api.isUsbLink(fixture.usb));
assert.ok(api.isThermalReport(fixture.thermal) && api.isMotorStatus(fixture.motors));
assert.ok(api.isPlanStatus(fixture.plan) && Object.values(fixture.video).every(api.isVideoStatus));
console.log('PASS: server snapshot accepted by every validator');

// Every step kind the server sends reads as a phrase.
assert.deepEqual(fixture.plan.plan.steps.map(api.stepText), [
  'arm', 'wait 2.5 s', 'pan camera 2 left 1 s', 'tilt camera 1 up 0.5 s',
  'recenter both cameras', 'recenter camera 2', 'link cameras', 'disarm',
]);
console.log('PASS: every plan step kind has a phrase');

// Each malformed section rejects the message.
const broken = [
  ['negative frame counter', s => { s.frames = -1; }],
  ['link state not a boolean', s => { s.link_up = 1; }],
  ['gimbal index out of range', s => { s.gimbals['0'].index = 9; }],
  ['stream command set over a byte', s => { s.streams[Object.keys(s.streams)[0]].cmd_set = 256; }],
  ['flight status name not a string', s => { s.flight.data.status.flight_name = 5; }],
  ['GPS velocity not a vector', s => { s.flight.data.gps.velocity_ned = [1, 2]; }],
  ['USB velocity not a vector', s => { s.usb.osd.unverified.velocity = [0, 0]; }],
  ['unknown video state', s => { s.video.cam1.state = 'playing'; }],
  ['unknown last motor request', s => { s.motors.last = 'launch'; }],
  ['plan step with a string duration', s => { s.plan.plan.steps[1].seconds = '2.5'; }],
  ['preview step without its flag', s => { delete s.plan.preview.steps[6].on; }],
  ['plan step counter negative', s => { s.plan.step = -1; }],
];
for (const [what, mutate] of broken) {
  const s = copy();
  mutate(s);
  assert.equal(api.isTelemetry(s), false, what);
}
// Sections an older server leaves out are optional.
const old = copy();
for (const k of ['flight', 'usb', 'thermal', 'thermal_auto', 'video', 'motors', 'plan']) delete old[k];
assert.ok(api.isTelemetry(old), 'a snapshot without the optional sections');
console.log('PASS: malformed sections rejected, absent optional sections accepted');

// A step kind the server added before this UI knew it is shown by name, not
// a reason to drop every snapshot.
{
  const s = copy();
  s.plan.preview.steps.push({ cmd: 'takeoff', height: 2 });
  assert.ok(api.isTelemetry(s), 'an unknown step kind is accepted');
  assert.equal(api.stepText({ cmd: 'takeoff' }), 'takeoff');
  const bad = copy();
  bad.plan.preview.steps.push({ cmd: 7 });
  assert.equal(api.isTelemetry(bad), false, 'a step must still name its kind');
  // A section with a reading the server could not represent is dropped
  // on its own.
  const nan = copy();
  nan.usb.osd.unverified.latitude = null;
  assert.deepEqual([...api.detachMalformed(nan)], ['usb']);
  assert.ok(api.isTelemetry(nan) && nan.usb === undefined && nan.flight !== undefined);
  console.log('PASS: unknown step kinds and unrepresentable readings degrade, not freeze');
}
