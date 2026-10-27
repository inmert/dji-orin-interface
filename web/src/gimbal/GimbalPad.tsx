import { useState } from "react";
import { Command, GimbalAttitude } from "../api";

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

export function GimbalPad({
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

  // A render helper, not a nested component: a component defined inside
  // render is a new type every render, so React would remount the button
  // mid-press (dropping its pointer capture) and the release could be missed,
  // leaving the gimbal moving.
  const padBtn = (dir: Dir, label: string) => (
    <button
      className={`pad-btn${held === dir ? " active" : ""}`}
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        start(dir);
      }}
      onPointerUp={stop}
      onPointerLeave={stop}
      onPointerCancel={stop}
      onLostPointerCapture={stop}
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

      <div className="video-placeholder">video feed — feasibility spike pending</div>

      <div className="dpad">
        <div />
        {padBtn("up", "▲")}
        <div />
        {padBtn("left", "◀")}
        <button className="pad-btn stop-sm" onPointerDown={stop}>
          ■
        </button>
        {padBtn("right", "▶")}
        <div />
        {padBtn("down", "▼")}
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
