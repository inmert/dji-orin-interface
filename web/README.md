# web

The portal: a Vite + React + TypeScript front end, built to `web/dist` and
served by the Rust server on `:8080` / `:8443`.

| Path | Purpose |
|------|---------|
| `src/api.ts` | the WebSocket link (`useLink`): snapshot validation, reconnect, the `Command` wire type mirrored from `apps/server/src/web/portal.rs` |
| `src/App.tsx`, `src/main.tsx` | layout and entry point |
| `src/gimbal/` | camera cards: `GimbalPad` (joystick, keyboard), `Thermal` (spot meter, isotherm band, auto range), `FpvCard` |
| `src/telemetry/` | `FlightPanel` (flight data, motors, the command bar with hold-to-talk), `UsbPanel`, `StreamsTable`, `Tile` |
| `src/video/playback.ts` | MJPEG multipart reader, newest-frame display |
| `src/components/` | `VideoView`, `Icon` |
| `tests/*.cjs` | node tests: controls, video playback, thermal mapping (`npm test`) |

Build: `npm ci && npm run build`. Dev: `npm run dev` proxies to a running
server. Every snapshot field a component dereferences is type-checked in
`api.ts` before it reaches a component.
