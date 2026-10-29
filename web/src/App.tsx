import { useState } from "react";
import { useLink } from "./api";
import { Icon, IconName } from "./components/Icon";
import { FpvCard } from "./gimbal/FpvCard";
import { GimbalPad } from "./gimbal/GimbalPad";
import { FlightPanel } from "./telemetry/FlightPanel";
import { StreamsTable } from "./telemetry/StreamsTable";
import { UsbPanel } from "./telemetry/UsbPanel";

type View = "cameras" | "flight" | "usb" | "streams";

const VIEWS: { id: View; label: string; icon: IconName }[] = [
  { id: "cameras", label: "Cameras", icon: "camera" },
  { id: "flight", label: "Flight", icon: "plane" },
  { id: "usb", label: "USB link", icon: "usb" },
  { id: "streams", label: "Streams", icon: "list" },
];

export function App() {
  const { status, telemetry, stale, send } = useLink();
  const [view, setView] = useState<View>("cameras");
  const [stopGeneration, setStopGeneration] = useState(0);
  const stopAll = () => {
    send({ cmd: "stop" });
    setStopGeneration((generation) => generation + 1);
  };
  const preview = new URLSearchParams(location.search).has("preview");
  const linked = telemetry.linked;
  const ready = status === "open" && telemetry.link_up && !stale;
  const connection =
    status !== "open"
      ? status === "connecting"
        ? "Connecting to server"
        : "Server disconnected"
      : stale
        ? "Telemetry stale"
        : telemetry.link_up
          ? "Aircraft connected"
          : "Waiting for aircraft";
  const streams = Object.keys(telemetry.streams).length;

  return (
    <div className="app">
      <header className="titlebar">
        <div className="title">
          <span className="app-mark" aria-hidden="true" />
          <strong>M210 Control</strong>
          <span className="muted">Jetson Orin · Matrice 210 V1</span>
        </div>
        <span className={`pill${ready ? " ok" : ""}`} role="status">
          <span className="dot" /> {connection}
        </span>
        <div className="toolbar">
          <button
            className={`tool${linked ? " on" : ""}`}
            disabled={!ready}
            aria-pressed={linked}
            onClick={() => send({ cmd: "link", on: !linked })}
            title="Camera 2 follows camera 1 while linked"
          >
            <Icon name="link" /> {linked ? "Linked" : "Link cameras"}
          </button>
          <button
            className="tool"
            disabled={!ready}
            onClick={() => send({ cmd: "reset" })}
            title={linked ? "Linked: recenters both via camera 1" : "Recenter yaw on both cameras"}
          >
            <Icon name="recenter" /> Recenter all
          </button>
          <button className="tool danger" onClick={stopAll} title="Release every gimbal now">
            <Icon name="stop" /> Stop all
          </button>
        </div>
      </header>
      <div className="frame">
        <nav className="rail" aria-label="Views">
          {VIEWS.map(({ id, label, icon }) => (
            <button
              key={id}
              className={view === id ? "active" : ""}
              aria-current={view === id ? "page" : undefined}
              onClick={() => setView(id)}
            >
              <Icon name={icon} />
              <span>{label}</span>
              {id === "streams" && <span className="badge">{streams}</span>}
            </button>
          ))}
        </nav>
        <main className="view">
          {preview && (
            <div className="banner" role="status">
              Offline preview: the control connection is disabled and no commands are sent.
            </div>
          )}
          {view === "cameras" && (
            <>
              {!ready && (
                <div className="banner" role="status">
                  {connection}. Gimbal movement is disabled until fresh telemetry arrives.
                </div>
              )}
              <div className="cameras">
                <GimbalPad
                  stopGeneration={stopGeneration}
                  camera={1}
                  attitude={telemetry.gimbals["0"]}
                  video={telemetry.video?.cam1}
                  thermal={telemetry.thermal}
                  thermalAuto={telemetry.thermal_auto ?? false}
                  linked={linked}
                  disabled={!ready}
                  send={send}
                />
                <GimbalPad
                  stopGeneration={stopGeneration}
                  camera={2}
                  attitude={telemetry.gimbals["2"]}
                  video={telemetry.video?.cam2}
                  linked={linked}
                  disabled={!ready || linked}
                  send={send}
                />
                <FpvCard telemetry={telemetry} stale={!ready} />
              </div>
            </>
          )}
          {view === "flight" && <FlightPanel flight={telemetry.flight} motors={telemetry.motors} rcLink={telemetry.link_up} connected={status === "open"} send={send} />}
          {view === "usb" && <UsbPanel usb={telemetry.usb} connected={status === "open"} />}
          {view === "streams" && <StreamsTable telemetry={telemetry} stale={!ready} />}
        </main>
      </div>
      <footer className="statusbar">
        <span className={ready ? "ok" : ""}>
          <span className="dot" /> {status === "open" ? "Server connected" : connection}
        </span>
        <span>Frames {telemetry.frames.toLocaleString()}</span>
        <span>Streams {streams}</span>
        <span>Gimbals {linked ? "linked" : "independent"}</span>
        <span>UART {telemetry.flight?.frames.toLocaleString() ?? "—"}</span>
        <span>USB {telemetry.usb?.frames.toLocaleString() ?? "—"}</span>
        <span className="spacer" />
        <span>Gimbals, cameras and motor start/stop · no flight control</span>
      </footer>
    </div>
  );
}
