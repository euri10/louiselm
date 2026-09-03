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

## Gates

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Scope

This crate stops at the Dossier. Hardware-signed Skill Admission and remote
witnessing (louiselm-d6fv.2), Provider-scoped views (louiselm-d6fv.3), Session
launch and containment (louiselm-d6fv.4), and portable Endorsements
(louiselm-d6fv.8) build on the canonical contract defined here.
