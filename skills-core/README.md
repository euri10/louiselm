# LouiseLM skills core

`louiselm-skills` is the trusted tool that turns an untrusted Skill candidate
into an immutable, content-addressed package, inspects every byte of it
deterministically, and renders the canonical Dossier a reviewer approves. It is
the only writer of immutable packages.

It decides nothing about whether a skill is safe. It decides what a reviewer is
shown, and it guarantees that the bytes shown are the bytes that were captured.

## Commands

```text
package <candidate-dir> [--captured-at MS]
verify <digest>
inspect <digest>
dossier <digest> [--against DIGEST] [--review-depth DEPTH]
                 [--assessment-model M --assessment-prompt P]
list
policy [--digest]

trust bootstrap --primary PUBLIC_KEY --release PUBLIC_KEY [--require-hardware]
trust show
trust reset --confirm  # development stores only

recovery setup --store PATH --primary PRIVATE_KEY --release PRIVATE_KEY
recovery status --store PATH
recovery change --store PATH --via primary|release|paper|passkey
                [--authorizer PRIVATE_KEY] [--primary NEW_PRIVATE_KEY]
                [--release NEW_PRIVATE_KEY] [--paper replace] [--passkey replace]
recovery reset --store PATH

generation admit --member DIGEST[:DEPTH] ... --key PRIVKEY
generation witness DIGEST --remote URL [--branch B]
generation activate DIGEST
generation status | list

quarantine exclude DIGEST... --reason TEXT
quarantine all --reason TEXT
quarantine show
```

Except for local-only `recovery`, commands accept `--store DIR`,
`--policy FILE --policy-digest D`, and `--robot-json`. Recovery requires an
explicit store; `status` returns public JSON, while setup/change/reset require
the trusted local foreground terminal and refuse robot mode.

Exit status is part of the contract, so an unattended caller never has to parse
prose:

| status | meaning |
| ------ | ------- |
| `0` | succeeded; the subject is admissible |
| `1` | failed; no success claimed; persistence errors may follow atomic publication |
| `2` | succeeded; the subject is **not** admissible — verification failed, or Inspection produced a fatal finding |

```sh
digest=$(louiselm-skills package ~/candidates/my-skill --robot-json | jq -r .digest)
louiselm-skills dossier "$digest" --review-depth read
```

## What is guaranteed

**A digest names one sequence of bytes.** The package digest is the SHA-256 of
the canonical manifest, and the manifest is the sorted list of every file's
path, executable bit, size, and content hash. Modification times, ownership,
permission bits beyond `executable`, empty directories, symlinks, and the
location the tree was captured from never enter it — so the same candidate
captured anywhere produces the same digest, and no local fact can change one.

**Capture fails closed.** A tree it cannot describe unambiguously produces no
package at all: device nodes, sockets, and FIFOs; files with more than one hard
link; directory cycles through symlinks; paths that collide once a filesystem
normalizes them; non-ASCII paths, unless a pinned policy admits them; files that
changed while being read; anything over a policy limit.

**Symlinks are resolved, not preserved.** A link's content is copied in as a
regular read-only file, so a later edit to the link or its target cannot change
bytes that were already reviewed. Where the link pointed is a local fact with no
portable meaning, so it lives in Supply lineage, outside the package, and the
Dossier flags every origin that reached outside the candidate root.

**Nothing recorded is trusted.** `verify` re-reads and re-hashes every file
against the manifest, and building a Dossier always verifies, re-inspects, and
re-derives the diff first. A recorded digest is a claim to be checked. An
Agent-produced digest, preview, or analysis is evidence only.

**The rules are content-addressed.** The Inspection policy and Unicode profile
are compiled into the binary, and every finding reports the exact policy digest
that produced it. A replacement policy is accepted only when the caller states
the digest it expects (`--policy` requires `--policy-digest`), which is what
stops a synced dotfile or an Agent-written file from silently widening or
weakening Inspection.

**Hostile bytes cannot act.** Every value that reaches a reviewer or an Agent is
escaped where it is produced, not where it is printed, so the human render and
the robot view carry the same escaped facts. Homoglyphs and hidden characters
are escaped too: showing a zero-width space as itself would hide the finding
inside the report of it.

## Fatal versus findings

Inspection produces two kinds of output.

_Fatal_ means the package cannot be reviewed at all. The class is deliberately
tiny: no `SKILL.md` at the root, `SKILL.md` without usable frontmatter, or a
file that is neither binary nor valid UTF-8. Growing this class moves judgement
from the reviewer to a scanner that cannot read prose.

Everything else is a _mandatory Dossier finding_ — a fact the reviewer must be
shown, never a verdict: hidden and bidirectional code points, ASCII homoglyphs,
terminal control sequences, URLs, credential references, encoded payloads and
decoders, network and process reach, SVG that acts rather than draws, declared
and undeclared binaries, executables, images, content contradicting its
extension, and any file the scan budget could not cover in full.

## Assessment

Assessment is a Model's advisory opinion and has no authority. It runs under an
empty capability envelope — an assessor offered any capability is refused before
it is called — and it is keyed to the exact package digest, Model, and prompt
version. An opinion about anything else is treated as absent, not as stale,
because showing a reviewer an opinion about different bytes is worse than
showing none.

## Skill Admission

A package that verifies is not a package anyone approved. Skill Admission is
the local ceremony that turns reviewed packages into a **Skill Generation**: one
signed record binding the complete admitted set, the Dossier each member was
approved from, the claimed review depth, the governing policy, the Provider view
roots, and its place in a chain — sequence and predecessor. The set is admitted
as a whole, with one touch, so addition, deletion, replacement, policy change,
and rollback are all visible as changes to a signed record.

One physical YubiKey holds distinct **Primary** and **Release** credentials.
Primary signs Admissions; Release authorizes trusted builds. A backed-up passkey
and a written paper phrase independently authorize recovery changes, never
ordinary Admissions or releases. There is no separate SSH Recovery role.

Follow the [one-token setup and recovery ceremony](../docs/recovery-ceremony.md)
for provisional first-release signing, atomic installed setup, method/key
replacement, readiness and last-resort reset. Production setup requires both
recovery methods and strict hardware presence/verification on both signing
roles. Development stores cannot be promoted to production authority.

Normal Admission and `release sign` record exact approved payload digests under
the trust mutation lock. Retired keys verify only that recorded history, never
new or backdated approvals. Signing outside `release sign` does not register a
release for verification after its signing key is retired. Losing all usable
authority requires explicit `recovery reset`, fresh setup and re-Admission.

A signed Generation governs nothing until it is **witnessed**. Its exact bytes
are published to a protected Git branch and read back from the remote before it
can be activated, so a Generation only takes effect once it exists somewhere the
operator does not solely control. The witness ledger is append-only per
Generation: a digest already published with different bytes is refused, never
overwritten. During a witness outage nothing changes, and the previous
Generation stays in force. Activation refuses an older sequence or a different
Generation at the current sequence, so intentional rollback requires a newly
admitted higher sequence. Each lineage pin records activation time. Retrying the
exact current digest confirms activation without appending another pin or
changing its original timestamp.

Activation serializes its Generation records and Supply lineage under the trust
lock. It writes and syncs `activation.pending.json` before replacing any of them;
ordinary failures restore the previous state. An interrupted activation rolls back
before the next `generation status`, `list`, or other Admission operation reads
the store. These APIs report a busy store instead of exposing an in-flight
transaction; raw files are not a committed-state inspection API.

If rollback cannot finish, repair the reported filesystem problem and retry the
operation; retain the journal, which contains the recovery evidence. Once every
new file is durable, journal removal commits the activation. Failure to sync that
removal reports **uncertain commit durability**: retry activation of the same
digest to settle the outcome without creating another pin. A crash in this final
window can recover the complete old or complete new state. Process-crash tests
cover publication boundaries; they do not emulate physical storage failure.

The commit to the witness branch is an ordinary commit. Branch protection on the
remote is the control; a second hardware signature there would cost another
touch and prove nothing the first one did not.

**Emergency quarantine** narrows authority immediately and needs no token:
excluded packages drop out of the current Generation the moment the file is
written. It only ever narrows. Giving authority back requires a newly admitted
Generation, so `quarantine clear` refuses by design rather than becoming a way
to re-enable quarantined supply without a touch.

### What v1 trusts

The ceremony trusts the kernel, the root-owned `louiselm-skills` binary, and the
local TTY. Hardware attestation bytes may be recorded alongside an enrolled key,
but nothing here validates a manufacturer certificate chain, so the record says
`validated: false` and the bytes are evidence only — never proof that a key is
genuine hardware.

Until louiselm-d6fv.7 installs root-owned binaries and protected trust data, the
trust store and Generation records live in the same store an operator can write.
That is the gap that release makes real; it is not closed here.

### The manual ceremony

Automated tests cover the chain, the state machine, the witness protocol, and
signature verification, using software keys. They cannot cover a physical touch.
Complete the linked one-token ceremony first, then use the installed tool and
protected production store for Admission. Replace digest/remote placeholders:

```sh
skills=/usr/local/lib/louiselm/current/bin/louiselm-skills
production_store=/var/lib/louiselm/skills
sudo "$skills" generation admit --store "$production_store" \
  --member 'sha256:<pkg>:read' --key "$HOME/.ssh/id_louiselm_primary"
sudo "$skills" generation witness --store "$production_store" \
  'sha256:<generation>' --remote git@your.host:infra/skill-witness.git
sudo "$skills" generation activate --store "$production_store" 'sha256:<generation>'
sudo "$skills" generation status --store "$production_store"
```

Check physical presence and PIN/user verification yourself. The verifier
requires the signature assertion flags; key-generation options alone do not
set its policy. Recovery changes inherit that policy, and a replaced Primary
must be refused for a new Admission. Verification/status need no token touch.

## Sandbox startup

Sandbox startup uses the release's existing `louiselm-launch` executable as a
single-threaded bootstrap. It blocks while the parent admits its PID to the
Session cgroup, then receives only the workload stdin and Bubblewrap's two gate
descriptors over a private socket and replaces itself with Bubblewrap. Descriptor
inheritance is configured only in that fresh process; concurrent parent spawns
cannot inherit the gates. Host identity is still checked before the workload is
released. The crate denies unsafe Rust across its binaries and tests. The sole
reviewed exception temporarily masks SIGTTOU while returning foreground terminal
ownership after bounded interactive signing (`launcher_install/foreground.rs`).

The privileged launcher pins its bootstrap to the validated release directory.
Development callers of `BubblewrapBackend` can select the Cargo-built launcher
explicitly with `with_bootstrap`; that executable must be trusted and traversable
by the assigned Session identity. This does not make a development build a
verified release.

Without a usable cgroup, NamespaceOnly preparation pins Bubblewrap's observed
PID-namespace leader with a pidfd before releasing the startup gate. Disposal
kills that leader first and lets the monitor reap it; success requires both
processes reaped, including when the workload never started. A failed proof
returns `CleanupUnproven` and retains the Session's handles for retry. Complete
membership is unavailable (`processes()` returns `NoCgroup`), and the disposal
report's initial count covers only the monitor and leader. This does not supply
cgroup freeze/interrupt control or verified Lifecycle evidence.

`without_cgroup()` selects this development path explicitly for conformance on
hosts that also offer delegated cgroups; HostIdentity plans remain refused.

## The trusted release

Everything above assumes the binary enforcing it is not one the Agent can
rewrite. Running from the development checkout makes that assumption false: the
code being confined can edit the code that decides whether confinement worked.

A **release** breaks the circle. `release build` refuses anything but a clean
commit — untracked files count as dirty, because a file that is not in the
commit cannot be reviewed by reading the commit and can still be compiled in —
and binds the commit, the locked dependencies, the toolchain, the policy, the
schema set, and every resulting byte into one manifest. Its digest is the
release identity. The **Release** credential signs it in its own namespace,
distinct from Primary even on one token. Neither recovery method can sign it.

Installing is content-addressed and atomic. Each release lands in its own
immutable directory and the prefix's `current` symlink is replaced by a rename,
the one operation that cannot be observed half-done:

```text
<prefix>/releases/<release-id>/{bin,policy,schemas,manifest.json,manifest.sig}
<prefix>/current -> releases/<release-id>
<prefix>/state.json
```

A failure anywhere before the rename leaves the previous release exactly as it
was, and the previous release's files stay on disk and usable. Downgrades are
refused: the safe path away from a bad release is building a newer one.

The install request supplies a bundle and a prefix and nothing else. Component
names come from a fixed allowlist, the layout is decided by the tool, and modes
are set by the tool — an install cannot introduce a new command, a different
policy path, or a different owner.

**Ownership is reported, not asserted.** This process cannot make a file
root-owned without being root, so `release status` says what the bytes actually
are — uid, world-writability, and whether any installed component's hash has
changed since install — and names the next action. It never claims a trust
boundary the filesystem does not show.

**A development build says so.** `release identity` reports `unverified` with
the code `development_build` for anything not running from inside an installed
release. A store created by a development build is marked untrusted at creation
and stays that way, and a development build may not activate a Skill Generation
in a trusted store. Promotion is not offered: an Agent that can write the store
must not also be able to decide the store is trustworthy.

### Manual release acceptance

Automated tests assemble bundles from fake component files and install into
temporary prefixes, so they cover identity, signing, tampering, atomicity, and
downgrade without a nested build or root. Root ownership and genuine hardware
signing remain `lm70`; installed Android/YubiKey recovery remains `.11.5`.

Use the [one-token release/recovery runbook](../docs/recovery-ceremony.md), not
older two-token rotation recipes. It distinguishes the permanently untrusted
first-release store from fresh installed production setup. Beads records the
exact clean source, two unsigned bundles and transfer checks; do not reuse
pre-recovery artifacts. Signing/install, upgrade/downgrade and destructive
tamper checks are maintainer-operated, inside the disposable acceptance VM.

### Manual launcher-authority acceptance

This procedure changes root trust data, subordinate-ID reservations, and
`sudoers`. Run it only in a disposable VM, using a dedicated operator account,
and take a snapshot before the destructive digest checks.

For the rootless host-side QEMU/KVM setup and disposable reset procedure, see
[the launcher acceptance VM runbook](../docs/launcher-vm.md).
Before installed-authority acceptance, run the separate
[hostile conformance gate](../docs/launcher-conformance.md). It uses deterministic
Agent/service doubles, requires actual kernel denials and cannot grant Verified
posture or substitute for the genuine signing ceremony below.

#### Installer authority (louiselm-d6fv.4.2)

The installer, status, rotation, and lease primitives in this slice are covered
by `cargo test --test launcher_install`. The standard release build includes
the real `louiselm-launch` executable; installation refuses a release that does
not contain those measured bytes.

Once that component exists, install its signed release at the fixed prefix as
described above. Run the following as the VM maintainer; choose unused ranges
if the example ranges collide on the host:

```sh
operator=louiselm-operator
uid_start=2000000
gid_start=3000000
slots=4
broker_uid=1500
broker_gid=1500
skills=/usr/local/lib/louiselm/current/bin/louiselm-skills
launcher=/usr/local/lib/louiselm/current/bin/louiselm-launch

test "$(id -u "$operator")" -ne 0
test -x "$skills"
test -x "$launcher"
sudo "$skills" release identity --robot-json | tee /tmp/release-identity.json
release_id=$(jq -er 'select(.verified == true) | .release_id' \
  /tmp/release-identity.json)
launcher_digest="sha256:$(sha256sum "$launcher" | awk '{ print $1 }')"
sudo "$skills" release status --robot-json | jq -e '.trusted == true'

sudo "$skills" launcher install \
  --operator "$operator" \
  --broker-uid "$broker_uid" \
  --broker-gid "$broker_gid" \
  --uid-start "$uid_start" \
  --gid-start "$gid_start" \
  --slots "$slots" \
  --robot-json | tee /tmp/launcher-install.json
jq -e --arg operator "$operator" \
  --arg release_id "$release_id" \
  --arg launcher_digest "$launcher_digest" \
  --argjson uid_start "$uid_start" \
  --argjson gid_start "$gid_start" \
  --argjson slots "$slots" '
    .schema == "louiselm.launch.install.status/1" and
    .trusted == true and (.failures | length) == 0 and
    .config.operator == $operator and
    .config.release_id == $release_id and
    .config.launcher_digest == $launcher_digest and
    .config.pool == {
      uid_start: $uid_start,
      gid_start: $gid_start,
      slots: $slots
    } and
    (.active_key_id | type) == "string" and
    .retained_key_ids == [] and .occupied_slots == []
  ' /tmp/launcher-install.json
! grep -q 'OPENSSH PRIVATE KEY' /tmp/launcher-install.json
```

Check the installed ownership and modes. Every displayed owner must be `0:0`:

```sh
sudo stat -c '%u:%g %a %n' \
  /usr/local/lib/louiselm/launcher \
  /usr/local/lib/louiselm/launcher/config.json \
  /usr/local/lib/louiselm/launcher/keyring.json \
  /usr/local/lib/louiselm/launcher/private \
  /usr/local/lib/louiselm/launcher/private/keys \
  /usr/local/lib/louiselm/launcher/private/scratch \
  /usr/local/lib/louiselm/launcher/locks \
  /etc/sudoers.d/louiselm-launch
sudo find /usr/local/lib/louiselm/launcher/private/keys \
  -mindepth 2 -maxdepth 2 -name key -exec stat -c '%u:%g %a %n' {} +
```

The expected modes are `0711` for `launcher`, `0444` for `keyring.json`,
`0440` for the sudoers fragment, `0600` for `config.json` and private keys,
and `0700` for all private and lock directories. The installer also records
exactly one non-overlapping reservation under numeric owner `0` in each
subordinate-ID ledger:

```sh
test "$(sudo awk -F: -v s="$uid_start" -v n="$slots" \
  '$1 == "0" && $2 == s && $3 == n { c++ } END { print c + 0 }' \
  /etc/subuid)" = 1
test "$(sudo awk -F: -v s="$gid_start" -v n="$slots" \
  '$1 == "0" && $2 == s && $3 == n { c++ } END { print c + 0 }' \
  /etc/subgid)" = 1

sudo awk -F: -v s="$uid_start" -v n="$slots" '
  !($1 == "0" && $2 == s && $3 == n) && $2 < s + n && s < $2 + $3 { bad = 1 }
  END { exit bad }
' /etc/subuid
sudo awk -F: -v s="$gid_start" -v n="$slots" '
  !($1 == "0" && $2 == s && $3 == n) && $2 < s + n && s < $2 + $3 { bad = 1 }
  END { exit bad }
' /etc/subgid

for uid in $(seq "$uid_start" "$((uid_start + slots - 1))"); do
  ! /usr/bin/getent passwd "$uid" >/dev/null
done
for gid in $(seq "$gid_start" "$((gid_start + slots - 1))"); do
  ! /usr/bin/getent group "$gid" >/dev/null
done
! awk -F: -v s="$gid_start" -v n="$slots" \
  '$4 >= s && $4 < s + n { found = 1 } END { exit !found }' /etc/passwd
```

Validate the exact sudo boundary independently. The operator is pinned by
numeric UID, the component by SHA-256, and the argument vector by the single
literal `run` argument:

```sh
operator_uid=$(id -u "$operator")
launcher_sha256=${launcher_digest#sha256:}
sudo /usr/sbin/visudo -cf /etc/sudoers.d/louiselm-launch
sudo grep -Fx "Defaults!$launcher fdexec=digest_only" \
  /etc/sudoers.d/louiselm-launch
sudo grep -Fx \
  "#$operator_uid ALL=(root:root) NOPASSWD: NOSETENV: sha256:$launcher_sha256 $launcher run" \
  /etc/sudoers.d/louiselm-launch
```

Prove reinstall and rotation are idempotent, and that rotation retains both
public and private history without exposing private bytes to the operator:

```sh
old_key=$(jq -r .active_key_id /tmp/launcher-install.json)
sudo "$skills" launcher install \
  --operator "$operator" --broker-uid "$broker_uid" --broker-gid "$broker_gid" \
  --uid-start "$uid_start" --gid-start "$gid_start" \
  --slots "$slots" --robot-json > /tmp/launcher-reinstall.json
test "$(jq -r .active_key_id /tmp/launcher-reinstall.json)" = "$old_key"

rotation_id=vm-acceptance-1
sudo "$skills" launcher rotate-key \
  --rotation-id "$rotation_id" --expected-key-id "$old_key" \
  --robot-json | tee /tmp/launcher-rotation.json
new_key=$(jq -r .active_key_id /tmp/launcher-rotation.json)
test "$new_key" != "$old_key"
jq -e --arg new "$new_key" '.created == true and .key_id == $new' \
  /tmp/launcher-rotation.json

sudo "$skills" launcher rotate-key \
  --rotation-id "$rotation_id" --expected-key-id "$old_key" \
  --robot-json | jq -e --arg new "$new_key" \
  '.created == false and .active_key_id == $new'
sudo "$skills" launcher status --robot-json | tee /tmp/launcher-status.json
jq -e --arg old "$old_key" --arg new "$new_key" '
  .trusted == true and .active_key_id == $new and
  (.retained_key_ids | index($old)) != null
' /tmp/launcher-status.json
sudo -u "$operator" test -r /usr/local/lib/louiselm/launcher/keyring.json
! sudo -u "$operator" test -r /usr/local/lib/louiselm/launcher/private
test "$(sudo find /usr/local/lib/louiselm/launcher/private/keys \
  -mindepth 2 -maxdepth 2 -name key | wc -l)" -eq 2
```

#### Runtime acceptance

The Rust relay boundary takes `RelayStdio`, not arbitrary blocking `Read`/`Write`
implementations. It owns descriptor-backed controller I/O and preserves bytes
prefetched while reading the launch frame. Callers give it exclusive use of the
open file descriptions, including duplicates, until cleanup restores their
original flags. The CLI duplicates stdin/stdout with close-on-exec enabled.

`SystemRunningAgent` owns one cancellable, nonblocking relay worker. Successful
quiescence joins it and closes controller/child I/O even when the controller stays
open or stops reading. Disposal requires both relay and process-tree cleanup
before the identity lease can be released. Event callbacks must return promptly;
`false` requests retry under backpressure. This does not make uninterruptible
kernel/filesystem faults cancellable, or prove the installed authority ceremony.
The spawning coordinator remains alive through lifecycle cleanup: handing a
sandbox to another Rust thread does not transfer its kernel parent-death binding.
Parent-death protection stays enabled.

An ACP relay failure closes capabilities and proves Disposal before signing a
terminal receipt with the closed `relay_failed` cause. It is not a fabricated
process-exit classification. Earlier lifecycle receipts remain ordered, and
failed signing/storage retains audit intent or exact signed bytes for broker
reconciliation. The owner returns `RelayFailed` only after the terminal receipt
is durably acknowledged; unproven cleanup fails without a successful Disposal
receipt or identity release. This uses the existing receipt schema, with no raw
I/O errors or controller payloads in the new cause.

`cargo test --test launch_supervisor` covers the complete launch transaction
against a deterministic fake Control broker. A live ceremony additionally
requires the real broker from louiselm-qbr.5.1.1 at the installed rendezvous.
Once it is installed, submit one canonical request line as the operator, then
continue ACP on the same stdin. This is the only privileged invocation the
sudo rule may admit; there is no release-ID argument:

```sh
sudo -u "$operator" sudo -n \
  /usr/local/lib/louiselm/current/bin/louiselm-launch run \
  < /tmp/launch-request.json
```

Confirm that `run extra`, a copied launcher at a different path, the exact path
after changing one byte, and a rule containing a different valid SHA-256 are all
rejected by `sudo -n`. Roll the VM back after these destructive checks; do not
repair an immutable release in place.

Capture both receipts from one launch: sequence zero records `Starting`, its
durable acknowledgement permits startup, and sequence one records `Running`.
Rotate the launcher key once and capture another pair. Verify both linked
receipts from each launch. The public keyring must retain both the active and
retired public keys while no private key is readable by the operator. Verify
each canonical payload against the public key selected by its `signing_key_id`
and the fixed namespace:

```sh
key_id=$(jq -r .signing_key_id /tmp/receipt.payload)
public_key=$(jq -r --arg key_id "$key_id" \
  '.keys[] | select(.key_id == $key_id) | .public_key' \
  /usr/local/lib/louiselm/launcher/keyring.json)
test -n "$public_key"
printf 'louiselm-launch %s\n' "$public_key" > /tmp/allowed-signers
/usr/bin/ssh-keygen -Y verify -f /tmp/allowed-signers -I louiselm-launch \
  -n louiselm.launch.receipt/2 -s /tmp/receipt.sig \
  < /tmp/receipt.payload
```

Finally, hold identity slot _N_ through one Session. A second acquisition of
slot _N_ must fail busy while an adjacent slot succeeds; after disposing the
first Session, slot _N_ must be acquirable again. This proves the persistent
`locks/<slot>.lock` inode coordinates live leases rather than merely recording
them.

## Gates

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
../scripts/test-skills-core
node --test --test-timeout=5000 tests/recovery_browser.test.cjs
```

The recovery browser gate uses Node.js 22+ built-ins, without npm packages or a
personal browser. It runs the shipped client with deterministic time and async
browser doubles; it does not replace installed Android/YubiKey acceptance.

## Scope

This crate owns packaging, Inspection, the Dossier, Skill Admission, and the
trusted release. It
requires `ssh-keygen` for signatures and `git` for witnessing; both are part of
the trusted base rather than vendored, and this crate implements no
cryptography of its own.

Provider-scoped views (louiselm-d6fv.3), Session launch and containment
(louiselm-d6fv.4), and portable Endorsements (louiselm-d6fv.8) build on the
canonical contract, the Generation chain, and the release identity defined here.
`louiselm-launch` is built into the signed bundle. The control-service binary
is still owned by louiselm-qbr.5.1; the bundle format already has a slot for it,
and a release that declares a component it cannot produce is refused rather
than shipped short.
