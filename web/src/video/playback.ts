/** MJPEG playback onto a canvas, newest frame first: no jitter buffer, since
 * every JPEG decodes on its own and the feed arrives intact. Keep this
 * outside React: neither network chunks nor animation ticks render UI.
 */
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

/** Holds only the newest decoded frame; an older one is released unseen. */
export class LatestFrame<T> {
  private value: T | undefined;
  constructor(private release: (value: T) => void) {}

  push(value: T) {
    if (this.value !== undefined) this.release(this.value);
    this.value = value;
  }

  take(): T | undefined {
    const value = this.value;
    this.value = undefined;
    return value;
  }

  clear() {
    if (this.value !== undefined) this.release(this.value);
    this.value = undefined;
  }
}

export type PlaybackState = "waiting" | "playing" | "reconnecting";

/** One connection, one decode in flight, newest frame only, automatic recovery. */
export function playMjpeg(canvas: HTMLCanvasElement, url: string,
  onState: (state: PlaybackState) => void): () => void {
  const ctx = canvas.getContext("2d", { alpha: false });
  if (!ctx) throw new Error("Canvas playback is unavailable");
  const controller = new AbortController();
  const latest = new LatestFrame<ImageBitmap>(bitmap => bitmap.close());
  // The newest JPEG not yet decoded; one that is overtaken is never decoded.
  let pending: EncodedFrame | undefined;
  let decoding = false;
  let stopped = false;
  let generation = 0;
  let raf = 0;
  let retry: number | undefined;
  let lastDraw = performance.now();
  let state: PlaybackState = "waiting";
  const update = (next: PlaybackState) => {
    if (state !== next) { state = next; onState(next); }
  };

  const decode = async () => {
    if (decoding || stopped) return;
    decoding = true;
    try {
      while (pending && !stopped) {
        const frame = pending;
        pending = undefined;
        const current = generation;
        try {
          const bitmap = await createImageBitmap(new Blob([frame.bytes], { type: "image/jpeg" }));
          if (stopped || current !== generation) bitmap.close();
          else latest.push(bitmap);
        } catch { /* A corrupt JPEG must not terminate the feed. */ }
      }
    } finally { decoding = false; }
  };

  const render = (now: number) => {
    if (stopped) return;
    const bitmap = latest.take();
    if (bitmap) {
      if (canvas.width !== bitmap.width || canvas.height !== bitmap.height) {
        canvas.width = bitmap.width; canvas.height = bitmap.height;
      }
      ctx.drawImage(bitmap, 0, 0);
      bitmap.close();
      lastDraw = now;
      update("playing");
    } else if (now - lastDraw > 1000 && state === "playing") update("waiting");
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
          pending = frame;
        });
        void decode();
      }
    } catch { /* EOF and transport failures share the reconnect path. */ }
    finally {
      await reader?.cancel().catch(() => {});
      reader?.releaseLock();
      generation++;
      pending = undefined;
      latest.clear();
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
    pending = undefined;
    latest.clear();
  };
}
