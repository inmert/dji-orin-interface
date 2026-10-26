#!/bin/bash
#
# Swap the Orin's USB device-mode gadget for the FunctionFS capture gadget and
# start ffs_capture, so the Orin impersonates the DJI app to the Cendence
# controller. Run as root. Restore the stock gadget with restore_gadget.sh.
#
# Usage: sudo scripts/run_capture.sh [logfile]
set -euo pipefail

G=/sys/kernel/config/usb_gadget
FFS=/dev/ffs-dji
REPO="$(cd "$(dirname "$0")/.." && pwd)"
DAEMON="$REPO/target/release/ffs_capture"
LOG="${1:-$REPO/capture.log}"
UDC_NAME="$(ls /sys/class/udc | head -1)"

[ -x "$DAEMON" ] || { echo "build first: cargo build --release   (missing $DAEMON)"; exit 1; }
modprobe usb_f_fs 2>/dev/null || true

# 1. Unbind any currently-bound gadget (e.g. the stock l4t one).
for g in "$G"/*/; do
    [ -e "$g/UDC" ] || continue
    cur="$(cat "$g/UDC" 2>/dev/null || true)"
    [ -n "$cur" ] && { echo "" > "$g/UDC"; echo "unbound $(basename "$g") (was $cur)"; }
done

# 2. Create the accessory gadget (identity matches the real controller).
mkdir -p "$G/dji"
echo 0x18d1 > "$G/dji/idVendor"      # Google — Android Open Accessory mode
echo 0x2d00 > "$G/dji/idProduct"     # accessory (no ADB)
echo 0x0100 > "$G/dji/bcdDevice"
echo 0x0200 > "$G/dji/bcdUSB"
mkdir -p "$G/dji/strings/0x409"
echo "0123456789"  > "$G/dji/strings/0x409/serialnumber"
echo "DJI"         > "$G/dji/strings/0x409/manufacturer"
echo "DJI Pilot"   > "$G/dji/strings/0x409/product"
mkdir -p "$G/dji/configs/c.1/strings/0x409"
echo 250 > "$G/dji/configs/c.1/MaxPower"
echo "cfg1" > "$G/dji/configs/c.1/strings/0x409/configuration"
mkdir -p "$G/dji/functions/ffs.dji"
ln -sf "$G/dji/functions/ffs.dji" "$G/dji/configs/c.1/ffs.dji"

# 3. Mount functionfs.
mkdir -p "$FFS"
mountpoint -q "$FFS" || mount -t functionfs dji "$FFS"

# 4. Start the capture daemon (writes descriptors, holds ep0 open).
: > "$LOG"
"$DAEMON" "$FFS" >> "$LOG" 2>&1 &
echo $! > /run/ffs_capture.pid
sleep 0.5

# 5. Bind to the UDC (host enumeration happens on plug-in).
echo "$UDC_NAME" > "$G/dji/UDC"
echo "OK: capture gadget bound to $UDC_NAME (18d1:2d00). Log: $LOG"
