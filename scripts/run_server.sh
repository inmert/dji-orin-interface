#!/bin/bash
#
# Bring up the FunctionFS gadget (Orin as the DJI app toward the Cendence RC)
# and start the web control server: telemetry + gimbal control in the browser.
# No DJI tablet is involved — the Orin is the app. Run as root. Tear down with
# restore_gadget.sh.
#
# SAFETY: the gimbal commands the UI sends move the real camera mount via the
# powered aircraft. Nothing is injected until the UI sends a command, and a UI
# STOP / browser disconnect releases every gimbal. Run props-off / aircraft secured.
#
# Usage: sudo scripts/run_server.sh [logfile]
#   Then open http://<orin-ip>:8080/ in a browser on the same network.
set -euo pipefail

G=/sys/kernel/config/usb_gadget
FFS=/dev/ffs-dji
REPO="$(cd "$(dirname "$0")/.." && pwd)"
DAEMON="$REPO/target/release/server"
WEB_ROOT="$REPO/web/dist"
LOG="${1:-$REPO/server.log}"
UDC_NAME="$(ls /sys/class/udc | head -1)"

DAEMON_PID=""
COMMITTED=0

cleanup() {
    [ "$COMMITTED" = 1 ] && return
    echo "run_server: startup failed; cleaning up gadget state" >&2
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null || true
    [ -e "$G/dji/UDC" ] && echo "" > "$G/dji/UDC" 2>/dev/null || true
    rm -f "$G/dji/configs/c.1/ffs.dji" 2>/dev/null || true
    mountpoint -q "$FFS" && umount "$FFS" 2>/dev/null || true
    rmdir "$G/dji/functions/ffs.dji" 2>/dev/null || true
    rmdir "$G/dji/configs/c.1/strings/0x409" 2>/dev/null || true
    rmdir "$G/dji/configs/c.1" 2>/dev/null || true
    rmdir "$G/dji/strings/0x409" 2>/dev/null || true
    rmdir "$G/dji" 2>/dev/null || true
    rm -f /run/ffs_capture.pid 2>/dev/null || true
    if [ -d "$G/l4t" ] && [ -z "$(cat "$G/l4t/UDC" 2>/dev/null)" ]; then
        echo "$UDC_NAME" > "$G/l4t/UDC" 2>/dev/null && echo "restored l4t gadget" >&2 || true
    fi
}

[ -x "$DAEMON" ] || { echo "build first: cargo build --release   (missing $DAEMON)"; exit 1; }
[ -f "$WEB_ROOT/index.html" ] || { echo "build the web UI first: (cd web && npm ci && npm run build)"; exit 1; }
modprobe usb_f_fs 2>/dev/null || true

if [ -d "$G/dji" ]; then
    echo "gadget $G/dji already exists; run 'sudo scripts/restore_gadget.sh' first" >&2
    exit 1
fi

trap cleanup EXIT INT TERM

# 1. Unbind any currently-bound gadget (e.g. the stock l4t one).
for g in "$G"/*/; do
    [ -e "$g/UDC" ] || continue
    cur="$(cat "$g/UDC" 2>/dev/null || true)"
    [ -n "$cur" ] && { echo "" > "$g/UDC"; echo "unbound $(basename "$g") (was $cur)"; }
done

# 2. Create the accessory gadget (identity matches the real controller).
mkdir -p "$G/dji"
echo 0x18d1 > "$G/dji/idVendor"
echo 0x2d00 > "$G/dji/idProduct"
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

# 4. Start the server (writes descriptors, serves HTTP :8080 + WS :8081).
: > "$LOG"
"$DAEMON" "$FFS" "$WEB_ROOT" >> "$LOG" 2>&1 &
DAEMON_PID=$!
echo "$DAEMON_PID" > /run/ffs_capture.pid

# 5. Wait for the gadget descriptors before binding the UDC; bail if the server
#    dies first (the cleanup trap then tears everything down).
for _ in $(seq 1 300); do          # up to ~15s
    grep -q "gadget ready" "$LOG" && break
    kill -0 "$DAEMON_PID" 2>/dev/null || { echo "server exited early; see $LOG" >&2; exit 1; }
    sleep 0.05
done
if ! grep -q "gadget ready" "$LOG"; then
    echo "timed out waiting for the server to write descriptors; see $LOG" >&2
    exit 1
fi

# 6. Bind the UDC so the RC can enumerate us.
echo "$UDC_NAME" > "$G/dji/UDC"
COMMITTED=1
IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
echo "OK: server gadget bound to $UDC_NAME (18d1:2d00). Log: $LOG"
echo "   Open the UI at http://${IP:-<orin-ip>}:8080/  (WebSocket on :8081)"
