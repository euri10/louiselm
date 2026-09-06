#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
vm="$script_dir/launcher-vm"

# The plan is the same argv used to launch, without downloads or VM side effects.
plan=$(bash "$vm" plan)
jq -e '
  .schema == "louiselm.launcher-vm.plan/1" and
  .network == "restricted" and
  (.qemu | index("q35,accel=kvm")) != null and
  (.qemu | index("4096")) != null and
  (.qemu | index("2")) != null and
  (.qemu | index("-no-reboot")) != null and
  (.qemu | index("file=/usr/share/OVMF/OVMF_CODE_4M.fd,format=raw,if=pflash,readonly=on")) != null and
  (.qemu | index("user,id=net0,restrict=on,hostfwd=tcp:127.0.0.1:22554-:22")) != null and
  (.qemu | index("on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny")) != null and
  (.systemd | index("MemoryMax=5G")) != null and
  (.systemd | index("MemorySwapMax=0")) != null and
  (.systemd | index("CPUQuota=200%")) != null and
  (.systemd | index("RuntimeMaxSec=3600")) != null and
  ([.qemu[] | select(test("virtfs|virtiofs|usb-host|vhost-vsock|guestfwd|/dev/sd"))] | length) == 0
' <<<"$plan" >/dev/null

# Refuse unsafe QEMU option characters and malformed invocations before effects.
if XDG_CACHE_HOME='relative' bash "$vm" plan >/dev/null 2>&1; then
  echo 'accepted relative state path' >&2; exit 1
fi
if XDG_CACHE_HOME='/tmp/cache,option' bash "$vm" plan >/dev/null 2>&1; then
  echo 'accepted QEMU option injection' >&2; exit 1
fi
XDG_CACHE_HOME='/tmp/regular-cache' bash "$vm" plan >/dev/null
if bash "$vm" reset >/dev/null 2>&1; then
  echo 'reset did not require explicit discard acknowledgement' >&2; exit 1
fi
if bash "$vm" exec >/dev/null 2>&1; then
  echo 'exec accepted no command' >&2; exit 1
fi

# A private fixture substitutes only process/SSH endpoints, never starts QEMU.
test_dir=$(mktemp -d "${TMPDIR:-/var/tmp}/louiselm-vm-test.XXXXXX")
cleanup() {
  rm -f -- "$test_dir/bin/systemctl" "$test_dir/bin/ssh" "$test_dir/ssh-args" \
    "$test_dir/cache/louiselm-launcher-vm/control.lock"
  rmdir -- "$test_dir/cache/louiselm-launcher-vm" "$test_dir/cache" "$test_dir/bin" "$test_dir"
}
trap cleanup EXIT
mkdir -m 700 -p "$test_dir/bin" "$test_dir/cache/louiselm-launcher-vm"
cat >"$test_dir/bin/systemctl" <<'MOCK'
#!/usr/bin/env bash
case $2 in
  is-active) [[ ${VM_TEST_ACTIVE:-0} == 1 ]] ;;
  is-failed) exit 1 ;;
  show) echo "${VM_TEST_LOAD:-not-found}" ;;
  *) exit 1 ;;
esac
MOCK
cat >"$test_dir/bin/ssh" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$@" >"$VM_TEST_SSH_ARGS"
exit 42
MOCK
chmod +x "$test_dir/bin/systemctl" "$test_dir/bin/ssh"
export XDG_CACHE_HOME="$test_dir/cache" PATH="$test_dir/bin:$PATH"
export VM_TEST_ACTIVE=1 VM_TEST_LOAD=loaded VM_TEST_SSH_ARGS="$test_dir/ssh-args"
if bash "$vm" reset --discard >/dev/null 2>&1; then
  echo 'reset accepted a live unit' >&2; exit 1
fi
set +e
bash "$vm" exec printf '%s' 'space; $literal'
result=$?
set -e
[[ $result == 42 ]] || { echo 'lost remote exit status' >&2; exit 1; }
for option in StrictHostKeyChecking=yes IdentityAgent=none ForwardAgent=no BatchMode=yes; do
  grep -Fxq "$option" "$test_dir/ssh-args"
done
grep -Fxq 'printf %s space\;\ \$literal ' "$test_dir/ssh-args"
export VM_TEST_ACTIVE=0 VM_TEST_LOAD=not-found
bash "$vm" stop
bash "$vm" stop
echo 'launcher-vm safety contract: passed'
