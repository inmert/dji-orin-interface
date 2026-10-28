import { ReactNode, useEffect, useRef, useState } from "react";
import { VideoStatus, videoUrl } from "../api";
import { PlaybackState, playMjpeg } from "../video/playback";
import { Icon } from "./Icon";

/** Feeds the user has started, so switching views and back resumes them. */
const started = new Set<string>();

const STATE_LABEL: Record<VideoStatus["state"], string> = {
  idle: "Idle",
  connecting: "Connecting",
  streaming: "Streaming",
  error: "Unavailable",
};

export const videoStateLabel = (status: VideoStatus | undefined) =>
  status ? STATE_LABEL[status.state] : "Off";

/**
 * A camera feed's live video with a Start/Stop button. Playback is only
 * mounted while the user has started it: opening the MJPEG URL is what makes
 * the server decode (and, for the FPV feed, connect to the aircraft), and
 * unmounting it lets the server stop again.
 */
export function VideoView({
  feed,
  status,
  label,
  corner,
  aspect,
  className = "",
  onPick,
  children,
}: {
  /** Feed name on the server, e.g. `cam1`. */
  feed: string;
  status: VideoStatus | undefined;
  /** Top-left caption, e.g. `CAMERA 01`. */
  label: string;
  /** Bottom-right caption. */
  corner: string;
  /** Width / height of the video, e.g. 16 / 9. */
  aspect: number;
  className?: string;
  /**
   * Called with a click's position as fractions (0..1) of the video frame,
   * only while the video is live.
   */
  onPick?: (x: number, y: number) => void;
  /** Overlays drawn above the video (e.g. a reticle). */
  children?: ReactNode;
}) {
  const [on, setOn] = useState(() => started.has(feed));
  const canvas = useRef<HTMLCanvasElement>(null);
  const [playback, setPlayback] = useState<PlaybackState>("buffering");
  const [visible, setVisible] = useState(!document.hidden);
  useEffect(() => {
    const visibility = () => setVisible(!document.hidden);
    document.addEventListener("visibilitychange", visibility);
    return () => document.removeEventListener("visibilitychange", visibility);
  }, []);
  useEffect(() => {
    if (!on || !visible || !canvas.current) return;
    setPlayback("buffering");
    return playMjpeg(canvas.current, videoUrl(feed), setPlayback);
  }, [feed, on, visible]);
  const live = on && visible && playback === "playing";
  const message = !status
    ? "This feed is turned off on the server"
    : status.state === "error"
      ? status.error ?? "No video; retrying"
      : `Waiting for video from ${status.source}`;
  return (
    <div
      className={`video-view${live ? " live" : ""}${live && onPick ? " pickable" : ""} ${className}`}
      style={{ aspectRatio: String(aspect) }}
      onClick={(e) => {
        if (!live || !onPick || (e.target as HTMLElement).closest("button")) return;
        // The viewport has the video's aspect ratio, so its box is the frame.
        const box = e.currentTarget.getBoundingClientRect();
        onPick((e.clientX - box.left) / box.width, (e.clientY - box.top) / box.height);
      }}
    >
      {on && <canvas ref={canvas} role="img" aria-label={`${label} live video`} />}
      <div className="viewport-label">
        {label}
        <span className={live ? "live" : ""}>{on ? (live ? "Live" : playback) : "Off"}</span>
      </div>
      {children}
      {!live && (
        <div className="video-message">
          <strong>{on ? (playback === "reconnecting" ? "Reconnecting" : "Buffering") : "Video is off"}</strong>
          <span>{on ? message : "Decoding starts when you start the video"}</span>
        </div>
      )}
      <button
        className="video-toggle"
        disabled={!status}
        onClick={() => {
          if (on) started.delete(feed);
          else started.add(feed);
          setOn(!on);
        }}
      >
        <Icon name={on ? "stop" : "play"} /> {on ? "Stop video" : "Start video"}
      </button>
      <span className="viewport-corner">{corner}</span>
    </div>
  );
}
