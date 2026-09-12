#!/bin/busybox sh
# Supervise the system service in the outer root; /tmp is shared with Alpine.
BASE=$(dirname "$0")
exec 9>/tmp/couch-system-supervisor.lock
/bin/busybox flock -n 9 || exit 0
while [ -x "$BASE/couch-system" ]; do
    "$BASE/couch-system" serve >>/tmp/system.log 2>&1
    /bin/busybox sleep 2
done
