import { Command, FlightData, FlightLink, MotorStatus, NO_PORT } from "../api";
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
      <Tile label="RC STICKS" value={d.rc ? "" : "—"}>
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

/** Start/stop all motors over the OSDK UART. Starting asks for confirmation. */
function Motors({ flight, motors, enabled, send }: {
  flight: FlightLink | undefined;
  motors: MotorStatus | undefined;
  enabled: boolean;
  send: (c: Command) => void;
}) {
  const s = flight?.frames ? flight.data.status : null;
  const busy = !!motors?.busy;
  const start = () => {
    if (window.confirm("Start all motors? Props off, people clear.")) send({ cmd: "motors", on: true });
  };
  const result = motors?.message
    ? `${motors.ok === false ? "Failed: " : ""}${motors.message}`
    : "No motor command sent yet.";
  return (
    <div className="motors-bar" aria-label="Motors">
      <div className="motors-state">
        <span className="metric-label">MOTORS</span>
        <strong>{s ? s.flight_name : "—"}</strong>
        <span className={`muted${motors?.ok === false ? " error-text" : ""}`} role="status">{result}</span>
      </div>
      <button
        className="tool"
        disabled={!enabled || busy}
        onClick={() => send({ cmd: "usb_flight" })}
        title="The FC refuses to arm while its USB API port is connected; this lifts that until the aircraft restarts"
      >
        Allow with USB
      </button>
      <button className="tool" disabled={!enabled || busy} onClick={start}>
        Start motors
      </button>
      <button className="tool danger" disabled={!enabled || busy} onClick={() => send({ cmd: "motors", on: false })}>
        Stop motors
      </button>
    </div>
  );
}

/** Flight data from the aircraft's OSDK UART broadcast, and motor start/stop. */
export function FlightPanel({ flight, motors, connected, send }: {
  flight: FlightLink | undefined;
  motors?: MotorStatus;
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
      {motors && <Motors flight={flight} motors={motors} enabled={connected && !!flight?.open} send={send} />}
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
