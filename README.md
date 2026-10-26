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
Cargo.toml              Package manifest (no external dependencies)
src/lib.rs              Crate root (module declarations)
src/duml.rs             DUML framing, checksums, Decoder, build + unit tests
src/aoa.rs              Android Open Accessory constants + DJI identity
src/usbfs.rs            Host-side USB: device lookup, control/bulk transfers
src/ffs.rs              Device-side FunctionFS: descriptors, ep0 events, setup
src/sys.rs              Raw kernel ABI (libc calls + usbfs ioctl structs/codes)
src/log.rs              Timestamped line logging shared by the binaries
src/bin/ffs_capture.rs  Orin as the app toward the controller
src/bin/aoa_capture.rs  Orin as the controller toward DJI Pilot
scripts/run_capture.sh  Bring up the FunctionFS gadget + start ffs_capture
scripts/restore_gadget.sh  Tear it down, restore the stock USB gadget
docs/                   Protocol and hardware notes
```

The project is **Rust** (edition 2024) with **no external crates** — the USB and
gadget plumbing uses the system libc through a small set of hand-declared
externs in `sys`, so it builds fully offline. The reusable protocol logic lives
in the `duml` module (`parse`/`scan`/`build`, the streaming `Decoder`, `Frame`,
`ParseError`) so the gadget side, the host side, and any future command sender
share one validated implementation of framing and checksums. Safe, typed USB
wrappers (`usbfs`, `ffs`) keep raw pointers out of the binaries. File
descriptors are owned by `std::os::fd::OwnedFd` (closed on drop); the gadget's
bulk reader runs on a `std::thread`.

## Building

**Toolchain:** Rust via `rustup`, edition 2024 (needs **rustc 1.85+**;
developed with 1.99 on aarch64). No external crates and no build-time kernel
headers — the usbfs/gadget ABI is declared in `sys`. If `cargo` isn't on your
PATH: `export PATH="$HOME/.cargo/bin:$PATH"`.

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

## Status

- [x] USB transport identified (AOA; controller is the USB host)
- [x] AOA handshake implemented both directions (device and host)
- [x] DUML framing + checksums implemented and validated against captures
- [x] Receive & decode DUML from the controller and from DJI Pilot
- [ ] Transparent MITM bridge (tablet ↔ Orin ↔ controller ↔ aircraft)
- [ ] Send/inject flight commands to the aircraft

## Acknowledgements

DUML has been studied extensively by the community (e.g. `dji-firmware-tools`,
`pyduml`, the Wireshark DUML dissector). The frame format and checksum
parameters used here were re-derived and verified against traffic captured from
this project's own hardware.
