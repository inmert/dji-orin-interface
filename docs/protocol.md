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
| 0x0d| DM36x video, ground side |
| 0x0e| HD link, ground side |
| 0x0f| USB controller, air side |
| 0x10| USB controller, ground side |
| 0x11| Monocular vision |
| 0x12| Binocular vision (stereo obstacle sensing) |

0x0d–0x12 are named after dji-firmware-tools' dissector. Of these only 0x12 is
busy on this aircraft: `Binocular[0]` → `App[0]` on the RC link, all on the
vision command set `0x0a` (dissector names): `0a/06` avoidance parameters
(7 B), `0a/07` obstacle info (14 B), `0a/19` flat check (4 B), `0a/2e`
function list push (17 B), `0a/2f` sensor status push (13 B), `0a/42` (26 B,
not in the dissector), plus the general `00/f1` component self-test state
(4 B). About 35k frames in a 460 MB RC capture; payloads not decoded further.
Higher types seen in captures (e.g. `0x1f`) are not all identified.

The index (high 3 bits) tells the payloads apart on this aircraft: `Camera[0]`
is the Zenmuse XT2's visual camera and `Camera[1]` its thermal core (both on
`Gimbal[0]`), `Camera[2]` is the Zenmuse Z30 on `Gimbal[2]`, and `Gimbal[4]` is
the fixed FPV camera's mount. `Gimbal[7]` is the dual-gimbal sync address.

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

### Portal recenter on the aircraft (2026-10-28)

The portal's reset path (`{"cmd":"reset"}` over the WebSocket, the same
message its buttons send) was run end to end on the M210 with props off,
cameras independent. Each test was a 1 s yaw move at the portal's default
speed (200), an explicit release, then one reset; the server log shows
exactly one `yaw recenter sent` per target camera. Angles in degrees; the FPV
mount (`Gimbal[4]`, aircraft heading) read 147.1 throughout.

| Test | Off-center yaw | After reset | Settle | Other camera |
|------|----------------|-------------|--------|--------------|
| Recenter camera 1 | 167.1 | 147.1 | ~1.7 s | camera 2 unmoved |
| Recenter camera 2 | 162.3 | 147.4 | ~2 s | camera 1 unmoved |
| Recenter all (cam 1 left, cam 2 right) | 126.9 / 162.4 | 147.1 / 147.4 | ~2 s | both returned |

Pitch was unchanged in every test (camera 1 1.2, camera 2 within ±0.1), so
"center" is yaw aligned with the aircraft heading. Linked-mode recenter through
the portal was not repeated here; it was verified by direct injection above.

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
(An earlier note here took **`Camera[1]`** for the FPV camera, because it sends a
100-byte `0x02/0xf2` the others don't. It is in fact the XT2's thermal core; see
[XT2 thermal camera](#xt2-thermal-camera-camera1).) Its mount reports as
**`Gimbal[4]`**: a 12-byte `0x04/0x05` whose yaw equals the aircraft heading
(143.0° against the flight controller's 142.9°) and whose pitch stays 0, so it is
fixed to the airframe. It does not appear on `ttyACM0` unasked.

DJI's Onboard SDK 4.1 reads this camera's video, for aircraft other than the M300,
by opening a **UDT** connection (a reliable stream protocol over UDP) to
**`192.168.42.2:40003`**: the aircraft's end of the network its USB API port
brings up (the Orin is `192.168.42.3`). See
`osdk-core/advanced-sensing/camera_stream/src/dji_camera_stream_link.cpp` in the
SDK. No DUML or OpenProtocol command precedes it, and the received bytes go
straight into FFmpeg's H.264 parser, so this is plain H.264, like the video in the
RC downlink. A passive 15-second listen on that network saw no
traffic at all; the stream only flows once a client connects.

**Verified on the M210 V1 (2026-10-28).** A minimal UDT4 client (SDK's bundled
`udt` library, built standalone) connected to `192.168.42.2:40003` with no
other setup, no activation and the RC link up, and received 791 KB in 10 s
(~630 kbit/s). The bytes are an Annex-B H.264 elementary stream: Main profile,
**608×448**, ~29 frames/s (290 access-unit delimiters in 10 s). There are **no
IDR frames**: only SPS/PPS (7 each, about every 1.4 s), non-IDR slices and
filler NALs, so the encoder uses intra refresh and a decoder must start at an
SPS and tolerate a partial first picture. Decoded with GStreamer
(`h264parse ! avdec_h264`) into clear images of the scene in front of the
aircraft. The client sends only UDT handshake/ACK packets. Note that Ubuntu's
FFmpeg 8 on the Orin has only the NVIDIA H.264 decoders (`h264_cuvid`,
`h264_nvv4l2dec`), so the software decode path is GStreamer's `avdec_h264`
or `openh264dec`.

**Which streams the aircraft serves is not fixed (2026-10-28, later).** In one
session the RC mux carried all three video channels (`0x11`, `0x13`, `0x15`)
with nothing sent by us; with DJI Pilot bridged it carried `0x11` and `0x15`
only; and in a later session, after the aircraft had been restarted and used
with DJI GO 4, DJI Assistant 2 and a different app, it carried `0x11` only,
UDT port `40003` connected but delivered nothing, and UDT port **`40001`** (the
SDK's "main camera" port, untested until then) delivered the XT2's stream
(Baseline 1280×720, ~27 fps, with IDR frames; the same picture as channel
`0x11`). All cameras and gimbals kept reporting over DUML throughout. So the
aircraft encodes a selectable set of streams, and some DJI app or RC setting
chose a single-stream mode; which command selects it is not known yet (the
DM36x `0x08/0x06` status read `01 01 02 00` in both the three-stream and the
two-stream captures). `cargo run -p dji-udt --example udt_recv -- <ip:port>
<secs> <file>` checks a UDT port in isolation.

## RC USB mux and camera video

What the RC sends the app on its bulk stream (ep1 on our gadget) is not
encrypted: it is a two-layer mux of DUML and three plain H.264 streams
(found 2026-10-28 from a raw capture, `RC_DUMP=<file>` on the server).

**Outer frames.** The whole stream is a chain of frames with an 8-byte header
and no padding. In a 460 MB capture they cover every byte with no resync:

```text
  55 cc | type | 57 | len (u16 LE) | 00 00 | payload…
```

| Type | Payload |
|------|---------|
| `0x4a` | a slice of the mux stream below (2–8192 bytes); a mux packet may span several |
| `0x49` | DUML frames directly (~1 KB, a few per second) |

Most camera traffic travels in type `0x49`: all of the thermal core's `0xF2`
reports and the ACKs, none of which appear on mux channel `0x40`. The two
are separate byte streams, so each needs its own DUML decoder. Scanning the
raw bytes instead loses frames that an outer header splits (several thousand
in the capture: flight-controller, gimbal and camera reports) and accepts
noise from the video as DUML.

**Mux packets.** The concatenated type-`0x4a` payloads are packets of a
12-byte header, `len` payload bytes, then zero padding to a multiple of 8
bytes:

```text
  00 00 01 ff | len (u16 LE) | ff 00 4c | channel | check | 00 | payload…
```

`check` is the XOR of the ten header bytes before it. After some packets
the RC adds a bare 8-byte H.264 access-unit delimiter
(`00 00 00 00 00 01 09 10`) outside any packet; the demuxer skips it, and it
carries no picture. Channels:

| Channel | Content |
|---------|---------|
| `0x40` | DUML (what `duml::Decoder` scans) |
| `0x11` | gimbal 0, Zenmuse XT2: H.264 Baseline 1280×720 ~29 fps (visual or thermal picture, see below) |
| `0x15` | gimbal 2, Zenmuse Z30: H.264 High 1280×720 ~29 fps |
| `0x13` | FPV camera, H.264 Main 608×448 (same stream as UDT `:40003`); absent while DJI Pilot runs |

The channel ↔ camera mapping was confirmed by picture through the bridge with
DJI Pilot running. Pilot (v2.5.1.17 on the CrystalSky) decodes both with
Android MediaCodec on the tablet's hardware decoder. An earlier demuxer that
ignored the outer frames spliced their 8-byte headers into the ~5% of video
packets that span two outer frames (about 6,700 in 150 MB), producing NAL
units with the forbidden bit set (e.g. starting `00 00 01 a7`) that the parser
took for a bogus SPS, stopping the decoder. With both layers parsed there are
none; the server still drops such NAL units, as a guard against lost data.

The cameras use intra refresh with few or no IDR frames. On the Orin, NVIDIA's
`nvv4l2decoder` decodes them only with `is-gdr-stream=true`; without it every
frame is dropped ("Delta Unit Received, Dropping"). NVIDIA's elements print to
stdout even with `-q`, so the server runs
`sh -c 'exec gst-launch-1.0 -q … ! fdsink fd=3 3>&1 1>&2'` and carries the JPEGs
on fd 3, never on stdout. `crates/protocol/examples/rcmux_split.rs` splits a
raw capture into per-channel `.h264` files.

## XT2 thermal camera (`Camera[1]`)

Gimbal 0 is a Zenmuse XT2: `Camera[0]` is its visual camera and `Camera[1]` its
FLIR thermal core (both report camera type `0x1A` in `0x02/0x80`). Decoded
2026-10-28 from DJI Pilot traffic captured through the bridge
(`BRIDGE_DUMP_DIR`). Code: `crates/protocol/src/thermal.rs`.

**Report.** `Camera[1]` → `App` pushes `0x02/0xF2` (100 bytes, ~6 Hz).
Offsets follow dji-firmware-tools' "Camera Tau Param" layout:

| Offset | Field |
|--------|-------|
| 6 | palette (`0x18` white hot, `0x19` black hot) |
| 17 / 21 | f32 spot x / y, normalised 0..1 over the thermal image |
| 25 | f32 spot temperature, °C |
| 31 | bit 0: isotherm enabled |
| 32 | unit (1 = °C) |
| 33 / 35 / 37 | i16 isotherm lower / middle / upper, °C |
| 39 | measurement type: 0 off, 1 spot, 2 area |
| 41 | gain mode |
| 48 | flags; `0x80` = spot reading valid |
| 66..72 | u16 area rect left, top, right, bottom, 1/10000 of the image |
| 74 / 78 / 82 | f32 area avg / min / max, °C |
| 86..92 | u16 coldest x, y and hottest x, y, 1/10000 |
| 94 | display mode |

**Settings.** `App[0]` → `Camera[1]`, `0x02/0xF1`, command type `0x40`
(ACK requested), payload = parameter id + value. The camera ACKs `00`, or `ff`
when it rejects the value, and echoes the change in its next `0xF2`.

| Payload | Setting |
|---------|---------|
| `87 <f32 x> <f32 y>` | spot-meter position (0..1); ACKed `ff` unless measurement type is spot |
| `8B` / `8C` / `8D` `<i16>` | isotherm lower / middle / upper, °C; rejected if it breaks lower ≤ middle ≤ upper |
| `8E <u8>` | measurement type: 0 off, 1 spot, 2 area |
| `8F <u8>` | gain mode, 0/1/2 (Pilot's AUTO/HIGH/LOW; which number is which is unverified) |
| `95 <u8>` | scene profile 1/2/3 |
| `97 <u16 l,t,r,b>` | area rectangle, 1/10000 of the image |
| `98 00 <mode>` | display mode: 3 = thermal picture, 0 = visual, 2 = visual in the capture (maybe PiP) |

Example spot write from Pilot:
`55 16 04 fc 02 21 7f 50 40 02 f1 87 f3 cc fe 3e 64 a8 0d 3f 85 c0`.
The palette is a separate command, `0x02/0x42 <id>`. Pilot's "Search people"
preset sets lower 27 and middle 32 °C, and "Search fire" sets middle 149 and
upper 538 °C. Both are just isotherm writes, and Pilot sends only the values
that change. Pilot sends `8E 01` before a spot write, and waits after
`8E 02` before sending the area rectangle; the camera ignores a rectangle sent
while it is still switching. The isotherm enable/disable command has not been
captured yet.

In thermal display mode, channel `0x11` carries the 640×512 (5:4) thermal
image scaled to the full 720 px height and centred, with black bars: 900 px
wide from x = 190. The portal maps clicks through that.

**Portal.** The server orders isotherm writes so lower ≤ middle ≤ upper holds
at every step: it widens to −40…800 °C, sets the middle, then narrows. Its
**Auto** range keeps full-frame area measurement on and fits the band to
`[floor(min) − 1, ceil(max) + 1]` with 4 °C of slack, one setup step per
second. Live video carries no per-pixel temperatures; only R-JPEG photos on
the SD card do.

## Observations

- From the controller the Orin receives mostly ~2 KB video packets of the RC
  mux (see [RC USB mux](#rc-usb-mux-and-camera-video)) plus small DUML
  heartbeat/telemetry frames.
- `duml::parse` checks the header CRC-8 before trusting the 10-bit length, so a
  stray `0x55` in the video bytes is rejected in one byte rather than
  skipping up to a kilobyte of real frames. Filter to `hdr_crc_ok && crc_ok`
  for genuine DUML; for a live stream use `duml::Decoder`, which reassembles
  frames that straddle USB read boundaries.
- With **no aircraft present** behind the Orin, DJI Pilot only emits
  `cmd_set=0x00 cmd_id=0x01` "get version" discovery polls to each module. Real
  flight commands require Pilot to believe a full aircraft is connected — which
  is why `bridge` relays a real aircraft to Pilot.
