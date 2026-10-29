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
#   Then open http://<orin-ip>:8080/ in a browser on the same network, or
#   https://<orin-ip>:8443/ (self-signed certificate) for the microphone.
#
# BRING-UP ORDER: run this with the RC unplugged, power the RC on and let it
# fully boot, THEN plug it in. An RC that boots with the cable attached locks the
# Orin's Type-C port into source mode (fusb301 fmode=SRC), and the log never gets
# past "gadget: BIND". See README "Bring-up order matters".
set -euo pipefail

G=/sys/kernel/config/usb_gadget
FFS=/dev/ffs-dji
REPO="$(cd "$(dirname "$0")/.." && pwd)"
DAEMON="$REPO/target/release/server"
WEB_ROOT="$REPO/web/dist"
LOG="${1:-$REPO/logs/server.log}"
mkdir -p "$(dirname "$LOG")"
UDC_NAME="$(ls /sys/class/udc | head -1)"

# shellcheck source-path=SCRIPTDIR source=gadget_lib.sh
. "$(dirname "$0")/gadget_lib.sh"

DAEMON_PID=""
COMMITTED=0

cleanup() {
    [ "$COMMITTED" = 1 ] && return
    echo "run_server: startup failed; cleaning up gadget state" >&2
    teardown_dji "$DAEMON_PID" || true
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

# 4. Start the server (writes descriptors, serves HTTP :8080 + HTTPS :8443).
: > "$LOG"
export TLS_CERT_DIR="${TLS_CERT_DIR:-$REPO/certs}"
# Voice: whisper.cpp lives in the invoking user's home, not root's.
USER_HOME="$(getent passwd "${SUDO_USER:-$USER}" | cut -d: -f6)"
export WHISPER_DIR="${WHISPER_DIR:-$USER_HOME/repos/third_party/whisper.cpp}"
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
echo "   Open the UI at http://${IP:-<orin-ip>}:8080/  (voice input: https://${IP:-<orin-ip>}:8443/)"
echo "   Now power the RC on, let it fully boot, THEN plug it into the Orin's USB-C."
echo "   Expect 'gadget: ENABLE' in $LOG within seconds."
