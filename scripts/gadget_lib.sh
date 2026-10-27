# shellcheck shell=bash
# Shared gadget teardown, sourced by restore_gadget.sh and the run_* cleanup
# traps. Expects G, FFS and UDC_NAME to be set by the caller.
#
# ORDER MATTERS: the daemon must be gone BEFORE the UDC is unbound. Unbinding a
# FunctionFS gadget waits in the kernel (uninterruptibly) for userspace to
# release the function's endpoint files; if the daemon still holds them, the
# unbind never returns, the gadget mutex stays held, and every later configfs
# access hangs too (hung-task warnings, and in practice a frozen machine).

# stop_daemon PID — SIGTERM, then SIGKILL, waiting for the process to exit.
# Returns 1 if it is still alive afterwards.
stop_daemon() {
    local pid="$1"
    [ -n "$pid" ] || return 0
    kill -0 "$pid" 2>/dev/null || return 0
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.05; done
    kill -9 "$pid" 2>/dev/null || true
    for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.05; done
    return 1
}

# teardown_dji PID — stop the daemon, then unbind and remove the dji gadget and
# rebind the stock l4t one. Refuses to unbind (returns 1) if the daemon would
# not die, since unbinding then deadlocks the kernel.
teardown_dji() {
    if ! stop_daemon "$1"; then
        echo "daemon $1 did not exit; NOT unbinding the gadget (that would deadlock)." >&2
        echo "Find what holds $FFS (e.g. 'sudo fuser -vm $FFS'), stop it, then re-run." >&2
        return 1
    fi
    rm -f /run/ffs_capture.pid 2>/dev/null || true

    if [ -e "$G/dji/UDC" ]; then echo "" > "$G/dji/UDC" 2>/dev/null || true; fi
    rm -f "$G/dji/configs/c.1/ffs.dji" 2>/dev/null || true
    if mountpoint -q "$FFS"; then
        umount "$FFS" 2>/dev/null || umount -l "$FFS" 2>/dev/null || true
    fi
    rmdir "$G/dji/functions/ffs.dji" 2>/dev/null || true
    rmdir "$G/dji/configs/c.1/strings/0x409" 2>/dev/null || true
    rmdir "$G/dji/configs/c.1" 2>/dev/null || true
    rmdir "$G/dji/strings/0x409" 2>/dev/null || true
    rmdir "$G/dji" 2>/dev/null || true

    if [ -d "$G/l4t" ] && [ -z "$(cat "$G/l4t/UDC" 2>/dev/null)" ]; then
        echo "$UDC_NAME" > "$G/l4t/UDC" 2>/dev/null && echo "restored l4t gadget to $UDC_NAME" >&2
    fi
    return 0
}
