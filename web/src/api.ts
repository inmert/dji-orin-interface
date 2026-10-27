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
  const wsRef = useRef<WebSocket | null>(null);

  useEffect(() => {
    let closed = false;
    let retry: number | undefined;

    const connect = () => {
      if (closed) return;
      setStatus("connecting");
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
          setTelemetry(JSON.parse(ev.data as string) as Telemetry);
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
      const ws = wsRef.current;
      wsRef.current = null;
      ws?.close();
    };
  }, []);

  const send = useCallback((c: Command) => {
    const ws = wsRef.current;
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(c));
  }, []);

  return { status, telemetry, send };
}
