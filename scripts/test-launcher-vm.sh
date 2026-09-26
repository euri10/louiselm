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
  ([.qemu[] | select(test("virtfs|virtiofs|usb-host|recovery-usb|u2f|pcap|vhost-vsock|guestfwd|/dev/sd"))] | length) == 0
' <<<"$plan" >/dev/null

# Real Provider acceptance opts the guest into egress; normal starts stay offline.
provider_plan=$(bash "$vm" plan --provider-egress)
jq -e '
  .network == "provider-egress" and
  (.qemu | index("user,id=net0,restrict=off,hostfwd=tcp:127.0.0.1:22554-:22")) != null and
  ([.qemu[] | select(test("guestfwd|virtfs|virtiofs|vhost-vsock"))] | length) == 0
' <<<"$provider_plan" >/dev/null
if bash "$vm" plan --provider-egress --yubikey 003:002 >/dev/null 2>&1; then
  echo 'accepted simultaneous Provider egress and hardware passthrough' >&2; exit 1
fi

# Recovery access is explicit and pins both the address and the known token.
recovery_plan=$(bash "$vm" plan --yubikey 003:002)
jq -e '
  (.qemu | index("qemu-xhci,id=recovery-usb")) != null and
  (.qemu | index("usb-host,bus=recovery-usb.0,hostbus=3,hostaddr=2,vendorid=0x1050,productid=0x0407")) != null and
  (.qemu | index("user,id=net0,restrict=on,hostfwd=tcp:127.0.0.1:22554-:22")) != null
' <<<"$recovery_plan" >/dev/null
for selector in 0:2 3:0 256:2 3:256 3:2,pcap=/tmp/capture /dev/hidraw2; do
  if bash "$vm" plan --yubikey "$selector" >/dev/null 2>&1; then
    echo 'accepted unsafe USB selector' >&2; exit 1
  fi
done
for port in 0 22 22554 65536 123456789123456789 1234:remote:22; do
  if bash "$vm" forward "$port" >/dev/null 2>&1; then
    echo 'accepted unsafe browser port' >&2; exit 1
  fi
done
if bash "$vm" terminal </dev/null >/dev/null 2>&1; then
  echo 'accepted non-operator terminal' >&2; exit 1
fi

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
    "$test_dir/cache/louiselm-launcher-vm/prepared.qcow2" \
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
touch "$test_dir/cache/louiselm-launcher-vm/prepared.qcow2"
if bash "$vm" start --yubikey 003:002 >/dev/null 2>&1; then
  echo 'accepted hardware attachment to active VM' >&2; exit 1
fi
if bash "$vm" start --provider-egress >/dev/null 2>&1; then
  echo 'accepted network-mode change to active VM' >&2; exit 1
fi
if bash "$vm" start >/dev/null 2>&1; then
  echo 'accepted ambiguous network mode on active VM' >&2; exit 1
fi
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
# Long-lived tunnels must not reserve the mutation lock or expose the LAN.
exec 8>"$test_dir/cache/louiselm-launcher-vm/control.lock"
flock -n 8
set +e
bash "$vm" forward 043219 >/dev/null 2>&1
result=$?
set -e
[[ $result == 64 ]] || { echo 'accepted ambiguous port spelling' >&2; exit 1; }
set +e
bash "$vm" forward 43219
result=$?
set -e
[[ $result == 42 ]] || { echo 'lost tunnel exit status or held control lock' >&2; exit 1; }
for option in StrictHostKeyChecking=yes IdentityAgent=none ForwardAgent=no BatchMode=yes ExitOnForwardFailure=yes 127.0.0.1:43219:127.0.0.1:43219; do
  grep -Fxq "$option" "$test_dir/ssh-args"
done
grep -Fxq -- '-N' "$test_dir/ssh-args"
flock -u 8
exec 8>&-
export VM_TEST_ACTIVE=0 VM_TEST_LOAD=not-found
if bash "$vm" start --yubikey 255:255 >/dev/null 2>&1; then
  echo 'started with missing/inaccessible hardware' >&2; exit 1
fi
[[ ! -e $test_dir/cache/louiselm-launcher-vm/run.qcow2 ]] || { echo 'hardware refusal created an image' >&2; exit 1; }
if bash "$vm" forward 43219 >/dev/null 2>&1; then
  echo 'forward accepted stopped VM' >&2; exit 1
fi
bash "$vm" stop
bash "$vm" stop
echo 'launcher-vm safety contract: passed'
