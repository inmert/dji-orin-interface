import { useMemo, useState } from "react";
import { Command, GimbalAttitude, Telemetry, useLink } from "./api";

// Camera N (UI) → gimbal device index (DUML). Camera 1 = index 0, camera 2 = 2.
const CAMERA_INDEX: Record<number, number> = { 1: 0, 2: 2 };

// Provisional sign mapping, isolated so it is a one-line flip after we calibrate
// against the physical gimbal on the first hardware test.
const DIRS = {
  up: (s: number) => ({ pitch: s, yaw: 0 }),
  down: (s: number) => ({ pitch: -s, yaw: 0 }),
  left: (s: number) => ({ pitch: 0, yaw: -s }),
  right: (s: number) => ({ pitch: 0, yaw: s }),
} as const;
type Dir = keyof typeof DIRS;

function GimbalPad({
  camera,
  attitude,
  linked,
  send,
}: {
  camera: number;
  attitude?: GimbalAttitude;
  linked: boolean;
  send: (c: Command) => void;
}) {
  const [speed, setSpeed] = useState(200);
  const [held, setHeld] = useState<Dir | null>(null);

  const start = (dir: Dir) => {
    setHeld(dir);
    const { pitch, yaw } = DIRS[dir](speed);
    send({ cmd: "gimbal", camera, pitch, yaw, active: true });
  };
  const stop = () => {
    if (held !== null) {
      setHeld(null);
      send({ cmd: "gimbal", camera, pitch: 0, yaw: 0, active: false });
    }
  };

  const Btn = ({ dir, label }: { dir: Dir; label: string }) => (
    <button
      className={`pad-btn${held === dir ? " active" : ""}`}
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        start(dir);
      }}
      onPointerUp={stop}
      onPointerLeave={stop}
      onPointerCancel={stop}
    >
      {label}
    </button>
  );

  return (
    <section className="card">
      <div className="card-head">
        <h2>Camera {camera}</h2>
        {linked ? (
          <span className="tag link">linked</span>
        ) : (
          <span className="muted">gimbal index {CAMERA_INDEX[camera]}</span>
        )}
      </div>

      <div className="video-placeholder">
        video feed — feasibility spike pending
      </div>

      <div className="dpad">
        <div />
        <Btn dir="up" label="▲" />
        <div />
        <Btn dir="left" label="◀" />
        <button className="pad-btn stop-sm" onPointerDown={stop}>
          ■
        </button>
        <Btn dir="right" label="▶" />
        <div />
        <Btn dir="down" label="▼" />
        <div />
      </div>

      <label className="speed">
        speed <span className="muted">{speed}</span>
        <input
          type="range"
          min={20}
          max={500}
          step={10}
          value={speed}
          onChange={(e) => setSpeed(Number(e.target.value))}
        />
      </label>

      <div className="attitude">
        {attitude ? (
          <>
            <span>pitch {attitude.pitch}</span>
            <span>roll {attitude.roll}</span>
            <span>yaw {attitude.yaw}</span>
            {attitude.provisional && <span className="tag">provisional</span>}
          </>
        ) : (
          <span className="muted">no attitude yet</span>
        )}
      </div>
    </section>
  );
}

function StreamsTable({ telemetry }: { telemetry: Telemetry }) {
  const rows = useMemo(
    () =>
      Object.entries(telemetry.streams).sort((a, b) => b[1].count - a[1].count),
    [telemetry.streams],
  );
  return (
    <section className="card wide">
      <div className="card-head">
        <h2>Telemetry streams</h2>
        <span className="muted">{telemetry.frames.toLocaleString()} frames</span>
      </div>
      <div className="table">
        <div className="trow thead">
          <span>set/id</span>
          <span>from → to</span>
          <span>len</span>
          <span>count</span>
        </div>
        {rows.length === 0 && <div className="muted pad">waiting for frames…</div>}
        {rows.map(([key, s]) => (
          <div className="trow" key={key}>
            <span className="mono">{key}</span>
            <span className="muted">
              {s.from} → {s.to}
            </span>
            <span>{s.last_len}</span>
            <span>{s.count.toLocaleString()}</span>
          </div>
        ))}
      </div>
    </section>
  );
}

export function App() {
  const { status, telemetry, send } = useLink();
  const [linked, setLinked] = useState(false);
  const stopAll = () => send({ cmd: "stop" });
  const toggleLink = () => {
    const on = !linked;
    setLinked(on);
    send({ cmd: "link", on });
  };

  return (
    <div className="app">
      <header className="topbar">
        <h1>M210 · Orin Control</h1>
        <div className="status">
          <span className={`dot ${status}`} />
          {status === "open" ? (telemetry.link_up ? "link up" : "server, no aircraft") : status}
          <button
            className={`link-toggle${linked ? " on" : ""}`}
            onClick={toggleLink}
            title="Snap camera 2 to camera 1 and move both together"
          >
            {linked ? "LINKED" : "LINK"}
          </button>
          <button className="estop" onClick={stopAll}>
            STOP
          </button>
        </div>
      </header>

      <main className="grid">
        <GimbalPad camera={1} attitude={telemetry.gimbals["0"]} linked={linked} send={send} />
        <GimbalPad camera={2} attitude={telemetry.gimbals["2"]} linked={linked} send={send} />
        <StreamsTable telemetry={telemetry} />
      </main>
    </div>
  );
}
