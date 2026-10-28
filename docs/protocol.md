# Protocol notes

## USB transport: Android Open Accessory (AOA)

The DJI app connects to the Cendence controller over USB using Android Open
Accessory. The **controller is the USB host**; the app device (phone/tablet, or
our Orin) is the USB **peripheral**.

Handshake, as captured from the real controller:

1. Host → device, vendor control IN `bRequest=51` (`GET_PROTOCOL`), `wLength=2`.
   Device replies a 2-byte little-endian protocol version (we reply `2`).
2. Host → device, vendor control OUT `bRequest=52` (`SEND_STRING`) for string
   indices 0..5. Captured values:

   | idx | field        | value               |
   |-----|--------------|---------------------|
   | 0   | manufacturer | `DJI`               |
   | 1   | model        | `T600`              |
   | 2   | description  | `DJI Inspire 1`     |
   | 3   | version      | `v1.0.2.6`          |
   | 4   | uri          | `www.dji.com`       |
   | 5   | serial       | `000000000000000`   |

   `manufacturer=DJI, model=T600` is the identity DJI Pilot registers to handle.
3. Host → device, vendor control OUT `bRequest=53` (`ACCESSORY_START`).
4. The device (re-)enumerates in accessory mode: **VID `0x18d1`**, PID `0x2d00`
   (accessory) or `0x2d01` (accessory + ADB), one vendor-specific interface with
   two bulk endpoints.
5. **DUML frames** flow over the two bulk endpoints.

> **Fresh handshake vs. control.** DJI Pilot only arms its control pipeline (it
> actually sends gimbal/flight commands) after a *full* AOA handshake. Teardown
> leaves the tablet in accessory mode, so a bridge re-run that reuses the
> existing accessory (skipping the handshake) leaves Pilot streaming telemetry
> but **inert** — no control. For a bridge capture where Pilot must be
> controllable, **replug the tablet** first (→ default `2ca3:4016`) so the next
> run does the full handshake. The standalone `server` doesn't care: it *is* the
> app, so the skip-handshake path is fine there.

## DUML frame format

Little-endian multi-byte fields. Total frame length is a 10-bit field.

```
off  size  field
0    1     SOF, always 0x55
1    1     length, low 8 bits
2    1     bits[1:0] = length high bits; bits[7:2] = protocol version
3    1     CRC-8 over bytes [0..2]            (header check)
4    1     sender    (low 5 bits = device type, high 3 bits = index)
5    1     receiver  (same encoding)
6    2     sequence number
8    1     command type / flags (ACK bit, encryption, ...)
9    1     command set
10   1     command id
11   N     payload  (N = length - 13)
..   2     CRC-16 over bytes [0 .. length-3]  (whole-frame check)
```

### Checksums (re-derived and verified against captured frames)

- **Header CRC-8:** reflected, polynomial `0x31` (reflected `0x8C`), init `0x77`.
- **Frame CRC-16:** reflected CRC-16/CCITT, polynomial `0x1021` (reflected
  `0x8408`), init `0x3692`.

Both are implemented in [`crates/protocol/src/duml.rs`](../crates/protocol/src/duml.rs)
and checked by its unit tests (`cargo test -p dji-protocol`) against real
captured frames.

### Worked example

A frame captured from DJI Pilot:

```
55 0d 04 33 02 2d 07 00 40 00 ff 34 7a
│  │  │  │  │  │  └─┬─┘ │  │  │  └─┬─┘
│  │  │  │  │  │   seq  │  │  │  CRC-16 (0x7a34)
│  │  │  │  │  └ receiver 0x2d    │
│  │  │  │  └ sender 0x02 (App)   └ cmd_id 0xff (broadcast)
│  │  │  └ header CRC-8          cmd_set 0x00 (common)
│  │  └ version 1               cmd_type 0x40
│  └ length 13
└ SOF
```

## Device types (low 5 bits of sender/receiver)

| id  | device         |
|-----|----------------|
| 0x00| Any            |
| 0x01| Camera         |
| 0x02| App / DJI Pilot|
| 0x03| Flight control |
| 0x04| Gimbal         |
| 0x05| Center board   |
| 0x06| Remote radio   |
| 0x07| WiFi           |
| 0x08| DM36x video    |
| 0x09| HD link        |
| 0x0a| PC (USB API port / ttyACM0) |
| 0x0b| Battery        |
| 0x0c| ESC            |

Higher types seen in captures (e.g. `0x0e`, `0x1f`) are not all identified.

## Command injection: gimbal control

Decoded from captured DJI Pilot traffic and implemented in the `gimbal` module
([`src/gimbal.rs`](../crates/protocol/src/gimbal.rs), frames in
[`src/gimbal/command.rs`](../crates/protocol/src/gimbal/command.rs)).

- **Move command:** `cmd_set = 0x04` (Gimbal), `cmd_id = 0x0c`, sender App
  (`0x02`), receiver Gimbal, `cmd_type = 0x00`. 7-byte payload, little-endian
  (field order **confirmed on hardware** — pitch/yaw are transposed from the
  first guess):

  | bytes | field | notes |
  |-------|-------|-------|
  | `[0:2]` | yaw   (int16) | pan; signed |
  | `[2:4]` | roll  (int16) | always 0 — the app does not drive roll |
  | `[4:6]` | pitch (int16) | tilt; signed |
  | `[6]`   | flag  (u8)    | `0x80` active, `0x00` release (all-zero = stop) |

  Sent at ~10 Hz while moving. Verified live: `cmd_type = 0x00` is accepted and
  the gimbal moves; up/down tilt and left/right pan with the signs as sent.
- **The two cameras are the gimbal device index** (high 3 bits of the receiver
  byte): camera 1 = index 0 (receiver `0x04`), camera 2 = index 2 (`0x44`). A
  single DJI Pilot instance only drives one gimbal at a time; our server
  addresses each independently.
- `gimbal::command::cmd_type()` defaults to `0x00` (verified) and can be overridden at
  runtime with `GIMBAL_CMD_TYPE`. Whether the values are a driven setpoint or a
  speed is not yet characterized, but the UI's hold-to-move at a fixed magnitude
  works well in practice.
- The attitude report is `cmd_set=0x04 cmd_id=0x05`: the first three `i16`
  are pitch, roll and yaw in 0.1°. Yaw was verified against the OSDK UART's
  gimbal yaw (see *Flight telemetry on the direct USB link* below); pitch and
  roll have only been compared at 0. Earlier sections quote these raw values.
- **Yaw recenter is verified on the M210.** Use `04/4c FE 01` with command
  type `40`; it preserves pitch and work mode. Earlier notes suggested
  `0x04/0x10`, but the available logs do not establish that interpretation.
  The [community dissector](https://github.com/o-gs/dji-firmware-tools/blob/master/comm_dissector/wireshark/dji-dumlv1-gimbal.lua)
  names `0x10` "User Params Get", `0x13` "User Params Reset Default", and
  `0x4c` "Reset And Set Mode". Do not substitute a settings reset for recenter.
  The SDK decode and live tests below establish the encoding and behavior.
  The server and portal expose it as per-camera **Recenter** and **Recenter
  all** buttons (yaw only, `FE 01`), following the semantics verified below.

### Recenter capture checklist (if an app reset control is available)

With props off and the aircraft secured, capture Pilot's recenter action with
`BRIDGE_HEX_SETS=04,19` and a new log filename. The bridge logs the command-type
byte as `type=0xNN` as well as receiver, sequence, set/id and payload.

Record each action and its timestamp separately:

1. Cameras unlinked: move camera 1 slightly off-center, then recenter it.
2. Cameras unlinked: repeat for camera 2.
3. Cameras linked: recenter the master; record whether the follower recenters
   and which receivers Pilot addresses.

Retain the exact outgoing bytes and resulting attitude reports. These become
protocol test fixtures and determine the per-camera and reset-all behavior.
Replug the CrystalSky before the run so Pilot gets a fresh AOA handshake;
start the bridge with the RC disconnected, then connect the already-booted RC.
Use `scripts/restore_gadget.sh` for teardown.

### RC-button observation (2026-10-27)

A passive capture of the physical RC reset button is saved locally as
`captures/rc-reset/capture.log` (captures are git-ignored). It contains 11,971
CRC-valid frames over approximately 50.5 seconds. No command injector or Pilot
bridge was running.

- Camera 1 (`Gimbal[0]`) moved and returned to approximately yaw -47. Near
  40.155 s, byte 12 of the incoming `0x06/0x05` RC report changed from `00`
  to `10`, then cleared at 40.359 s. Reported yaw fell from 477 at 40.020 s
  to -46 at 42.042 s, while pitch stayed near -893 (provisional raw units).
- Camera 2 (`Gimbal[2]`) stayed near pitch 96 / yaw -216 throughout.
- The only gimbal command-set routes observed were incoming `0x04/0x05`
  attitude and `0x04/0x1c` type reports. No `0x04/0x10`, `0x04/0x4c`, or
  `0x19` frame appeared.

This is evidence of camera 1 yaw recentering, not a captured app-to-gimbal
reset command. The RC button report is input telemetry to the app and must not
be replayed toward the aircraft as a reset command. A Pilot bridge capture is
one way to establish the outgoing command and linked-camera behavior. The
SDK-derived alternative below is needed because this Pilot UI has no reset.
For passive payload logging, set `CAPTURE_HEX=1` when running `ffs_capture`.

### RC reset with Pilot bridged (2026-10-27)

The fresh-handshake retry is saved locally at
`captures/pilot-reset/bridge-retry.log`. Both relay directions were active.
Camera 1 pitch moved from about 300 to -327; its yaw subsequently returned to
-47 by approximately 29 s and stayed there through the end of the capture.
Pitch remained -327 during the yaw return. Camera 2 did not recenter.

No outgoing `0x04` command was captured anywhere in this retry. The only
outgoing `0x19` commands were the existing unlock pair (`07: 00 00`, `05: 00`)
at approximately 5.75 s and 15.89 s, before the recenter. Consequently, this
capture does not establish an app-originated reset command, even with Pilot
present. It supports yaw-only recenter as the observed RC-button behavior;
at this stage, full pitch/yaw reset and linked-camera reset were unverified.
Later live injection results are recorded below.

### SDK reset decode (2026-10-27)

Read-only ADB inspection of the installed Pilot APK (`com.dji.industry.pilot`,
v2.5.1.17) found `DataGimbalNewResetAndSetMode` and `ReturnCenterCmd` names.
Its packed DEX did not expose those method bodies through ordinary DEX parsing;
the APK alone did **not** establish the payload. The older installed GO APK is
also packed. No tablet software was modified.

DJI's official [Onboard SDK](https://github.com/dji-sdk/Onboard-SDK/tree/118e2825a347499efb8ed253146552c5b9b10779)
provides an independent encoding reference:

- `osdk-core/modules/inc/payload/dji_gimbal_module.hpp` defines a packed two-byte
  work-mode/action structure: `FE` preserves the mode, `01` means drone yaw
  recenter, and `03` means pitch-and-yaw recenter.
- `osdk-core/modules/src/payload/dji_gimbal_module.cpp` uses `FE 03` for reset,
  a completion ACK, and gimbal device indices 0 and 2 for the first two payloads.
- In the shipped ARM64 `libdji-linker.a`, `dji_internal_command.cpp.o` contains
  the two-byte `V1ProtocolCMD::Gimbal::resetAngle` symbol: `04 4c`.
- `OsdkProtocol_v1Pack` puts `needAck=2` into bits 5–6 of byte 8, giving command
  type `40` for an unencrypted request. This differs from our movement packets.

The native SDK packer was called **offline**, with sender App (`02`), sequence
`1234`, receiver `04` or `44`, command `04/4c`, and the two-byte payload. Its
output is retained as independent fixtures for `gimbal::command::recenter_frame`:

| Target / action | Complete frame (hex; sequence 0x1234) |
| --- | --- |
| Camera 1 yaw | `55 0f 04 a2 02 04 34 12 40 04 4c fe 01 4d 9d` |
| Camera 1 pitch + yaw | `55 0f 04 a2 02 04 34 12 40 04 4c fe 03 5f be` |
| Camera 2 yaw | `55 0f 04 a2 02 44 34 12 40 04 4c fe 01 bc f8` |
| Camera 2 pitch + yaw | `55 0f 04 a2 02 44 34 12 40 04 4c fe 03 ae db` |

SDK encoding alone did not establish M210 V1 acceptance over the RC USB
route; the live tests below subsequently verified yaw reset. The builder is
not connected to the server or UI. Testing proceeded with cameras unlinked,
movement released before each reset, and both cameras' attitudes recorded.
Independent camera-2 and linked-master tests followed camera 1's test. Full
pitch-and-yaw reset remains outside the verified feature.

### First live yaw-reset probe (2026-10-27)

With the user's explicit approval, ran passive capture, requested that the user
pan camera 1 off-center, then sent camera-1 release, the known unlink pair, and
**one** `04/4c FE 01` request. No retry or injected move was sent. The offline
example `reset_probe_frames` prints the exact packets without opening USB.
Local evidence: `captures/reset-probe/camera1-yaw.log` and `sent.jsonl`.

The reset was sent near capture time 51.48 s, sequence `7103`:
`55 0f 04 a2 02 04 03 71 40 04 4c fe 01 d3 07`.
Camera 1 had been stable at pitch 52 / yaw -558 before injection; yaw moved
to -917 by second 52 and settled at -918, while pitch remained 52. Camera 2
remained near pitch 294 / yaw -342 during the response. Values are provisional
raw attitude units. No incoming `04/4c` acknowledgement was observed.

The user visually confirmed that camera 1 recentered correctly. Both cameras'
yaw readings had shifted earlier in the run, before any injection, so the earlier
-47 center is not a valid fixed reference. Camera-1 yaw reset is now hardware
validated. Camera-2 and linked reset were verified in the follow-up below.
Pitch-and-yaw reset remains untested.
Capture was stopped and the stock USB gadget restored after the observation.

### Camera-2 and linked yaw-reset verification (2026-10-27)

User authorized both tests with props still off and authorized direct movement
when RC camera selection was unavailable. Evidence is saved locally in
`captures/reset-probe-followup/capture.log` and `sent.jsonl`. The offline
`reset_probe_frames` example generates the probe packets; no automatic retries
were used. Each short pan was followed by an explicit release.

- **Independent camera 2:** after a brief yaw-80 pan, camera 2 was stable near
  pitch 297 / yaw -338; camera 1 stayed at pitch 60 / yaw -723. At 79.64 s,
  sent `04/4c FE 01` to receiver `44`, sequence `7203`. Camera 2 returned an
  ACK at 79.96 s, matching sequence 29187 (`7203`), type `80`, payload `00`.
  Its yaw crossed the signed-angle wrap during the return and settled near
  -900; pitch stayed about 296. Camera 1 did not move.
- **Linked:** engaged the existing native lock at 102.78 s. Camera 2 matched
  the master's pitch/yaw, confirming that the link was active. A brief master
  pan left camera 1 at pitch 60 / yaw -707 and camera 2 near 60 / -712.
  At 121.91 s sent one `04/4c FE 01` to **camera 1 only**, sequence `7303`.
  Both yaw readings returned: camera 1 settled at -899, camera 2 near -891;
  both pitches remained near 60. No camera-1 reset ACK was captured.

Yaw reset is now verified for each camera independently and for the linked
pair via the master. Portal semantics: per-camera reset when independent;
while linked, either reset control and reset-all target the master only.
Independent reset-all targets each camera. These actions preserve pitch;
`FE 03` (pitch-and-yaw reset) remains untested. A missing ACK from camera 1
must not trigger blind retries or be presented as proof that it did not move.
After the tests, released both cameras, restored independent mode, stopped
capture, and restored the stock USB gadget.

### Camera lock (dual-gimbal follow)

Pilot's "lock cameras together" button, captured from its traffic. Addressed to
the gimbal **sync index 7** (receiver byte `0xe4`), `cmd_set = 0x19`:

| action | command | payload |
|--------|---------|---------|
| lock on  | `cmd_id = 0x07` | `01 00` |
| lock off | `cmd_id = 0x07` | `00 00`, then `cmd_id = 0x05` payload `00` |

Byte 0 of the `0x07` payload is the enable flag. **While locked, Pilot drives
only `Gimbal[0]` (the master)** and the aircraft moves camera 2 to follow — so to
replicate it, engage the lock then drive the master only. (`cmd_set=0x19
cmd_id=0x06 len=5` is just a periodic status push.) Built in
[`src/gimbal/command.rs`](../crates/protocol/src/gimbal/command.rs) (`lock_frame`) and used by the web
server's LINK toggle.

## OSDK flight-data broadcast (UART)

The aircraft's OSDK UART speaks DJI's Onboard SDK **OpenProtocol**, not DUML:
SOF `0xAA`, a 12-byte header (10-bit length, session/ack bits, encryption bits,
sequence) with a CRC-16 (poly `0xA001`), then command set/id, payload and a
CRC-32 (poly `0xEDB88320`); both CRCs are seeded `0x3AA3`. The flight
controller pushes `cmd_set 0x02 / cmd_id 0x00` unasked, with no activation.
Its payload is a `u16` flag word and then each flagged section in a fixed
order (packed little-endian, from the SDK's `DataBroadcast::unpackData`):

| flag | section | bytes |
|------|---------|-------|
| `0x0001` | FC time (ms, ns) + sync stamp | 15 |
| `0x0002` | attitude quaternion (w, x, y, z) | 16 |
| `0x0004` | acceleration | 12 |
| `0x0008` | velocity (m/s) + GPS-health bit | 13 |
| `0x0010` | angular rate | 12 |
| `0x0020` | position (lat/lon rad, baro altitude, height, health) + obstacle distances | 50 |
| `0x0040` | GPS (date, time, lat/lon deg·1e7, MSL height mm, NED velocity cm/s, DOP, satellites) | 68 |
| `0x0080` | RTK | 44 |
| `0x0100` | magnetometer | 6 |
| `0x0200` | RC sticks (±10000) | 12 |
| `0x0400` | gimbal roll/pitch/yaw (deg) | 13 |
| `0x0800` | status: flight state, flight mode, landing gear, error | 4 |
| `0x1000` | battery (mAh, mV, mA, %) | 13 |
| `0x2000` | SDK control info | 2 |
| `0x4000` | compass status | 1 |

Observed on our M210 V1: one session sent a time-only frame (~50 Hz) plus a
267-byte frame whose flag word read `0` but whose body was exactly sections
`0x0001`–`0x0800`. Another session used real flag words, `0x00FF` and `0x2FFF`.
The decoder ([`osdk/broadcast.rs`](../crates/protocol/src/osdk/broadcast.rs))
trusts a non-zero flag word only if the section sizes add up to the body length,
resolves a zero word by matching the length against the section order, and
rejects anything else. Values were checked live: the quaternion heading matches
the gimbal yaw, the magnetometer sits in the normal range, and battery voltage
and percentage are plausible. GPS time arrives as UTC.

## Flight telemetry on the direct USB link (`ttyACM0`)

Mapped from a passive 20-second capture of `ttyACM0` and the OSDK UART at the
same time (both opened read-only, aircraft on the ground indoors). The UART's
decoded values served as ground truth. Without being asked, the aircraft
sends only three streams to `PC[0]` on this link:

| route | rate | content |
|-------|------|---------|
| `FlightCtrl[0] 0x03/0x43`, 55 bytes | 1 Hz | flight controller general data (below) |
| `Gimbal[0] 0x04/0x05`, 24 bytes | 2 Hz | gimbal attitude: first three `i16` = pitch, roll, yaw in **0.1°** |
| `FlightCtrl[0] 0x11/0x01`, 5 bytes | 1 Hz | bytes 1–2 count up by 17 per second; purpose unknown |

`0x03/0x43` payload, little-endian:

| offset | type | field | checked against the UART |
|--------|------|-------|--------------------------|
| 0, 8 | `f64` | longitude, latitude | zero without a GPS fix; not yet verified |
| 16 | `i16` | relative height, 0.1 m | zero on the ground; not yet verified |
| 18, 20, 22 | `i16` | velocity x, y, z, 0.1 m/s | zero at rest; not yet verified |
| 24 | `i16` | pitch, 0.1° | matches (1.3° vs 1.3°) |
| 26 | `i16` | roll, 0.1° | matches (2.3° vs 2.3–2.4°) |
| 28 | `i16` | yaw, 0.1° | matches (142.9–143.0° vs 143.0°) |
| 30 | `u8` | flight mode (6 = P-GPS, same numbering as OSDK) | matches |
| 36 | `u8` | GPS satellites | matches (0 indoors) |

The gimbal report's yaw matched the UART's gimbal yaw exactly (1425 = 142.5°),
which settles the units of the attitude decode in
[`gimbal/attitude.rs`](../crates/protocol/src/gimbal/attitude.rs); its pitch and
roll read 0 on both links, so they are consistent but not strongly tested.
Battery, RC sticks and obstacle distances are not sent unasked here; reading
them over USB would mean sending a query to the aircraft.

The web server decodes these frames read-only
([`flyc/osd.rs`](../crates/protocol/src/flyc/osd.rs)) and shows them in its
*Direct USB link* panel. In a live check, the panel's attitude matched the UART
panel's attitude at the same moment (143.1°, 2.3°, 1.3°).

## FPV camera (third camera)

The M210 has a fixed centre-front FPV camera besides the two gimbal cameras.
On the RC link it is **`Camera[1]`** (the gimbal cameras are `Camera[0]` and
`Camera[2]`). It sends the same 37-byte `0x02/0x80` status push as the other two,
plus a 100-byte `0x02/0xf2` that only it sends; it does not send the `0x02/0x81`
and `0x02/0x87` reports the gimbal cameras do. Its mount reports as
**`Gimbal[4]`**: a 12-byte `0x04/0x05` whose yaw equals the aircraft heading
(143.0° against the flight controller's 142.9°) and whose pitch stays 0, so it is
fixed to the airframe. It does not appear on `ttyACM0` unasked.

DJI's Onboard SDK 4.1 reads this camera's video, for aircraft other than the M300,
by opening a **UDT** connection (a reliable stream protocol over UDP) to
**`192.168.42.2:40003`**: the aircraft's end of the network its USB API port
brings up (the Orin is `192.168.42.3`). See
`osdk-core/advanced-sensing/camera_stream/src/dji_camera_stream_link.cpp` in the
SDK. No DUML or OpenProtocol command precedes it, and the received bytes go
straight into FFmpeg's H.264 parser, so this is plain H.264, unlike the
encrypted RC downlink. Whether the M210 V1's firmware serves this port has not
been tested. A passive 15-second listen on that network saw no traffic at all.

## Observations

- From the controller the Orin receives mostly ~716-byte high-entropy frames
  (the **camera video downlink**) plus small heartbeat/telemetry frames; many
  payloads are encrypted.
- `duml::parse` checks the header CRC-8 before trusting the 10-bit length, so a
  stray `0x55` in the encrypted/video bytes is rejected in one byte rather than
  skipping up to a kilobyte of real frames. Filter to `hdr_crc_ok && crc_ok`
  for genuine DUML; for a live stream use `duml::Decoder`, which reassembles
  frames that straddle USB read boundaries.
- With **no aircraft present** behind the Orin, DJI Pilot only emits
  `cmd_set=0x00 cmd_id=0x01` "get version" discovery polls to each module. Real
  flight commands require Pilot to believe a full aircraft is connected — which
  is why `bridge` relays a real aircraft to Pilot.
