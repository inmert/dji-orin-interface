import { useMemo } from "react";
import { Telemetry } from "../api";

const hex2 = (n: number) => `0x${n.toString(16).padStart(2, "0")}`;

export function StreamsTable({ telemetry }: { telemetry: Telemetry }) {
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
            <span className="mono">
              {hex2(s.cmd_set)}/{hex2(s.cmd_id)}
            </span>
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
