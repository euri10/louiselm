#!/bin/sh
if [ "$1" = "--require-interface=1" ] && [ "$2" = "phone-control" ] && [ "$3" = "open-prepared" ]; then
  printf '%s' "$LOUISELM_TEST_PREPARED_RESPONSE"
  exit 0
fi
exit 2
