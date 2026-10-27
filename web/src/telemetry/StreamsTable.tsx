import { useMemo, useState } from "react";
import { Telemetry } from "../api";

const hex2 = (n: number) => `0x${n.toString(16).padStart(2, "0")}`;

export function StreamsTable({ telemetry, stale }: { telemetry: Telemetry; stale: boolean }) {
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState("count");
  const rows = useMemo(() => {
    const search = query.trim().toLowerCase();
    return Object.entries(telemetry.streams)
      .filter(([key, s]) => `${key} ${s.from} ${s.to} ${hex2(s.cmd_set)} / ${hex2(s.cmd_id)}`.toLowerCase().includes(search))
      .sort((a, b) => sort === "count" ? b[1].count - a[1].count : a[0].localeCompare(b[0]));
  }, [telemetry.streams, query, sort]);
  return (
    <section id="telemetry" className="telemetry-section" aria-labelledby="telemetry-heading">
      <div className="section-heading"><div><h2 id="telemetry-heading">Telemetry streams <span className="count-badge">{Object.keys(telemetry.streams).length}</span></h2><p>A closer look at the traffic between your aircraft and controller.</p></div><span className={`feed-state${stale ? "" : " live"}`}><span className="dot" />{stale ? "Awaiting data" : "Receiving data"}</span></div>
      <div className="table-card">
        <div className="table-toolbar"><label className="search"><span aria-hidden="true">⌕</span><input type="search" placeholder="Search devices or command IDs…" aria-label="Search telemetry streams" value={query} onChange={e => setQuery(e.target.value)} /></label><label className="sort">Sort by <select value={sort} onChange={e => setSort(e.target.value)}><option value="count">Frame count</option><option value="route">Command / route</option></select></label></div>
        <div className="table-scroll"><table><thead><tr><th>COMMAND SET / ID</th><th>SOURCE → DESTINATION</th><th>PAYLOAD</th><th>FRAME COUNT</th></tr></thead><tbody>
          {rows.map(([key, s]) => <tr key={key}><td><span className="command-id">{hex2(s.cmd_set)} <span>/</span> {hex2(s.cmd_id)}</span></td><td>{s.from} <span className="route-arrow">→</span> {s.to}</td><td className="mono">{s.last_len} <span className="muted">B</span></td><td className="mono">{s.count.toLocaleString()}</td></tr>)}
        </tbody></table>
        {rows.length === 0 && <div className="table-empty"><span>≋</span><strong>{query ? "No matching streams" : "Listening for telemetry"}</strong><p>{query ? "Try another device name or command ID." : "CRC-verified frames will appear here when the aircraft connects."}</p></div>}</div>
        <div className="table-footer"><span>{rows.length} of {Object.keys(telemetry.streams).length} streams</span><span>{telemetry.frames.toLocaleString()} valid frames received</span></div>
      </div>
    </section>
  );
}
