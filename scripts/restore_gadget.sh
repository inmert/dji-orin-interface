#!/bin/bash
# Tear down the capture gadget and rebind the stock NVIDIA l4t gadget. Run as root.
G=/sys/kernel/config/usb_gadget
FFS=/dev/ffs-dji
UDC_NAME="$(ls /sys/class/udc | head -1)"

[ -e "$G/dji/UDC" ] && echo "" > "$G/dji/UDC" 2>/dev/null
[ -f /run/ffs_capture.pid ] && { kill "$(cat /run/ffs_capture.pid)" 2>/dev/null; rm -f /run/ffs_capture.pid; }
sleep 0.3
rm -f "$G/dji/configs/c.1/ffs.dji" 2>/dev/null
umount "$FFS" 2>/dev/null
rmdir "$G/dji/functions/ffs.dji" 2>/dev/null
rmdir "$G/dji/configs/c.1/strings/0x409" 2>/dev/null
rmdir "$G/dji/configs/c.1" 2>/dev/null
rmdir "$G/dji/strings/0x409" 2>/dev/null
rmdir "$G/dji" 2>/dev/null
# Rebind stock gadget if present and currently unbound
if [ -d "$G/l4t" ] && [ -z "$(cat "$G/l4t/UDC" 2>/dev/null)" ]; then
    echo "$UDC_NAME" > "$G/l4t/UDC" 2>/dev/null && echo "restored l4t gadget to $UDC_NAME"
fi
echo "teardown done"
