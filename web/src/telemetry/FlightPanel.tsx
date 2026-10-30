import { useEffect, useRef, useState } from "react";
import { AiProvider, AiStatus, Command, FlightData, FlightLink, MotorStatus, NO_PORT, PlanStatus, Transcript, fetchAiStatus, stepText } from "../api";
import { Icon } from "../components/Icon";
import { Row, Tile, deg, fixed, useAdvancing } from "./Tile";

const pad2 = (n: number) => n.toString().padStart(2, "0");

/** hhmmss (as the FC sends it, UTC) → hh:mm:ss. */
function clock(hhmmss: number) {
  const h = Math.floor(hhmmss / 10000);
  const m = Math.floor(hhmmss / 100) % 100;
  return `${pad2(h)}:${pad2(m)}:${pad2(hhmmss % 100)}`;
}

/** Obstacle sensors report 100 m (or more) when nothing is in range. */
const range = (m: number) => (m >= 100 ? "clear" : `${fixed(m, 2)} m`);

const COMPASS = ["Normal", "Needs calibration", "Needs direction fix", "Needs restart"];

/** The RC's flight-mode switch from its channel value (P +8000, S 0, A −8000;
 * verified 2026-10-28). */
export function modeSwitch(v: number) {
  if (v > 4000) return "P";
  if (v < -4000) return "A";
  return "S";
}

/** A centred stick bar, ±10000 → ±50% of the track. */
function Stick({ label, value }: { label: string; value: number }) {
  const v = Math.max(-10000, Math.min(10000, value)) / 10000;
  return (
    <div className="stick">
      <span>{label}</span>
      <span className="stick-track" aria-label={`${label} ${Math.round(v * 100)}%`}>
        <span
          className="stick-fill"
          style={{ left: `${50 + Math.min(0, v) * 50}%`, width: `${Math.abs(v) * 50}%` }}
        />
      </span>
      <span className="mono">{Math.round(v * 100)}%</span>
    </div>
  );
}

function Tiles({ d }: { d: FlightData }) {
  const a = d.attitude;
  const s = d.status;
  const p = d.position;
  const v = d.velocity;
  const g = d.gps;
  const o = d.obstacles;
  const b = d.battery;
  const hasFix = !!g && g.satellites > 0;
  const mag = d.mag ? Math.hypot(...d.mag) : undefined;
  return (
    <div className="flight-grid">
      <Tile label="FLIGHT STATUS" value={s ? s.flight_name : "—"} note={s ? `Mode: ${s.mode_name}` : undefined}>
        {s && <Row label="Landing gear" value={s.gear_name} />}
        {s && <Row label="FC error code" value={s.error.toString()} />}
      </Tile>
      <Tile label="ATTITUDE" value={a ? deg(a.yaw) : "—"} note="Heading (yaw)">
        {a && <Row label="Roll" value={deg(a.roll)} />}
        {a && <Row label="Pitch" value={deg(a.pitch)} />}
      </Tile>
      <Tile label="ALTITUDE" value={p ? `${fixed(p.altitude)} m` : "—"} note="Barometric">
        {p && <Row label="Height" value={`${fixed(p.height, 2)} m`} />}
        {o && <Row label="Ground (down sensor)" value={range(o.down)} />}
      </Tile>
      <Tile
        label="VELOCITY"
        value={v ? `${fixed(Math.hypot(v.x, v.y), 2)} m/s` : "—"}
        note={v ? (v.gps ? "Horizontal · GPS-aided" : "Horizontal · no GPS") : undefined}
      >
        {v && <Row label="Vertical" value={`${fixed(v.z, 2)} m/s`} />}
      </Tile>
      <Tile
        label="GPS"
        value={g ? `${g.satellites} sats` : "—"}
        note={g ? (hasFix ? `Fix ${fixed(g.fix, 0)} · HDOP ${fixed(g.hdop)}` : "No fix") : undefined}
      >
        {g && hasFix && <Row label="Position" value={`${g.latitude.toFixed(6)}, ${g.longitude.toFixed(6)}`} />}
        {g && hasFix && <Row label="Height (MSL)" value={`${fixed(g.hfsl)} m`} />}
        {g && <Row label="Time (UTC)" value={clock(g.time)} />}
      </Tile>
      <Tile label="BATTERY" value={b ? `${b.percent}%` : "—"} note={b ? `${b.capacity.toLocaleString()} mAh remaining` : "No battery section received yet"}>
        {b && <Row label="Voltage" value={`${fixed(b.voltage / 1000, 2)} V`} />}
        {b && <Row label="Current" value={`${fixed(b.current / 1000, 2)} A`} />}
      </Tile>
      <Tile label="OBSTACLES" value={o ? range(Math.min(o.front, o.back, o.left, o.right)) : "—"} note="Nearest horizontal">
        {o && <Row label="Front / back" value={`${range(o.front)} / ${range(o.back)}`} />}
        {o && <Row label="Left / right" value={`${range(o.left)} / ${range(o.right)}`} />}
        {o && <Row label="Up" value={range(o.up)} />}
      </Tile>
      <Tile label="RC STICKS" value={d.rc ? "" : "—"} note={d.rc ? `Mode switch: ${modeSwitch(d.rc.mode)}` : undefined}>
        {d.rc && (
          <>
            <Stick label="Roll" value={d.rc.roll} />
            <Stick label="Pitch" value={d.rc.pitch} />
            <Stick label="Yaw" value={d.rc.yaw} />
            <Stick label="Throttle" value={d.rc.throttle} />
          </>
        )}
      </Tile>
      <Tile label="GIMBAL (FC VIEW)" value={d.gimbal ? deg(d.gimbal.yaw) : "—"} note="Yaw">
        {d.gimbal && <Row label="Pitch" value={deg(d.gimbal.pitch)} />}
        {d.gimbal && <Row label="Roll" value={deg(d.gimbal.roll)} />}
      </Tile>
      <Tile label="COMPASS" value={mag !== undefined ? fixed(mag, 0) : "—"} note="Field strength (normal 1000–2000)">
        {d.compass !== null && <Row label="Status" value={COMPASS[d.compass] ?? `Code ${d.compass}`} />}
      </Tile>
    </div>
  );
}

/** Start/stop all motors (over the RC link when it is up, else the OSDK
 * UART). Starting asks for confirmation. */
const USB_FLIGHT: Record<string, string> = {
  allowed: "On",
  locked: "Off",
  no_usb: "No USB",
  unknown: "?",
};

export function Motors({ motors, enabled, uartOpen, send }: {
  motors: MotorStatus;
  /** The server can send start/stop on some path. */
  enabled: boolean;
  /** Allow with USB needs the UART (it takes control authority there). */
  uartOpen: boolean;
  send: (c: Command) => void;
}) {
  const busy = motors.busy;
  const armed = motors.armed ?? null;
  const usb = motors.usb_flight ?? "unknown";
  const start = () => {
    if (window.confirm("Start all motors? Props off, people clear.")) send({ cmd: "motors", on: true });
  };
  const result = motors.message
    ? `${motors.ok === false ? "Failed: " : ""}${motors.message}`
    : "No motor command sent yet.";
  return (
    <div className="motors-bar" aria-label="Motors">
      <div className="motors-state">
        <span className="metric-label">MOTORS</span>
        <strong className={armed ? "armed-text" : undefined}>
          {armed === null ? "Unknown" : armed ? "Armed" : "Disarmed"}
          {motors.armed_from && <span className="muted"> · {motors.armed_from}</span>}
        </strong>
        <span className={`muted${motors.ok === false ? " error-text" : ""}`} role="status">{result}</span>
      </div>
      <button
        className={`tool${usb === "allowed" ? " on" : ""}`}
        aria-pressed={usb === "allowed"}
        disabled={!enabled || !uartOpen || busy || usb === "no_usb" || usb === "allowed"}
        onClick={() => send({ cmd: "usb_flight" })}
        title="The FC refuses to arm while its USB API port is connected; this lifts that until the aircraft restarts"
      >
        USB flight: {USB_FLIGHT[usb] ?? usb}
      </button>
      <button
        className={`tool${armed ? " on" : ""}`}
        aria-pressed={armed === true}
        disabled={!enabled || busy}
        onClick={start}
      >
        {armed ? "Armed" : "Arm"}
      </button>
      {/* Never held back by a request in progress: a stop must always be
          sendable, and the server makes a running start give way to it. */}
      <button
        className={`tool danger${armed === false ? " on" : ""}`}
        aria-pressed={armed === false}
        disabled={!enabled}
        onClick={() => send({ cmd: "motors", on: false })}
      >
        {armed === false ? "Disarmed" : "Disarm"}
      </button>
    </div>
  );
}

type VoiceState = "idle" | "listening" | "transcribing" | "failed";

/** The browser can record: a microphone API on a secure origin (HTTPS or localhost). */
export const canRecord = () =>
  typeof navigator !== "undefined" && !!navigator.mediaDevices?.getUserMedia
  && typeof MediaRecorder !== "undefined" && window.isSecureContext;

/** Push-to-talk: record while held, then transcribe on the server. */
export function useVoice(onText: (text: string) => void) {
  const [state, setState] = useState<VoiceState>("idle");
  const [error, setError] = useState<string | null>(null);
  const recorder = useRef<MediaRecorder | null>(null);
  // Held between press and release; a release that comes while the
  // microphone is still opening must not leave it recording.
  const held = useRef(false);
  const start = async () => {
    if (held.current || recorder.current || !canRecord()) return;
    held.current = true;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      if (!held.current) {
        stream.getTracks().forEach(t => t.stop());
        return;
      }
      const mimeType = ["audio/webm;codecs=opus", "audio/webm", "audio/mp4", "audio/ogg"]
        .find(t => MediaRecorder.isTypeSupported(t));
      const rec = new MediaRecorder(stream, mimeType ? { mimeType } : undefined);
      const chunks: BlobPart[] = [];
      rec.ondataavailable = (e) => { if (e.data.size > 0) chunks.push(e.data); };
      rec.onstop = async () => {
        stream.getTracks().forEach(t => t.stop());
        recorder.current = null;
        const blob = new Blob(chunks, { type: rec.mimeType || "audio/webm" });
        setState("transcribing");
        try {
          const res = await fetch("/voice", { method: "POST", body: blob, headers: { "Content-Type": blob.type } });
          if (!res.ok) throw new Error(await res.text());
          const t = (await res.json()) as Transcript;
          setState("idle");
          setError(null);
          if (t.text) onText(t.text);
        } catch (e) {
          setState("failed");
          setError(e instanceof Error ? e.message : String(e));
        }
      };
      recorder.current = rec;
      rec.start();
      setState("listening");
      setError(null);
    } catch (e) {
      held.current = false;
      setState("failed");
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  const stop = () => {
    held.current = false;
    const rec = recorder.current;
    if (rec && rec.state === "recording") rec.stop();
  };
  return { state, error, start, stop };
}

/** One line on a role's service: provider, model, key, balance. */
export function roleSummary(role: string, providers: AiProvider[] | null, none: string): string {
  if (providers === null) return `${role}: checking…`;
  const active = providers.find(p => p.active);
  if (!active) return `${role}: ${none}`;
  const key = active.key_ok === null ? "key unchecked" : active.key_ok ? "key OK" : `key rejected: ${active.note}`;
  return `${role}: ${active.provider} ${active.model} · ${key}${active.balance ? ` · ${active.balance}` : ""}`;
}

export const voiceSummary = (status: AiStatus | null) =>
  roleSummary("Speech", status && status.speech, "no provider key (secrets/openai.key or deepgram.key)");
export const understandingSummary = (status: AiStatus | null) =>
  roleSummary("Understanding", status && status.understanding, "local grammar only (add secrets/anthropic.key or openai.key for a model)");

/** Text commands: type (or speak) a sentence, check what the server made of it, run it. */
export function CommandBar({ plan, enabled, send }: {
  plan: PlanStatus;
  /** The server can carry out motor steps. */
  enabled: boolean;
  send: (c: Command) => void;
}) {
  const [text, setText] = useState("");
  const voice = useVoice((spoken) => {
    setText(spoken);
    send({ cmd: "plan", text: spoken });
  });
  const mic = canRecord();
  const [ai, setAi] = useState<AiStatus | null>(null);
  const [statusAt, setStatusAt] = useState(0);
  useEffect(() => {
    let gone = false;
    fetchAiStatus().then(p => { if (!gone) setAi(p); })
      .catch(() => { if (!gone) setAi({ speech: [], understanding: [] }); });
    return () => { gone = true; };
  }, [statusAt]);
  const checked = plan.preview && plan.text === text.trim();
  const steps = plan.running || plan.ok !== null ? plan.plan?.steps ?? [] : [];
  const run = () => {
    if (!checked) return;
    if (window.confirm(`Run: ${plan.preview!.steps.map(stepText).join(", then ")}?\nProps may spin. People clear.`)) {
      // Run exactly what was shown: a model asked again may answer differently.
      send({ cmd: "plan", text: text.trim(), run: true, plan: plan.preview! });
    }
  };
  return (
    <form
      className="command-bar"
      aria-label="Text commands"
      onSubmit={(e) => {
        e.preventDefault();
        if (checked) run();
        else if (text.trim()) send({ cmd: "plan", text: text.trim() });
      }}
    >
      <div className="command-row">
        <input
          type="text"
          value={text}
          placeholder="e.g. arm for 5 seconds then disarm"
          aria-label="Command"
          disabled={plan.running}
          onChange={(e) => setText(e.target.value)}
        />
        <button
          type="button"
          className={`tool${voice.state === "listening" ? " on" : ""}`}
          disabled={!mic || plan.running || voice.state === "transcribing"}
          aria-label="Hold to talk"
          title={mic ? "Hold to talk; release to transcribe" : "Microphone needs HTTPS: open this page at https://<orin>:8443/"}
          onPointerDown={(e) => { e.preventDefault(); void voice.start(); }}
          onPointerUp={voice.stop}
          onPointerCancel={voice.stop}
          onPointerLeave={voice.stop}
          onKeyDown={(e) => { if (e.key === " " && !e.repeat) { e.preventDefault(); void voice.start(); } }}
          onKeyUp={(e) => { if (e.key === " ") voice.stop(); }}
        >
          <Icon name="mic" /> {voice.state === "listening" ? "Listening" : voice.state === "transcribing" ? "Transcribing" : "Talk"}
        </button>
        <button type="button" className="tool" disabled={plan.running || plan.parsing || !text.trim()}
          onClick={() => send({ cmd: "plan", text: text.trim() })}>
          Check
        </button>
        <button type="button" className={`tool${checked ? " on" : ""}`}
          disabled={!enabled || plan.running || plan.parsing || !checked} onClick={run}>
          Run
        </button>
        <button type="button" className="tool danger" disabled={!plan.running}
          onClick={() => send({ cmd: "cancel" })}>
          Cancel
        </button>
      </div>
      <div className="command-status muted">
        <button type="button" className="link" title="Check the online services again" onClick={() => { setAi(null); setStatusAt(Date.now()); }}>
          {voiceSummary(ai)}<br />{understandingSummary(ai)}
        </button>
      </div>
      <div className="command-status" role="status">
        {voice.state === "failed" ? (
          <span className="error-text">Voice: {voice.error}</span>
        ) : voice.state === "transcribing" ? (
          <span>Transcribing…</span>
        ) : plan.parsing ? (
          <span>Understanding…</span>
        ) : plan.running ? (
          <span>Running step {plan.step + 1} of {steps.length}: <strong>{plan.message}</strong></span>
        ) : plan.preview_error ? (
          <span className="error-text">{plan.preview_error}</span>
        ) : checked ? (
          <span>Will do{plan.source ? ` (${plan.source})` : ""}: <strong>{plan.preview!.steps.map(stepText).join(", then ")}</strong></span>
        ) : plan.ok !== null ? (
          <span className={plan.ok ? "" : "error-text"}>Last plan: {plan.message}</span>
        ) : (
          <span className="muted">Type a command and press Check.</span>
        )}
      </div>
    </form>
  );
}

/** Flight data from the aircraft's OSDK UART broadcast, and motor start/stop. */
export function FlightPanel({ flight, motors, plan, rcLink, connected, send }: {
  flight: FlightLink | undefined;
  motors?: MotorStatus;
  plan?: PlanStatus;
  /** The RC link (gadget) is up, so motor commands can go over it. */
  rcLink: boolean;
  connected: boolean;
  send: (c: Command) => void;
}) {
  const live = useAdvancing(flight?.frames);
  const fresh = connected && live;

  let note: string;
  if (!flight) note = "This server does not report flight data.";
  else if (!flight.port) note = "UART reader disabled on the server (OSDK_UART=off).";
  else if (flight.port === NO_PORT) note = "No SDK UART found.";
  else if (!flight.open) note = `Waiting for ${flight.port} to open.`;
  else if (flight.frames === 0) note = `Listening on ${flight.port}; no broadcast received yet.`;
  else note = `${flight.port} · ${flight.frames.toLocaleString()} broadcasts${flight.rejected ? ` · ${flight.rejected} rejected` : ""}`;

  return (
    <section className="page" aria-labelledby="flight-heading">
      <header className="view-head">
        <h1 id="flight-heading">Flight telemetry</h1>
        <span className="muted">OSDK UART · {note}</span>
        <span className={`tag${fresh ? " ready" : ""}`}>
          {fresh ? "Receiving" : flight && flight.frames > 0 ? "Stale" : "Waiting"}
        </span>
      </header>
      {motors && (
        <Motors
          motors={motors}
          enabled={connected && (rcLink || !!flight?.open)}
          uartOpen={!!flight?.open}
          send={send}
        />
      )}
      {plan && (
        <CommandBar plan={plan} enabled={connected && (rcLink || !!flight?.open)} send={send} />
      )}
      {flight && flight.frames > 0 ? (
        <div className={fresh ? "" : "flight-stale"}>
          <Tiles d={flight.data} />
        </div>
      ) : (
        <div className="empty">
          <strong>No flight data yet</strong>
          <span>Attitude, altitude, GPS and status appear once the SDK UART broadcasts.</span>
        </div>
      )}
    </section>
  );
}
