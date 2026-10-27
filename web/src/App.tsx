import { useLink } from "./api";
import { GimbalPad } from "./gimbal/GimbalPad";
import { StreamsTable } from "./telemetry/StreamsTable";

export function App() {
  const { status, telemetry, send } = useLink();
  // Link state is server-authoritative (survives page refresh); the button just
  // asks the server to toggle, and the UI follows the next telemetry update.
  const linked = telemetry.linked;
  const stopAll = () => send({ cmd: "stop" });
  const toggleLink = () => send({ cmd: "link", on: !linked });

  return (
    <div className="app">
      <header className="topbar">
        <h1>M210 · Orin Control</h1>
        <div className="status">
          <span className={`dot ${status}`} />
          {status === "open" ? (telemetry.link_up ? "link up" : "server, no aircraft") : status}
          <button
            className={`link-toggle${linked ? " on" : ""}`}
            onClick={toggleLink}
            title="Engage the aircraft's camera lock: camera 2 follows camera 1"
          >
            {linked ? "LINKED" : "LINK"}
          </button>
          <button className="estop" onClick={stopAll}>
            STOP
          </button>
        </div>
      </header>

      <main className="grid">
        <GimbalPad camera={1} attitude={telemetry.gimbals["0"]} linked={linked} send={send} />
        <GimbalPad camera={2} attitude={telemetry.gimbals["2"]} linked={linked} send={send} />
        <StreamsTable telemetry={telemetry} />
      </main>
    </div>
  );
}
