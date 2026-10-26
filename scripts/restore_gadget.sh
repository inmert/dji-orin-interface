#!/bin/bash
# Tear down the capture/server gadget and rebind the stock NVIDIA l4t gadget.
# Run as root.
G=/sys/kernel/config/usb_gadget
FFS=/dev/ffs-dji
UDC_NAME="$(ls /sys/class/udc | head -1)"

# 1. Unbind our gadget from the UDC so the host detaches.
[ -e "$G/dji/UDC" ] && echo "" > "$G/dji/UDC" 2>/dev/null

# 2. Stop the daemon and WAIT for it to actually exit before unmounting. Its
#    bulk-reader threads block in uninterruptible reads, so SIGTERM can be slow
#    to take; escalate to SIGKILL. A still-running daemon holds the functionfs
#    endpoints open, which makes `umount` hang (the bug this guards against).
PID="$(cat /run/ffs_capture.pid 2>/dev/null || true)"
if [ -n "$PID" ]; then
    kill "$PID" 2>/dev/null || true
    for _ in $(seq 1 20); do kill -0 "$PID" 2>/dev/null || break; sleep 0.05; done
    kill -9 "$PID" 2>/dev/null || true
    for _ in $(seq 1 40); do kill -0 "$PID" 2>/dev/null || break; sleep 0.05; done
fi
rm -f /run/ffs_capture.pid

# 3. Unlink the function and unmount functionfs (lazy as a last resort).
rm -f "$G/dji/configs/c.1/ffs.dji" 2>/dev/null
if mountpoint -q "$FFS"; then
    umount "$FFS" 2>/dev/null || umount -l "$FFS" 2>/dev/null || true
fi

# 4. Remove the gadget config tree.
rmdir "$G/dji/functions/ffs.dji" 2>/dev/null
rmdir "$G/dji/configs/c.1/strings/0x409" 2>/dev/null
rmdir "$G/dji/configs/c.1" 2>/dev/null
rmdir "$G/dji/strings/0x409" 2>/dev/null
rmdir "$G/dji" 2>/dev/null

# 5. Rebind the stock gadget if present and currently unbound.
if [ -d "$G/l4t" ] && [ -z "$(cat "$G/l4t/UDC" 2>/dev/null)" ]; then
    echo "$UDC_NAME" > "$G/l4t/UDC" 2>/dev/null && echo "restored l4t gadget to $UDC_NAME"
fi
echo "teardown done"
