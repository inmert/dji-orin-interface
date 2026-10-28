import { Telemetry } from "../api";
import { VideoView, videoStateLabel } from "../components/VideoView";
import { deg } from "../telemetry/Tile";

/** Gimbal device index of the FPV camera's fixed mount. */
const FPV_MOUNT_INDEX = "4";

/**
 * The centre-front FPV camera: fixed to the airframe, so it has no controls.
 * Its mount reports as Gimbal[4] (angles in 0.1°).
 */
export function FpvCard({ telemetry, stale }: { telemetry: Telemetry; stale: boolean }) {
  const mount = telemetry.gimbals[FPV_MOUNT_INDEX];
  const fpv = telemetry.video?.fpv;
  return (
    <section className={`pane fpv-pane${stale ? " stale" : ""}`} aria-label="FPV camera">
      <header className="pane-head">
        <h2>FPV</h2>
        <span className="muted">Centre front · fixed mount</span>
        <span className="tag">{videoStateLabel(fpv)}</span>
      </header>
      <VideoView feed="fpv" status={fpv} label="FPV" corner="608×448" aspect={608 / 448} />
      <dl className="facts">
        <div><dt>Heading</dt><dd>{mount ? deg(mount.yaw / 10) : "—"}</dd></div>
        <div><dt>Roll</dt><dd>{mount ? deg(mount.roll / 10) : "—"}</dd></div>
        <div><dt>Pitch</dt><dd>{mount ? deg(mount.pitch / 10) : "—"}</dd></div>
        <div><dt>Frames</dt><dd>{fpv ? fpv.frames.toLocaleString() : "—"}</dd></div>
        <div><dt>Received</dt><dd>{fpv ? `${(fpv.bytes / 1e6).toFixed(1)} MB` : "—"}</dd></div>
        <div><dt>Viewers</dt><dd>{fpv ? fpv.viewers : "—"}</dd></div>
      </dl>
      <span className="hint">{fpv ? `H.264 from ${fpv.source}` : "Disabled on the server"}</span>
    </section>
  );
}
