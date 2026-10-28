import { GimbalAttitude, UsbLink } from "../api";
import { Row, Tile, deg, fixed, useAdvancing } from "./Tile";

/** Gimbal report angles arrive in tenths of a degree. */
function GimbalTile({ label, g }: { label: string; g: GimbalAttitude | undefined }) {
  return (
    <Tile label={label} value={g ? deg(g.yaw / 10) : "—"} note={g ? "Yaw" : "No report yet"}>
      {g && <Row label="Pitch" value={deg(g.pitch / 10)} />}
      {g && <Row label="Roll" value={deg(g.roll / 10)} />}
    </Tile>
  );
}

/** What the aircraft pushes unasked on its USB API port (receive-only). */
export function UsbPanel({ usb, connected }: { usb: UsbLink | undefined; connected: boolean }) {
  const fresh = useAdvancing(usb?.frames) && connected;
  const o = usb?.osd;
  const u = o?.unverified;
  const hasPosition = !!u && (u.latitude !== 0 || u.longitude !== 0);

  let note: string;
  if (!usb) note = "This server does not report the USB link.";
  else if (!usb.port) note = "USB reader disabled on the server (USB_ACM=off).";
  else if (!usb.open) note = `Waiting for ${usb.port} (aircraft USB mode switch in the PC position).`;
  else if (usb.frames === 0) note = `Listening on ${usb.port}; no reports received yet.`;
  else note = `${usb.port} · ${usb.frames.toLocaleString()} frames`;

  return (
    <section className="page" aria-labelledby="usb-heading">
      <header className="view-head">
        <h1 id="usb-heading">Direct USB link</h1>
        <span className="muted">USB API port · read-only · {note}</span>
        <span className={`tag${fresh ? " ready" : ""}`}>
          {fresh ? "Receiving" : usb && usb.frames > 0 ? "Stale" : "Waiting"}
        </span>
      </header>
      {usb && usb.frames > 0 ? (
        <div className={`flight-grid${fresh ? "" : " flight-stale"}`}>
          <Tile label="ATTITUDE" value={o ? deg(o.yaw) : "—"} note="Heading (yaw)">
            {o && <Row label="Roll" value={deg(o.roll)} />}
            {o && <Row label="Pitch" value={deg(o.pitch)} />}
          </Tile>
          <Tile label="FLIGHT MODE" value={o ? o.mode_name : "—"} note={o ? `Code ${o.mode}` : undefined}>
            {o && <Row label="GPS satellites" value={o.satellites.toString()} />}
          </Tile>
          <GimbalTile label="CAMERA 1 GIMBAL" g={usb.gimbals["0"]} />
          <GimbalTile label="CAMERA 2 GIMBAL" g={usb.gimbals["2"]} />
          <Tile label="MOTION" value={u ? `${fixed(Math.hypot(u.velocity[0], u.velocity[1]), 1)} m/s` : "—"} note="Unverified offsets">
            {u && <Row label="Vertical" value={`${fixed(u.velocity[2], 1)} m/s`} />}
            {u && <Row label="Height" value={`${fixed(u.height, 1)} m`} />}
          </Tile>
          <Tile label="POSITION" value={hasPosition ? "Fix" : "—"} note="Unverified offsets">
            {u && hasPosition && <Row label="Lat, lon" value={`${u.latitude.toFixed(6)}, ${u.longitude.toFixed(6)}`} />}
            {u && !hasPosition && <Row label="Lat, lon" value="no fix" />}
          </Tile>
        </div>
      ) : (
        <div className="empty">
          <strong>No USB reports yet</strong>
          <span>Attitude, flight mode and gimbal angles appear once the USB API port is connected.</span>
        </div>
      )}
    </section>
  );
}
