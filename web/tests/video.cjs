// Deterministic offline tests: arbitrary HTTP chunking, newest-frame display and cleanup.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require('typescript');
const exportsUnderTest = {};
vm.runInNewContext(ts.transpileModule(fs.readFileSync('src/video/playback.ts', 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
}).outputText, { exports: exportsUnderTest, TextDecoder, Uint8Array });
const { MultipartParser, LatestFrame } = exportsUnderTest;
const jpeg = Buffer.from([255, 216, 3, 13, 10, 13, 10, 255, 217]);
const part = time => Buffer.concat([
  Buffer.from(`\r\n--frame\r\nContent-Type: image/jpeg\r\nContent-Length: ${jpeg.length}\r\nX-Timestamp-Ms: ${time}\r\n\r\n`), jpeg,
]);
const stream = Buffer.concat([part(100), part(133), part(166)]);
for (let size = 1; size <= stream.length; size++) {
  const parser = new MultipartParser();
  const frames = [];
  for (let offset = 0; offset < stream.length; offset += size) {
    parser.push(stream.subarray(offset, offset + size), 500, frame => frames.push(frame));
  }
  assert.deepEqual(frames.map(f => f.timestamp), [100, 133, 166]);
  for (const frame of frames) assert.deepEqual(Buffer.from(frame.bytes), jpeg);
}
const parser = new MultipartParser();
for (let i = 0; i < 3000; i++) parser.push(Buffer.from('\r\n'), 0, () => assert.fail());
assert.throws(() => parser.push(Buffer.from('--frame\r\nContent-Length: 999999999\r\n\r\n'), 0, () => {}));

// Idle heartbeats can coalesce with each other and with the next frame.
const idleStream = Buffer.concat([Buffer.from('\r\n\r\n\r\n'), part(100),
  Buffer.from('\r\n\r\n'), part(133)]);
for (let size = 1; size <= idleStream.length; size++) {
  const parser = new MultipartParser();
  const frames = [];
  for (let offset = 0; offset < idleStream.length; offset += size) {
    parser.push(idleStream.subarray(offset, offset + size), 500, f => frames.push(f));
  }
  assert.deepEqual(frames.map(f => f.timestamp), [100, 133]);
}

const released = [];
const latest = new LatestFrame(f => released.push(f));
assert.equal(latest.take(), undefined, 'nothing to draw yet');
latest.push(0);
assert.equal(latest.take(), 0);
assert.equal(latest.take(), undefined, 'a frame is drawn once');
// A burst between two animation frames: only the newest is drawn.
for (let n = 1; n <= 3; n++) latest.push(n);
assert.deepEqual(released, [1, 2], 'overtaken bitmaps closed unseen');
assert.equal(latest.take(), 3);
latest.push(4);
latest.clear();
assert.deepEqual(released, [1, 2, 4], 'unmount frees the retained bitmap');
console.log('PASS: multipart chunk boundaries, idle heartbeats, size limits, newest-frame display and bitmap cleanup');

async function lifecycle() {
  let tick;
  let signal;
  let finishRead;
  let reads = 0, cancelled = 0, unlocked = 0, drawn = 0;
  const bitmaps = [];
  const states = [];
  const module = {};
  const reader = {
    read: async () => ++reads === 1 ? { value: stream, done: false }
      : new Promise(resolve => { finishRead = resolve; }),
    cancel: async () => { cancelled++; },
    releaseLock: () => { unlocked++; },
  };
  vm.runInNewContext(ts.transpileModule(fs.readFileSync('src/video/playback.ts', 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
  }).outputText, {
    exports: module, TextDecoder, Uint8Array, AbortController, Blob,
    performance: { now: () => 1000 },
    requestAnimationFrame: fn => { tick = fn; return 1; }, cancelAnimationFrame: () => {},
    clearTimeout: () => {}, window: { setTimeout: () => assert.fail('no retry after stop') },
    fetch: async (_url, options) => {
      signal = options.signal;
      signal.addEventListener('abort', () => finishRead?.({ done: true }));
      return { ok: true, body: { getReader: () => reader } };
    },
    createImageBitmap: async () => {
      const bitmap = { width: 640, height: 360, closed: 0, close() { this.closed++; } };
      bitmaps.push(bitmap);
      return bitmap;
    },
  });
  const canvas = { width: 0, height: 0, getContext: () => ({ drawImage: () => drawn++ }) };
  const stop = module.playMjpeg(canvas, '/video/test.mjpg', state => states.push(state));
  for (let n = 0; n < 12; n++) await Promise.resolve();
  tick(1016);
  assert.equal(drawn, 1, 'paint the newest frame on the next animation frame');
  assert.equal(bitmaps.length, 1, 'a burst of three JPEGs decodes only the newest');
  assert.equal(canvas.width, 640);
  assert.deepEqual(states, ['playing'], 'frame ticks do not drive React updates');
  stop();
  for (let n = 0; n < 12; n++) await Promise.resolve();
  assert.ok(signal.aborted, 'stop aborts streaming HTTP request');
  assert.equal(cancelled, 1);
  assert.equal(unlocked, 1);
  assert.ok(bitmaps.every(b => b.closed === 1), 'all decoded bitmaps closed exactly once');
  console.log('PASS: async decode, canvas paint, bounded state updates, stop/abort and reader/bitmap cleanup');
}
lifecycle().catch(error => { console.error(error); process.exitCode = 1; });
