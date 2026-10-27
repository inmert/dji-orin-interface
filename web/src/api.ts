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
          const next = JSON.parse(ev.data as string) as Telemetry;
          if (!next || typeof next.frames !== "number" || !next.gimbals || !next.streams) return;
          if (next.frames !== previousFrames) {
            // Require an advancing counter, not a cached snapshot after reconnect.
            if (previousFrames !== undefined) {
              lastFrameChange = Date.now();
              setStale(false);
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
