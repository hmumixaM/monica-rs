#!/bin/sh
set -eu
# An explicit source connection owns every session and pending page.
case "${MONICA_WHATSAPP_CONNECTION_ID:-}" in
  ''|*[!0-9a-fA-F-]*) echo 'Set MONICA_WHATSAPP_CONNECTION_ID to the connection UUID shown in Sources' >&2; exit 2 ;;
esac
if [ "${#MONICA_WHATSAPP_CONNECTION_ID}" -ne 36 ]; then
  echo 'Invalid source connection UUID' >&2
  exit 2
fi
exec /usr/local/bin/monica-whatsapp --root "/data/media/sources/whatsapp/$MONICA_WHATSAPP_CONNECTION_ID"
