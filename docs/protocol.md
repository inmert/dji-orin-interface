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

Both are implemented in [`src/duml.rs`](../src/duml.rs) and checked by its unit
tests (`cargo test`) against real captured frames.

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
| 0x0a| Battery        |
| 0x0b| ESC            |

Higher types seen in captures (e.g. `0x0e`, `0x1f`) are not all identified.

## Command injection: gimbal control

Decoded from captured DJI Pilot traffic and implemented in the `gimbal` module
([`src/gimbal.rs`](../src/gimbal.rs), frames in
[`src/gimbal/command.rs`](../src/gimbal/command.rs)).

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
- Also seen: `cmd_set=0x04 cmd_id=0x10` (1-byte mode/recenter enum, sent to both
  gimbals) and the attitude report `cmd_set=0x04 cmd_id=0x05` the gimbals push up.

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
[`src/gimbal/command.rs`](../src/gimbal/command.rs) (`lock_frame`) and used by the web
server's LINK toggle.

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
  flight commands require Pilot to believe a full aircraft is connected — hence
  the planned transparent bridge to the real drone.
