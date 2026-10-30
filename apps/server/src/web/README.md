# web

The browser-facing half of the server.

| File | Purpose |
|------|---------|
| `../web.rs` | `Context` shared by every connection: web root, feeds, vehicle |
| `http.rs` | hand-rolled HTTP/1.1, one thread per connection, on the plain and TLS listeners: static files, `/video/<feed>.mjpg`, `/ws` upgrade, `POST /voice`, `GET /ai/status` |
| `ws.rs` | the WebSocket: the telemetry snapshot down at 10 Hz, portal messages up; origin check; gimbals released when a client goes |
| `portal.rs` | `WsCommand` (mirrors `web/src/api.ts`) mapped onto the vehicle's typed operations; sentence plans interpreted off-thread |
| `tls.rs` | the self-signed certificate for the HTTPS listener (browsers only open the microphone on a secure origin) |

The front end itself is the Vite + React app in `/web`, built to `web/dist`.
