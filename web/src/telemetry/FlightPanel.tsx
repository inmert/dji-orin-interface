import { ReactNode, useEffect, useRef, useState } from "react";
import { FlightData, FlightLink } from "../api";

const fixed = (n: number, digits = 1) => n.toFixed(digits);
const deg = (n: number) => `${fixed(n)}°`;
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

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flight-row">
      <span>{label}</span>
      <span className="mono">{value}</span>
    </div>
  );
}

function Tile({ label, value, note, children }: {
  label: string; value: string; note?: string; children?: ReactNode;
}) {
  return (
    <div className="flight-tile">
      <span className="metric-label">{label}</span>
      <strong>{value}</strong>
      {note && <small>{note}</small>}
      {children && <div className="flight-rows">{children}</div>}
    </div>
  );
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

/** Flight data from the aircraft's OSDK UART broadcast (receive-only). */
export function FlightPanel({ flight, connected }: { flight: FlightLink | undefined; connected: boolean }) {
  // Live only while the frame counter keeps advancing.
  const [live, setLive] = useState(false);
  const last = useRef<{ frames?: number; at: number }>({ at: 0 });
  const frames = flight?.frames;
  useEffect(() => {
    if (frames === undefined) return;
    if (last.current.frames !== undefined && frames > last.current.frames) {
      last.current.at = Date.now();
      setLive(true);
    }
    last.current.frames = frames;
  }, [frames]);
  useEffect(() => {
    const timer = window.setInterval(() => setLive(Date.now() - last.current.at < 2000), 500);
    return () => clearInterval(timer);
  }, []);
  const fresh = connected && live;

  let note: string;
  if (!flight) note = "This server does not report flight data.";
  else if (!flight.port) note = "UART reader disabled on the server (OSDK_UART=off).";
  else if (!flight.open) note = `Waiting for ${flight.port} to open.`;
  else if (flight.frames === 0) note = `Listening on ${flight.port}; no broadcast received yet.`;
  else note = `${flight.port} · ${flight.frames.toLocaleString()} broadcasts${flight.rejected ? ` · ${flight.rejected} rejected` : ""}`;

  return (
    <section id="flight" className="flight-section" aria-labelledby="flight-heading">
      <div className="section-heading">
        <div>
          <h2 id="flight-heading">Flight telemetry <span className="count-badge">UART</span></h2>
          <p>Read-only flight data from the aircraft's SDK port. {note}</p>
        </div>
        <span className={`feed-state${fresh ? " live" : ""}`}>
          <span className="dot" />
          {fresh ? "Receiving data" : flight && flight.frames > 0 ? "Stale" : "Awaiting data"}
        </span>
      </div>
      {flight && flight.frames > 0 ? (
        <div className={fresh ? "" : "flight-stale"}>
          <Tiles d={flight.data} />
        </div>
      ) : (
        <div className="table-card">
          <div className="table-empty">
            <span>≋</span>
            <strong>No flight data yet</strong>
            <p>Decoded attitude, altitude, GPS and status appear here once the aircraft's SDK UART broadcasts.</p>
          </div>
        </div>
      )}
    </section>
  );
}
