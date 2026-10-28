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
  const total = Object.keys(telemetry.streams).length;
  return (
    <section className="page streams-page" aria-labelledby="telemetry-heading">
      <header className="view-head">
        <h1 id="telemetry-heading">Telemetry streams</h1>
        <span className="muted">CRC-verified DUML routes on the RC link</span>
        <span className={`tag${stale ? "" : " ready"}`}>{stale ? "Waiting" : "Receiving"}</span>
      </header>
      <div className="table-card">
        <div className="table-toolbar">
          <input
            type="search"
            placeholder="Filter by device or command ID"
            aria-label="Search telemetry streams"
            value={query}
            onChange={e => setQuery(e.target.value)}
          />
          <label className="sort">
            Sort
            <select value={sort} onChange={e => setSort(e.target.value)}>
              <option value="count">Frame count</option>
              <option value="route">Command / route</option>
            </select>
          </label>
        </div>
        <div className="table-scroll">
          <table>
            <thead><tr><th>Command set / ID</th><th>Route</th><th>Payload</th><th>Frames</th></tr></thead>
            <tbody>
              {rows.map(([key, s]) => (
                <tr key={key}>
                  <td className="mono">{hex2(s.cmd_set)} / {hex2(s.cmd_id)}</td>
                  <td>{s.from} <span className="muted">→</span> {s.to}</td>
                  <td className="mono">{s.last_len} B</td>
                  <td className="mono">{s.count.toLocaleString()}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {rows.length === 0 && (
            <div className="empty">
              <strong>{query ? "No matching streams" : "No telemetry yet"}</strong>
            </div>
          )}
        </div>
        <div className="table-footer">
          <span>{rows.length} of {total} streams</span>
          <span>{telemetry.frames.toLocaleString()} valid frames</span>
        </div>
      </div>
    </section>
  );
}
