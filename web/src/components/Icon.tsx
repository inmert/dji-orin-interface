/** Small stroke icons (16 px, currentColor), inline so nothing is fetched. */
const PATHS = {
  camera: "M2.5 5.5h2l1.2-1.8h4.6l1.2 1.8h2v7h-11z M8 11a2 2 0 1 0 0-4 2 2 0 0 0 0 4z",
  plane: "M8 1.8v12.4 M2.5 8.8 8 6.5l5.5 2.3 M5.5 13.5 8 12.3l2.5 1.2",
  usb: "M8 1.8v10 M5.5 4.5 8 1.8l2.5 2.7 M8 9 4.5 7V5.5 M8 10.5l3.5-2V7 M8 14.2a1.4 1.4 0 1 0 0-2.8 1.4 1.4 0 0 0 0 2.8z",
  list: "M5.5 4h8 M5.5 8h8 M5.5 12h8 M2.5 4h.5 M2.5 8h.5 M2.5 12h.5",
  link: "M6.8 9.2a2.6 2.6 0 0 0 3.7 0l2-2a2.6 2.6 0 0 0-3.7-3.7l-.6.6 M9.2 6.8a2.6 2.6 0 0 0-3.7 0l-2 2a2.6 2.6 0 0 0 3.7 3.7l.6-.6",
  recenter: "M13.2 8A5.2 5.2 0 1 1 11.6 4.3 M13.2 2.5v3h-3",
  stop: "M4.5 4.5h7v7h-7z",
  play: "M5 3.5v9l7-4.5z",
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({ name }: { name: IconName }) {
  return (
    <svg className="icon" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
      <path d={PATHS[name]} />
    </svg>
  );
}
