import { Children, ReactNode, useEffect, useRef, useState } from "react";

export const fixed = (n: number, digits = 1) => n.toFixed(digits);
export const deg = (n: number) => `${fixed(n)}°`;

/** One labelled value row inside a tile. */
export function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flight-row">
      <span>{label}</span>
      <span className="mono">{value}</span>
    </div>
  );
}

export function Tile({ label, value, note, children }: {
  label: string; value: string; note?: string; children?: ReactNode;
}) {
  return (
    <div className="flight-tile">
      <span className="metric-label">{label}</span>
      <strong>{value}</strong>
      {note && <small>{note}</small>}
      {/* `false` rows are dropped, so a tile with none draws no divider. */}
      {Children.toArray(children).length > 0 && <div className="flight-rows">{children}</div>}
    </div>
  );
}

/**
 * Live while `frames` keeps advancing: true for two seconds after each
 * increase. A counter that only resets or repeats never counts as live.
 */
export function useAdvancing(frames: number | undefined) {
  const [live, setLive] = useState(false);
  const last = useRef<{ frames?: number; at: number }>({ at: 0 });
  useEffect(() => {
    if (frames === undefined) return;
    if (last.current.frames !== undefined && frames > last.current.frames) {
      last.current.at = Date.now();
      setLive(true);
    }
    last.current.frames = frames;
  }, [frames]);
  useEffect(() => {
    const timer = window.setInterval(() => setLive(Date.now() - last.current.at < 2000), 500);
    return () => clearInterval(timer);
  }, []);
  return live;
}
