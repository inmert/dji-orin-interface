// Deterministic offline tests: arbitrary HTTP chunking, jitter and bounded latency.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require('typescript');
const exportsUnderTest = {};
vm.runInNewContext(ts.transpileModule(fs.readFileSync('src/video/playback.ts', 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
}).outputText, { exports: exportsUnderTest, TextDecoder, Uint8Array });
const { MultipartParser, PlayoutBuffer, BUFFER_MS } = exportsUnderTest;
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
const buffer = new PlayoutBuffer(f => released.push(f));
// Three frames arrive in one burst but retain their individual presentation times.
for (let n = 0; n < 3; n++) buffer.push(n, n * 33, 100);
assert.equal(buffer.take(100 + BUFFER_MS - 66 - 1), undefined);
assert.equal(buffer.take(100 + BUFFER_MS - 66), 0);
assert.equal(buffer.take(100 + BUFFER_MS - 33), 1);
assert.equal(buffer.take(100 + BUFFER_MS), 2);
assert.equal(buffer.take(500), undefined);
for (let n = 3; n < 23; n++) buffer.push(n, n * 33, 100 + n * 33);
assert.equal(released.length, 12, 'only eight queued bitmaps retained');
assert.equal(buffer.take(2000), 22, 'late renderer jumps to newest due frame');
assert.equal(released.length, 19, 'skipped bitmaps closed');
buffer.push(23, 2000, 2100);
assert.equal(buffer.take(2100 + BUFFER_MS - 1), undefined, 'stall rebuffers');
assert.equal(buffer.take(2100 + BUFFER_MS), 23);
buffer.push(24, 2033, 2133);
buffer.push(25, 0, 2200); // server restart
assert.ok(released.includes(24));
buffer.clear();
assert.ok(released.includes(25), 'unmount frees retained bitmap');
console.log('PASS: multipart chunk boundaries, idle heartbeats, size limits, burst pacing, bounded backlog, stalls, restart and bitmap cleanup');

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
  tick(1120);
  assert.equal(drawn, 1, 'paint only newest due bitmap after delayed animation');
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
