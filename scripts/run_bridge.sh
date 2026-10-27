#!/bin/bash
#
# Bring up the FunctionFS gadget (Orin as the DJI app toward the Cendence RC)
# and start the transparent DUML bridge, which also drives the DJI tablet over
# a USB-A port as an AOA host. Pilot on the tablet then flies the real aircraft
# through the Orin while every DUML frame is logged. Run as root. Restore the
# stock gadget with restore_gadget.sh.
#
# SAFETY: the forwarded stream carries Pilot's flight commands to the powered
# aircraft. Run only with propellers removed / aircraft secured.
#
# Order: plug the tablet (CrystalSky) into a USB-A port first; this script does
# its AOA handshake (or reuses an accessory-mode tablet left by a prior run).
# The RC goes on the USB-C port.
#
# Usage: sudo scripts/run_bridge.sh [logfile]
set -euo pipefail

G=/sys/kernel/config/usb_gadget
FFS=/dev/ffs-dji
REPO="$(cd "$(dirname "$0")/.." && pwd)"
DAEMON="$REPO/target/release/bridge"
LOG="${1:-$REPO/bridge.log}"
UDC_NAME="$(ls /sys/class/udc | head -1)"

# shellcheck source-path=SCRIPTDIR source=gadget_lib.sh
. "$(dirname "$0")/gadget_lib.sh"

DAEMON_PID=""
COMMITTED=0

# Tear down whatever partial state we created, then rebind the stock gadget.
# Same teardown as restore_gadget.sh; safe to call at any point after we start.
cleanup() {
    [ "$COMMITTED" = 1 ] && return   # success: leave the running bridge in place
    echo "run_bridge: startup failed; cleaning up gadget state" >&2
    teardown_dji "$DAEMON_PID" || true
}

[ -x "$DAEMON" ] || { echo "build first: cargo build --release   (missing $DAEMON)"; exit 1; }
modprobe usb_f_fs 2>/dev/null || true

if [ -d "$G/dji" ]; then
    echo "gadget $G/dji already exists; run 'sudo scripts/restore_gadget.sh' first" >&2
    exit 1
fi

# From here on we create state the stock system doesn't own, so clean it up on
# any failure or signal (the COMMITTED guard spares the successful path).
trap cleanup EXIT INT TERM

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

# 4. Start the bridge (writes descriptors, then brings up the tablet side).
: > "$LOG"
"$DAEMON" "$FFS" >> "$LOG" 2>&1 &
DAEMON_PID=$!
echo "$DAEMON_PID" > /run/ffs_capture.pid

# 5. Wait until BOTH sides are staged before binding the UDC. The daemon logs
#    "both sides staged" only after the tablet AOA handshake (host side, no UDC
#    needed) has succeeded, so a tablet-side failure makes the daemon exit here
#    and the cleanup trap tears everything down instead of leaving a half-built
#    gadget that fails UDC bind with EBUSY. Allow time for a screen tap.
MARKER="both sides staged"
for _ in $(seq 1 1200); do          # up to ~60s at 0.05s/poll
    grep -q "$MARKER" "$LOG" && break
    kill -0 "$DAEMON_PID" 2>/dev/null || { echo "bridge exited early; see $LOG" >&2; exit 1; }
    sleep 0.05
done
if ! grep -q "$MARKER" "$LOG"; then
    echo "timed out waiting for the tablet side to come up; see $LOG" >&2
    exit 1
fi

# 6. Both sides up: bind the UDC so the RC can enumerate us.
echo "$UDC_NAME" > "$G/dji/UDC"
COMMITTED=1                          # past this point the bridge is live; keep it
echo "OK: bridge gadget bound to $UDC_NAME (18d1:2d00). Log: $LOG"
echo "   Watch the log for 'relays: ... forwarding' once the RC enables the gadget."
