# Disposable launcher acceptance VM

Use `scripts/launcher-vm` for privileged launcher checks, never host `sudo`.
This is one headless QEMU/KVM guest, not an installed desktop launcher or a
replacement for the real Control broker acceptance in `skills-core/README.md`.

See the [launcher acceptance audit](launcher-acceptance.md) for the AC-by-AC
evidence map, confirmed gaps, and remaining privileged acceptance work.
The [hostile conformance recipe](launcher-conformance.md) runs the required
positive-control/denial matrix, existing runtime-mutation regressions and
production relay/loss composition. It is a component gate, not installed authority.

## Host boundary

- Run as the operator, with existing access to `/dev/kvm`. There is no sudo
  fallback, package installation, bridge, firewall change, or system daemon.
- Existing tools: QEMU, OVMF (`/usr/share/OVMF/*_4M.fd`), xorriso, OpenSSH,
  curl, jq, rustup, GNU coreutils, flock, and the systemd user manager.
- VM files live in `${XDG_CACHE_HOME:-$HOME/.cache}/louiselm-launcher-vm`,
  owned by the operator with mode `0700`. Do not place them in a RAM-backed
  `/tmp`. This machine's `/tmp` was already 99% full during preparation.
- Guest: 2 vCPUs, 4 GiB RAM, 32 GiB sparse disk. The transient user unit caps
  total memory at 5 GiB, swap at zero, CPU at two cores, and lifetime at one
  hour. It runs at nice 10 with no new privileges and QEMU seccomp filtering.
- No host directory, agent socket, existing SSH credential, USB device, or
  physical disk is exposed to the guest. The VM gets its own SSH client and
  host keys; host checking is pinned, not disabled. SSH ignores user config
  and agents. Only `127.0.0.1:22554` is forwarded to guest SSH.
  The explicit recovery-only YubiKey exception is described below; ordinary
  `start` and `prepare` do not enable it.
- `prepare` temporarily allows guest egress to install distro packages and
  Rust 1.97.1. Normal `start` uses QEMU `restrict=on`: guest-originated traffic
  cannot reach the host or outside networks. Explicit host-to-guest SSH remains.
- `start --provider-egress` is an explicit opt-in for the live Provider
  acceptance in `louiselm-qbr.5.1.3.13`. It allows outbound networking from
  the whole guest, not just the broker. The confined Session must still prove
  that its own network namespace has no ambient route. Use it only with a fresh
  disposable overlay and a dedicated capped API project; stop the VM and remove
  the credential-bearing overlay after the run. `plan --provider-egress` shows
  the exact QEMU mode before startup. This option cannot be combined with
  YubiKey passthrough or added to an already running guest.
- A VM is not a zero-risk guarantee: the host kernel, KVM, QEMU, and firmware
  remain trusted. No host security setting is weakened by this procedure.

The base is pinned to Debian 13 genericcloud amd64 `20260831-2587`; URL and
SHA-512 are visible in `plan`. The image hash is checked before use.
[Debian publishes cloud-image checksums over HTTPS](https://cloud.debian.org/images/cloud/),
not signed checksum manifests for these current images. Do not describe this
as signature verification. Guest packages are authenticated by Debian APT;
the existing host rustup executable provisions the pinned guest toolchain.

## Commands

```sh
./scripts/launcher-vm plan                  # JSON argv, limits, image pin; no effects
./scripts/launcher-vm plan --provider-egress # inspect explicit live-API network mode
./scripts/launcher-vm prepare               # build the base for the current inputs
./scripts/launcher-vm start                 # restricted network; bounded SSH readiness
./scripts/launcher-vm start --provider-egress # live-API guest only, fresh overlay
./scripts/launcher-vm status                # JSON systemd state and limits
./scripts/launcher-vm exec uname -a          # stdout/stderr + original exit status
./scripts/launcher-vm put ./file /home/vm/file
./scripts/launcher-vm get /home/vm/result ./result
./scripts/launcher-vm logs                  # bounded journal and serial tail
./scripts/launcher-vm stop                  # guest shutdown, then bounded unit cleanup
./scripts/launcher-vm reset --discard       # stopped only; fresh disk AND UEFI variables
```

## Capped live OpenAI acceptance

`louiselm-qbr.5.1.3.13` is a maintainer-run check, never a CI gate. First create
a dedicated OpenAI API project and set a monthly spend limit of about $20 with
**Enforce a hard limit** enabled in Project settings → Limits → Spend. A spend
alert alone does not stop requests, and [OpenAI says hard-limit enforcement can
lag slightly](https://developers.openai.com/api/docs/guides/spend-limits). Create
a project-scoped key for this disposable check. Do not send the key to an Agent,
put it in an environment variable, or paste it into Beads.

Use a committed revision containing
`privileged_installed_brokered_stock_codex_real_openai`; a `git archive HEAD`
does not contain uncommitted edits. Start from a fresh overlay and inspect the
explicit guest-egress mode:

```sh
./scripts/launcher-vm status
./scripts/launcher-vm reset --discard
./scripts/launcher-vm plan --provider-egress
./scripts/launcher-vm start --provider-egress
git archive HEAD skills-core scripts/fetch-stock-codex-chain \
  docs/recovery-ceremony.md tests/fixtures/verified_posture_v1.json \
  tests/fixtures/provider_config.json tests/fixtures/preflight_v1.json \
  tests/fixtures/preflight_request_v2.json \
  tests/fixtures/broker_attention_projection.json \
  tests/fixtures/provider_metadata_disclosure.json |
  ./scripts/launcher-vm exec tar -x -C /home/vm
./scripts/launcher-vm exec python3 /home/vm/scripts/fetch-stock-codex-chain \
  /var/tmp/louiselm-stock-chain
# A failed build must stop here: the target dir can hold older binaries.
set -o pipefail
TEST_BIN=$(./scripts/launcher-vm exec env PATH=/home/vm/.cargo/bin:/usr/bin:/bin \
  CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=2 \
  CARGO_PROFILE_DEV_DEBUG=line-tables-only \
  CARGO_TARGET_DIR=/var/tmp/louiselm-skills-target \
  cargo test --manifest-path /home/vm/skills-core/Cargo.toml \
  --all-features --locked --no-run 2>&1 |
  sed -n 's/.*unittests src\/lib.rs (\(.*\))$/\1/p') && test -n "$TEST_BIN"
./scripts/launcher-vm exec sudo -n strip --strip-debug \
  /var/tmp/louiselm-skills-target/debug/louiselm-launch
./scripts/launcher-vm exec sudo -n env \
  LOUISELM_REQUIRE_BROKER_GUARD=1 \
  LOUISELM_STOCK_CODEX_DIR=/var/tmp/louiselm-stock-chain \
  timeout 180 unshare --net "$TEST_BIN" \
  launch_supervisor::system::installed_tests::guard::privileged_installed_brokered_stock_codex_completes_prompt \
  --exact --nocapture --test-threads=1
./scripts/launcher-vm exec getent ahostsv4 api.openai.com
```

The offline stock gate must pass before provisioning a key or making a paid
request. It also checks the installed guard and measured runtime. The stripped
debug launcher stays within the installed profile's 128 MiB file limit.
`TEST_BIN` is the library test binary cargo reports: the target dir also holds
the `main.rs` test binary and leftovers from earlier builds, so do not glob for it.
`start` refuses a base prepared from older provisioning inputs or
`Cargo.lock`, so this build does not meet a stale crate cache.

Choose one IPv4 literal from the last command for `OPENAI_IP` below. Before
provisioning the key, the maintainer opens `./scripts/launcher-vm terminal` in a
private, unrecorded terminal and runs the following inside the guest. The key is
typed at the silent prompt; it never appears in an argument, shell history or
the Agent's environment:

```sh
sudo -n bash -c 'umask 077; read -r -s -p "Project API key: " key </dev/tty; printf "\n" >/dev/tty; printf %s "$key" > /root/louiselm-live-openai.key; unset key'
```

Certification refuses the guest's initial network namespace, while the live
tests need egress. Create a separate namespace with NAT egress, and enter only
its network namespace with `nsenter`. `ip netns exec` also remounts `/sys` in a
new mount namespace, which hides the guard's BPF state (`GuardUnavailable`).
The namespace does not survive a guest restart; run this again after one.

```sh
./scripts/launcher-vm exec sudo -n env DEBIAN_FRONTEND=noninteractive \
  apt-get install -y -qq --no-install-recommends nftables
./scripts/launcher-vm exec sudo -n bash -c 'set -euo pipefail
ip netns add live
ip link add veth-host type veth peer name veth-live
ip link set veth-live netns live
ip addr add 10.200.0.1/30 dev veth-host; ip link set veth-host up
ip -n live addr add 10.200.0.2/30 dev veth-live
ip -n live link set veth-live up; ip -n live link set lo up
ip -n live route add default via 10.200.0.1
echo 1 > /proc/sys/net/ipv4/ip_forward
nft add table ip livenat
nft "add chain ip livenat post { type nat hook postrouting priority 100; }"
nft add rule ip livenat post ip saddr 10.200.0.0/30 oifname enp0s4 masquerade'
```

Run both ignored tests as guest root, replacing `OPENAI_IP` with the selected
literal. They refuse absent/unsafe key files and require both opt-in variables.
Their only upstream is `https://api.openai.com/v1/responses`, through the
broker's selected IP and TLS hostname.

- `privileged_installed_brokered_real_openai_refusals_precede_upstream` costs
  nothing. The raw fixture sender, not Codex, sends an above-ceiling effort and
  a Model outside the allowlist to the real-key broker. Both must be refused
  with `capability_denied` and zero recorded upstream attempts. Stock Codex may
  lower an unsupported effort before sending, so it cannot prove this refusal
  (louiselm-4j6lt).
- `privileged_installed_brokered_stock_codex_real_openai` runs two real Luna
  turns at low effort under a two-request Run limit. A third prompt must exhaust
  the Run, Park the Session and create a `run_parked` Attention item.

```sh
for test in privileged_installed_brokered_real_openai_refusals_precede_upstream \
  privileged_installed_brokered_stock_codex_real_openai; do
  ./scripts/launcher-vm exec sudo -n env \
    LOUISELM_REQUIRE_BROKER_GUARD=1 LOUISELM_REQUIRE_LIVE_OPENAI=1 \
    LOUISELM_STOCK_CODEX_DIR=/var/tmp/louiselm-stock-chain \
    LOUISELM_LIVE_OPENAI_IP=OPENAI_IP \
    nsenter --net=/run/netns/live timeout 180 "$TEST_BIN" \
    "launch_supervisor::system::installed_tests::guard::$test" \
    --exact --ignored --nocapture --test-threads=1 || break
done
```

Record the test's redacted markers, compiled revision, Node/Codex/adapter
versions and hashes, chosen IP, request count and typed denial/Park/Attention
outcomes on the issue. Never record key bytes, response content or ACP payloads.
The custody probes scan Session process state, files and broker records for the
key. A passing result is disposable-VM acceptance, not host installation.
Finally stop the VM, revoke the project key, and discard the credential-bearing
overlay. `reset --discard` archives the old overlay instead of erasing it; after
revocation, remove that exact archived disk by its printed path. Keep the prior
unrelated overlays untouched.

## Explicit recovery connections

Only after the maintainer authorizes temporary token access, identify the
connected YubiKey with `lsusb -d 1050:0407`. Use that exact BUS:DEVICE pair,
not a saved address from a previous boot/replug.

The base image's Debian cloud kernel has `CONFIG_USB_SUPPORT` disabled.
Recovery needs a separately approved USB-capable **guest** kernel; no host
kernel change is needed. The retained recovery VM was upgraded in
`louiselm-d5y8`, preserving its cloud kernel and a standalone disk/UEFI backup.
That does not change the prepared base: a fresh/reset VM still needs this check.
Inspect `uname -r` and the corresponding `/boot/config-*`; installation alone
does not prove the right kernel booted. In the retained VM, a guest GRUB drop-in
selects the USB-capable kernel because the retained cloud flavor otherwise sorts
first. Exact package authentication, selection and rollback evidence live in
`louiselm-d5y8`. Startup with `--yubikey` checks guest enumeration and stops the VM if
hardware is unavailable. SSH readiness alone is not enough.

```sh
./scripts/launcher-vm plan --yubikey 003:002   # inspect only, no device access
./scripts/launcher-vm start --yubikey 003:002  # stopped VM only
```

This exposes the **whole selected USB YubiKey**, including its FIDO, OTP and
CCID interfaces, to the guest until the VM stops. It does not limit access to
particular resident credentials. Use only the trusted disposable guest; create
fresh QA credentials with distinct unused resident user IDs. Never reset the
token, overwrite existing credentials, or use it simultaneously on the host.
This is native USB passthrough, not software-key emulation or an SSH agent.
The currently accepted device is 1050:0407; the wrapper pins bus/address and
vendor/product, requires existing device access, and never changes host
permissions. A replug or unavailable device is a reason to stop and recheck,
not to choose another device automatically. Requesting attachment on an already
running VM refuses. Afterward stop the VM explicitly; verify the token returns
to the host. Normal startup has no hardware attachment.

For the real ceremony, the **maintainer** opens a private, unrecorded local
terminal and runs:

```sh
./scripts/launcher-vm terminal
```

This opens an interactive SSH TTY as guest `vm`, with no agent forwarding and
the existing pinned host key. It does not run setup/signing commands. The TTY
check rejects pipes but cannot detect a terminal recorder: the operator must
ensure privacy. Never run this command through an Agent terminal for secrets.

When the installed tool prints a temporary `http://localhost:PORT/random-path/`
URL, the maintainer uses a **second private host terminal**:

```sh
./scripts/launcher-vm forward PORT  # substitute only the printed numeric port
```

Keep that foreground tunnel running while opening the exact URL in the trusted
host browser. It binds only 127.0.0.1 and forwards to the same guest loopback port,
preserving the WebAuthn origin. It fails if the local port is occupied; do not
substitute another port or expose it on the LAN. Ctrl-C closes it. Each new URL
may require a different port/tunnel, including the final confirmation page;
never paste the random path or browser challenge into chat. Both SSH helpers
are bounded to one hour, and do not hold the VM mutation lock. The original VM
deadline still applies; they cannot extend it. Guest egress remains restricted.

`exec` has a 15-minute deadline; transfers have five minutes. Effectful commands
take an exclusive nonblocking lock, so competing commands fail clearly;
`status` and `logs` remain available during preparation or execution.
`stop` is idempotent and does not delete state. `reset --discard` retains the
previous disk and firmware variables under a printed `discarded.*` directory;
these consume disk until explicitly removed. There is deliberately no recursive
cleanup command. The one-hour unit deadline is an emergency stop, not a clean
snapshot: stop the guest explicitly after work.

UEFI is intentional: on the maintainer's QEMU 10.0.11 / Ryzen AI host, this
image reset immediately through legacy BIOS with either host or qemu64 CPU.
OVMF with the host CPU booted normally. `-no-reboot` prevents silent reboot
loops. Firmware code is read-only; guest-writable variables are private copies.

## Run existing checks

Copy only committed crate sources, not the host checkout, `.git`, credentials,
or a writable shared mount. For uncommitted changes, explicitly select the
files to transfer; do not assume this archive contains them.

```sh
git archive HEAD skills-core docs/recovery-ceremony.md \
  tests/fixtures/verified_posture_v1.json tests/fixtures/provider_config.json \
  tests/fixtures/preflight_v1.json tests/fixtures/preflight_request_v2.json \
  tests/fixtures/broker_attention_projection.json |
  ./scripts/launcher-vm exec tar -x -C /home/vm
./scripts/launcher-vm exec env \
  PATH=/home/vm/.cargo/bin:/usr/bin:/bin \
  CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=2 \
  CARGO_PROFILE_DEV_DEBUG=line-tables-only \
  CARGO_TARGET_DIR=/var/tmp/louiselm-skills-target \
  cargo test --manifest-path /home/vm/skills-core/Cargo.toml --all-features --locked
```

`prepare` copies committed `skills-core` sources, their shared JSON fixtures,
and the recovery guide included by the onboarding test. It fetches locked
dependencies and compiles tests without running them.
Builds use the same line-table debug metadata as the skills-core CI gate.
The clean baseline thus retains dependency and build caches across resets.
Normal builds are offline; do not silently enable guest egress.

The base is named by its provisioning inputs: `prepared-<id>.qcow2`, where the
id hashes the Debian image checksum, the package list, the Rust toolchain and
`Cargo.lock` at `HEAD`. `plan` prints the current `base_image`. After any of
those inputs change:

```sh
./scripts/launcher-vm prepare         # builds the new base; the old one stays
./scripts/launcher-vm reset --discard # archives the old overlay, new one on the new base
```

Until then `start` and `reset` refuse with that instruction. `start` also
refuses an overlay built on another base, rather than failing later inside the
guest. A base is never overwritten or deleted by the script: archived overlays
(`discarded.*`) and retained disks keep their backing file. Delete an old base
by hand only after checking with `qemu-img info` that no kept overlay uses it
(louiselm-6y1ee).

The launcher VM has one shared systemd unit and SSH port, so do not prepare
while another launcher VM is active or in use.

Existing privileged CI recipes are in `.github/workflows/ci.yml`, under
`cargo (skills-core)`. Execute them **inside the guest**. Keep build artifacts
under `/var/tmp/louiselm-skills-target`, mode `0755`, so the assigned test UID
can traverse them (louiselm-4p9v).

The privileged registry step runs the entire registry integration binary with
`LOUISELM_REQUIRE_ROOT_REGISTRY=1`. It rejects missing root/initial-user-namespace
authority instead of skipping. Runtime trees must contain only root-owned
regular files and directories, without group/other write permission; even
internal symlinks, FIFOs and sockets are refused. Unlisted immutable files are
supported, but their contents are not added to the existing measurement format.

The production relay step runs the library tests selected by
`launch_supervisor::system::relay_tests` with `LOUISELM_REQUIRE_SYSTEM_RELAY=1`.
Build `louiselm-launch` first, as in CI: the fixture requires its real bootstrap
and Bubblewrap, and cannot silently skip when the flag is set. It proves
quiescence and direct Disposal close controller I/O after a positive-control
echo, with the controller left open. These NamespaceOnly fixtures do not prove
assigned outer identity or end-to-end Verified posture.

The privileged supervisor composition runs three scenarios through the production
relay: an Agent exits successfully after echo while its controller remains open;
and a controller closes stdin, with the fake broker acknowledging the receipt
chain and settling any controller-loss Park before Disposal; and, after a
positive-control echo, the controller closes its output read half while keeping
input open. The resulting real relay write failure must produce a causal
`relay_failed` terminal receipt before returning its error. All three check
actual outer credentials. The ordinary kernel-signal regression separately proves
handoff survival and retained parent-death protection, without root or unsafe code.

Prove initial-user-namespace identity with
`LOUISELM_REQUIRE_INITIAL_HOST_IDENTITY=1`; a passing test that printed a
skip is not conformance evidence. Fake-broker tests do not prove the installed
real-broker ceremony. The remaining broker work is `louiselm-qbr.5.1.1`.

The nonprivileged wrapper contract is checked by
`./scripts/test-launcher-vm.sh`; it executes no VM, download, or sudo command.

## Embedded Sender guard

The `Sender guard object and loader (disposable KVM)` CI job prepares this same pinned
guest and runs both gates below. Fresh preparation includes `clang`,
`linux-libc-dev`, `libbpf-dev`, `libelf-dev`, Python 3 and `iproute2`; a base
prepared from an older package list is refused until the next `prepare`.

```sh
./scripts/launcher-vm start
git archive HEAD scripts/test-sender-guard.py scripts/test-sender-guard-loader.py scripts/test-sender-guard-handoff.py scripts/probes/codex-kernel-guard \
  skills-core/src/launch_supervisor/sender_guard/binding.bpf.c skills-core/src/launch_supervisor/sender_guard/lifecycle.bpf.c |
  ./scripts/launcher-vm exec tar -x -C /var/tmp
./scripts/launcher-vm exec python3 /var/tmp/scripts/test-sender-guard.py \
  --disposable-vm /var/tmp/louiselm-skills-target/debug/louiselm-launch
./scripts/launcher-vm exec sudo -n python3 /var/tmp/scripts/test-sender-guard-loader.py \
  /var/tmp/louiselm-skills-target/debug/deps
./scripts/launcher-vm exec sudo -n python3 /var/tmp/scripts/test-sender-guard-handoff.py \
  /var/tmp/louiselm-skills-target/debug/deps /var/tmp/louiselm-skills-target/debug/deps
./scripts/launcher-vm stop
```

Preparation builds the launcher from the archived crate. Transfer and rebuild
explicitly selected changed sources when testing uncommitted work. The gate
extracts the object from that launcher without privilege, checks its six
programs, eight maps and BTF data, then loads those bytes using the existing
ownership probe and system libbpf inside the guest. It also compiles the
binding-only missing-owner-hook negative control from the shared source.

All four ownership variants are required: owner death at the actual upstream
write, death before admission, owner exec with orderly close, and broker crash.
They retain helper-denial positive controls, protected pins/frozen maps and
confirmed cleanup. The second gate invokes the production Rust loader through
the library-test executable, with distinct-UID broker/runtime fixtures and real
authenticated enrollment responses. It checks two Sessions sharing an upstream,
helper denials, runtime/supervisor exit and exec, broker crash, revision,
connection retirement and disposal. A cached admission cannot cross loss at the
actual write. All owned map IDs must disappear after cleanup. A directory
argument requires exactly one library-test executable; an exact path can be
used for transferred local builds.

Production Provider activation remains separate; a passing result is component
evidence, not installed conformance or a Verified Session. The host needs no
root access, account key or Provider
connection. Missing KVM/BPF-LSM/BTF or failed probes fail the gate, never skip.
