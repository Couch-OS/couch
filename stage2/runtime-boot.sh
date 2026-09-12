#!/bin/busybox sh
# Stable bootstrap outside versioned application slots. Never changes partitions.
BB=/bin/busybox
ROOT=$(dirname "$0")
RUNTIME=$ROOT/runtime
export COUCH_RUNTIME_SELECTED=1
# Recovery must remain usable even when an application update is broken.
[ "${COUCH_NO_UI:-0}" = 1 ] && exec "$BB" sh "$ROOT/stage2.sh"
valid_id() {
    [ ${#1} -eq 64 ] || return 1
    case "$1" in *[!0-9a-f]*) return 1;; esac
}
rollback() {
    if [ "$PREVIOUS" = base ]; then
        $BB rm -f "$RUNTIME/current"
    else
        $BB rm -f "$RUNTIME/rollback-next"
        $BB ln -s "slots/$PREVIOUS" "$RUNTIME/rollback-next" || return 1
        # This device's BusyBox has no mv -T. Remove the symlink first;
        # power loss in this interval safely selects the base runtime.
        $BB rm -f "$RUNTIME/current" || return 1
        $BB mv -f "$RUNTIME/rollback-next" "$RUNTIME/current" || return 1
    fi
    $BB sync
    $BB rm -f "$RUNTIME/pending" "$RUNTIME/attempted"
}
CURRENT=$($BB readlink "$RUNTIME/current" 2>/dev/null)
SELECTED=${CURRENT#slots/}
if [ -f "$RUNTIME/pending" ]; then
    read -r PREVIOUS CANDIDATE EXTRA < "$RUNTIME/pending"
    if { [ "$PREVIOUS" = base ] || valid_id "$PREVIOUS"; } && valid_id "$CANDIDATE" && [ -z "$EXTRA" ] && [ "$CURRENT" = "slots/$CANDIDATE" ]; then
        if [ -f "$RUNTIME/attempted" ]; then
            rollback || exit 1
            CURRENT=$($BB readlink "$RUNTIME/current" 2>/dev/null)
            SELECTED=${CURRENT#slots/}
        else
            echo "$CANDIDATE" > "$RUNTIME/attempted"
            $BB sync
            (
                n=0; healthy=0; healthy_pid=0; healthy_stamp=0
                START=$($BB cut -d. -f1 /proc/uptime)
                while [ $n -lt 90 ]; do
                    $BB sleep 1; n=$((n+1))
                    NOW=$($BB cut -d. -f1 /proc/uptime)
                    [ $((NOW-START)) -lt 90 ] || break
                    read -r PID STAMP EXTRA_HEALTH < /tmp/couch-gui.health 2>/dev/null || { healthy=0; continue; }
                    [ -z "$EXTRA_HEALTH" ] || { healthy=0; continue; }
                    case "$PID:$STAMP:$NOW" in *[!0-9:]*|::*|:*|*:) healthy=0; continue;; esac
                    if [ -d "/proc/$PID" ] && [ "$STAMP" -le "$NOW" ] && [ $((NOW-STAMP)) -le 10 ] && $BB timeout 2 "$RUNTIME/slots/$CANDIDATE/couch-system" health >/dev/null 2>&1; then
                        # A live PID and a recently written marker do not prove
                        # the GUI is still running. Require advancing heartbeats
                        # from one process before accepting the candidate.
                        if [ "$PID" = "$healthy_pid" ] && [ "$STAMP" -gt "$healthy_stamp" ]; then
                            healthy=$((healthy+1))
                        else
                            healthy=1
                        fi
                        healthy_pid=$PID; healthy_stamp=$STAMP
                        if [ $healthy -ge 5 ]; then
                            echo "$PREVIOUS" > "$RUNTIME/previous"
                            $BB sync
                            $BB rm -f "$RUNTIME/pending" "$RUNTIME/attempted"
                            $BB sync
                            exit 0
                        fi
                    else
                        healthy=0
                    fi
                done
                rollback && $BB reboot -f
            ) </dev/null >/tmp/update-boot.log 2>&1 &
        fi
    else
        # Journal written before pointer switch: keep the already active runtime.
        $BB rm -f "$RUNTIME/pending" "$RUNTIME/attempted"
    fi
fi
if [ "$CURRENT" = "slots/$SELECTED" ] && valid_id "$SELECTED" && [ -f "$RUNTIME/slots/$SELECTED/stage2.sh" ]; then
    exec "$BB" sh "$RUNTIME/slots/$SELECTED/stage2.sh"
fi
exec "$BB" sh "$ROOT/stage2.sh"
