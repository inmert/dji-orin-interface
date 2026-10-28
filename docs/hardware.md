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
serial (`/dev/ttyACM0`, idle). `aoa_capture` runs the AOA host handshake toward
it; the CrystalSky then re-enumerates as `18d1:2d01` and DJI Pilot streams DUML.

## Direct aircraft UART diagnosis (2026-10-27)

The aircraft OSDK UART is being investigated separately from the USB tablet
bridge. Physical header pin 8 is TX (`/dev/ttyTHS1`, UARTA at `0x03100000`),
pin 10 is RX, and pin 6 is ground. Aircraft RX connects to Orin TX and vice
versa. The configured baud rate is 230400, 8N1, without flow control.

Aircraft-to-Orin reception works: a three-second capture contained 182 valid
OSDK frames (SOF `AA`, header CRC16 and frame CRC32 verified), all broadcasts.
Firmware-version queries received no matching ACK, including writes split
into single bytes to exercise the serial driver's short-write PIO path.
Activation has therefore not been verified. OSDK 4.1 targets newer aircraft;
the M210 V1 is listed by OSDK 3.8.1. An earlier 3.8.1 version probe also failed.

With the aircraft TX/RX disconnected and the user reporting a jumper between
pins 8 and 10, local loopback failed at both 230400 and 115200 baud, with both
full-buffer and single-byte writes. This failure occurs independently of DJI
credentials, API-control settings, or the OSDK handshake.

The UART pad settings were TX `0x024300a8=0x400`, RX
`0x024300a0=0x458`, consistent with UART SFIO/output and SFIO/input respectively.
Temporarily clearing the TX GPIO output-float bit did not restore loopback.
A separate temporary GPIO test drove TX high/low while reading RX: the TX
output latch changed, but RX remained low on all five samples. All modified
registers were restored and read back successfully. Register readback does
not prove that the physical TX pad changed voltage.

The remaining fault is not isolated: jumper contact/pin placement, the header
electrical path, and low-level pad/controller behavior remain candidates.
Measure physical pin 8 against ground or test with an independently verified
3.3 V TTL adapter before making persistent pinmux/boot changes. Diagnostic
captures and the version-query script are in ignored
`captures/osdk/uart-diagnostics/`; private activation configuration is also
under ignored `captures/osdk/` and must not be committed.

## Other interfaces on the Orin

- `/dev/ttyTHS2` — another Tegra UART; not the tested header pins 8/10.
- `can0` — CAN controller present, no traffic.
- `usb0` / `l4tbr0` — the Orin's own USB-gadget RNDIS networking; the controller
  does not use IP over this link.

## Access notes

- The user `dog` is in `dialout`. Root access is still needed for GPIO/pinmux
  register diagnostics and USB gadget management.
