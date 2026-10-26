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
binds the UDC (`3550000.usb`). `scripts/restore_gadget.sh` tears it down and
rebinds the stock NVIDIA `l4t` gadget.

Kernel support present: `CONFIG_USB_CONFIGFS=m`, `CONFIG_USB_CONFIGFS_F_FS=y`.
Not present: `usbmon`, `raw-gadget`, `dummy_hcd`, `CONFIG_FUNCTION_TRACER` —
so USB sniffing uses the gadget/host programs themselves, not `usbmon`.

### Orin as the controller (toward DJI Pilot)

The CrystalSky plugs into a Orin **USB-A** port. By default it enumerates as a
DJI device (`2ca3:4016`) in Android PC mode: mass storage + ADB + a CDC-ACM
serial (`/dev/ttyACM0`, idle). `aoa_capture` runs the AOA host handshake toward
it; the CrystalSky then re-enumerates as `18d1:2d01` and DJI Pilot streams DUML.

## Other interfaces on the Orin (not used)

- `/dev/ttyTHS1`, `/dev/ttyTHS2` — Tegra UARTs (idle). These would be the DJI
  Onboard SDK path (direct to the aircraft), which this project does not use.
- `can0` — CAN controller present, no traffic.
- `usb0` / `l4tbr0` — the Orin's own USB-gadget RNDIS networking; the controller
  does not use IP over this link.

## Access notes

- The user `dog` is not in the `dialout` group; serial/USB work currently uses
  `sudo`. Add to `dialout` (and a udev rule for the USB nodes) to drop `sudo`.
