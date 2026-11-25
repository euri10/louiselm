#!/bin/sh
binary=$1
shift
if [ "$1" = phone-control ] && [ "$2" = verify ]; then
  IFS= read -r line || exit 1
  printf '%s\n' '{"v":1,"type":"verified"}'
else
  exec "$binary" "$@"
fi
