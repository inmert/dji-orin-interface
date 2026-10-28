import { useEffect, useState } from "react";
import { Command, ThermalReport } from "../api";

// In thermal display mode the camera's 1280×720 video holds its 640×512
// (5:4) thermal image scaled to the full height and centred, with black bars
// either side: 900 px wide, from x = 190 px.
const THERMAL_LEFT = 190 / 1280;
const THERMAL_WIDTH = 900 / 1280;

/** Display mode values the camera uses (see crates/protocol/src/thermal.rs). */
export const MODE_VISUAL = 0;
export const MODE_THERMAL = 3;
/** Measurement types. */
const MEASURE_SPOT = 1;
const MEASURE_AREA = 2;

/** Isotherm presets, as DJI Pilot sets them for the XT2 (°C). */
const PRESETS: { name: string; band: [number, number, number] }[] = [
  { name: "People", band: [27, 32, 38] },
  { name: "Fire", band: [27, 149, 538] },
];
/** Bounds the server accepts. */
const ISOTHERM_MIN = -40;
const ISOTHERM_MAX = 800;

/**
 * Map a click at frame fractions (fx, fy) to the camera's spot-meter
 * coordinates (0..1 over the thermal image), or null on the black bars.
 */
export function frameToThermal(fx: number, fy: number): [number, number] | null {
  const x = (fx - THERMAL_LEFT) / THERMAL_WIDTH;
  if (x < 0 || x > 1 || fy < 0 || fy > 1) return null;
  return [x, fy];
}

/** CSS position of a point given in thermal-image coordinates. */
const at = (x: number, y: number) => ({
  left: `${(THERMAL_LEFT + x * THERMAL_WIDTH) * 100}%`,
  top: `${y * 100}%`,
});

const celsius = (v: number) => `${v.toFixed(1)} °C`;

/** A bound as typed: whole number or NaN (`Number("")` would be 0). */
export const parseBound = (text: string) => (text.trim() === "" ? NaN : Number(text));

/** Check a band the user typed: whole °C, in range, lower ≤ middle ≤ upper. */
export function validBand(lower: number, middle: number, upper: number): boolean {
  const all = [lower, middle, upper];
  return all.every(v => Number.isInteger(v) && v >= ISOTHERM_MIN && v <= ISOTHERM_MAX)
    && lower <= middle && middle <= upper;
}

/**
 * What the camera is measuring, drawn on the video: the spot with its
 * reading, or (in area measurement) the coldest and hottest points.
 */
export function ThermalMarker({ thermal }: { thermal: ThermalReport }) {
  const area = thermal.measure === MEASURE_AREA ? thermal.area : null;
  if (area) {
    return (
      <>
        <div className="thermal-spot cold" style={at(area.min_x, area.min_y)} aria-hidden="true">
          <span className="thermal-spot-ring" />
          <span className="thermal-spot-label">min {celsius(area.min)}</span>
        </div>
        <div className="thermal-spot hot" style={at(area.max_x, area.max_y)} aria-hidden="true">
          <span className="thermal-spot-ring" />
          <span className="thermal-spot-label">max {celsius(area.max)}</span>
        </div>
      </>
    );
  }
  if (thermal.measure !== MEASURE_SPOT) return null;
  return (
    <div
      className={`thermal-spot${thermal.spot_valid ? "" : " pending"}`}
      style={at(thermal.spot_x, thermal.spot_y)}
      aria-hidden="true"
    >
      <span className="thermal-spot-ring" />
      <span className="thermal-spot-label">
        {thermal.spot_valid ? celsius(thermal.spot_temp) : "measuring…"}
      </span>
    </div>
  );
}

/** Lower / middle / upper editor for a custom isotherm band. */
function BandEditor({
  thermal,
  disabled,
  send,
}: {
  thermal: ThermalReport;
  disabled: boolean;
  send: (c: Command) => void;
}) {
  const current = [thermal.isotherm_lower, thermal.isotherm_middle, thermal.isotherm_upper];
  const [draft, setDraft] = useState(current.map(String));
  const [editing, setEditing] = useState(false);
  // Follow the camera (presets, auto) until the user starts typing.
  useEffect(() => {
    if (!editing) setDraft(current.map(String));
  }, [editing, ...current]);
  const [lower, middle, upper] = draft.map(parseBound);
  const ok = validBand(lower, middle, upper);
  return (
    <form
      className="thermal-band"
      onSubmit={(e) => {
        e.preventDefault();
        if (!ok) return;
        send({ cmd: "isotherm", lower, middle, upper });
        setEditing(false);
      }}
    >
      {(["Low", "Mid", "High"] as const).map((label, i) => (
        <label key={label}>
          <span>{label}</span>
          <input
            type="number"
            inputMode="numeric"
            step={1}
            min={ISOTHERM_MIN}
            max={ISOTHERM_MAX}
            value={draft[i]}
            disabled={disabled}
            onChange={(e) => {
              setEditing(true);
              setDraft(d => d.map((v, j) => (j === i ? e.target.value : v)));
            }}
          />
        </label>
      ))}
      <span className="muted">°C</span>
      <button type="submit" disabled={disabled || !ok || !editing}>
        Apply
      </button>
      {editing && (
        <button type="button" className="link" onClick={() => setEditing(false)}>
          Cancel
        </button>
      )}
      {editing && !ok && <span className="thermal-error">Whole °C, {ISOTHERM_MIN}…{ISOTHERM_MAX}, low ≤ mid ≤ high</span>}
    </form>
  );
}

/** Visual / IR switch, range controls and readings, under the thermal camera's video. */
export function ThermalPanel({
  thermal,
  auto,
  disabled,
  send,
}: {
  thermal: ThermalReport;
  /** Server-side auto range is on. */
  auto: boolean;
  disabled: boolean;
  send: (c: Command) => void;
}) {
  const mode = thermal.display_mode;
  const ir = mode === MODE_THERMAL;
  const area = thermal.measure === MEASURE_AREA ? thermal.area : null;
  const band = `${thermal.isotherm_lower} · ${thermal.isotherm_middle} · ${thermal.isotherm_upper} °C`;
  return (
    <div className="thermal-panel">
      <div className="thermal-row">
        <div className="thermal-modes" role="group" aria-label="Thermal camera view">
          <button
            className={mode === MODE_VISUAL ? "active" : ""}
            disabled={disabled}
            aria-pressed={mode === MODE_VISUAL}
            onClick={() => send({ cmd: "thermal_mode", mode: MODE_VISUAL })}
          >
            Visual
          </button>
          <button
            className={ir ? "active" : ""}
            disabled={disabled}
            aria-pressed={ir}
            onClick={() => send({ cmd: "thermal_mode", mode: MODE_THERMAL })}
          >
            IR
          </button>
        </div>
        <div className="thermal-reading">
          {area ? (
            <>
              <span className="control-label">SCENE</span>
              <strong>{celsius(area.min)} – {celsius(area.max)}</strong>
              <span className="muted">avg {celsius(area.avg)}</span>
            </>
          ) : (
            <>
              <span className="control-label">SPOT</span>
              <strong>
                {thermal.measure === MEASURE_SPOT && thermal.spot_valid ? celsius(thermal.spot_temp) : "—"}
              </strong>
            </>
          )}
          <span className="muted">
            {ir
              ? auto
                ? "Clicking the video measures a spot and turns Auto off"
                : "Click the video to measure"
              : "Switch to IR to measure by clicking"}
          </span>
        </div>
      </div>
      <div className="thermal-row">
        <span className="control-label">RANGE</span>
        <div className="thermal-modes" role="group" aria-label="Isotherm range">
          <button
            className={auto ? "active" : ""}
            disabled={disabled}
            aria-pressed={auto}
            title="Fit the band to the coldest and hottest temperatures in view"
            onClick={() => send({ cmd: "thermal_auto", on: !auto })}
          >
            Auto
          </button>
          {PRESETS.map(({ name, band: [lower, middle, upper] }) => {
            const active = !auto && thermal.isotherm_lower === lower
              && thermal.isotherm_middle === middle && thermal.isotherm_upper === upper;
            return (
              <button
                key={name}
                className={active ? "active" : ""}
                disabled={disabled}
                aria-pressed={active}
                title={`${lower} · ${middle} · ${upper} °C`}
                onClick={() => send({ cmd: "isotherm", lower, middle, upper })}
              >
                {name}
              </button>
            );
          })}
        </div>
        <span className="thermal-band-now mono" title="Isotherm lower · middle · upper">
          {band}
          {!thermal.isotherm_enabled && " (isotherm off in the camera)"}
        </span>
      </div>
      <BandEditor thermal={thermal} disabled={disabled || auto} send={send} />
    </div>
  );
}
