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

The aircraft can also be reached **directly, without the RC**: its USB API
port (mode switch in the PC position) appears on the Orin as `/dev/ttyACM0` and
carries the same DUML in both directions, with no SDK activation. Its OSDK UART
works both ways through a 3.3 V USB-TTL adapter (the flight controller answers
version queries); through the Orin's own UART it only receives. Its flight-data
broadcast (attitude, altitude, velocity, GPS, battery, status) is decoded
receive-only and shown in the web UI. See
[docs/hardware.md](docs/hardware.md#direct-aircraft-links-no-rc).

See [docs/protocol.md](docs/protocol.md) for the DUML frame format and the AOA
handshake, and [docs/hardware.md](docs/hardware.md) for the USB topology and
the specific interfaces each device exposes.

## Repository layout

A Cargo **workspace**, layered so each crate has one job and dependencies only
point downward (apps → vehicle → transport/protocol → common):

```
Cargo.toml                    Workspace root: members, shared versions + lints
crates/
  common/                     SHARED — no deps
    src/log.rs                  timestamped logging (logln!), Hex / hex
  protocol/                   DOMAIN — pure logic, no I/O (serde optional)
    src/duml.rs                 DUML framing, checksums, Decoder, build, addressing
    src/gimbal.rs               gimbal feature: ids, Camera addressing
    src/gimbal/command.rs       move + camera-lock frames we inject
    src/gimbal/attitude.rs      attitude report decode (0.1° units; yaw verified)
    src/flyc.rs, src/flyc/osd.rs  flight controller: 0x03/0x43 report decode, motor + USB-flight frames
    src/osdk.rs                 OSDK OpenProtocol (0xAA) framing
    src/osdk/broadcast.rs       OSDK flight-data broadcast decode
    src/osdk/command.rs         activation, control authority, motor requests + ACK codes
    src/rcmux.rs                RC USB packet mux demuxer (DUML + camera video channels)
    src/thermal.rs              XT2 thermal core: 0xF2 report decode, 0xF1 settings frames
    src/telemetry.rs            link snapshot for the web UI
    tests/  benches/  examples/ integration tests, CRC/scan bench, decode examples
  transport/                  INFRASTRUCTURE — libc via hand-declared externs
    src/usb.rs                  read/write/errno for endpoint fds
    src/usb/usbfs.rs            host side: device lookup, control/bulk transfers
    src/usb/ffs.rs              gadget side: FunctionFS descriptors, ep0 events
    src/usb/sys.rs              raw kernel ABI (private to the crate)
    src/aoa.rs                  Android Open Accessory constants + DJI identity
    src/aoa/host.rs             Orin as AOA host (toward a tablet)
    src/aoa/device.rs           Orin as AOA device (toward the RC)
    src/serial.rs               serial ports: RxPort (read-only) + TxPort (one command at a time)
  udt/                        INFRASTRUCTURE — UDT4 client for camera video
    vendor/udt4/                UDT 4.11 from DJI's Onboard SDK (BSD licence)
    shim.cpp, build.rs          C API over it, built with the system C++ compiler
    src/lib.rs                  safe receive-only Stream (connect/recv/close)
  commands/                   DOMAIN — the command model (serde, serde_json)
    src/lib.rs                  Command, Plan + Limits, the JSON shape for structured output
    src/capability.rs           the registry: one descriptor per step; derives the models' schema, instructions and step decoding
    src/grammar.rs              plain sentences → Plan ("arm for 5 seconds then disarm")
  vehicle/                    APPLICATION — the aircraft as typed capabilities over its links
    src/lib.rs                  Vehicle: telemetry snapshot + gimbal, thermal, motors, executor
    src/executor.rs             runs a Plan step by step: progress, cancel, motors stopped on failure
    src/gimbal.rs               desired move per camera, camera lock, yaw recenters
    src/thermal.rs, thermal/auto.rs  XT2 settings queue; auto range fits the isotherm band
    src/motors.rs, motors/      motor requests + status; rc.rs (0x03/0x2a over the RC link),
                                uart.rs (OSDK activation/authority sequence, USB unlock), live.rs (armed state)
    src/mailbox.rs              ACKs from the readers, matched by sequence for a waiting request
    src/links/rc.rs             RC gadget: reads ep1 (telemetry + video channels), writes ep2
    src/links/serial.rs         direct aircraft readers: OSDK UART + USB API port (rx only)
apps/
  tools/                      PRESENTATION — capture/bridge CLIs
    src/bin/ffs_capture.rs      Orin as the app toward the controller
    src/bin/aoa_capture.rs      Orin as the controller toward DJI Pilot
    src/bin/bridge.rs           transparent DUML MITM (both roles at once)
  server/                     PRESENTATION — web control server (bin `server`)
    src/main.rs                 start-up: settings, the vehicle, the workers, then the gadget loop
    src/config.rs               config.toml + the env overrides; the only place the environment is read
    src/gadget.rs               gadget ep0 event loop; starts the RC link workers on ENABLE
    src/ports.rs                default serial ports for the direct readers (re-picked on reopen)
    src/video.rs                camera video: RC mux / UDT H.264 → NVDEC → MJPEG, on demand
    src/web.rs, web/            the browser side
      http.rs                     HTTP(S) on one port each: files, video, /ws upgrade, POST /voice
      ws.rs                       the WebSocket: snapshots down, portal messages up
      portal.rs                   the portal's WebSocket messages → Vehicle operations
      tls.rs                      self-signed certificate for the HTTPS listener (browser microphone)
    src/ai.rs, ai/              the services behind the command bar
      online.rs                   API keys from secrets/ (by prefix or file name) + HTTP helpers
      voice.rs, voice/*.rs        voice upload → chosen speech service (OpenAI, Deepgram) → text; status
      understand.rs, understand/* sentence → plan: Anthropic, OpenAI, or llama-server on the Orin; grammar fallback
web/                          Vite + React + TypeScript front end (built to web/dist)
scripts/                      gadget bring-up/teardown + run_{capture,bridge,server}.sh, run_local_llm.sh
config.toml                   the server's settings, every value at its default (see Configuration)
logs/                         run-script logs (git-ignored)
docs/                         protocol and hardware notes
.github/workflows/ci.yml      fmt, clippy, tests, docs, web build
```

Every crate and module folder carries a `README.md` saying what it is for,
what each file holds and the rule the folder follows; the tree above is the
map, those are the detail.

Each aircraft feature gets its own module in `dji-protocol` (`gimbal`, `flyc`,
`thermal`); `rcmux` is a decode-only wire format and `osdk` decodes the UART
broadcast and builds the few commands the motor path sends. Where a feature
goes both ways, the frames we inject are kept apart from the reports the
aircraft pushes up: `gimbal` has `command` and `attitude` submodules, and
`thermal` keeps its settings frames and its `Report` decoder in one file. A
future feature slots in as a sibling `src/<feature>.rs` + `src/<feature>/`.

Everything that *controls* the aircraft is in `dji-vehicle`: a `Vehicle` holds
the telemetry snapshot and one struct per feature (`gimbal`, `thermal`,
`motors`) with typed operations, and its `links` workers (RC gadget endpoints,
serial ports) turn that state into frames and fold the replies back. The web
server is presentation only: it maps the portal's messages onto those
operations, so a second caller (a command API, say) uses the same ones.

The project is **Rust** (edition 2024). `common`, `protocol` and `transport` are
**dependency-free**, so the capture tools (`cargo build -p dji-tools`) never
compile the web stack. `dji-commands` adds `serde` and `serde_json` (plans
are serialized to the UI; the capability registry derives the models' JSON
schema), `dji-vehicle` adds `serde` and turns on `dji-protocol`'s `serde`
feature; `dji-server` alone pulls the web crates (`tungstenite`, `toml`, and
`openssl` against the system library for the HTTPS listener). `dji-udt` has no crate dependencies either; its
build script compiles the vendored C++ with the system `c++` and `ar`. Framing and checksums live once in
`dji_protocol::duml` (`parse`/`scan`/`build`, the streaming `Decoder`), shared
by the gadget side, the host side, and the command builders. Safe USB wrappers
(`usb::usbfs`, `usb::ffs`) keep raw pointers inside `dji-transport`; file
descriptors are `std::os::fd::OwnedFd` (closed on drop).

## Building

**Toolchain:** Rust via `rustup`, edition 2024 (needs **rustc 1.85+**;
developed with 1.99 on aarch64). No build-time kernel headers — the usbfs/gadget
ABI is declared in `sys`. If `cargo` isn't on your PATH:
`export PATH="$HOME/.cargo/bin:$PATH"`. The web UI additionally needs Node 18+
to build (CI uses 22; developed with Node 24 / npm 11), and a few small crates
are fetched for the `server` binary. `dji-udt` needs a C++ compiler (`c++`,
`ar`) for its vendored UDT library.

**Runtime, for camera video only:** GStreamer (`gst-launch-1.0`) with NVIDIA's
L4T plugins (`nvv4l2decoder`, `nvvidconv`, `nvjpegenc`), which JetPack
installs. Without them the server runs, but the video feeds fail to start (see
[docs/hardware.md](docs/hardware.md#video-decoding-on-the-orin)).

```bash
cargo build --release   # binaries in target/release/{ffs_capture,aoa_capture,bridge,server}
cargo test --workspace  # all unit + integration tests
cargo clippy --workspace --all-targets   # lint
```

### VSCode

Install the **rust-analyzer** extension (not the C/C++ one); no extra config is
needed. The repo ships `.vscode/tasks.json`, so **Terminal → Run Build Task**
runs `cargo build` (the whole workspace, including the server's web crates;
use `cargo build -p dji-tools` for just the capture tools) and **Run Test
Task** runs `cargo test --workspace`. The
capture programs need `sudo` and the real USB hardware, so run those from a
terminal, not the VSCode run button.

## Usage

All the binaries require root (USB gadget config and raw USB access) and are specific
to this Orin's USB hardware. See the docs for wiring details.

**Capture / act as the app toward the Cendence controller** (Orin USB-C ↔ RC
USB-C tablet port):

```bash
cargo build --release
sudo scripts/run_capture.sh            # logs to logs/capture.log
# ... plug in / power the controller ...
sudo scripts/restore_gadget.sh         # when done
```

**Capture DJI Pilot's command stream** (DJI CrystalSky on a Orin USB-A port,
Pilot running):

```bash
sudo target/release/aoa_capture logs/pilot.log
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
sudo scripts/run_bridge.sh             # logs to logs/bridge.log
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
(cd web && npm ci && npm run build)       # build the React UI into web/dist
sudo scripts/run_server.sh                 # logs to logs/server.log; RC NOT plugged in yet
# power the RC on, let it fully boot, THEN plug its USB-C tablet port into the Orin
# open http://<orin-ip>:8080/ on a device on the same network
# (https://<orin-ip>:8443/, self-signed, for voice input)
sudo scripts/restore_gadget.sh             # when done
```

**Bring-up order matters.** Start the server first, power the RC on and let it
fully boot, and only then plug the cable in; `logs/server.log` shows `gadget: ENABLE`
within seconds. Two ways it fails silently (the log stops at `gadget: BIND`):

- *The RC boots with the cable already attached.* While booting it looks like a
  sink, so the Orin's Type-C controller locks the port into source (host) mode
  and never re-checks, and no gadget can attach. Check with
  `cat /sys/bus/i2c/devices/1-0025/fusb301/fmode`: `SNK(4)` is right, `SRC(1)`
  is this failure. Unplug, then replug with the RC already booted.
- *The roles are right (`SNK(4)`) but `/sys/class/udc/*/state` stays `default`.*
  The RC reset the bus but never enumerated, typically after it sat connected to
  a gadget that couldn't answer it (the stock `l4t` one, or a wedged teardown).
  Replugging doesn't clear this; power-cycle the RC with the cable unplugged,
  then plug in after it boots.

**Configuration.** Everything the server can be told lives in
[`config.toml`](config.toml) at the repository root: listeners and web root,
the secrets directory, serial ports, the Onboard SDK app id, gimbal frame
settings, video feeds, the speech and understanding providers and models,
and the plan limits. Every key is optional and the checked-in file holds the
defaults, so it doubles as the reference (a test keeps the two equal). Relative
paths are relative to the file; `CONFIG=<file>` names another one, which
`run_server.sh` exports as `$REPO/config.toml`. The environment variable noted
beside each key (`OSDK_APP_ID`, `OSDK_UART`, `LLM_PROVIDER`, …) overrides it
for one run, which is what the `sudo VAR=… scripts/run_server.sh` examples below
do; the server reads the environment nowhere else. Protocol timing (frame rates,
ACK timeouts) is deliberately not configurable: those values were verified on the
aircraft.

**Restarting without replugging.** Once the RC link is up, restart the server
(new build, other env vars) with the RC left plugged in:

```bash
sudo OSDK_APP_ID=<app id> scripts/restart_server.sh
```

It stops the server and removes its gadget *without* rebinding the stock `l4t`
gadget in between (which is what left the RC stuck in `default` before), starts
the new server, and waits for `gadget: ENABLE` (about half a second on the
aircraft). If the RC does not enable it within 5 s, it makes the RC
re-enumerate with a soft disconnect/connect of the UDC
(`/sys/class/udc/*/soft_connect`): the data pull-up drops and returns as on a
replug while the cable and Type-C roles stay put. The same two writes by hand
re-enumerate a running server's link (ENABLE 1.4 s later, data flowing again).
This does not clear the Type-C source lock above; that still needs a replug
after the RC has booted.

**Flight telemetry.** The server also reads the aircraft's OSDK UART
(230400 baud) and shows its flight-data broadcast in
the UI's *Flight telemetry* panel: status and flight mode, attitude, altitude,
velocity, GPS, battery, obstacle distances, RC sticks, gimbal and compass. The
reader opens the port read-only; only the motor buttons below send on it. It runs
independently of the RC link, so it works with the RC unplugged too. By
default it reads a CP210x USB-TTL adapter when one is plugged in (the path that
works both ways, see [docs/hardware.md](docs/hardware.md)), else the Orin's own
UART, `/dev/ttyTHS1`. It chooses again each time it reopens the port.
`OSDK_UART=<port>` fixes the port, `OSDK_UART=off` disables it, and
`OSDK_BAUD` changes the rate. `sudo` drops variables set before it, so put
them after:

```bash
sudo OSDK_UART=/dev/ttyTHS1 scripts/run_server.sh
```

**Motors.** The *Flight telemetry* page has **Start motors** (asks for
confirmation) and **Stop motors**; they start and stop all four motors
together. With the RC link up they go over it as the DJI app would (DUML
`0x03/0x2a`, no activation needed), so neither the UART nor the USB cable is
required. Without the RC link, or if the FC does not answer there, the server
falls back to the OSDK UART: it activates the Onboard SDK app given by
`OSDK_APP_ID` (an app activated once with DJI Assistant 2 online), takes SDK
control authority, sends the command and hands authority back to the RC. The
panel shows the FC's answer and which path was used. The RC must be in P mode.
While the aircraft's USB API port is connected the FC refuses to arm until
**Allow with USB** has been pressed once since the aircraft last started (it
sends `0x03/0x1b` on that port under authority taken over the UART). **Props
off** unless you mean to fly:

```bash
sudo OSDK_APP_ID=<app id> scripts/run_server.sh
```

**Text and voice commands.** The *Flight telemetry* page also has a command
bar. Type a sentence such as `arm for 5 seconds then disarm`, press **Check**
to see the steps the server made of it, then **Run** (asks for confirmation);
**Cancel** stops a running plan after its current step. Understanding the
sentence is a model's job first (`apps/server/src/ai/understand.rs`:
Claude through Anthropic's API, OpenAI, or a model served on the Orin, see
below; the first with a key in `secrets/` is used, `LLM_PROVIDER` /
`LLM_MODEL` choose), asked for the plan as structured output against a
fixed schema, so free-form phrasing works. The local grammar
(`crates/commands`) is the fallback when no model is configured or the call
fails, and understands:

- motors: `arm` / `start motors` / `spin up`, `disarm` / `stop`; `arm for N`
  always disarms again afterwards;
- `wait` / `hold` with seconds or minutes;
- gimbal: `pan left 2 seconds`, `pan camera 2 right for 1 s`, `look up`,
  `tilt down 3 s` (camera 1 and one second unless said), `recenter`,
  `recenter camera 2`, `link cameras`, `unlink`;

joined by *then*, *and* or commas. A JSON plan
(`{"steps":[{"cmd":"arm"},{"cmd":"wait","seconds":5},{"cmd":"disarm"}]}`) is
accepted in the same box and skips both. Whatever produced it, every plan
is checked on the Orin against the limits (60 s per wait, 60 s of motors per
plan, 10 s per gimbal move) before it runs, and a plan that fails or is
cancelled after arming stops the motors first. The bar says which source
made the plan. Motor steps go out over the same path as the buttons: the RC link
first. Verified on the aircraft with `arm for 3 seconds then disarm`
(2026-10-29, props on): ACK 0.2 s after each step, FC status On ground for
the 3 s, then Stopped.

**A model on the Orin instead.** The same interface takes a model served
locally by llama.cpp's `llama-server` (`scripts/run_local_llm.sh` starts it
on the GPU; `[understanding.local]` in `config.toml` points at it,
`provider = "local"` makes it the one used). It gets the plan schema as a
grammar, so its output is always well-formed JSON, and the Orin validates it
like any other. Measured 2026-10-30 on this Orin with the ignored test in
`apps/server/src/ai/understand/bench.rs` (twelve sentences: the two from the
first live run, the grammar's own forms, free phrasing, two flight requests
that must be declined; every provider gets the same instructions and schema):

| Provider | Right | Latency, median | Notes |
|----------|-------|-----------------|-------|
| Claude `claude-opus-5-5`, effort low | 12 / 12 | 2.4 s (1.6 to 6.3 s) | both flight requests declined with a reason |
| Llama 3.2 3B Instruct Q4_K_M, GPU, 17 tok/s | 2 / 12 | 4.5 s (2.5 to 11 s) | drops the wait in `arm for N seconds`, and **invents motor and gimbal steps for "take off"** |
| Llama 3.2 1B Instruct Q4_K_M | 2 / 12 | 3.4 s | declines almost everything, or echoes the instructions as the problem |

So the online model stays the default and the local provider is a choice, not
a recommendation, at this model size: a 3B model that answers a flight
request with an arm step is a safety problem the limits do not catch. The
benchmark also found why Claude had looked worse than this: `serde_json` sorts
object keys, so the schema reached the model with `problem` before `steps`
and `camera` before `cmd`, and Claude then stopped after the first step on
most multi-step sentences (6 of 12 right). Building `serde_json` with
`preserve_order` fixed it.

**Hold to talk.** The same bar has a **Talk** button: hold it, speak, release,
and the sentence appears in the box already checked. The browser's recording
goes to the server (`POST /voice`), which forwards it to an online
speech-recognition service and returns the text; the Orin itself does no
speech decoding, only the validation and execution of the resulting command.
The services sit behind one interface (`apps/server/src/ai/voice.rs`), so the
provider is a choice: **OpenAI** (`gpt-4o-transcribe`) or **Deepgram**
(`nova-3`), each given the command vocabulary as context. Put the API key,
one line, in `secrets/openai.key` or `secrets/deepgram.key` (git-ignored;
`OPENAI_API_KEY` / `DEEPGRAM_API_KEY` override) and restart; the first
configured provider is used, `STT_PROVIDER=deepgram` picks one, and
`STT_MODEL` changes the model. Only the server ever holds a key; an Anthropic
key is recognised and ignored, since Claude does not transcribe speech. The
bar shows, for speech and for understanding, the active service, whether
its key was accepted, and the balance or spend when the service reports one
(Deepgram balance; Anthropic month-to-date spend with an Admin API key in
`secrets/`; OpenAI's API has neither); `GET /ai/status` returns the same for
every configured provider. Browsers only open the microphone on a secure origin, so use the
HTTPS listener:
`https://<orin-ip>:8443/`, with a self-signed certificate the server
generates on first start into `certs/` (accept the browser's warning once;
`TLS=off` disables the listener). The WebSocket is served on the same port
as the page (`/ws`), so there is nothing else to accept.

**Direct USB link.** The server likewise reads the aircraft's USB API port
(mode switch in the PC position) read-only: the first `ttyACM` that is not the
CrystalSky. After an aircraft restart its port may not come back, and the
CrystalSky then takes `ttyACM0`; the panel then says no port was found
instead of showing the tablet's traffic. It shows the reports
the aircraft sends there unasked in the *Direct USB link* panel: attitude,
flight mode, satellite count and the camera 1 gimbal angles. Position, height
and velocity are shown as unverified until checked with a GPS fix and in
motion. `USB_ACM` picks another port, or `off` disables it.

**Camera video.** Each camera card has a **Start video** button. The server
serves three feeds as MJPEG at `http://<orin>:8080/video/<feed>.mjpg`:

| Feed | Camera | Source |
|------|--------|--------|
| `cam1` | gimbal 0, Zenmuse XT2 (visual or thermal picture) | RC USB mux channel `0x11`, 1280×720, scaled to 640×360 |
| `cam2` | gimbal 2, Zenmuse Z30 | RC USB mux channel `0x15`, 1280×720, scaled to 640×360 |
| `fpv` | fixed FPV camera | UDT `192.168.42.2:40003` on the aircraft's USB network, 608×448 |

A feed decodes only while someone watches and stops 3 s after the last viewer
leaves. The RC feeds are receive-only: the reader already gets those bytes.
The FPV feed opens its UDT connection only while watched, and sends only UDT's
own handshake and ACKs. Decoding runs in a `gst-launch-1.0` child on the Orin's
hardware decoder (`nvv4l2decoder is-gdr-stream=true` → `nvvidconv` →
`nvjpegenc`, from NVIDIA's L4T GStreamer plugins). That costs about a quarter
of the CPU of software decoding, and each viewer gets about 5 Mbit/s per
feed. `RC_VIDEO` maps the RC feeds to mux channels (default
`cam1=0x11,cam2=0x15`, or `off`); adding `fpv=0x13` takes the FPV feed from the
RC link instead of UDT. `FPV_UDT` sets the FPV address, or `off`.
`[video] rc_dump` (or `RC_DUMP=<file>`) appends the raw RC→Orin bytes to a file (capped at 512 MB) for
offline analysis; `cargo run -p dji-protocol --example rcmux_split` splits
one into per-channel H.264. MJPEG keeps it working from any browser over plain
HTTP, where in-browser H.264 decoding (WebCodecs) would need HTTPS.

**Thermal camera.** Under camera 01 the portal shows the XT2's thermal
controls, sent to its thermal core (`Camera[1]`, see
[docs/protocol.md](docs/protocol.md#xt2-thermal-camera-camera1)):
**Visual / IR** display mode; in IR, click the video to move the spot meter
and read its temperature. **Range** sets the isotherm band: **Auto** keeps it
fitted to the coldest and hottest temperatures in view (full-frame area
measurement, refit with 4 °C of hysteresis); **People** (27 · 32 · 38 °C) and
**Fire** (27 · 149 · 538 °C) match DJI Pilot's presets; or type a custom
lower · middle · upper. The server orders the writes so the camera never sees
lower > middle > upper. Every thermal setting is a command to the aircraft.

**Bridge captures.** `BRIDGE_DUMP_DIR=<dir>` makes `bridge` save both raw
streams (`rc2tab.bin`, `tab2rc.bin`, each with an `.idx` of read times; up to
512 MB each). It never overwrites an earlier dump, so use a new directory per
run.

To check a raw capture offline:
`cargo run -p dji-protocol --example osdk_broadcast -- <capture.bin>`.

For an **offline UI preview**, run `cd web && npm run dev` and open the printed
local URL with `?preview=1` appended. Preview mode never opens the control
WebSocket. `npm test` runs the frontend's offline checks: control handlers and
connection lifecycle, Streams search, MJPEG playback (chunking, newest-frame
display, cleanup), the thermal click mapping, and isotherm band validation
(including empty fields). The UI disables movement until the received frame counter
advances, and marks telemetry stale after two seconds without new frames.
Hold and drag a camera's joystick to pan and tilt; drag distance scales the
command up to the speed slider's limit, including diagonal movement. Releasing
returns it to center and stops movement. Arrow keys provide keyboard control.
Camera 2's joystick is disabled while linked; stop controls remain available.
**Recenter** (per camera) and **Recenter all** return yaw to center and keep
pitch, using the aircraft's native reset. Any movement on the camera is released
first. While linked, every recenter goes to camera 1 only, and the aircraft
brings camera 2 along. A reset is sent once and never retried automatically,
because a missing acknowledgement does not mean the gimbal did not move.

### Portal design

The portal is laid out like a desktop app: a title bar with the global actions
(link cameras, recenter all, stop all), a view rail (Cameras, Flight, USB link,
Streams), and a status bar with link state and frame counts. Only the active
view is mounted. The styling is flat, with no animation loops, canvas effects,
blur or gradients, so the page's cost is essentially the video canvases.
Started videos resume when you come back to the Cameras view. Preview it with
`cd web && npm run dev` and append `?preview=1` to the URL; this never opens
the aircraft control WebSocket.

Cameras 01 and 02 show the gimbal cameras' live video from the RC link. The
aircraft's third, fixed **FPV camera** at the centre front has its own pane, with
its mount's heading and the video stream described above (see
[docs/protocol.md](docs/protocol.md#fpv-camera-third-camera)). It has no controls.

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
- [x] Native yaw reset decoded and hardware-verified: each camera independently,
      both via the master when linked; pitch is preserved
- [x] Direct USB link to the aircraft (`/dev/ttyACM0`, DUML both ways, no RC)
- [x] Flight telemetry in the UI from the OSDK UART broadcast (receive-only, live-verified)
- [x] Flight telemetry from the direct USB link (`ttyACM0`): attitude, mode, satellites, gimbal (live-verified)
- [ ] Verify USB-link position/height/velocity offsets (needs a GPS fix and motion)
- [x] OSDK UART two-way via a 3.3 V USB-TTL adapter: FC acks version queries
- [x] OSDK 3.9.0 activation + telemetry subscription on the M210 V1 (via DJI Assistant 2 online)
- [ ] OSDK UART via the Orin's own pins: receive-only, cause on the Orin side not isolated
- [x] Portal yaw-recenter per camera and recenter-all, verified on the aircraft
      (2026-10-28; see docs/protocol.md)
- [x] Third (FPV) camera identified: fixed mount `Gimbal[4]`; shown in the UI
- [x] FPV camera video received as H.264 over USB/UDT (192.168.42.2:40003), decoded offline
- [x] Live FPV camera video in the UI (MJPEG from the UDT stream, on demand)
- [x] RC USB downlink decoded: plain packet mux of DUML + three H.264 streams
- [x] Live gimbal camera video in the UI (both cameras, Orin hardware decode → MJPEG)
- [x] XT2 thermal: display mode, spot meter, isotherm band + presets, area measurement
      (decoded from DJI Pilot via the bridge; display mode, area and band set from the portal on the aircraft)
- [x] Thermal auto range: full-frame setup and refit verified on the aircraft (2026-10-29)
- [ ] Isotherm enable/disable command (capture from Pilot)
- [ ] Smoother browser video (frame pacing)
- [x] Motor start/stop from the portal: over the RC link (`0x03/0x2a`) and the OSDK UART
      (activation + control authority), with the USB flight unlock (2026-10-28, props off)
- [x] Text command plans (arm / wait / disarm) run on the aircraft from the portal (2026-10-29)
- [x] Gimbal steps (pan, tilt, recenter, link) in the command model
- [x] Voice input: hold-to-talk in the portal, transcribed by a chosen online service (OpenAI or
      Deepgram) with key check and balance; on-device whisper.cpp was tried first (2026-10-29)
      and dropped for accuracy
- [x] Voice input checked live with Deepgram (2026-10-30): word-perfect in 0.25–0.5 s
- [x] Command understanding by an online model (Claude or OpenAI) with the grammar as fallback
- [ ] Flight steps (take off, land, hover, move) in the command model


## Acknowledgements

DUML has been studied extensively by the community (e.g. `dji-firmware-tools`,
`pyduml`, the Wireshark DUML dissector). The frame format and checksum
parameters used here were re-derived and verified against traffic captured from
this project's own hardware.
