import { Telemetry } from "../api";
import { VideoView, videoStateLabel } from "../components/VideoView";
import { Row, Tile, deg } from "../telemetry/Tile";

/** Gimbal device index of the FPV camera's fixed mount. */
const FPV_MOUNT_INDEX = "4";

/**
 * The centre-front FPV camera: fixed to the airframe, so it has no controls.
 * It reports as Camera[1], and its mount as Gimbal[4] (angles in 0.1°).
 */
export function FpvCard({ telemetry, stale }: { telemetry: Telemetry; stale: boolean }) {
  const mount = telemetry.gimbals[FPV_MOUNT_INDEX];
  const reports = Object.values(telemetry.streams)
    .filter((s) => s.from === "Camera[1]")
    .reduce((n, s) => n + s.count, 0);
  const fpv = telemetry.video?.fpv;
  return (
    <div className={`fpv-card${stale ? " flight-stale" : ""}`}>
      <div className="camera-title fpv-heading">
        <span className="camera-number">03</span>
        <div>
          <h3>FPV camera</h3>
          <span className="muted">Centre front · fixed mount · no gimbal control</span>
        </div>
      </div>
      <VideoView
        feed="fpv"
        status={fpv}
        label="CAMERA 03"
        corner="M210 / FPV 608×448"
        aspect={608 / 448}
        className="video-card"
      />
      <div className="usb-grid">
        <Tile label="MOUNT HEADING" value={mount ? deg(mount.yaw / 10) : "—"} note={mount ? "Follows the aircraft" : "No report yet"}>
          {mount && <Row label="Roll" value={deg(mount.roll / 10)} />}
          {mount && <Row label="Pitch" value={deg(mount.pitch / 10)} />}
        </Tile>
        <Tile label="CAMERA REPORTS" value={reports.toLocaleString()} note="Status frames from Camera[1]" />
        <Tile
          label="VIDEO STREAM"
          value={videoStateLabel(fpv)}
          note={fpv ? `H.264 from ${fpv.source}` : "Disabled on the server"}
        >
          {fpv && <Row label="Frames" value={fpv.frames.toLocaleString()} />}
          {fpv && <Row label="Received" value={`${(fpv.bytes / 1e6).toFixed(1)} MB`} />}
          {fpv && <Row label="Viewers" value={String(fpv.viewers)} />}
        </Tile>
      </div>
    </div>
  );
}
