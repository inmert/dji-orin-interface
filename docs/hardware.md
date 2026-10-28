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

### OSDK UART — receive only

The Onboard SDK serial port is on the aircraft's expansion header, column 1
from top to bottom: `SDK_Tx`, `SDK_Rx`, `GND` (3.3 V TTL). On the Orin it is the
40-pin header: pin 8 TX (`/dev/ttyTHS1`, UARTA at `0x03100000`), pin 10 RX,
pin 6 ground; aircraft TX goes to Orin RX and vice versa. 230400 baud, 8N1, no
flow control. The wire format is OSDK OpenProtocol (SOF `0xAA`, CRC16 `0xA001`
and CRC32 `0xEDB88320`, both seeded `0x3AA3`), not DUML.

Aircraft-to-Orin works: a three-second capture contained 182 valid OSDK frames,
all unsolicited broadcasts. The flight controller never acknowledges anything
sent to it, including the firmware-version query, so activation never starts.
DJI's own OSDK 4.1 telemetry sample fails the same way ("Drone version not
obtained"). Ruled out: API Control is on, the RC is on, the aircraft is out of
PC mode, baud and config are correct, and nothing else holds the port.

An earlier pin 8/10 jumper loopback test failed, but a later measurement showed
pin 8 dropping to about 0.5 V while sending and the signal reaching the
aircraft's `SDK_Rx` pin, so the Orin's TX is good. The fault is on the aircraft
side: either a damaged SDK-port input on the flight controller, or M210 V1
firmware that OSDK 4.1 does not support (4.1 lists only the M210 V2; the V1 is
listed by OSDK 3.8.1, and a 3.8.1 version probe also failed).

To rule the Orin out completely, test with a **3.3 V** USB-TTL adapter
(enumerates as `/dev/ttyUSB0`): adapter TXD to `SDK_Rx`, RXD to `SDK_Tx`, GND to
GND. Do **not** use a 5 V adapter such as the FTDI `TTL-232R-5V`; its TXD would
drive 5 V into the aircraft's 3.3 V input.

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
