#!/bin/sh
# Boot-time settings for the Monica Rust data SSD on the Synology NAS.
# Install as /usr/local/etc/rc.d/monica-rs.sh (root, mode 755).
#
# DSM treats the USB SSD as a spinning disk and schedules it with CFQ, which
# makes an interactive query wait behind the time slices of background queries.
MOUNT=/volumeUSB1/usbshare

case "${1:-start}" in
start)
  (
    # The disk can appear after this script runs; wait for it.
    for _ in $(seq 1 120); do
      device=$(awk -v mount="$MOUNT" '$2==mount {print $1}' /proc/mounts | sed 's#/dev/##; s#p[0-9]*$##')
      if [ -n "$device" ] && [ -d "/sys/block/$device/queue" ]; then
        echo 0 > "/sys/block/$device/queue/rotational"
        echo deadline > "/sys/block/$device/queue/scheduler"
        exit 0
      fi
      sleep 5
    done
  ) &
  ;;
esac
