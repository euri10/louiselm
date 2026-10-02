#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
vm="$script_dir/launcher-vm"
fido_probe="$script_dir/launcher-vm-fido"
sh -n "$fido_probe"

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
  ([.qemu[] | select(test("virtfs|virtiofs|usb-host|recovery-usb|u2f|pcap|vhost-vsock|guestfwd|/dev/sd"))] | length) == 0 and
  (.base_image | test("/louiselm-launcher-vm/prepared-[0-9a-f]{16}\\.qcow2$"))
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
vm_fixture_pid=
cleanup() {
  if [[ -n $vm_fixture_pid ]]; then
    kill "$vm_fixture_pid" 2>/dev/null || true
    wait "$vm_fixture_pid" 2>/dev/null || true
  fi
  rm -rf -- "$test_dir/checkout-one" "$test_dir/checkout two"
  rm -f -- "$test_dir/bin/systemctl" "$test_dir/bin/ssh" "$test_dir/bin/udevadm" "$test_dir/ssh-args" \
    "$test_dir/hold" "$test_dir/other cache/louiselm-launcher-vm/control.lock" \
    "$test_dir/cache/louiselm-launcher-vm/"prepared*.qcow2 \
    "$test_dir/cache/louiselm-launcher-vm/run.qcow2" \
    "$test_dir/cache/louiselm-launcher-vm/control.lock"
  if [[ -d $test_dir/fido ]]; then
    rm -f -- "$test_dir/fido/usb/1-1/"{idVendor,idProduct,bConfigurationValue} \
      "$test_dir/fido/hidraw/hidraw0/device" "$test_dir/fido/nodes/hidraw0"
    rmdir -- "$test_dir/fido/usb/1-1/fido" "$test_dir/fido/usb/2-1/fido" \
      "$test_dir/fido/usb/1-1" "$test_dir/fido/usb/2-1" "$test_dir/fido/usb" \
      "$test_dir/fido/hidraw/hidraw0" "$test_dir/fido/hidraw" \
      "$test_dir/fido/nodes" "$test_dir/fido"
  fi
  rmdir -- "$test_dir/other cache/louiselm-launcher-vm" "$test_dir/other cache" \
    "$test_dir/cache/louiselm-launcher-vm" "$test_dir/cache" "$test_dir/bin" "$test_dir"
}
trap cleanup EXIT
mkdir -m 700 -p "$test_dir/bin" "$test_dir/cache/louiselm-launcher-vm"
cat >"$test_dir/bin/systemctl" <<'MOCK'
#!/usr/bin/env bash
case $2 in
  is-active) [[ ${VM_TEST_ACTIVE:-0} == 1 ]] ;;
  is-failed) exit 1 ;;
  show)
    if [[ $* == *MainPID* ]]; then echo "${VM_TEST_PID:-0}"
    else echo "${VM_TEST_LOAD:-not-found}"; fi
    ;;
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
mkdir -m 700 -p "$test_dir/other cache/louiselm-launcher-vm"
mkfifo "$test_dir/hold"
exec 7<>"$test_dir/hold"
# Only the process argv is doubled: no QEMU, SSH server or real unit starts.
bash -c 'exec -a /usr/bin/qemu-system-x86_64 bash -c "read -r -t 600" -serial "$1"' \
  _ "file:$XDG_CACHE_HOME/louiselm-launcher-vm/serial.log" <&7 &
vm_fixture_pid=$!
export VM_TEST_PID=$vm_fixture_pid
for attempt in {1..100}; do
  if tr '\0' '\n' <"/proc/$vm_fixture_pid/cmdline" | grep -Fxq /usr/bin/qemu-system-x86_64; then break; fi
  sleep 0.01
done
base=$(bash "$vm" plan | jq -r .base_image)
# louiselm-d7mxk: identical inputs must select the same base from any checkout.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
for checkout in "$test_dir/checkout-one" "$test_dir/checkout two"; do
  mkdir -p "$checkout/scripts" "$checkout/skills-core"
  cp -- "$vm" "$script_dir/verifier-toolchain" "$checkout/scripts/"
  git -C "$script_dir/.." show HEAD:skills-core/Cargo.lock >"$checkout/skills-core/Cargo.lock"
  git init -q --template= --initial-branch=main "$checkout"
  git -C "$checkout" add -- skills-core/Cargo.lock
  git -C "$checkout" -c user.name=Fixture -c user.email=fixture@example.invalid \
    -c core.hooksPath=/dev/null commit --no-gpg-sign -qm fixture
  checkout_base=$(bash "$checkout/scripts/launcher-vm" plan | jq -r .base_image)
  [[ $checkout_base == "$base" ]] || { echo 'identical checkout inputs selected a different VM base' >&2; exit 1; }
done
changed_checkout="$test_dir/checkout two"
printf '\n# changed verifier bytes\n' >>"$changed_checkout/scripts/verifier-toolchain"
changed_base=$(bash "$changed_checkout/scripts/launcher-vm" plan | jq -r .base_image)
[[ $changed_base != "$base" ]] || { echo 'changed verifier bytes reused the VM base' >&2; exit 1; }
cp -- "$script_dir/verifier-toolchain" "$changed_checkout/scripts/verifier-toolchain"
printf '\n# changed locked dependencies\n' >>"$changed_checkout/skills-core/Cargo.lock"
git -C "$changed_checkout" add -- skills-core/Cargo.lock
git -C "$changed_checkout" -c user.name=Fixture -c user.email=fixture@example.invalid \
  -c core.hooksPath=/dev/null commit --no-gpg-sign -qm 'changed lockfile'
changed_base=$(bash "$changed_checkout/scripts/launcher-vm" plan | jq -r .base_image)
[[ $changed_base != "$base" ]] || { echo 'changed HEAD lockfile reused the VM base' >&2; exit 1; }
for change in 's/^image_sha=/image_sha=changed-/' \
  's/^rust_toolchain=/rust_toolchain=changed-/' \
  's/^packages=(bubblewrap/packages=(changed-package bubblewrap/'; do
  sed "$change" "$vm" >"$test_dir/checkout-one/scripts/launcher-vm"
  changed_base=$(bash "$test_dir/checkout-one/scripts/launcher-vm" plan | jq -r .base_image)
  [[ $changed_base != "$base" ]] || { echo "changed provisioning input reused the VM base: $change" >&2; exit 1; }
done
# louiselm-6y1ee: a base built from other provisioning inputs is never used.
touch "$test_dir/cache/louiselm-launcher-vm/prepared.qcow2"
export VM_TEST_ACTIVE=0 VM_TEST_LOAD=not-found
if message=$(bash "$vm" start 2>&1); then
  echo 'started without a base for the current provisioning inputs' >&2; exit 1
fi
grep -Fq 'run prepare' <<<"$message" || { echo "unhelpful missing-base refusal: $message" >&2; exit 1; }
if bash "$vm" reset --discard >/dev/null 2>&1; then
  echo 'reset created an overlay without a current base' >&2; exit 1
fi
touch "$base"
qemu-img create -q -u -f qcow2 -F qcow2 -b "$test_dir/cache/louiselm-launcher-vm/prepared.qcow2" \
  "$test_dir/cache/louiselm-launcher-vm/run.qcow2" 1G
if message=$(bash "$vm" start 2>&1); then
  echo 'started an overlay built on a stale base' >&2; exit 1
fi
grep -Fq 'reset --discard' <<<"$message" || { echo "unhelpful stale-overlay refusal: $message" >&2; exit 1; }
rm -- "$test_dir/cache/louiselm-launcher-vm/run.qcow2"
export VM_TEST_ACTIVE=1 VM_TEST_LOAD=loaded
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
# louiselm-w9xgw: the unit/port is shared; another cache must never connect or stop it.
rm -- "$test_dir/ssh-args"
for action in 'exec true' stop 'forward 43219' 'put fixture /home/vm/fixture' 'get /home/vm/fixture fixture'; do
  if message=$(XDG_CACHE_HOME="$test_dir/other cache" bash "$vm" $action 2>&1); then
    echo "wrong cache accepted $action" >&2; exit 1
  fi
  grep -Fq 'running VM belongs to a different cache' <<<"$message" || {
    echo "wrong cache reached $action instead of refusing: $message" >&2; exit 1
  }
  [[ ! -e $test_dir/ssh-args ]] || { echo 'wrong-cache command reached SSH' >&2; exit 1; }
done
if message=$(VM_TEST_PID=0 bash "$vm" exec true 2>&1); then
  echo 'accepted an unidentifiable running VM' >&2; exit 1
fi
grep -Fq 'cannot identify running VM' <<<"$message" || { echo "wrong identity refusal: $message" >&2; exit 1; }
[[ ! -e $test_dir/ssh-args ]] || { echo 'unidentifiable VM reached SSH' >&2; exit 1; }
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

# louiselm-bzfc4: enumeration alone succeeded in the actual recovery guest,
# although configuration failed and no FIDO HID transport existed.
export VM_TEST_FIDO_ROOT="$test_dir/fido" VM_TEST_FIDO_PROPERTIES=ID_FIDO_TOKEN=1
export VM_TEST_FIDO_UDEV_STATUS=0
mkdir -p "$test_dir/fido/usb/"{1-1,2-1}/fido \
  "$test_dir/fido/hidraw/hidraw0" "$test_dir/fido/nodes"
printf '1050\n' >"$test_dir/fido/usb/1-1/idVendor"
printf '0407\n' >"$test_dir/fido/usb/1-1/idProduct"
printf '\n' >"$test_dir/fido/usb/1-1/bConfigurationValue"
cat >"$test_dir/bin/udevadm" <<'MOCK'
#!/usr/bin/env bash
[[ $* == "info --query=property --name=$VM_TEST_FIDO_ROOT/nodes/hidraw0" ]] || exit 90
[[ $VM_TEST_FIDO_UDEV_STATUS == 0 ]] || { echo 'udev inspection failed' >&2; exit 1; }
printf '%s\n' "$VM_TEST_FIDO_PROPERTIES"
MOCK
chmod +x "$test_dir/bin/udevadm"
check_fido() {
  setsid --wait sh "$fido_probe" "$test_dir/fido/usb" "$test_dir/fido/hidraw" "$test_dir/fido/nodes"
}
refuse_fido() {
  if message=$(check_fido 2>&1); then
    echo "accepted $1" >&2; exit 1
  fi
  grep -Fq 'selected YubiKey has no configured accessible FIDO HID transport' <<<"$message" || {
    echo "wrong FIDO refusal for $1: $message" >&2; exit 1
  }
}
refuse_fido 'unconfigured VID/PID-only USB device'
printf '1\n' >"$test_dir/fido/usb/1-1/bConfigurationValue"
refuse_fido 'configured USB device without an associated HID'
ln -s "$test_dir/fido/usb/1-1/fido" "$test_dir/fido/hidraw/hidraw0/device"
ln -s /dev/null "$test_dir/fido/nodes/hidraw0"
check_fido
printf '\n' >"$test_dir/fido/usb/1-1/bConfigurationValue"
refuse_fido 'unconfigured USB device with lingering HID metadata'
printf '0\n' >"$test_dir/fido/usb/1-1/bConfigurationValue"
refuse_fido 'configuration zero with lingering HID metadata'
printf '1\n' >"$test_dir/fido/usb/1-1/bConfigurationValue"
export VM_TEST_FIDO_PROPERTIES=ID_INPUT_KEYBOARD=1
refuse_fido 'OTP-only HID'
export VM_TEST_FIDO_PROPERTIES=ID_FIDO_TOKEN=1
rm -- "$test_dir/fido/hidraw/hidraw0/device"
ln -s "$test_dir/fido/usb/2-1/fido" "$test_dir/fido/hidraw/hidraw0/device"
refuse_fido 'FIDO HID belonging to another USB device'
rm -- "$test_dir/fido/hidraw/hidraw0/device"
ln -s "$test_dir/fido/usb/1-1/fido" "$test_dir/fido/hidraw/hidraw0/device"
rm -- "$test_dir/fido/nodes/hidraw0"
touch "$test_dir/fido/nodes/hidraw0"
refuse_fido 'regular file in place of a HID character device'
rm -- "$test_dir/fido/nodes/hidraw0"
ln -s /dev/tty "$test_dir/fido/nodes/hidraw0"
# setsid leaves no controlling terminal: the character node exists, but its
# read/write open fails. No test reads from or writes to a real terminal.
refuse_fido 'FIDO character node that cannot be opened'
rm -- "$test_dir/fido/nodes/hidraw0"
ln -s /dev/null "$test_dir/fido/nodes/hidraw0"
export VM_TEST_FIDO_UDEV_STATUS=1
refuse_fido 'failed HID metadata inspection'
export VM_TEST_FIDO_UDEV_STATUS=0
check_fido
echo 'launcher-vm safety contract: passed'
