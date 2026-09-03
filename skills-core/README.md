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

trust bootstrap --primary KEY --recovery KEY [--require-hardware]
trust show | rotation-payload | rotate | reset --confirm

generation admit --member DIGEST[:DEPTH] ... --key PRIVKEY
generation witness DIGEST --remote URL [--branch B]
generation activate DIGEST
generation status | list

quarantine exclude DIGEST... --reason TEXT
quarantine all --reason TEXT
quarantine show
```

Every command accepts `--store DIR`, `--policy FILE --policy-digest D`, and
`--robot-json`.

Exit status is part of the contract, so an unattended caller never has to parse
prose:

| status | meaning |
| ------ | ------- |
| `0` | succeeded; the subject is admissible |
| `1` | failed; nothing was published and nothing is claimed |
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

*Fatal* means the package cannot be reviewed at all. The class is deliberately
tiny: no `SKILL.md` at the root, `SKILL.md` without usable frontmatter, or a
file that is neither binary nor valid UTF-8. Growing this class moves judgement
from the reviewer to a scanner that cannot read prose.

Everything else is a *mandatory Dossier finding* — a fact the reviewer must be
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

Three roles, kept distinct even on one physical token. **Primary** signs routine
Admissions. **Recovery** exists only to replace key policy or the primary, and
is refused as an ordinary signer — a recovery key that could also admit skills
would just be a second primary. **Release** authorizes trusted builds
(louiselm-d6fv.7). There is no seed phrase and no extractable master secret:
losing both tokens means an explicit `trust reset` and re-Admission, which is
the honest cost of not having a secret to steal.

A signed Generation governs nothing until it is **witnessed**. Its exact bytes
are published to a protected Git branch and read back from the remote before it
can be activated, so a Generation only takes effect once it exists somewhere the
operator does not solely control. The witness ledger is append-only per
Generation: a digest already published with different bytes is refused, never
overwritten. During a witness outage nothing changes, and the previous
Generation stays in force. Activation also refuses any sequence at or below what
is current, so restoring an older signed record is not a rollback path —
intentional rollback is a newly admitted higher sequence.

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
Run this once on Linux with the real tokens:

```sh
# 1. Enrol. Two resident FIDO keys, on two physically separate tokens.
ssh-keygen -t ed25519-sk -O resident -O verify-required -C admission-primary  -f ~/.ssh/id_admission
ssh-keygen -t ed25519-sk -O resident -O verify-required -C admission-recovery -f ~/.ssh/id_recovery
louiselm-skills trust bootstrap \
  --primary  ~/.ssh/id_admission.pub \
  --recovery ~/.ssh/id_recovery.pub \
  --require-hardware
louiselm-skills trust show

# 2. Admit. One touch for the whole set; ssh-keygen prompts for it.
louiselm-skills generation admit \
  --member sha256:<pkg>:read \
  --key ~/.ssh/id_admission
louiselm-skills generation witness sha256:<generation> --remote git@your.host:infra/skill-witness.git
louiselm-skills generation activate sha256:<generation>

# 3. Verify without a token. Nothing below should prompt for a touch.
louiselm-skills generation status

# 4. Rotate the primary with the recovery key. This is the touch that matters:
#    it must come from the recovery token, not the primary.
louiselm-skills trust rotation-payload --role primary --key ~/.ssh/id_admission2.pub > /tmp/change
ssh-keygen -Y sign -n louiselm.skills.trust/1 -f ~/.ssh/id_recovery /tmp/change
louiselm-skills trust rotate --role primary --key ~/.ssh/id_admission2.pub --signature /tmp/change.sig

# 5. Confirm the replaced primary is dead. This must fail.
louiselm-skills generation admit --member sha256:<pkg>:read --key ~/.ssh/id_admission
```

Check at each touch that the token actually blinked. A ceremony that completes
without a touch means `--require-hardware` did not reach the enrolled key, and
the assertion flags in the signature are what the verifier checks.

## The trusted release

Everything above assumes the binary enforcing it is not one the Agent can
rewrite. Running from the development checkout makes that assumption false: the
code being confined can edit the code that decides whether confinement worked.

A **release** breaks the circle. `release build` refuses anything but a clean
commit — untracked files count as dirty, because a file that is not in the
commit cannot be reviewed by reading the commit and can still be compiled in —
and binds the commit, the locked dependencies, the toolchain, the policy, the
schema set, and every resulting byte into one manifest. Its digest is the
release identity. A separate **release** role signs it, distinct from Admission
and recovery even on one token, in its own signature namespace.

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
downgrade without a nested build or root. Root ownership and the release-role
touch need a machine. Run this once:

```sh
# 1. Enrol the release role. Distinct from Admission and recovery.
ssh-keygen -t ed25519-sk -O resident -O verify-required -C louiselm-release -f ~/.ssh/id_release
louiselm-skills trust rotation-payload --role release --key ~/.ssh/id_release.pub > /tmp/change
ssh-keygen -Y sign -n louiselm.skills.trust/1 -f ~/.ssh/id_recovery /tmp/change
louiselm-skills trust rotate --role release --key ~/.ssh/id_release.pub --signature /tmp/change.sig

# 2. Clean build, then sign. The build refuses a dirty tree; check that first.
git status --porcelain          # must be empty
louiselm-skills release build --source skills-core --output /tmp/bundle
louiselm-skills release sign --bundle /tmp/bundle --key ~/.ssh/id_release

# 3. Install as root, into the fixed prefix.
sudo louiselm-skills release install --bundle /tmp/bundle
sudo louiselm-skills release status   # trusted: yes, ownership root-owned

# 4. Upgrade. Build a newer release and install it; `current` flips, the old
#    release stays on disk.
sudo louiselm-skills release install --bundle /tmp/bundle-2
ls /usr/local/lib/louiselm/releases    # both present

# 5. Prove the failure paths. Each must refuse.
sudo louiselm-skills release install --bundle /tmp/bundle      # downgrade
sudo sed -i s/x/y/ /usr/local/lib/louiselm/current/bin/louiselm-skills
sudo louiselm-skills release status                           # release_tampered
```

No private key material leaves the token at any point in this procedure.

## Gates

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Scope

This crate owns packaging, Inspection, the Dossier, Skill Admission, and the
trusted release. It
requires `ssh-keygen` for signatures and `git` for witnessing; both are part of
the trusted base rather than vendored, and this crate implements no
cryptography of its own.

Provider-scoped views (louiselm-d6fv.3), Session launch and containment
(louiselm-d6fv.4), and portable Endorsements (louiselm-d6fv.8) build on the
canonical contract, the Generation chain, and the release identity defined here.
`louiselm-launch` and the control-service binary do not exist yet; the bundle
format has slots for them, and a release that declares a component it cannot
produce is refused rather than shipped short.
