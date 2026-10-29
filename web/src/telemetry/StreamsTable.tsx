import { useMemo, useState } from "react";
import { Telemetry } from "../api";

const hex2 = (n: number) => `0x${n.toString(16).padStart(2, "0")}`;

type Stream = Telemetry["streams"][string];
type GroupBy = "device" | "set" | "none";

/** Source device types, as the server names them (DevName in crates/protocol/src/duml.rs). */
const DEVICE: Record<string, string> = {
  Any: "Broadcast",
  Camera: "Cameras",
  App: "App",
  FlightCtrl: "Flight controller",
  Gimbal: "Gimbals",
  CenterBoard: "Center board",
  Remote: "Remote controller",
  WiFi: "Wi-Fi",
  DM36x: "Video transcoder (DM36x)",
  HDLink: "HD link",
  PC: "PC (USB API port)",
  Battery: "Battery",
  ESC: "ESCs",
  DM36xGnd: "Video transcoder, ground side",
  HDLinkGnd: "HD link, ground side",
  USBAir: "USB controller, air side",
  USBGnd: "USB controller, ground side",
  Monocular: "Vision (monocular)",
  Binocular: "Vision (binocular)",
};

/** What each device on this aircraft is (docs/protocol.md). */
const INSTANCE: Record<string, string> = {
  "Camera[0]": "XT2 visual",
  "Camera[1]": "XT2 thermal",
  "Camera[2]": "Z30",
  "Gimbal[0]": "XT2 gimbal",
  "Gimbal[2]": "Z30 gimbal",
  "Gimbal[4]": "FPV mount",
};

/** DUML command sets, named as in dji-firmware-tools' dissector. */
const COMMAND_SET = [
  "General", "Special", "Camera", "Flight controller", "Gimbal", "Center board",
  "Remote controller", "Wi-Fi", "Video transcoder (DM36x)", "HD link", "Vision",
  "Simulator", "ESC", "Battery", "Data recorder", "RTK",
];

/** Commands this project has decoded (docs/protocol.md), by `set/id`. */
const COMMAND: Record<string, string> = {
  "00/01": "Get version",
  "00/f1": "Self-test state",
  "02/42": "Thermal palette",
  "02/80": "Camera status",
  "02/f1": "Thermal setting",
  "02/f2": "Thermal report",
  "03/43": "Flight status (OSD)",
  "04/05": "Gimbal attitude",
  "04/0c": "Gimbal move",
  "04/4c": "Gimbal recenter",
  "0a/06": "Avoidance parameters",
  "0a/07": "Obstacle info",
  "0a/19": "Flat check",
  "0a/2e": "Vision function list",
  "0a/2f": "Vision sensor status",
};

const deviceType = (name: string) => name.replace(/\[\d+\]$/, "");
const commandKey = (s: Stream) => `${hex2(s.cmd_set).slice(2)}/${hex2(s.cmd_id).slice(2)}`;
const meaning = (s: Stream) => COMMAND[commandKey(s)];

function groupOf(s: Stream, by: GroupBy): string {
  if (by === "device") {
    const type = deviceType(s.from);
    return DEVICE[type] ?? `Unknown device (${type})`;
  }
  if (by === "set") return `${hex2(s.cmd_set)} · ${COMMAND_SET[s.cmd_set] ?? "Unknown set"}`;
  return "";
}

const device = (name: string) => (INSTANCE[name] ? `${name} · ${INSTANCE[name]}` : name);

export function StreamsTable({ telemetry, stale }: { telemetry: Telemetry; stale: boolean }) {
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState("count");
  const [groupBy, setGroupBy] = useState<GroupBy>("device");
  const [collapsed, setCollapsed] = useState<string[]>([]);
  const rows = useMemo(() => {
    const search = query.trim().toLowerCase();
    return Object.entries(telemetry.streams)
      .filter(([key, s]) => [key, device(s.from), device(s.to), `${hex2(s.cmd_set)} / ${hex2(s.cmd_id)}`,
        meaning(s) ?? "", groupOf(s, "device"), groupOf(s, "set")].join(" ").toLowerCase().includes(search))
      .sort((a, b) => sort === "count" ? b[1].count - a[1].count : a[0].localeCompare(b[0]));
  }, [telemetry.streams, query, sort]);
  // Busiest group first (or by name when sorting by route); rows keep the chosen sort.
  const groups = useMemo(() => {
    const map = new Map<string, [string, Stream][]>();
    for (const row of rows) {
      const g = groupOf(row[1], groupBy);
      map.set(g, [...(map.get(g) ?? []), row]);
    }
    const frames = (members: [string, Stream][]) => members.reduce((n, [, s]) => n + s.count, 0);
    return Array.from(map.entries()).sort((a, b) =>
      sort === "count" ? frames(b[1]) - frames(a[1]) : a[0].localeCompare(b[0]));
  }, [rows, groupBy, sort]);
  const total = Object.keys(telemetry.streams).length;
  const toggle = (g: string) =>
    setCollapsed(c => (c.includes(g) ? c.filter(x => x !== g) : [...c, g]));
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
            placeholder="Filter by device, command or ID"
            aria-label="Search telemetry streams"
            value={query}
            onChange={e => setQuery(e.target.value)}
          />
          <label className="sort">
            Group
            <select value={groupBy} onChange={e => setGroupBy(e.target.value as GroupBy)}>
              <option value="device">Source device</option>
              <option value="set">Command set</option>
              <option value="none">None</option>
            </select>
          </label>
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
            <thead><tr><th>Command</th><th>Route</th><th>Payload</th><th>Frames</th></tr></thead>
            {groups.map(([group, members]) => {
              const open = !collapsed.includes(group);
              const frames = members.reduce((n, [, s]) => n + s.count, 0);
              return (
                <tbody key={group || "all"}>
                  {groupBy !== "none" && (
                    <tr className="group-row">
                      <th colSpan={4}>
                        <button type="button" aria-expanded={open} onClick={() => toggle(group)}>
                          <span className="chevron" aria-hidden="true">{open ? "▾" : "▸"}</span>
                          {group}
                          <span className="muted">
                            {members.length} {members.length === 1 ? "route" : "routes"} · {frames.toLocaleString()} frames
                          </span>
                        </button>
                      </th>
                    </tr>
                  )}
                  {open && members.map(([key, s]) => (
                    <tr key={key}>
                      <td>
                        <span className="mono">{hex2(s.cmd_set)} / {hex2(s.cmd_id)}</span>
                        {meaning(s) && <span className="muted"> {meaning(s)}</span>}
                      </td>
                      <td>{device(s.from)} <span className="muted">→</span> {device(s.to)}</td>
                      <td className="mono">{s.last_len} B</td>
                      <td className="mono">{s.count.toLocaleString()}</td>
                    </tr>
                  ))}
                </tbody>
              );
            })}
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
