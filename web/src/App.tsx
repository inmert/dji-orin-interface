import { useLink } from "./api";
import { GimbalPad } from "./gimbal/GimbalPad";
import { StreamsTable } from "./telemetry/StreamsTable";

export function App() {
  const { status, telemetry, stale, send } = useLink();
  const linked = telemetry.linked;
  const ready = status === "open" && telemetry.link_up && !stale;
  const connection = status !== "open" ? (status === "connecting" ? "Connecting" : "Disconnected")
    : stale ? "Telemetry stale" : telemetry.link_up ? "Aircraft connected" : "Waiting for aircraft";

  return (
    <div className="app">
      <aside className="sidebar">
        <a className="brand" href="#overview" aria-label="Orin overview"><span className="brand-mark">o<span /></span>orin<span className="brand-period">.</span></a>
        <div className="workspace-label">AIRCRAFT WORKSPACE</div>
        <nav aria-label="Main navigation">
          <a href="#overview"><span>◫</span> Overview</a>
          <a href="#cameras"><span>⊙</span> Camera control</a>
          <a href="#telemetry"><span>≋</span> Telemetry <span className="nav-count">{Object.keys(telemetry.streams).length}</span></a>
        </nav>
        <div className="sidebar-bottom"><span className="device-mark">M</span><div><strong>Matrice 210</strong><small>Jetson Orin Nano · V1</small></div></div>
      </aside>
      <div className="workspace" id="overview">
        <header className="topbar"><span>Workspace <span className="breadcrumb">/</span> <strong>Control station</strong></span><span className="session-label"><span className={`dot ${ready ? "open" : "idle"}`} /> {connection}</span></header>
        <main>
          <div className="page-heading"><div><div className="eyebrow">M210 / CONTROL STATION</div><h1>Aircraft overview</h1><p>Dual-camera gimbal control and aircraft telemetry.</p></div><button className="estop" onClick={() => send({ cmd: "stop" })}><span>■</span> Stop all gimbals</button></div>
          <div className="metrics">
            <div><span className="metric-label">AIRCRAFT LINK</span><strong className="connection-value"><span className={`dot ${ready ? "open" : "idle"}`} />{ready ? "Connected" : "Standby"}</strong><small>{connection}</small></div>
            <div><span className="metric-label">VALID FRAMES</span><strong>{telemetry.frames.toLocaleString()}</strong><small>CRC-verified traffic</small></div>
            <div><span className="metric-label">TELEMETRY STREAMS</span><strong>{Object.keys(telemetry.streams).length.toString().padStart(2, "0")}</strong><small>Distinct routes observed</small></div>
            <div><span className="metric-label">GIMBAL MODE</span><strong>{linked ? "Linked" : "Independent"}</strong><small>{linked ? "Camera 2 follows camera 1" : "Control each camera separately"}</small></div>
          </div>
          <section id="cameras" className="camera-section" aria-labelledby="camera-heading">
            <div className="section-heading"><div><h2 id="camera-heading">Camera control <span className="count-badge">02</span></h2><p>Hold and drag to pan and tilt. Drag farther for faster movement.</p></div><button className={`link-toggle${linked ? " on" : ""}`} disabled={!ready} aria-pressed={linked} onClick={() => send({ cmd: "link", on: !linked })}><span>↔</span> {linked ? "Unlink cameras" : "Link cameras"}</button></div>
            {!ready && <div className="connection-notice" role="status"><span>ⓘ</span> {connection}. Movement controls become available when fresh aircraft telemetry arrives.</div>}
            <div className="camera-grid"><GimbalPad camera={1} attitude={telemetry.gimbals["0"]} linked={linked} disabled={!ready} send={send} /><GimbalPad camera={2} attitude={telemetry.gimbals["2"]} linked={linked} disabled={!ready || linked} send={send} /></div>
          </section>
          <StreamsTable telemetry={telemetry} stale={!ready} />
          <footer><span><span className="dot" /> ORIN INTERFACE <span className="footer-divider">/</span> M210 V1</span><span>Gimbal control only · Attitude decoding is provisional</span></footer>
        </main>
      </div>
    </div>
  );
}
