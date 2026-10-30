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

// --- OSDK UART flight data (mirror crates/protocol/src/osdk/broadcast.rs) ---

export type Vec3 = [number, number, number];

export interface FlightData {
  time_ms: number | null;
  attitude: { roll: number; pitch: number; yaw: number } | null;
  acceleration: Vec3 | null;
  velocity: { x: number; y: number; z: number; gps: boolean } | null;
  angular_rate: Vec3 | null;
  position: { latitude: number; longitude: number; altitude: number; height: number; health: number } | null;
  obstacles: { down: number; front: number; right: number; back: number; left: number; up: number; health: number } | null;
  gps: {
    date: number; time: number; latitude: number; longitude: number; hfsl: number;
    velocity_ned: Vec3; hdop: number; pdop: number; fix: number; satellites: number;
  } | null;
  mag: Vec3 | null;
  rc: { roll: number; pitch: number; yaw: number; throttle: number; mode: number; gear: number } | null;
  gimbal: { roll: number; pitch: number; yaw: number } | null;
  status: {
    flight: number; flight_name: string; mode: number; mode_name: string;
    gear: number; gear_name: string; error: number;
  } | null;
  battery: { capacity: number; voltage: number; current: number; percent: number } | null;
  compass: number | null;
  last_flags: number;
}

export interface FlightLink {
  /** Serial port being read; empty when the reader is disabled. */
  port: string;
  open: boolean;
  frames: number;
  rejected: number;
  data: FlightData;
}

// --- USB API port DUML (mirror UsbLink in crates/protocol/src/telemetry.rs
//     and crates/protocol/src/flyc/osd.rs) ---

export interface FcOsd {
  /** Degrees. */
  pitch: number;
  roll: number;
  yaw: number;
  mode: number;
  mode_name: string;
  satellites: number;
  /** Offsets only seen at zero so far. */
  unverified: { latitude: number; longitude: number; height: number; velocity: Vec3 };
}

export interface UsbLink {
  port: string;
  open: boolean;
  frames: number;
  osd: FcOsd | null;
  /** Gimbal attitude per device index, wire units of 0.1°. */
  gimbals: Record<string, GimbalAttitude>;
}

/** One camera video feed (VideoStatus in apps/server/src/video.rs). */
export interface VideoStatus {
  source: string;
  state: "idle" | "connecting" | "streaming" | "error";
  viewers: number;
  frames: number;
  bytes: number;
  dropped: number;
  error: string | null;
}

/** Thermal camera report (Report in crates/protocol/src/thermal.rs). */
export interface ThermalReport {
  /** Spot-meter position, normalised 0..1 over the thermal image. */
  spot_x: number;
  spot_y: number;
  /** °C at the spot. */
  spot_temp: number;
  spot_valid: boolean;
  /** 0 visual, 2 picture-in-picture, 3 thermal; null if not reported. */
  display_mode: number | null;
  /** What the camera measures: 0 off, 1 spot, 2 area. */
  measure: number;
  /** Isotherm band, °C: colours highlight temperatures in it. */
  isotherm_enabled: boolean;
  isotherm_lower: number;
  isotherm_middle: number;
  isotherm_upper: number;
  /** Area statistics in area measurement; positions normalised 0..1 over the thermal image. */
  area: ThermalArea | null;
}

export interface ThermalArea {
  rect: [number, number, number, number];
  avg: number;
  min: number;
  max: number;
  min_x: number;
  min_y: number;
  max_x: number;
  max_y: number;
}

export interface Telemetry {
  link_up: boolean;
  frames: number;
  gimbals: Record<string, GimbalAttitude>;
  streams: Record<string, StreamStat>;
  /** OSDK UART flight data; absent from servers that predate it. */
  flight?: FlightLink;
  /** Aircraft USB API port DUML; absent from servers that predate it. */
  usb?: UsbLink;
  /** Server-authoritative camera-link state, so a refreshed page reflects it. */
  linked: boolean;
  /** Latest thermal camera report; absent until the thermal camera reports. */
  thermal?: ThermalReport | null;
  /** Whether the server keeps the isotherm band fitted to the scene. */
  thermal_auto?: boolean;
  /** Camera video feeds by name (`cam1`, `cam2`, `fpv`); absent from older servers. */
  video?: Record<string, VideoStatus>;
  /** Motor start/stop progress; absent from older servers. */
  motors?: MotorStatus;
  /** The command plan being run or last checked; absent from older servers. */
  plan?: PlanStatus;
}

/** One plan step (Command in crates/commands/src/lib.rs). */
export type PlanStep =
  | { cmd: "arm" }
  | { cmd: "disarm" }
  | { cmd: "wait"; seconds: number }
  | { cmd: "pan"; camera: number; direction: "left" | "right"; seconds: number }
  | { cmd: "tilt"; camera: number; direction: "up" | "down"; seconds: number }
  | { cmd: "recenter"; camera: number | null }
  | { cmd: "link"; on: boolean };

/** A step as a short phrase, for the command bar. */
export function stepText(s: PlanStep): string {
  switch (s.cmd) {
    case "wait": return `wait ${s.seconds} s`;
    case "pan": return `pan camera ${s.camera} ${s.direction} ${s.seconds} s`;
    case "tilt": return `tilt camera ${s.camera} ${s.direction} ${s.seconds} s`;
    case "recenter": return s.camera === null ? "recenter both cameras" : `recenter camera ${s.camera}`;
    case "link": return s.on ? "link cameras" : "unlink cameras";
    default: return s.cmd;
  }
}

/** Voice transcription result (`POST /voice`). */
export interface Transcript {
  text: string;
  ms: number;
  provider: string;
}

/** One speech-recognition provider's state (`GET /voice/status`). */
export interface VoiceProvider {
  provider: string;
  /** The one hold-to-talk uses. */
  active: boolean;
  model: string;
  /** Whether the service accepted the key; null when it could not be asked. */
  key_ok: boolean | null;
  /** Balance or spend when the service exposes it, e.g. "$4.12 left". */
  balance: string | null;
  note: string;
}

export const isVoiceProvider = (v: unknown): v is VoiceProvider =>
  record(v) && typeof v.provider === "string" && typeof v.active === "boolean"
  && typeof v.model === "string" && optional(v.key_ok, b => typeof b === "boolean")
  && optional(v.balance, b => typeof b === "string") && typeof v.note === "string";

/** Fetch every provider's status; an empty list when none is configured. */
export async function fetchVoiceStatus(): Promise<VoiceProvider[]> {
  const res = await fetch("/voice/status", { cache: "no-store" });
  if (!res.ok) throw new Error(`status ${res.status}`);
  const body: unknown = await res.json();
  return Array.isArray(body) ? body.filter(isVoiceProvider) : [];
}

/** Progress of the command executor (Status in crates/vehicle/src/executor.rs). */
export interface PlanStatus {
  running: boolean;
  plan: { steps: PlanStep[] } | null;
  text: string;
  step: number;
  message: string;
  ok: boolean | null;
  preview: { steps: PlanStep[] } | null;
  preview_error: string | null;
}

/** Motor start/stop progress (Status in apps/server/src/motors.rs). */
export interface MotorStatus {
  busy: boolean;
  /** The last request. */
  last: "start" | "stop" | "usb_flight" | null;
  ok: boolean | null;
  message: string;
  /** Motors running; null when neither the UART nor the RC link is live. */
  armed?: boolean | null;
  /** Where `armed` came from: "UART" or "RC link". */
  armed_from?: string | null;
  /** Arming with the USB API port connected: no_usb, locked, allowed or unknown. */
  usb_flight?: string;
}

// --- Commands (mirror WsCommand in apps/server/src/state.rs) ---

export type Command =
  | { cmd: "gimbal"; camera: number; pitch: number; yaw: number; active: boolean }
  | { cmd: "link"; on: boolean }
  /** Yaw recenter; omit `camera` for both. While linked the server resets the master. */
  | { cmd: "reset"; camera?: number }
  /** Move the thermal spot meter (normalised over the thermal image). */
  | { cmd: "spot"; x: number; y: number }
  /** Thermal camera display mode: 0 visual, 2 picture-in-picture, 3 thermal. */
  | { cmd: "thermal_mode"; mode: 0 | 2 | 3 }
  /** Isotherm band in °C; the server orders the writes so lower ≤ middle ≤ upper holds. */
  | { cmd: "isotherm"; lower: number; middle: number; upper: number }
  /** Keep the isotherm band fitted to the scene (full-frame area measurement). */
  | { cmd: "thermal_auto"; on: boolean }
  /** Start (on) or stop all motors over the OSDK UART. */
  | { cmd: "motors"; on: boolean }
  /** Allow arming with the aircraft's USB API port connected (until restart). */
  | { cmd: "usb_flight" }
  /** A command sentence: checked and shown back, or run when `run` is true. */
  | { cmd: "plan"; text: string; run?: boolean }
  /** Stop the running plan after its current step. */
  | { cmd: "cancel" }
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

const nums = (value: unknown, keys: string[]) =>
  record(value) && keys.every(k => finite(value[k]));
const vec3 = (value: unknown): value is Vec3 =>
  Array.isArray(value) && value.length === 3 && value.every(finite);
const optional = (value: unknown, check: (v: unknown) => boolean) =>
  value === null || value === undefined || check(value);

// Every field a flight component dereferences is type-checked here, so a
// malformed section is rejected rather than rendered.
export function isFlightLink(value: unknown): value is FlightLink {
  if (!record(value) || typeof value.port !== "string" || typeof value.open !== "boolean"
    || !counter(value.frames) || !counter(value.rejected) || !record(value.data)) return false;
  const d = value.data;
  return optional(d.time_ms, counter)
    && optional(d.attitude, v => nums(v, ["roll", "pitch", "yaw"]))
    && optional(d.acceleration, vec3)
    && optional(d.velocity, v => nums(v, ["x", "y", "z"]) && typeof (v as { gps: unknown }).gps === "boolean")
    && optional(d.angular_rate, vec3)
    && optional(d.position, v => nums(v, ["latitude", "longitude", "altitude", "height", "health"]))
    && optional(d.obstacles, v => nums(v, ["down", "front", "right", "back", "left", "up", "health"]))
    && optional(d.gps, v => nums(v, ["date", "time", "latitude", "longitude", "hfsl", "hdop", "pdop", "fix", "satellites"])
      && vec3((v as { velocity_ned: unknown }).velocity_ned))
    && optional(d.mag, vec3)
    && optional(d.rc, v => nums(v, ["roll", "pitch", "yaw", "throttle", "mode", "gear"]))
    && optional(d.gimbal, v => nums(v, ["roll", "pitch", "yaw"]))
    && optional(d.status, v => nums(v, ["flight", "mode", "gear", "error"])
      && ["flight_name", "mode_name", "gear_name"].every(k => typeof (v as Record<string, unknown>)[k] === "string"))
    && optional(d.battery, v => nums(v, ["capacity", "voltage", "current", "percent"]))
    && optional(d.compass, byte)
    && counter(d.last_flags);
}

const isGimbal = (g: unknown) => record(g)
  && counter(g.index) && g.index <= 7
  && finite(g.pitch) && finite(g.roll) && finite(g.yaw)
  && typeof g.raw === "string" && typeof g.provisional === "boolean";

export function isUsbLink(value: unknown): value is UsbLink {
  return record(value) && typeof value.port === "string" && typeof value.open === "boolean"
    && counter(value.frames) && record(value.gimbals)
    && Object.values(value.gimbals).every(isGimbal)
    && optional(value.osd, o => nums(o, ["pitch", "roll", "yaw", "mode", "satellites"])
      && typeof (o as Record<string, unknown>).mode_name === "string"
      && nums((o as Record<string, unknown>).unverified, ["latitude", "longitude", "height"])
      && vec3(((o as Record<string, unknown>).unverified as Record<string, unknown>).velocity));
}

const VIDEO_STATES = ["idle", "connecting", "streaming", "error"];

export function isThermalReport(value: unknown): value is ThermalReport {
  if (!nums(value, ["spot_x", "spot_y", "spot_temp", "isotherm_lower", "isotherm_middle", "isotherm_upper"])) {
    return false;
  }
  const v = value as Record<string, unknown>;
  return typeof v.spot_valid === "boolean" && typeof v.isotherm_enabled === "boolean"
    && byte(v.measure) && optional(v.display_mode, byte)
    && optional(v.area, a => nums(a, ["avg", "min", "max", "min_x", "min_y", "max_x", "max_y"])
      && Array.isArray((a as Record<string, unknown>).rect)
      && ((a as Record<string, unknown>).rect as unknown[]).length === 4
      && ((a as Record<string, unknown>).rect as unknown[]).every(finite));
}

export function isVideoStatus(value: unknown): value is VideoStatus {
  return record(value) && typeof value.source === "string"
    && typeof value.state === "string" && VIDEO_STATES.includes(value.state)
    && counter(value.viewers) && counter(value.frames) && counter(value.bytes)
    && counter(value.dropped) && optional(value.error, e => typeof e === "string");
}

/** The port a direct reader shows while it finds none (NO_PORT in apps/server/src/flight.rs). */
export const NO_PORT = "no port found";

/** URL of a feed's MJPEG stream; opening it starts that feed on the server. */
export const videoUrl = (feed: string) =>
  `/video/${encodeURIComponent(feed)}.mjpg`;

const bool = (value: unknown): value is boolean => typeof value === "boolean";

export function isMotorStatus(value: unknown): value is MotorStatus {
  return record(value) && bool(value.busy) && typeof value.message === "string"
    && optional(value.last, v => v === "start" || v === "stop" || v === "usb_flight")
    && optional(value.ok, bool) && optional(value.armed, bool)
    && optional(value.armed_from, v => typeof v === "string")
    && optional(value.usb_flight, v => typeof v === "string");
}

const isPlanStep = (s: unknown) => record(s) && (
  s.cmd === "arm" || s.cmd === "disarm"
  || (s.cmd === "wait" && finite(s.seconds))
  || ((s.cmd === "pan" || s.cmd === "tilt") && finite(s.camera) && typeof s.direction === "string" && finite(s.seconds))
  || (s.cmd === "recenter" && optional(s.camera, finite))
  || (s.cmd === "link" && typeof s.on === "boolean"));
const isPlan = (p: unknown) => record(p) && Array.isArray(p.steps) && p.steps.every(isPlanStep);

export function isPlanStatus(value: unknown): value is PlanStatus {
  return record(value) && bool(value.running) && typeof value.text === "string"
    && counter(value.step) && typeof value.message === "string"
    && optional(value.ok, bool) && optional(value.plan, isPlan)
    && optional(value.preview, isPlan) && optional(value.preview_error, e => typeof e === "string");
}

// JSON parsing alone does not validate a snapshot. Reject the whole message
// before it changes freshness or reaches components that dereference its rows.
export function isTelemetry(value: unknown): value is Telemetry {
  if (!record(value) || !counter(value.frames) || typeof value.link_up !== "boolean"
    || typeof value.linked !== "boolean" || !record(value.gimbals) || !record(value.streams)
    || (value.flight !== undefined && !isFlightLink(value.flight))
    || (value.usb !== undefined && !isUsbLink(value.usb))
    || !optional(value.video, v => record(v) && Object.values(v).every(isVideoStatus))
    || !optional(value.thermal, isThermalReport)
    || !optional(value.thermal_auto, v => typeof v === "boolean")
    || !optional(value.motors, isMotorStatus)
    || !optional(value.plan, isPlanStatus)) return false;
  return Object.values(value.gimbals).every(isGimbal)
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
      // Same origin, same port: the server upgrades `/ws` on both listeners.
      const scheme = location.protocol === "https:" ? "wss" : "ws";
      const url = `${scheme}://${location.host}/ws`;
      const ws = new WebSocket(url);
      wsRef.current = ws;
      ws.onopen = () => {
        if (!closed && wsRef.current === ws) setStatus("open");
      };
      ws.onmessage = (ev) => {
        if (closed || wsRef.current !== ws) return;
        try {
          const next: unknown = JSON.parse(ev.data as string);
          // A malformed thermal report must not hide the rest of the snapshot.
          if (record(next) && next.thermal != null && !isThermalReport(next.thermal)) {
            next.thermal = null;
          }
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
