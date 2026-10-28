import { Telemetry } from "../api";
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
  return (
    <div className={`fpv-card${stale ? " flight-stale" : ""}`}>
      <div className="camera-title fpv-heading">
        <span className="camera-number">03</span>
        <div>
          <h3>FPV camera</h3>
          <span className="muted">Centre front · fixed mount · no gimbal control</span>
        </div>
      </div>
      <div className="usb-grid">
        <Tile label="MOUNT HEADING" value={mount ? deg(mount.yaw / 10) : "—"} note={mount ? "Follows the aircraft" : "No report yet"}>
          {mount && <Row label="Roll" value={deg(mount.roll / 10)} />}
          {mount && <Row label="Pitch" value={deg(mount.pitch / 10)} />}
        </Tile>
        <Tile label="CAMERA REPORTS" value={reports.toLocaleString()} note="Status frames from Camera[1]" />
        <Tile label="VIDEO" value="—" note="Not available yet: the FPV stream over USB is still being probed" />
      </div>
    </div>
  );
}
