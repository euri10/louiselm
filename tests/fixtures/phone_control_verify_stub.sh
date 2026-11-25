#!/bin/sh
IFS= read -r line || exit 1
case "$line" in
  *'"operation_id":"ffffffffffffffffffffffffffffffff"'*) printf '%s\n' '{"v":1,"type":"error","code":"unauthenticated"}' ;;
  *'"operation_id":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"'*) printf '%s\n' '{"v":1,"type":"ver ified"}' ;;
  *'"operation_id":"dddddddddddddddddddddddddddddddd"'*) exit 1 ;;
  *'"operation_id":"cccccccccccccccccccccccccccccccc"'*) exec sleep 10 ;;
  *) printf '%s\n' '{"v":1,"type":"verified"}' ;;
esac
