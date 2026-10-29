#!/bin/bash
#
# Restart the server without unplugging the RC. Stops the running server,
# removes its gadget (without rebinding the stock l4t gadget in between), starts
# a fresh server, and if the RC has not enabled the new gadget within a few
# seconds, makes it re-enumerate with a soft disconnect/connect of the UDC (the
# data pull-up drops and comes back, as on a replug, while the cable and the
# Type-C roles stay as they are). Run as root; put env vars after sudo:
#
#   sudo OSDK_APP_ID=<app id> scripts/restart_server.sh [logfile]
#
# This only helps an RC that was already attached and working. An RC that
# booted with the cable attached locks the Orin's Type-C port into source mode,
# and only the plug-after-boot order fixes that (see docs/hardware.md).
set -euo pipefail

G=/sys/kernel/config/usb_gadget
FFS=/dev/ffs-dji
REPO="$(cd "$(dirname "$0")/.." && pwd)"
LOG="${1:-$REPO/logs/server.log}"
UDC_NAME="$(ls /sys/class/udc | head -1)"
UDC="/sys/class/udc/$UDC_NAME"

# shellcheck source-path=SCRIPTDIR source=gadget_lib.sh
. "$(dirname "$0")/gadget_lib.sh"

wait_enable() {   # wait_enable SECONDS — 0 once the RC has enabled the gadget
    for _ in $(seq 1 $(( $1 * 10 ))); do
        grep -q "gadget: ENABLE" "$LOG" 2>/dev/null && return 0
        sleep 0.1
    done
    return 1
}

if [ -d "$G/dji" ]; then
    teardown_dji "$(cat /run/ffs_capture.pid 2>/dev/null || true)" keep-l4t-unbound
fi
"$REPO/scripts/run_server.sh" "$LOG" | grep -E "^OK|^   Open" || true
[ -n "$(cat "$G/dji/UDC" 2>/dev/null)" ] || { echo "restart_server: server did not start; see $LOG" >&2; exit 1; }

if wait_enable 5; then
    echo "RC link up (gadget enabled)"
    exit 0
fi
echo "RC has not enabled the gadget (UDC $(cat "$UDC/state")); soft reconnect"
echo disconnect > "$UDC/soft_connect"
sleep 1
echo connect > "$UDC/soft_connect"
if wait_enable 10; then
    echo "RC link up after soft reconnect"
else
    echo "RC still not enabled (UDC $(cat "$UDC/state")). If the RC is attached, check its" \
         "Type-C state, or unplug and replug it." >&2
    exit 1
fi
