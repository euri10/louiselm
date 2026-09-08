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
./scripts/launcher-vm prepare               # one-time download and guest provisioning
./scripts/launcher-vm start                 # restricted network; bounded SSH readiness
./scripts/launcher-vm status                # JSON systemd state and limits
./scripts/launcher-vm exec uname -a          # stdout/stderr + original exit status
./scripts/launcher-vm put ./file /home/vm/file
./scripts/launcher-vm get /home/vm/result ./result
./scripts/launcher-vm logs                  # bounded journal and serial tail
./scripts/launcher-vm stop                  # guest shutdown, then bounded unit cleanup
./scripts/launcher-vm reset --discard       # stopped only; fresh disk AND UEFI variables
```

## Explicit recovery connections

Only after the maintainer authorizes temporary token access, identify the
connected YubiKey with `lsusb -d 1050:0407`. Use that exact BUS:DEVICE pair,
not a saved address from a previous boot/replug.

The prepared Debian cloud kernel currently has `CONFIG_USB_SUPPORT` disabled.
Recovery needs a separately approved USB-capable **guest** kernel first; no
host kernel change is needed. Startup with `--yubikey` checks guest enumeration
and stops the VM if hardware is unavailable. SSH readiness alone is not enough.

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
git archive HEAD skills-core tests/fixtures/verified_posture_v1.json |
  ./scripts/launcher-vm exec tar -x -C /home/vm
./scripts/launcher-vm exec env \
  PATH=/home/vm/.cargo/bin:/usr/bin:/bin \
  CARGO_TARGET_DIR=/var/tmp/louiselm-skills-target \
  cargo test --manifest-path /home/vm/skills-core/Cargo.toml --all-features --locked
```

`prepare` copies committed `skills-core` sources and their one shared posture
fixture, fetches locked dependencies, and compiles tests without running them.
The clean baseline thus retains dependency and build caches across resets.
Normal builds are offline. A changed lockfile needs a new baseline or an explicit
transfer of its dependency cache; do not silently enable guest egress.
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
