#!/bin/sh
IFS= read -r line || exit 1
case "$line" in
  *'"type":"begin"'*) printf '%s\n' '{"type":"begun","v":2}' ;;
  *'"type":"fence"'*) printf '%s\n' '{"type":"begun","type":"begun","v":1}' ;;
  *'"type":"report"'*) exec sleep 10 ;;
  *) exit 1 ;;
esac
