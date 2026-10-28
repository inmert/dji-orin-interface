import { useCallback, useEffect, useRef, useState } from "react";
import SpotlightCard from "../components/SpotlightCard";
import { Command, GimbalAttitude, VideoStatus, ThermalReport } from "../api";
import { VideoView } from "../components/VideoView";
import { MODE_THERMAL, ThermalMarker, ThermalPanel, frameToThermal } from "./Thermal";

// Camera N (UI) → gimbal device index (DUML). Camera 1 = index 0, camera 2 = 2.
const CAMERA_INDEX: Record<number, number> = { 1: 0, 2: 2 };

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

  return (
    <SpotlightCard
      className="camera-card"
      spotlightColor="#76dfea"
      intensity={0.08}
      proximity={30}
      flare={false}
    >
      <div className="card-head">
        <div className="camera-title">
          <span className="camera-number">0{camera}</span>
          <div>
            <h3>Camera {camera}</h3>
            <span className="muted">
              Gimbal {CAMERA_INDEX[camera]} ·{" "}
              {linked ? (camera === 1 ? "Master" : "Follower") : "Independent"}
            </span>
          </div>
        </div>
        <span
          className={`tag${Math.hypot(offset.x, offset.y) > 0.08 ? " moving" : ""}`}
        >
          {Math.hypot(offset.x, offset.y) > 0.08
            ? "Moving"
            : disabled
              ? linked && camera === 2
                ? "Linked"
                : "Standby"
              : "Ready"}
        </span>
      </div>
      <VideoView
        feed={`cam${camera}`}
        status={video}
        label={`CAMERA 0${camera}`}
        corner={`M210 / CAM 0${camera} · 640×360`}
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
          <div className="reticle" aria-hidden="true">
            <span>+</span>
          </div>
        )}
      </VideoView>
      {thermal && <ThermalPanel thermal={thermal} auto={thermalAuto} disabled={disabled} send={send} />}
      <div className="camera-controls">
        <div className="direction-controls">
          <span className="control-label">GIMBAL JOYSTICK</span>
          <button
            className={`joystick${heldRef.current ? " engaged" : ""}`}
            disabled={disabled}
            aria-label={`Camera ${camera} joystick`}
            aria-describedby={`joystick-help-${camera}`}
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
            <span className="joystick-ring" aria-hidden="true" />
            <span
              className="joystick-thumb"
              aria-hidden="true"
              style={{
                transform: `translate(${offset.x * 48}px, ${offset.y * 48}px)`,
              }}
            >
              +
            </span>
          </button>
          <span id={`joystick-help-${camera}`} className="hold-hint">
            {linked && camera === 2
              ? "Following camera 1"
              : "Hold & drag · release to stop"}
          </span>
          <span className="joystick-key-hint">Arrow keys also supported</span>
          <div className="pad-actions">
            <button
              className="joystick-stop"
              onClick={release}
              aria-label={`Stop camera ${camera}`}
            >
              ■ Stop
            </button>
            <button
              className="recenter"
              disabled={disabled}
              onClick={() => {
                release();
                send({ cmd: "reset", camera });
              }}
              aria-label={`Recenter camera ${camera} yaw`}
              title={linked ? "Linked: recenters both cameras via camera 1" : "Return yaw to center; pitch is kept"}
            >
              ⟲ Recenter
            </button>
          </div>
        </div>
        <div className="control-settings">
          <label className="speed">
            <span className="control-label">
              MOVEMENT SPEED <output>{speed}</output>
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
            <span className="range-labels">
              <span>Precise</span>
              <span>Fast</span>
            </span>
          </label>
          <div className="attitude-heading">
            <span className="control-label">ATTITUDE</span>
          </div>
          <div className="attitude">
            {(["pitch", "roll", "yaw"] as const).map((axis) => (
              <div key={axis}>
                <span>{axis}</span>
                {/* The gimbal reports tenths of a degree. */}
                <strong>{attitude ? `${(attitude[axis] / 10).toFixed(1)}°` : "—"}</strong>
              </div>
            ))}
          </div>
          <span className="attitude-note">
            {attitude
              ? "Reported by the gimbal"
              : "Waiting for attitude data"}
          </span>
        </div>
      </div>
    </SpotlightCard>
  );
}
