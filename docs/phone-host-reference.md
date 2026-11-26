# Phone host reference candidate

The administrator provisioner in `scripts/phone-host.py` installs a retained
catalog and a systemd boot refresh for one capture state directory and one
receiver public key. This is an **incomplete prerequisite** for phone Send and
recovery. `check` always returns `unavailable`, including when every record
matches. Do not use its files, a successful boot unit, or a free socket as
permission to initialize stores, register a receiver, recover, or send.

The supported design needs proof that the files and network namespace came from
the actual host despite a caller-controlled mount view, that the root is local
and cannot be used by a second host, and that no older or nonparticipating
registrar can retain authority. These proofs are not supplied by this candidate.
Device and inode numbers, root ownership, the current boot ID, and a matching
namespace number are useful diagnostics; none authenticates that full scope.
There is also no proof of a genuinely first receiver, no safe root/key change
procedure, and no external witness for a complete consistent rollback. An
established or ambiguous receiver must remain unavailable until its existing
recovery contract is satisfied. A reboot does not prove native no-effect.

For a **disposable Linux/systemd fixture**, a trusted administrator can exercise
the real installation using an existing absolute capture state directory and a
lowercase SHA-256 fingerprint of the receiver's DER SubjectPublicKeyInfo:

```sh
sudo /usr/bin/python3 -I scripts/phone-host.py install /absolute/capture/state/directory RECEIVER_SHA256
/usr/bin/python3 -I /usr/local/lib/louiselm-phone-host/phone-host.py check /absolute/capture/state/directory RECEIVER_SHA256
```

The selected directory is the `louiselm/capture` directory below the effective
`LOUISELM_CAPTURE_STATE_DIR` or `XDG_STATE_HOME` root, not the data directory
holding captured audio. The candidate accepts aliases that open the same device
and inode. It does not create or publish a receiver key; the existing key
creation path is not a proof of safe first enrollment. There is not yet a
production setup procedure that can establish the required pre-publication
proof. These instructions are for fixture verification, not enabling Send on a
maintainer host.

The installer publishes a read-only program and systemd unit, then a separate
read-only catalog under `/var/lib/louiselm-phone-host`. It enables and starts
`louiselm-phone-host.service`; the root oneshot compares its network namespace
with PID 1's, then publishes a read-only boot reference under
`/run/louiselm-phone-host`. The service needs `CAP_SYS_PTRACE` to inspect PID 1's
namespace on the tested kernel. It runs with a read-only system, no private
network or user namespace, and no other capability. A conflicting existing
program, unit, catalog, or boot reference is preserved and causes refusal;
installing a different root, key, or program is not implemented. A failed
installation may have installed some of those files and must not be treated as
successful provisioning. The service startup order alone does not prevent a
manual or user-service receiver from starting first.

`check` reads the fixed locations, opens the supplied directory, and returns one
JSON line with `version: 1`, `status: "unavailable"`, and a `reason` such as
`catalog_missing`, `reference_missing`, `root_mismatch`, `receiver_mismatch`,
`boot_mismatch`, `namespace_mismatch`, or `scope_unverified`. Its exit status is
2. This is a refusal/diagnostic interface, not a validated-scope token; the
opened directory is not retained across the call. Pair, serve, owner helpers,
protected writes, and recovery must not infer authority from its output. Those
consumers still need a single authenticated, continuously held scope and their
own store, exclusion, and recovery proofs. Capture-only use and unrelated reads
do not call this candidate and remain available under their existing rules.

Run `python3 scripts/test-phone-host.py` for the unprivileged filesystem
fixtures. Installation, ordinary operator access, service restart, and actual
reboot require an isolated disposable systemd guest. These checks do not prove
host authentication or old-registrar exclusion.
