# Phone host reference

The administrator provisioner registers one capture state directory and one
receiver public-key fingerprint. On supported storage, its boot service reopens
the registered directory by a persistent kernel file handle and exposes that
open directory to the system manager. This is a prerequisite for phone Send,
not permission to send or recover. Send remains disabled.

## Provisioning and limits

The current implementation requires Linux, systemd, Python 3, glibc, util-linux
(`blkid` and `setpriv`), and a mounted, block-backed ext4 filesystem with a
nonzero UUID, a `/dev/disk/by-uuid` entry and reopenable file handles. It
refuses missing, stale, unsupported or ambiguous identities; two mounted ext4
devices with the same UUID are ambiguous. Mount IDs and device/inode numbers
are not retained as cross-boot identity. The UUID locates a mounted filesystem;
the opaque handle identifies the directory within it. Registration verifies
that reopening the handle gives the same directory while both are held open.

For a disposable Linux/systemd fixture, an administrator installs the host
reference and the on-demand pairing-status service with:

```sh
sudo /usr/bin/python3 -I scripts/phone-host.py install \
  /absolute/capture/state/directory RECEIVER_SHA256 \
  --operator OPERATOR_LOGIN --capture-binary /absolute/path/to/louiselm-capture
```

The selected directory is `louiselm/capture` below the effective
`LOUISELM_CAPTURE_STATE_DIR` or `XDG_STATE_HOME`, not the captured-audio data
directory. The fingerprint is the lowercase SHA-256 of the receiver's DER
SubjectPublicKeyInfo. The operator must be a non-root account. The executable
and every canonical parent must be owned by root or that operator and must not
be group- or world-writable; the operator must be able to execute it. The
canonical executable path must also be representable safely in a systemd unit.
The installer reads the shipped service templates, installs their exact bytes
and the rendered units, then enables the boot unit and on-demand socket. It
refuses conflicting installed files; replacement of a root, receiver, program,
or unit is not an automatic migration or recovery procedure.

## Held directory interface

The root `louiselm-phone-host.service` runs after local filesystems are mounted.
It validates the catalog, checks that its mount and network namespaces match
PID 1, reopens the catalog's filesystem and directory handle, and bind-mounts
that open directory at `/run/louiselm-phone-host/root`. It verifies the mount
against the still-open directory before publishing the current-boot reference.
On restart it accepts an existing mount only when it is the same open object.
The mount remains pinned after the oneshot exits; a renamed source directory
does not make the service reopen an unrelated pathname.

The boot publisher needs `CAP_DAC_READ_SEARCH` to reopen the handle,
`CAP_SYS_PTRACE` for PID 1 namespace inspection on the tested kernel, and
`CAP_SYS_ADMIN` to publish the bind mount in PID 1's mount namespace. It must
not run in a private mount namespace. These privileges are limited to the
administrator-installed boot oneshot; no permanently running root helper is
required. If the filesystem is not mounted when it runs, or any check fails,
the service fails and managed phone operations depending on it are unavailable.

The on-demand pairing unit requires and starts after the boot unit. PID 1 opens
`/run/louiselm-phone-host/root` and passes that descriptor to the unprivileged
capture operation, which retains it for its whole operation. A caller's root
or alias must open the same directory, and actual store I/O must use the held
directory. The manager-launched process and inherited descriptor establish the
handoff; a path, environment variable, or descriptor number supplied by an
arbitrary process does not. Pairing data returned by the query is not a scope
token and must not be used to authorize phone actions.

`phone-host.py check ROOT RECEIVER_SHA256` is an unprivileged diagnostic. It
always returns JSON `status: "unavailable"` and exit status 2, including for a
matching root (`scope_unverified`). Missing or invalid records, a different
root, and an unavailable root have distinct refusal reasons. It does not hold a
directory open and is not the managed consumer interface. Ordinary capture and
healthy unrelated reads retain their own existing behavior when setup is absent.

## Authority still unavailable

A filesystem UUID and file handle are not an external witness: a complete
filesystem clone or consistent rollback can retain both. The present checks do
not prove that block storage is local-only, exclude a second host or old
registrars, establish a genuinely first receiver, authenticate every manual or
user-service operation, or recover old action evidence. Root ownership and a
matching boot or network-namespace number do not fill those gaps. A known
restore and any established or ambiguous receiver remain unavailable until the
existing recovery contract is satisfied. A reboot does not prove native
no-effect, and the provisioner does not create or publish a receiver key.

Run `python3 scripts/test-phone-host.py` for the unprivileged fixture suite.
The actual provisioning, manager handoff and reboot must also be exercised in a
disposable systemd guest. These checks do not authorize Send.
