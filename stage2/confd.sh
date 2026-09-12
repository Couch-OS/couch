#!/bin/sh
# Run inside Alpine so the daemon's default configuration path is persistent.
# flock prevents duplicate supervisors when started manually after boot.
BASE=$(dirname "$0")
exec 9>/tmp/couch-confd.lock
flock -n 9 || exit 0
while [ -x "$BASE/couch-confd" ]; do
    "$BASE/couch-confd" --config /opt/couch/config.json >>/tmp/confd.log 2>&1
    sleep 2
done
