/** Small, bounded jitter buffer for independently decodable JPEG frames.
 * Keep this outside React: neither network chunks nor animation ticks render UI.
 */
export const BUFFER_MS = 120;
const MAX_FRAMES = 8;
const MAX_JPEG = 2 * 1024 * 1024;
const MAX_HEADER = 4096;

export interface EncodedFrame {
  bytes: Uint8Array;
  timestamp: number;
}

/** Content-Length framing, so JPEG metadata containing marker bytes is harmless. */
export class MultipartParser {
  private buffer = new Uint8Array(0);
  private length = 0;
  private timestamp = 0;

  push(chunk: Uint8Array, now: number, emit: (frame: EncodedFrame) => void) {
    const joined = new Uint8Array(this.buffer.length + chunk.length);
    joined.set(this.buffer);
    joined.set(chunk, this.buffer.length);
    this.buffer = joined;
    for (;;) {
      if (!this.length) {
        // Inter-part CRLF and idle heartbeats may arrive together or split anywhere.
        let padding = 0;
        while (padding < this.buffer.length
          && (this.buffer[padding] === 13 || this.buffer[padding] === 10)) padding++;
        this.buffer = this.buffer.subarray(padding);
        let end = -1;
        for (let i = 0; i + 3 < this.buffer.length; i++) {
          if (this.buffer[i] === 13 && this.buffer[i + 1] === 10
            && this.buffer[i + 2] === 13 && this.buffer[i + 3] === 10) {
            end = i; break;
          }
        }
        if (end < 0) {
          // An idle server sends CRLF heartbeats between parts.
          if (this.buffer.every(b => b === 13 || b === 10)) this.buffer = new Uint8Array(0);
          else if (this.buffer.length > MAX_HEADER) throw new Error("Video headers too large");
          return;
        }
        if (end > MAX_HEADER) throw new Error("Video headers too large");
        const header = new TextDecoder().decode(this.buffer.subarray(0, end));
        const length = Number(/(?:^|\r\n)content-length:\s*(\d+)/i.exec(header)?.[1]);
        if (!Number.isSafeInteger(length) || length < 4 || length > MAX_JPEG) {
          throw new Error("Invalid video frame length");
        }
        const timestamp = /(?:^|\r\n)x-timestamp-ms:\s*(\d+)/i.exec(header)?.[1];
        this.timestamp = timestamp === undefined ? now : Number(timestamp);
        this.length = length;
        this.buffer = this.buffer.slice(end + 4);
      }
      if (this.buffer.length < this.length) return;
      emit({ bytes: this.buffer.slice(0, this.length), timestamp: this.timestamp });
      this.buffer = this.buffer.slice(this.length);
      this.length = 0;
    }
  }
}

interface Timed<T> { value: T; timestamp: number }

/** Server monotonic time mapped to local time; no synchronized clocks needed. */
export class PlayoutBuffer<T> {
  private frames: Timed<T>[] = [];
  private offset: number | undefined;
  private latest: number | undefined;
  constructor(private release: (value: T) => void) {}

  push(value: T, timestamp: number, now: number) {
    if (this.latest !== undefined && (timestamp < this.latest || timestamp - this.latest > 500)) {
      this.clear(); // reconnect, source restart, or a stall: rebuffer at live edge
    }
    this.latest = timestamp;
    this.offset = Math.min(this.offset ?? Infinity, now - timestamp);
    this.frames.push({ value, timestamp });
    while (this.frames.length > MAX_FRAMES) this.release(this.frames.shift()!.value);
  }

  take(now: number): T | undefined {
    if (this.offset === undefined) return;
    let selected: T | undefined;
    while (this.frames.length && this.frames[0].timestamp + this.offset + BUFFER_MS <= now) {
      if (selected !== undefined) this.release(selected);
      selected = this.frames.shift()!.value;
    }
    return selected;
  }

  clear() {
    for (const frame of this.frames) this.release(frame.value);
    this.frames = [];
    this.offset = undefined;
    this.latest = undefined;
  }
}

export type PlaybackState = "buffering" | "playing" | "reconnecting";

/** One connection, one decode in flight, bounded queues, automatic recovery. */
export function playMjpeg(canvas: HTMLCanvasElement, url: string,
  onState: (state: PlaybackState) => void): () => void {
  const ctx = canvas.getContext("2d", { alpha: false });
  if (!ctx) throw new Error("Canvas playback is unavailable");
  const controller = new AbortController();
  const playout = new PlayoutBuffer<ImageBitmap>(bitmap => bitmap.close());
  let pending: EncodedFrame[] = [];
  let decoding = false;
  let stopped = false;
  let generation = 0;
  let raf = 0;
  let retry: number | undefined;
  let lastDraw = performance.now();
  let state: PlaybackState = "buffering";
  const update = (next: PlaybackState) => {
    if (state !== next) { state = next; onState(next); }
  };

  const decode = async () => {
    if (decoding || stopped) return;
    decoding = true;
    try {
      while (pending.length && !stopped) {
        const frame = pending.shift()!;
        const current = generation;
        try {
          const bitmap = await createImageBitmap(new Blob([frame.bytes], { type: "image/jpeg" }));
          if (stopped || current !== generation) bitmap.close();
          else playout.push(bitmap, frame.timestamp, performance.now());
        } catch { /* A corrupt JPEG must not terminate the feed. */ }
      }
    } finally { decoding = false; }
  };

  const render = (now: number) => {
    if (stopped) return;
    const bitmap = playout.take(now);
    if (bitmap) {
      if (canvas.width !== bitmap.width || canvas.height !== bitmap.height) {
        canvas.width = bitmap.width; canvas.height = bitmap.height;
      }
      ctx.drawImage(bitmap, 0, 0);
      bitmap.close();
      lastDraw = now;
      update("playing");
    } else if (now - lastDraw > 1000 && state === "playing") update("buffering");
    raf = requestAnimationFrame(render);
  };

  const connect = async () => {
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    try {
      const response = await fetch(url, { signal: controller.signal, cache: "no-store" });
      if (!response.ok || !response.body) throw new Error("Video connection failed");
      reader = response.body.getReader();
      const parser = new MultipartParser();
      for (;;) {
        const { value, done } = await reader.read();
        if (done || stopped) break;
        parser.push(value, performance.now(), frame => {
          pending.push(frame);
          // Drop only complete JPEGs. Never discard compressed H.264 payloads.
          if (pending.length > MAX_FRAMES) pending.shift();
        });
        void decode();
      }
    } catch { /* EOF and transport failures share the reconnect path. */ }
    finally {
      await reader?.cancel().catch(() => {});
      reader?.releaseLock();
      generation++;
      pending = [];
      playout.clear();
      if (!stopped) {
        update("reconnecting");
        retry = window.setTimeout(() => void connect(), 1000);
      }
    }
  };

  raf = requestAnimationFrame(render);
  void connect();
  return () => {
    stopped = true;
    generation++;
    controller.abort();
    clearTimeout(retry);
    cancelAnimationFrame(raf);
    pending = [];
    playout.clear();
  };
}
