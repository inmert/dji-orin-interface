import { useCallback, useEffect, useRef, useState } from "react";

// --- Telemetry shapes (mirror crates/protocol/src/telemetry.rs,
//     crates/protocol/src/gimbal/attitude.rs, and Snapshot in apps/server/src/ws.rs) ---

export interface GimbalAttitude {
  index: number;
  pitch: number;
  roll: number;
  yaw: number;
  raw: string;
  provisional: boolean;
}

export interface StreamStat {
  from: string;
  to: string;
  cmd_set: number;
  cmd_id: number;
  count: number;
  last_len: number;
}

export interface Telemetry {
  link_up: boolean;
  frames: number;
  gimbals: Record<string, GimbalAttitude>;
  streams: Record<string, StreamStat>;
  /** Server-authoritative camera-link state, so a refreshed page reflects it. */
  linked: boolean;
}

// --- Commands (mirror WsCommand in apps/server/src/state.rs) ---

export type Command =
  | { cmd: "gimbal"; camera: number; pitch: number; yaw: number; active: boolean }
  | { cmd: "link"; on: boolean }
  | { cmd: "stop" };

export type Status = "connecting" | "open" | "closed";

const EMPTY: Telemetry = { link_up: false, frames: 0, gimbals: {}, streams: {}, linked: false };

const record = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);
const finite = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value);
const counter = (value: unknown): value is number =>
  finite(value) && Number.isSafeInteger(value) && value >= 0;
const byte = (value: unknown): value is number => counter(value) && value <= 255;

// JSON parsing alone does not validate a snapshot. Reject the whole message
// before it changes freshness or reaches components that dereference its rows.
export function isTelemetry(value: unknown): value is Telemetry {
  if (!record(value) || !counter(value.frames) || typeof value.link_up !== "boolean"
    || typeof value.linked !== "boolean" || !record(value.gimbals) || !record(value.streams)) return false;
  return Object.values(value.gimbals).every(g => record(g)
    && counter(g.index) && g.index <= 7
    && finite(g.pitch) && finite(g.roll) && finite(g.yaw)
    && typeof g.raw === "string" && typeof g.provisional === "boolean")
    && Object.values(value.streams).every(s => record(s)
      && typeof s.from === "string" && typeof s.to === "string"
      && byte(s.cmd_set) && byte(s.cmd_id) && counter(s.count) && counter(s.last_len));
}

/** WebSocket link to the Orin server: telemetry down, commands up, auto-reconnect. */
export function useLink() {
  const [status, setStatus] = useState<Status>("connecting");
  const [telemetry, setTelemetry] = useState<Telemetry>(EMPTY);
  const [stale, setStale] = useState(true);
  const wsRef = useRef<WebSocket | null>(null);

  useEffect(() => {
    // Explicit offline preview never opens a control connection.
    if (new URLSearchParams(location.search).has("preview")) {
      setStatus("closed");
      return;
    }
    let closed = false;
    let retry: number | undefined;
    let lastFrameChange = 0;
    let previousFrames: number | undefined;
    const freshness = window.setInterval(() => {
      setStale(Date.now() - lastFrameChange > 2000);
    }, 500);

    const connect = () => {
      if (closed) return;
      setStatus("connecting");
      setStale(true);
      lastFrameChange = 0;
      previousFrames = undefined;
      const scheme = location.protocol === "https:" ? "wss" : "ws";
      const url = `${scheme}://${location.hostname}:8081`;
      const ws = new WebSocket(url);
      wsRef.current = ws;
      ws.onopen = () => {
        if (!closed && wsRef.current === ws) setStatus("open");
      };
      ws.onmessage = (ev) => {
        if (closed || wsRef.current !== ws) return;
        try {
          const next: unknown = JSON.parse(ev.data as string);
          if (!isTelemetry(next)) return;
          if (next.frames !== previousFrames) {
            // Require an advancing counter, not a cached snapshot after reconnect.
            if (previousFrames !== undefined && next.frames > previousFrames) {
              lastFrameChange = Date.now();
              setStale(false);
            } else {
              lastFrameChange = 0;
              setStale(true);
            }
            previousFrames = next.frames;
          }
          setTelemetry(next);
        } catch {
          /* ignore malformed frame */
        }
      };
      const reconnect = () => {
        if (closed || wsRef.current !== ws) return;
        setStatus("closed");
        wsRef.current = null;
        if (!closed) retry = window.setTimeout(connect, 1000);
      };
      ws.onclose = reconnect;
      ws.onerror = () => ws.close();
    };

    connect();
    return () => {
      closed = true;
      if (retry) clearTimeout(retry);
      clearInterval(freshness);
      const ws = wsRef.current;
      wsRef.current = null;
      ws?.close();
    };
  }, []);

  const send = useCallback((c: Command) => {
    const ws = wsRef.current;
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(c));
  }, []);

  return { status, telemetry, stale, send };
}
