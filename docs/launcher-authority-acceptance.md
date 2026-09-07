# Installed launcher authority acceptance

`louiselm-d6fv.4.9` passed on 2026-09-07 in the disposable VM. Session:
`codex/01a07c88-7c3d-7150-b6cd-f169d26e07c5`.

This establishes the installed launcher, sudo, signing-key storage, and identity
lease boundaries. The admitted `run` invocation stopped at **Control broker
unavailable**. It did not start a Session or produce a runtime receipt.
The real Control broker remains `louiselm-qbr.5.1.1`; end-to-end Verified posture
remains `louiselm-d6fv.9`. This record does not close `louiselm-d6fv.4`.

## Exact subject

- Signed release: `sha256:36f55a317f0cda9cd867db91a6ad423e1d379f22215768699b555b225bedfaaf`.
- Source: `816563dc9bc37017ff86002a7576f9d8e5abd3d9`; lockfile SHA-256
  `d94f6f26ca37b2dde28a74befc34ea81a9b0df2bf64dd84a07a40eb88e88fad1`.
- Launcher SHA-256: `e9944b1e04b143ead061c0fc0124f85d7856d2d088d2bef8d150d0cb3b29370f`.
- Installed prefix: `/usr/local/lib/louiselm`; signature verification used the
  retained `/var/lib/louiselm-lm70-fix-91qF9s` public provisional trust store.
  This is the previously accepted `louiselm-lm70` release, not production
  recovery enrollment or a newly signed build.
- Guest: Debian 13, kernel `6.12.107+deb13-cloud-amd64`, Bubblewrap `0.12.0`,
  sudo `1.9.16p2`, root in the initial guest user namespace.
- Dedicated accounts: operator UID/GID `1501`, broker UID/GID `1500`, no
  supplementary groups or general sudo grants. Four pool slots start at
  UID `2000000` and GID `3000000`.

## Observations

| Check | Observed result |
| --- | --- |
| Genuine installed release | Installed `release verify`, `identity`, and `status` succeeded; expected source/digests, root ownership, and no operator write authority. |
| Conformance prerequisite | All 45 hostile checks, fork/Park/Resume/Interrupt/Disposal, six registry tests, and the three-scenario production relay composition passed without skips. |
| Installer and modes | Installed CLI created trusted authority with no failures. State `0711`, public keyring `0444`, sudo fragment `0440`, config/private keys `0600`, private and lock directories `0700`; all root-owned. |
| Reservation | Exactly one numeric-owner `0` reservation per subordinate-ID ledger; no overlap, existing accounts, or group collisions in the selected pools. |
| Sudo policy | Whole-policy `visudo -c` and exact fragment validation passed. Numeric operator UID, root UID/GID, SHA-256, fixed path, sole `run`, `NOSETENV`, and `fdexec=digest_only` matched. |
| Allowed command | Actual operator `sudo -n .../louiselm-launch run` reached the launcher's explicit `Control broker unavailable` failure with empty stdout. An empty root-owned registry was a fixture; the request's Agent/authorization identifiers were deliberately unregistered. |
| Rejected commands | Extra/missing arguments, `/bin/sh`, wrong account, copied launcher path, wrong valid SHA-256, explicit environment assignment, and `-E` all failed inside sudo before launcher execution. |
| Reinstall and rotation | Reinstall preserved the active key, sudo rule, and reservations. Rotation created one new key; repeating its ID returned the same key without creating another. Both public and private key records remained. |
| Key disclosure | Operator could read the public keyring; neither operator nor broker could read either private key. Status/rotation/public output contained no private-key material. |
| Installed leases | A helper linked to frozen source used `LauncherPaths::system()`. A second process could not acquire held slot 0 but could acquire/release slot 1. Installed CLI reinstall preserved the occupied lock inode. Explicit release allowed another process to acquire slot 0; out-of-range slot 4 failed. Final occupancy was empty. No workload was spawned under these leases. |
| Executable tamper | Exactly one final byte changed, preserving size/owner/mode. The unchanged sudo rule refused execution; the intact installed skills binary returned exit 2 and `release_tampered`. |
| Rollback | Stopped guest; retained tested disk and firmware; restored the complete original snapshot byte-identically. After reboot, signature/identity/status, launcher hash, release state, and protected public trust matched. Test accounts, authority, reservations, registry, and guest artifacts were absent. Guest stopped again. |

The offline guest cache predates recovery dependencies and could not resolve
`bip39`. Instead, the exact frozen release sources were compiled offline on the
host with Rust 1.97.1. The three test executables and their development
bootstrap were transferred without changing the installed release. The required invocations in
[`scripts/launcher-conformance`](../scripts/launcher-conformance) ran inside the
guest. Its expected compile-time bootstrap path was recreated inside the guest;
no host mount or credential was exposed. These executables were test fixtures,
not installed release components.

## Recheckable evidence

Local evidence root: `/home/lotso/.cache/louiselm-d6fv49.gdFpgw`.
Beads comments retain this location and the acceptance summary.

- `evidence.tar.gz`: stdout/stderr, structured status, public keyring, modes,
  reservations, conformance and lease logs. SHA-256:
  `94bc40e07f5afe60f14695f3cc23957c1983cb93c872765f7cdea7dde68b4edb`.
- `install-checks.sh`, `sudo-checks.sh`, `tamper-check.sh`: exact assertions and
  commands for the installed boundary. They guard the guest hostname and pin
  the tested release or executable digest.
- `installed_lease.rs` and `installed-lease`: the bounded cross-process probe
  against the installed configuration; `conformance-prebuilt.sh` records the
  required conformance invocations.
- `verify-rollback.sh`, `rollback-verification.log`, `final-vm-status.json`:
  restored signed release and final `inactive/dead`, `MainPID=0` evidence.

The original snapshot is
`/home/lotso/.cache/louiselm-launcher-vm/d6fv49-before.bc8JmJ`; the tested,
tampered snapshot is retained at the sibling `d6fv49-after.aAfY0T`.
Both include disk and UEFI variables. Do not boot the tampered snapshot as a
trusted release. No immutable executable was repaired in place.

For a repeat, follow the [VM boundary](launcher-vm.md) and the
[manual installer recipe](../skills-core/README.md#manual-launcher-authority-acceptance),
using a fresh disposable copy of the original snapshot and reviewing the exact
artifact paths first. The captured scripts mutate guest accounts, trust state,
sudo policy, and executable bytes; they are not desktop checks.

No product code changed in this acceptance round. No new hardware signing,
token passthrough, guest egress, host sudo, or real-broker authorization occurred.
