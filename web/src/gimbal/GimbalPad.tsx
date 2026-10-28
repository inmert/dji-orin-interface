import { useCallback, useEffect, useRef, useState } from "react";
import { Icon } from "../components/Icon";
import { Command, GimbalAttitude, VideoStatus, ThermalReport } from "../api";
import { VideoView } from "../components/VideoView";
import { MODE_THERMAL, ThermalMarker, ThermalPanel, frameToThermal } from "./Thermal";

// Camera N (UI) → gimbal device index (DUML). Camera 1 = index 0, camera 2 = 2.
const CAMERA_INDEX: Record<number, number> = { 1: 0, 2: 2 };
const CAMERA_MODEL: Record<number, string> = { 1: "Zenmuse XT2", 2: "Zenmuse Z30" };

// Arrow keys remain available as an accessible alternative to dragging.
const KEYS: Record<string, [number, number]> = {
  ArrowUp: [0, -1],
  ArrowDown: [0, 1],
  ArrowLeft: [-1, 0],
  ArrowRight: [1, 0],
};

export function GimbalPad({
  camera,
  stopGeneration = 0,
  attitude,
  video,
  thermal,
  thermalAuto = false,
  linked,
  disabled,
  send,
}: {
  camera: number;
  stopGeneration?: number;
  attitude?: GimbalAttitude;
  /** This camera's live video feed (`cam1` / `cam2` on the server). */
  video?: VideoStatus;
  /** The thermal camera's report, for the camera that carries it. */
  thermal?: ThermalReport | null;
  /** Whether the server's thermal auto range is on. */
  thermalAuto?: boolean;
  linked: boolean;
  disabled: boolean;
  send: (c: Command) => void;
}) {
  const [speed, setSpeed] = useState(200);
  // Clicks measure temperature only while the thermal picture is shown.
  const thermalView = thermal?.display_mode === MODE_THERMAL;
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const heldRef = useRef(false);
  const pointerRef = useRef<{
    id: number;
    x: number;
    y: number;
    radius: number;
  } | null>(null);
  const keyRef = useRef<string | null>(null);

  const move = (x: number, y: number) => {
    const length = Math.hypot(x, y);
    const scale = Math.max(1, length);
    x /= scale;
    y /= scale;
    setOffset({ x, y });
    // Small center dead zone prevents jitter. Signs match the verified controls.
    const active = length > 0.08;
    send({
      cmd: "gimbal",
      camera,
      pitch: active ? Math.round(-y * speed) : 0,
      yaw: active ? Math.round(x * speed) : 0,
      active,
    });
  };
  const release = useCallback(() => {
    heldRef.current = false;
    pointerRef.current = null;
    keyRef.current = null;
    setOffset({ x: 0, y: 0 });
    send({ cmd: "gimbal", camera, pitch: 0, yaw: 0, active: false });
  }, [camera, send]);
  const stop = useCallback(() => {
    if (heldRef.current) release();
  }, [release]);

  useEffect(() => {
    const visibility = () => {
      if (document.hidden) stop();
    };
    window.addEventListener("blur", stop);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      window.removeEventListener("blur", stop);
      document.removeEventListener("visibilitychange", visibility);
      stop();
    };
  }, [stop]);

  useEffect(() => {
    if (disabled) stop();
  }, [disabled, stop]);

  // A global stop also cancels the gesture, so the next drag event cannot restart it.
  useEffect(() => {
    stop();
  }, [stopGeneration, stop]);

  const moving = Math.hypot(offset.x, offset.y) > 0.08;
  const follower = linked && camera === 2;
  return (
    <section className="pane camera-pane" aria-label={`Camera ${camera}`}>
      <header className="pane-head">
        <h2>Camera {camera}</h2>
        <span className="muted">
          {CAMERA_MODEL[camera]} · Gimbal {CAMERA_INDEX[camera]} ·{" "}
          {linked ? (camera === 1 ? "Master" : "Follower") : "Independent"}
        </span>
        <span className={`tag${moving ? " moving" : disabled ? "" : " ready"}`}>
          {moving ? "Moving" : disabled ? (follower ? "Linked" : "Standby") : "Ready"}
        </span>
      </header>
      <VideoView
        feed={`cam${camera}`}
        status={video}
        label={`CAM ${camera}`}
        corner="640×360"
        aspect={16 / 9}
        onPick={
          thermalView && !disabled
            ? (fx, fy) => {
                const spot = frameToThermal(fx, fy);
                if (spot) send({ cmd: "spot", x: spot[0], y: spot[1] });
              }
            : undefined
        }
      >
        {thermalView && thermal ? (
          <ThermalMarker thermal={thermal} />
        ) : (
          <div className="reticle" aria-hidden="true" />
        )}
      </VideoView>
      {thermal && <ThermalPanel thermal={thermal} auto={thermalAuto} disabled={disabled} send={send} />}
      <div className="camera-controls">
        <button
          className={`joystick${heldRef.current ? " engaged" : ""}`}
          disabled={disabled}
          aria-label={`Camera ${camera} joystick`}
          aria-describedby={`joystick-help-${camera}`}
          title="Hold and drag to pan and tilt; farther is faster. Arrow keys also work."
          onPointerDown={(e) => {
            if (disabled || e.button !== 0 || heldRef.current) return;
            e.preventDefault();
            e.currentTarget.focus();
            e.currentTarget.setPointerCapture(e.pointerId);
            const bounds = e.currentTarget.getBoundingClientRect();
            pointerRef.current = {
              id: e.pointerId,
              x: e.clientX,
              y: e.clientY,
              radius: bounds.width / 2 - 22,
            };
            heldRef.current = true;
            move(0, 0);
          }}
          onPointerMove={(e) => {
            const pointer = pointerRef.current;
            if (disabled || !pointer || pointer.id !== e.pointerId) return;
            move(
              (e.clientX - pointer.x) / pointer.radius,
              (e.clientY - pointer.y) / pointer.radius,
            );
          }}
          onPointerUp={(e) => {
            if (pointerRef.current?.id === e.pointerId) stop();
          }}
          onPointerCancel={(e) => {
            if (pointerRef.current?.id === e.pointerId) stop();
          }}
          onLostPointerCapture={(e) => {
            if (pointerRef.current?.id === e.pointerId) stop();
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              stop();
              return;
            }
            const direction = KEYS[e.key];
            if (!direction) return;
            e.preventDefault();
            if (disabled || heldRef.current || e.repeat) return;
            keyRef.current = e.key;
            heldRef.current = true;
            move(...direction);
          }}
          onKeyUp={(e) => {
            if (keyRef.current === e.key) {
              e.preventDefault();
              stop();
            }
          }}
          onBlur={stop}
        >
          <span className="joystick-axis horizontal" aria-hidden="true" />
          <span className="joystick-axis vertical" aria-hidden="true" />
          <span
            className="joystick-thumb"
            aria-hidden="true"
            style={{
              transform: `translate(${offset.x * 48}px, ${offset.y * 48}px)`,
            }}
          />
        </button>
        <div className="control-settings">
          <label className="field">
            <span className="field-label">
              Speed <output>{speed}</output>
            </span>
            <input
              type="range"
              min={20}
              max={500}
              step={10}
              value={speed}
              disabled={disabled}
              onChange={(e) => setSpeed(Number(e.target.value))}
            />
          </label>
          <div className="readout" title={attitude ? "Reported by the gimbal" : "Waiting for attitude data"}>
            {(["pitch", "roll", "yaw"] as const).map((axis) => (
              <div key={axis}>
                <span>{axis}</span>
                {/* The gimbal reports tenths of a degree. */}
                <strong>{attitude ? `${(attitude[axis] / 10).toFixed(1)}°` : "—"}</strong>
              </div>
            ))}
          </div>
          <div className="pad-actions">
            <button
              className="tool"
              onClick={release}
              aria-label={`Stop camera ${camera}`}
            >
              <Icon name="stop" /> Stop
            </button>
            <button
              className="tool"
              disabled={disabled}
              onClick={() => {
                release();
                send({ cmd: "reset", camera });
              }}
              aria-label={`Recenter camera ${camera} yaw`}
              title={linked ? "Linked: recenters both cameras via camera 1" : "Return yaw to center; pitch is kept"}
            >
              <Icon name="recenter" /> Recenter
            </button>
          </div>
          <span id={`joystick-help-${camera}`} className="hint">
            {follower ? "Following camera 1" : "Drag the pad or use arrow keys"}
          </span>
        </div>
      </div>
    </section>
  );
}
