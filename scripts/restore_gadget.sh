#!/bin/bash
# Tear down the capture/server gadget and rebind the stock NVIDIA l4t gadget.
# Run as root.
G=/sys/kernel/config/usb_gadget
FFS=/dev/ffs-dji
UDC_NAME="$(ls /sys/class/udc | head -1)"

# shellcheck source-path=SCRIPTDIR source=gadget_lib.sh
. "$(dirname "$0")/gadget_lib.sh"

# Stop the daemon first, then unbind and remove the gadget (see gadget_lib.sh
# for why the order matters).
if teardown_dji "$(cat /run/ffs_capture.pid 2>/dev/null || true)"; then
    echo "teardown done"
else
    exit 1
fi
