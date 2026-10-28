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
LOG="${1:-$REPO/logs/capture.log}"
mkdir -p "$(dirname "$LOG")"
UDC_NAME="$(ls /sys/class/udc | head -1)"

[ -x "$DAEMON" ] || { echo "build first: cargo build --release   (missing $DAEMON)"; exit 1; }
modprobe usb_f_fs 2>/dev/null || true

# Refuse to run on top of a half-configured gadget: re-running without a
# teardown fails confusingly (the ffs.dji symlink already exists, functionfs is
# mounted, etc.). Make the user tear down first.
if [ -d "$G/dji" ]; then
    echo "gadget $G/dji already exists; run 'sudo scripts/restore_gadget.sh' first" >&2
    exit 1
fi

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
# Cap negotiation at high speed: tegra-xudc can run SuperSpeed, but the gadget
# only ships FS/HS descriptors, so pin the speed to match what the controller
# actually uses and avoid a SuperSpeed bind failure. (Ignored on older kernels.)
echo high-speed > "$G/dji/max_speed" 2>/dev/null || true
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
DAEMON_PID=$!
echo "$DAEMON_PID" > /run/ffs_capture.pid

# 5. Wait for the daemon to finish writing descriptors before binding the UDC
#    (binding before descriptors are written fails). Poll its log, not a fixed
#    sleep, and bail if the daemon dies.
for _ in $(seq 1 150); do
    grep -q "descriptors+strings written" "$LOG" && break
    kill -0 "$DAEMON_PID" 2>/dev/null || { echo "daemon exited early; see $LOG" >&2; exit 1; }
    sleep 0.02
done
if ! grep -q "descriptors+strings written" "$LOG"; then
    echo "timed out waiting for daemon to write descriptors; see $LOG" >&2
    exit 1
fi

# 6. Bind to the UDC (host enumeration happens on plug-in).
echo "$UDC_NAME" > "$G/dji/UDC"
echo "OK: capture gadget bound to $UDC_NAME (18d1:2d00). Log: $LOG"
