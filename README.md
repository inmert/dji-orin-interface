# dji-orin-interface

Controlling a **DJI Matrice 210 V1** from an **NVIDIA Jetson Orin Nano** by
speaking the controller's own wire protocol — **DUML** — instead of the DJI
Onboard/Mobile SDKs.

The Orin impersonates the DJI app (DJI Pilot) to the **DJI Cendence** remote
controller over USB. Once connected, the controller relays commands to the
aircraft over its normal radio link, so the Orin can both **observe** the
drone↔app traffic and **send** commands as if it were the app.

> ⚠️ **Safety.** This project can command a real aircraft. Any test that sends
> commands must be done with **propellers removed or the aircraft physically
> secured**. You are responsible for operating your own hardware safely and
> legally. This is interoperability/research work on hardware you own.

## How it works

The DJI app connects to the Cendence controller using **Android Open Accessory
(AOA)**: the controller is the USB *host*, the app device is the USB *peripheral*.
After an AOA handshake (in which the controller announces the accessory identity
`DJI` / `T600` / `DJI Inspire 1`), the two exchange **DUML** frames over a pair
of bulk endpoints.

```
                 USB-C (Orin = USB device)          radio
 ┌──────────┐   AOA handshake + DUML bulk    ┌──────────────┐        ┌───────────┐
 │  Orin    │ ◀───────────────────────────▶ │  Cendence RC  │ ◀────▶ │  M210 V1  │
 │ (as app) │                               │  (USB host)   │        │ aircraft  │
 └──────────┘                               └──────────────┘        └───────────┘

 For learning the command set, the roles flip and the Orin drives a DJI
 CrystalSky running DJI Pilot:

                 USB-A (Orin = USB host)
 ┌──────────┐   AOA handshake + DUML bulk    ┌──────────────┐
 │  Orin    │ ◀───────────────────────────▶ │  CrystalSky   │
 │ (as RC)  │                               │ (DJI Pilot)   │
 └──────────┘                               └──────────────┘
```

See [docs/protocol.md](docs/protocol.md) for the DUML frame format and the AOA
handshake, and [docs/hardware.md](docs/hardware.md) for the USB topology and
the specific interfaces each device exposes.

## Repository layout

```
Cargo.toml              Package manifest (web-server deps; core stays dep-free)
src/lib.rs              Library root (module declarations)
src/log.rs              Timestamped line logging + hex helpers
src/duml.rs             DUML framing, checksums, Decoder, build, addressing
src/usb.rs              USB transport submodule root
src/usb/sys.rs          Raw kernel ABI (libc calls + usbfs ioctl structs/codes)
src/usb/usbfs.rs        Host-side USB: device lookup, control/bulk transfers
src/usb/ffs.rs          Device-side FunctionFS: descriptors, ep0 events/loop
src/aoa.rs              Android Open Accessory constants + DJI identity
src/aoa/host.rs         Orin as AOA host (toward a tablet): handshake, discovery
src/aoa/device.rs       Orin as AOA device (toward the RC): ep0 answers
src/gimbal.rs           Gimbal feature: ids, Camera addressing
src/gimbal/command.rs   Gimbal move + camera-lock frames we inject
src/gimbal/attitude.rs  Gimbal attitude report decode (provisional)
src/telemetry.rs        Serializable link snapshot for the web UI
src/bin/ffs_capture.rs  Orin as the app toward the controller
src/bin/aoa_capture.rs  Orin as the controller toward DJI Pilot
src/bin/bridge.rs       Transparent DUML MITM (both roles at once)
src/bin/server/         Orin as the app + web UI:
  main.rs                 gadget event loop, worker lifecycle
  state.rs                desired gimbal state + UI command messages
  injector.rs             streams gimbal commands to the RC (ep2)
  reader.rs               decodes RC telemetry (ep1)
  ws.rs / http.rs         WebSocket + static file server
web/                    Vite + React + TypeScript front end (built to web/dist)
  src/api.ts              WebSocket link + shared message types
  src/gimbal/             gimbal pad (per-camera controls)
  src/telemetry/          telemetry streams table
tests/duml_roundtrip.rs Black-box integration tests of the public DUML API
examples/decode_frames.rs  Minimal Decoder usage example
benches/duml_bench.rs   CRC/scan micro-benchmark (no external harness)
scripts/run_capture.sh  Bring up the FunctionFS gadget + start ffs_capture
scripts/run_bridge.sh   Bring up the gadget + start the MITM bridge
scripts/run_server.sh   Bring up the gadget + start the web control server
scripts/restore_gadget.sh  Tear it down, restore the stock USB gadget
docs/                   Protocol and hardware notes
```

Each aircraft feature gets its own module (`gimbal` today) with the same split:
a `command` submodule for frames we inject and a report submodule for what the
aircraft pushes up, plus a matching folder under `web/src/`. A future feature
(e.g. props/motors) slots in as a sibling `src/<feature>.rs` + `src/<feature>/`.

The project is **Rust** (edition 2024). The **core is dependency-free** — the USB
and gadget plumbing uses the system libc through a small set of hand-declared
externs in `usb::sys`, and `duml`/`gimbal`/`telemetry` add nothing. Only the
web `server` binary pulls crates (`tiny_http`, `tungstenite`, `serde`/
`serde_json`); the capture/bridge tools and the library still build offline. The
reusable protocol logic
lives in the `duml` module (`parse`/`scan`/`build`, the streaming `Decoder`,
`Frame`, `ParseError`) so the gadget side, the host side, and any future command
sender share one validated implementation of framing and checksums. Safe, typed
USB wrappers (`usb::usbfs`, `usb::ffs`) keep raw pointers out of the binaries.
File
descriptors are owned by `std::os::fd::OwnedFd` (closed on drop); the gadget's
bulk reader runs on a `std::thread`.

## Building

**Toolchain:** Rust via `rustup`, edition 2024 (needs **rustc 1.85+**;
developed with 1.99 on aarch64). No build-time kernel headers — the usbfs/gadget
ABI is declared in `sys`. If `cargo` isn't on your PATH:
`export PATH="$HOME/.cargo/bin:$PATH"`. The web UI additionally needs Node
(developed with Node 24 / npm 11) to build, and a few small crates are fetched
for the `server` binary.

```bash
cargo build --release   # binaries in target/release/{ffs_capture,aoa_capture}
cargo test              # runs the DUML unit tests
cargo clippy            # lint
```

### VSCode

Install the **rust-analyzer** extension (not the C/C++ one); no extra config is
needed. The repo ships `.vscode/tasks.json`, so **Terminal → Run Build Task**
runs `cargo build` and **Run Test Task** runs `cargo test`. The capture programs
need `sudo` and the real USB hardware, so run those from a terminal, not the
VSCode run button.

## Usage

Both tools require root (USB gadget config and raw USB access) and are specific
to this Orin's USB hardware. See the docs for wiring details.

**Capture / act as the app toward the Cendence controller** (Orin USB-C ↔ RC
USB-C tablet port):

```bash
cargo build --release
sudo scripts/run_capture.sh            # logs to capture.log
# ... plug in / power the controller ...
sudo scripts/restore_gadget.sh         # when done
```

**Capture DJI Pilot's command stream** (DJI CrystalSky on a Orin USB-A port,
Pilot running):

```bash
sudo target/release/aoa_capture pilot.log
# tap OK on any USB-accessory prompt on the CrystalSky
```

**Transparent MITM bridge** (tablet on a USB-A port, RC on the USB-C port): the
Orin is an AOA host toward the tablet and an AOA device toward the RC at once,
forwarding the raw bulk streams both ways and logging every DUML frame, so DJI
Pilot flies the real aircraft through the Orin. This milestone is pure
passthrough — nothing is injected.

> ⚠️ The forwarded stream carries Pilot's flight commands to the **powered**
> aircraft. Run only with **propellers removed / aircraft secured**.

```bash
cargo build --release
# plug the CrystalSky into a USB-A port first, then:
sudo scripts/run_bridge.sh             # logs to bridge.log
# ... connect the RC on USB-C; tap OK on any USB-accessory prompt ...
sudo scripts/restore_gadget.sh         # when done
```

**Web control server** (no tablet): the Orin is the app toward the RC and serves
a browser UI — live telemetry and **gimbal control for both cameras**. The
gimbal move command is injected onto the RC; the two cameras are gimbal device
index 0 and 2 (see [docs/protocol.md](docs/protocol.md)). The **LINK** toggle
engages the aircraft's native dual-gimbal lock (camera 2 snaps to camera 1 and
follows it); its state is server-side, so a page refresh reflects it.

> ⚠️ The UI's gimbal buttons move the real camera mount via the **powered**
> aircraft. Nothing is injected until you press a control, and the on-screen
> **STOP** or closing the browser releases every gimbal. Run with **propellers
> removed / aircraft secured**.

```bash
cargo build --release
(cd web && npm install && npm run build)   # build the React UI into web/dist
# connect the RC on USB-C, then:
sudo scripts/run_server.sh                 # logs to server.log
# open http://<orin-ip>:8080/ on a device on the same network
sudo scripts/restore_gadget.sh             # when done
```

Live video (the two camera feeds) is a separate research spike: the downlink is
DJI's encrypted/encoded stream, so the UI shows a placeholder for now.

## Status

- [x] USB transport identified (AOA; controller is the USB host)
- [x] AOA handshake implemented both directions (device and host)
- [x] DUML framing + checksums implemented and validated against captures
- [x] Receive & decode DUML from the controller and from DJI Pilot
- [x] Transparent MITM bridge — passthrough + logging (hardware-tested)
- [x] Gimbal control command decoded (cmd_set 0x04/0x0c, both cameras)
- [x] Web UI — telemetry + gimbal control, over a native Rust server
- [x] Inject/calibrate gimbal control on hardware — working, both cameras (props off)
- [x] Camera LINK — native dual-gimbal lock (both move together), hardware-verified
- [ ] Live camera video in the UI (encrypted downlink — research spike)
- [ ] Send/inject flight commands to the aircraft

## Acknowledgements

DUML has been studied extensively by the community (e.g. `dji-firmware-tools`,
`pyduml`, the Wireshark DUML dissector). The frame format and checksum
parameters used here were re-derived and verified against traffic captured from
this project's own hardware.
