# Hardware notes

## Components

- **NVIDIA Jetson Orin Nano** (L4T, kernel `6.8.12-tegra`). USB-C port is
  device/OTG-capable (`tegra-xudc`, role switch `usb2-0-role-switch`); the
  full-size USB-A ports are hosts.
- **DJI Cendence** remote controller.
- **DJI Matrice 210 V1** aircraft.
- **DJI CrystalSky** monitor (`ZS600B`) running DJI Pilot — used to learn the
  command set.

## USB roles (important)

The Cendence controller is a **USB host** that drives the Android Open Accessory
handshake; the app device is the **peripheral**. Consequences:

- Connecting the controller to a Orin **USB-A (host)** port does nothing —
  host-to-host. The link must use the controller's **USB-C "tablet" port** into
  the Orin's **USB-C (device)** port, so the Orin enumerates as a peripheral.
- A USB-A-to-C cable forces the C end into device mode regardless, which can be
  misleading when probing roles.

### Orin as the app (toward the controller)

The Orin presents a **FunctionFS** gadget (`18d1:2d00`, one vendor interface +
2 bulk endpoints) on its USB-C port. `scripts/run_capture.sh` builds the gadget
via configfs, mounts functionfs at `/dev/ffs-dji`, starts `ffs_capture`, and
binds the UDC (`3550000.usb`). `scripts/run_bridge.sh` and
`scripts/run_server.sh` build the same gadget for `bridge` and `server`; all
three record the daemon in `/run/ffs_capture.pid`, so
`scripts/restore_gadget.sh` tears down any of them and rebinds the stock NVIDIA
`l4t` gadget.

Kernel support present: `CONFIG_USB_CONFIGFS=m`, `CONFIG_USB_CONFIGFS_F_FS=y`.
Not present: `usbmon`, `raw-gadget`, `dummy_hcd`, `CONFIG_FUNCTION_TRACER` —
so USB sniffing uses the gadget/host programs themselves, not `usbmon`.

### Orin as the controller (toward DJI Pilot)

The CrystalSky plugs into a Orin **USB-A** port. By default it enumerates as a
DJI device (`2ca3:4016`) in Android PC mode: mass storage + ADB + a CDC-ACM
serial (`/dev/ttyACM*`, idle). `aoa_capture` runs the AOA host handshake toward
it; the CrystalSky then re-enumerates as `18d1:2d01` and DJI Pilot streams DUML.

## Direct aircraft links (no RC)

The aircraft can also be wired straight to the Orin, bypassing the RC. Anything
sent on these links reaches the real flight controller, so the same props-off
rule applies to everything beyond read-only queries.

### USB API port — working

With the aircraft's USB mode switch in the **left (PC)** position and the
aircraft's USB port cabled to an Orin USB-A port, the aircraft enumerates as
`2ca3:001f` with RNDIS (the Orin gets `192.168.42.3`), mass storage, a vendor
bulk interface (no driver), and a CDC-ACM serial port, **`/dev/ttyACM0`**. (If
the CrystalSky is also plugged in, it takes the next ACM number.)

`ttyACM0` carries **DUML** (same framing and checksums as the RC link) in both
directions, with no OSDK activation or credentials. At idle the aircraft pushes
about four frames per second to `PC[0]`: gimbal attitude `0x04/0x05`
(2 Hz), flight controller `0x03/0x43` (1 Hz), `0x11/0x01`, and `Gimbal[7]`
`0x00/0x0e`. A read-only get-version (`0x00/0x01`, command type `0x40`) sent
from **`PC[0]` (`0x0A`)** to the flight controller is acknowledged.

### OSDK UART — works through a USB-TTL adapter

The Onboard SDK serial port is on the aircraft's expansion header, column 1
from top to bottom: `SDK_Tx`, `SDK_Rx`, `GND` (3.3 V TTL). 230400 baud, 8N1, no
flow control. The wire format is OSDK OpenProtocol (SOF `0xAA`, CRC16 `0xA001`
and CRC32 `0xEDB88320`, both seeded `0x3AA3`), not DUML.

**Through a CP2102 USB-TTL adapter (`10c4:ea60`, `/dev/ttyUSB0`, 3.3 V logic) it
works both ways (2026-10-28).** The flight controller acknowledged 29 of 30
read-only firmware-version queries, replying with hardware ID `095XE9X00200W8`
(the same ID it reports over USB DUML) and firmware
`SDK-v1.1 BETA PM410-03.03.10.18`. Its flight-data broadcast decodes cleanly
there too (256 frames in 5 s, none rejected), mixing flag words `0x00FF`,
`0x2FFF`, `0x3000` and the zero-flag 267-byte layout. The web server reads it
with `OSDK_UART=/dev/ttyUSB0`.

**OSDK activation works (2026-10-28).** OSDK 4.1 rejects this aircraft ("Only
support M210 V2 series and M300"); OSDK **3.9.0** identifies it (`PM410`,
firmware 3.3.10.18) and activates. Activation answered `NEW_DEVICE_ERROR` until
the aircraft's USB port was connected to a computer running **DJI Assistant 2
for Matrice, logged in and online**. With DJI Pilot or GO 4 online on the RC
alone it never succeeded. After that the SDK's telemetry subscription worked
(five packages at 1–200 Hz). DJI's OSDK 4.1 Linux sample also enables "take-off
permission while USB is plugged in" after activating; it was kept from doing so
by pointing its `acm_port` at a nonexistent device. OSDK 3.9.0's sample does not
send it.

**Through the Orin's own UART it only receives.** Wired directly to the 40-pin
header (pin 8 TX = `/dev/ttyTHS1`, pin 10 RX, pin 6 ground), the broadcast
arrives but the flight controller never acknowledges anything, including the
version query. DJI's OSDK 4.1 telemetry sample failed the same way ("Drone
version not obtained"). Since the aircraft answers the same query through the
CP2102, the fault is on the Orin side of that path: the Orin's UART transmit,
its pin configuration, or that wiring. Pin 8 was seen toggling while sending,
and an earlier pin 8/10 jumper loopback test failed; the exact cause is not
isolated. Use a 3.3 V USB-TTL adapter for the SDK port in the meantime.

Do **not** use a 5 V adapter such as the FTDI `TTL-232R-5V`; its TXD would drive
5 V into the aircraft's 3.3 V input. Leave an adapter's VCC pin unconnected.

Diagnostic captures and the version-query script are in ignored
`captures/osdk/uart-diagnostics/`. Private activation configuration lives
outside the repo and must not be committed.

## Other interfaces on the Orin

- `/dev/ttyTHS2` — another Tegra UART; not the tested header pins 8/10.
- `can0` — CAN controller present, no traffic.
- `usb0` / `l4tbr0` — the Orin's own USB-gadget RNDIS networking; the controller
  does not use IP over this link.

## Access notes

- The user `dog` is in `dialout`. Root access is still needed for GPIO/pinmux
  register diagnostics and USB gadget management.
