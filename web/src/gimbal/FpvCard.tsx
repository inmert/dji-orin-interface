import { useState } from "react";
import { FpvStatus, Telemetry, fpvStreamUrl } from "../api";
import { Row, Tile, deg } from "../telemetry/Tile";

/** Gimbal device index of the FPV camera's fixed mount. */
const FPV_MOUNT_INDEX = "4";

const STATE_LABEL: Record<FpvStatus["state"], string> = {
  idle: "Idle",
  connecting: "Connecting",
  streaming: "Streaming",
  error: "Unavailable",
};

/**
 * The FPV camera's live video. The <img> is only mounted while the user has
 * started it: opening the MJPEG URL is what makes the server connect to the
 * aircraft, and unmounting it lets the server drop the stream again.
 */
function FpvVideo({ fpv }: { fpv: FpvStatus | null | undefined }) {
  const [on, setOn] = useState(false);
  // A new key per start forces a fresh request after an error or a stop.
  const [session, setSession] = useState(0);
  // MJPEG load events differ between browsers; the server's state is reliable.
  const live = on && fpv?.state === "streaming";
  const label = fpv ? STATE_LABEL[fpv.state] : "Disabled";
  const message = !fpv
    ? "The server has FPV video turned off (FPV_UDT=off)"
    : fpv.state === "error"
      ? fpv.error ?? "Connection failed; retrying"
      : `Connecting to ${fpv.addr}`;
  return (
    <div className={`video-placeholder fpv-video${live ? " live" : ""}`}>
      {on && (
        <img
          key={session}
          src={`${fpvStreamUrl()}?s=${session}`}
          alt="FPV camera live video"
        />
      )}
      <div className="viewport-label">
        <span className="dot" /> CAMERA 03
        <span>{on ? label.toUpperCase() : "VIDEO OFF"}</span>
      </div>
      {!live && (
        <div className="video-message">
          <strong>{on ? label : "FPV video is off"}</strong>
          <span>{on ? message : "Starting it opens the stream from the aircraft over USB"}</span>
        </div>
      )}
      <button
        className="fpv-toggle"
        disabled={!fpv}
        onClick={() => {
          if (!on) setSession((n) => n + 1);
          setOn(!on);
        }}
      >
        {on ? "■ Stop video" : "▶ Start video"}
      </button>
      <span className="viewport-corner">M210 / FPV 608×448</span>
    </div>
  );
}

/**
 * The centre-front FPV camera: fixed to the airframe, so it has no controls.
 * It reports as Camera[1], and its mount as Gimbal[4] (angles in 0.1°).
 */
export function FpvCard({ telemetry, stale }: { telemetry: Telemetry; stale: boolean }) {
  const mount = telemetry.gimbals[FPV_MOUNT_INDEX];
  const reports = Object.values(telemetry.streams)
    .filter((s) => s.from === "Camera[1]")
    .reduce((n, s) => n + s.count, 0);
  const fpv = telemetry.fpv;
  return (
    <div className={`fpv-card${stale ? " flight-stale" : ""}`}>
      <div className="camera-title fpv-heading">
        <span className="camera-number">03</span>
        <div>
          <h3>FPV camera</h3>
          <span className="muted">Centre front · fixed mount · no gimbal control</span>
        </div>
      </div>
      <FpvVideo fpv={fpv} />
      <div className="usb-grid">
        <Tile label="MOUNT HEADING" value={mount ? deg(mount.yaw / 10) : "—"} note={mount ? "Follows the aircraft" : "No report yet"}>
          {mount && <Row label="Roll" value={deg(mount.roll / 10)} />}
          {mount && <Row label="Pitch" value={deg(mount.pitch / 10)} />}
        </Tile>
        <Tile label="CAMERA REPORTS" value={reports.toLocaleString()} note="Status frames from Camera[1]" />
        <Tile
          label="VIDEO STREAM"
          value={fpv ? STATE_LABEL[fpv.state] : "Off"}
          note={fpv ? `H.264 over UDT from ${fpv.addr}` : "Disabled on the server"}
        >
          {fpv && <Row label="Frames" value={fpv.frames.toLocaleString()} />}
          {fpv && <Row label="Received" value={`${(fpv.bytes / 1e6).toFixed(1)} MB`} />}
          {fpv && <Row label="Viewers" value={String(fpv.viewers)} />}
        </Tile>
      </div>
    </div>
  );
}
