#!/bin/sh
# Boot-time start of Monica Rust on the Synology NAS.
# Install as /usr/local/etc/rc.d/monica-rs.sh (root, mode 755); DSM runs it with "start".
#
# Everything lives on the USB SSD, which is mounted after Docker starts. This
# waits for the disk, tunes its queue, then starts the services and the two
# host-side import helpers. Running it again is harmless.
ROOT=/volumeUSB1/usbshare/monica-rs
DOCKER=/var/packages/ContainerManager/target/usr/bin/docker
COMPOSE=/var/packages/ContainerManager/target/usr/bin/docker-compose
LOG=$ROOT/state/boot.log

running() { ps -eo args | grep -v grep | grep -q "$1"; }

start() {
  for _ in $(seq 1 240); do
    [ -f "$ROOT/data/.monica-rs-ssd-target" ] && "$DOCKER" info >/dev/null 2>&1 && break
    sleep 5
  done
  [ -f "$ROOT/data/.monica-rs-ssd-target" ] || exit 1
  exec >>"$LOG" 2>&1
  echo "== boot $(date -u +%FT%TZ)"

  # DSM treats the USB SSD as a spinning disk and schedules it with CFQ, which
  # makes an interactive query wait behind the time slices of background ones.
  device=$(awk -v mount="${ROOT%/*}" '$2==mount {print $1}' /proc/mounts | sed 's#/dev/##; s#p[0-9]*$##')
  if [ -d "/sys/block/$device/queue" ]; then
    echo 0 > "/sys/block/$device/queue/rotational"
    echo deadline > "/sys/block/$device/queue/scheduler"
  fi

  "$COMPOSE" -p monica-rs -f "$ROOT/config/compose.yaml" --env-file "$ROOT/config/.env.live" up -d postgres server proxy </dev/null
  # The import worker is a plain container; its name is recorded when it is created.
  if [ -f "$ROOT/config/live-imports.container" ]; then
    "$DOCKER" start "$(cat "$ROOT/config/live-imports.container")" </dev/null
  fi

  # Host helpers: keep Chatlog's SQLite files readable for the import worker,
  # and supervise the recent WeChat photo worker. Each is retried until it stays up.
  if [ -f "$ROOT/config/chatlog-readiness.py" ] && ! running "config/chatlog-readiness.py"; then
    nohup sh -c "while true; do /usr/bin/python3 -u $ROOT/config/chatlog-readiness.py --watch; sleep 60; done" >>"$ROOT/state/chatlog-readiness.log" 2>&1 </dev/null &
  fi
  if [ -f "$ROOT/config/wechat-images.args" ] && ! running "config/wechat-images-supervisor.py"; then
    nohup sh -c "cd $ROOT/state && while true; do /usr/bin/python3 -u $ROOT/config/wechat-images-supervisor.py \$(cat $ROOT/config/wechat-images.args); sleep 60; done" >>"$ROOT/state/wechat-images-supervisor.log" 2>&1 </dev/null &
  fi
}

case "${1:-start}" in
start) start & ;;
esac
