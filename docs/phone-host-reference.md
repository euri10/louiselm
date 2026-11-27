# Phone host reference

The administrator provisioner registers one capture state directory and one
receiver public-key fingerprint. On supported storage, its boot service reopens
the registered directory by a persistent kernel file handle and exposes that
open directory to the system manager. This guide covers managed receiver
registration and recovery. For Android Send on a trusted personal desktop,
follow [Send from a personal desktop](../capture-service/README.md#send-from-a-personal-desktop).

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

For a new managed candidate, the administrator may instead run the separate
first-install command before any receiver identity is published:

```sh
sudo /usr/bin/python3 -I scripts/phone-host.py install-first \
  /absolute/capture/state/directory --operator OPERATOR_LOGIN \
  --capture-binary /absolute/path/to/louiselm-capture \
  --writer-binary /absolute/trusted/path/to/louiselm-phone-writer
```

The writer must be a regular administrator-owned executable with no group or
world write access, under administrator-owned non-writable directories. The
installer copies its exact bytes into a fixed root-owned location; it never
executes the operator-owned capture binary as root. It records the opened
capture directory, its actual parent, the operator UID/GID, and private staging
directories on the destination filesystems. Each staging directory is created
under a root-owned, non-writable ext4 host mount and remains mode 0700. A
filesystem without such a verified mount is unavailable for this setup. The
candidate TLS key and certificate are generated under separately protected
mode-0700 host state, and repeat setup accepts only the exact retained binding
and candidate. A known existing receiver or conflicting catalog follows
explicit recovery; this command does not adopt or replace it. An interrupted
candidate whose exact state cannot be verified remains unavailable.

This first-install path prepares a real private candidate and installs the
on-demand writer, but it does not yet establish historical first origin. Until
the independent native authority and complete old-registrar proof exist, the
registration request returns unavailable and the fixed store transaction is
not invoked. Ordinary capture-only behavior remains available; if it creates
or publishes a different receiver in the meantime, that installation cannot
later be assumed fresh. Send through this managed registration path stays
unavailable.

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

For a new managed catalog, the boot publisher also reopens and holds the
original capture parent and the root-private staging directory for each
destination filesystem. It requires the parent's direct `capture` child to
remain the registered object; aliases of that same object work, while a moved
or reparented root requires the separately authorized changed-root recovery.
The on-demand registration unit receives these directories, the private
authority directory, catalog, reference, host procfs and host network namespace
as manager-opened descriptors. Its socket accepts one `R` byte with exactly one
opened capture-root descriptor. The closed v1 response is `committed` with the
public fingerprint, `unavailable`, or `root_mismatch`; it is a description of
the fixed operation, never a transferable permit. The current implementation
cannot produce `committed` until independent first-origin proof is available.

`phone-host.py check ROOT RECEIVER_SHA256` is an unprivileged diagnostic. It
always returns JSON `status: "unavailable"` and exit status 2, including for a
matching root (`scope_unverified`). Missing or invalid records, a different
root, and an unavailable root have distinct refusal reasons. It does not hold a
directory open and is not the managed consumer interface. Ordinary capture and
healthy unrelated reads retain their own existing behavior when setup is absent.

## Conditional receiver exclusion

`phone_control::barrier::namespace_exclusion` provides two fixed Linux abstract
socket names in the calling network namespace. `Startup::try_acquire()` takes
the short startup exclusion; `Startup::registrar()` takes the separate receiver
lease while startup is held. Both are nonblocking. Keep the registrar lease
until the receiver and every delegated registrar have lost the ability to
register gates. A stopped process or a surviving socket descriptor keeps the
name occupied. Contention means unavailable, not permission to replace it.

The existing receiver barrier acquires both names before opening its bound
registrar and keeps the registrar lease until that barrier is dropped. Offline
cutoff holds both through its durable operation. When receiver acquisition
fails, the service retains its existing read-only fallback. This is consumed by
the real receiver and offline cutoff, but pair, first publication, protected
store updates, helpers and recovery do not yet all use the startup exclusion.
Future users must acquire startup before the generation barrier and store locks,
must not reenter it or hold it across an async await, and must retain or retry
valid owner phase reports independently of archive health.

These names serialize only cooperating processes in the **same** network
namespace. They do not authenticate that namespace, the manager, root, receiver,
boot or callers. They do not prevent a legacy or foreign-namespace registrar,
prove that the old receiver died or an action had no effect, establish a fresh
installation, or authorize initialization, recovery or Send. The held-root
pairing read above is not a bearer token for these claims either.

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
