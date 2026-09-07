# One-token signing and independent recovery

This is the replacement ceremony for `d6fv.11.4`. The old SSH Recovery role,
`--recovery`, `trust rotation-payload`, and `trust rotate` no longer exist.
Do not run older two-token bootstrap/rotation recipes.

One physical YubiKey holds **distinct Primary and Release credentials**.
Primary signs Admissions; Release signs trusted builds. A password-manager-backed
passkey and an independently written paper phrase authorize recovery changes,
never ordinary Admissions or releases. Either recovery method works alone.

Software tests and virtual Chrome acceptance are not physical acceptance.
`lm70` covers the genuine signed release, `.11.5` the installed Android/YubiKey
recovery workflow, and `.4.9` the installed launcher. None establishes Verified
cutover by itself.

## Operator boundary

Run real ceremonies yourself in a trusted local foreground terminal, not an
Agent terminal, recorded terminal, screen share or screenshot session. Never
paste an actual phrase into chat, command arguments, stdin, a file, or a browser.
The tool uses hidden `/dev/tty` input and an alternate screen, clears displayed
words, zeroizes owned secret buffers, and disables core dumps. A compromised
root account or terminal recorder is outside this boundary.

Open each printed `http://localhost:PORT/random-path/` URL in your normal,
non-root browser. Check it against the trusted terminal. Use your Android
password manager through the browser's supported cross-device flow. Registration
requires user verification and reported backup eligibility/state; that metadata
does not prove a provider's durability or identify a particular provider.
Confirm yourself that the credential is saved where you expect.

The fixed relying party is `localhost`; each verifier checks the exact temporary
origin, challenge and predecessor. Other local applications also use localhost,
so the trusted browser and terminal remain part of the boundary. The phone's
prompt is not an independent display of the complete change.

## First release: explicitly provisional

Only do this when no trusted release is installed. Use a disposable Linux VM
for `lm70`; do not install or change launcher policy on the host as preparation.
Transfer the reviewed artifacts using the recipe below. The VM wrapper exposes
neither USB nor a browser: the maintainer must explicitly arrange the hardware
and trusted-browser access needed by a real ceremony. Agent preparation does
not attach devices, forward an agent socket or run these signing/setup commands.

Generate two fresh credentials on the same token. Choose unused file names;
do not overwrite existing keys. Hardware attestation is not validated by this
tool, but signatures must prove user presence and verification.

```sh
ssh-keygen -t ed25519-sk -O resident -O verify-required \
  -C louiselm-primary -f "$HOME/.ssh/id_louiselm_primary"
ssh-keygen -t ed25519-sk -O resident -O verify-required \
  -C louiselm-release -f "$HOME/.ssh/id_louiselm_release"

# DEV_TOOL is the reviewed, not-yet-installed binary; FIRST_BUNDLE and
# SECOND_BUNDLE are the reviewed acceptance bundles (transfer recipe below).
provisional_store=$(mktemp -d)
"$DEV_TOOL" trust bootstrap --store "$provisional_store" \
  --primary "$HOME/.ssh/id_louiselm_primary.pub" \
  --release "$HOME/.ssh/id_louiselm_release.pub" \
  --trust-domain louiselm/skills --require-hardware --robot-json |
  jq -e '(.keys | length) == 2 and all(.keys[];
    .sk_policy.require_hardware and .sk_policy.require_user_presence and
    .sk_policy.require_user_verification)'

"$DEV_TOOL" release sign --store "$provisional_store" \
  --bundle "$FIRST_BUNDLE" --key "$HOME/.ssh/id_louiselm_release"
"$DEV_TOOL" release verify --store "$provisional_store" --bundle "$FIRST_BUNDLE"
"$DEV_TOOL" release sign --store "$provisional_store" \
  --bundle "$SECOND_BUNDLE" --key "$HOME/.ssh/id_louiselm_release"
"$DEV_TOOL" release verify --store "$provisional_store" --bundle "$SECOND_BUNDLE"

# Maintainer-operated installation INSIDE the disposable acceptance VM only.
sudo "$DEV_TOOL" release install --store "$provisional_store" \
  --bundle "$FIRST_BUNDLE"
skills=/usr/local/lib/louiselm/current/bin/louiselm-skills
sudo "$skills" release identity --robot-json | jq -e '.verified == true'
sudo "$skills" release status --robot-json | jq -e '.trusted == true'
```

The provisional store remains a development store forever, including when an
installed tool opens it. It is not recovery-ready and must not be copied into,
promoted to, or edited into production provenance. No paper/passkey enrollment
is needed to prepare that first release. Production enrollment is separate.

## Atomic production setup

Use a fresh root-owned store under protected ancestors, not `/tmp`, a home
directory, or the provisional store. All existing provenance/trust paths must
also be root-owned, unaliased and not group/world-writable. The installed command
checks the path **before** opening or creating the store.

```sh
production_store=/var/lib/louiselm/skills
sudo "$skills" recovery setup --store "$production_store" \
  --primary "$HOME/.ssh/id_louiselm_primary" \
  --release "$HOME/.ssh/id_louiselm_release" \
  --trust-domain louiselm/skills
sudo "$skills" recovery status --store "$production_store"
```

Private-key paths need adjacent `.pub` files. Production setup always requires
hardware presence and verification; there is no flag to weaken that policy.

The flow generates 24 checksummed English words from fresh OS randomness. Write
them down, then re-enter all words with input hidden. This is not a user-chosen
password or an existing wallet seed. Next, register the backed-up passkey in the
browser. It is still only a candidate, not enrolled authority.

Review the exact public plan in the trusted terminal and type `apply`. Both
signing keys prove possession of that same plan in the setup namespace. Confirm
the final browser page. Only then does one locked, atomic publication enroll
both keys and both methods. Cancellation, wrong confirmation, expired proof or
failed possession does not publish partial authority. A concurrent enrollment
cannot be overwritten.

`recovery status` returns public JSON: fingerprints, enrollment facts,
`recovery_ready` and `next_action`. It does not expose credentials or paper
verifiers. `recovery_ready=true` additionally requires the installed executable,
protected production store and strict policy on both distinct signing roles.
It is not a claim of actual Android acceptance or Verified posture.

## Recovery and method management

Only explicitly named replacements change. Replacement signing credentials must
be new and distinct and prove possession of the exact plan. Their hardware
policy is inherited; the CLI cannot weaken it. Generate replacement hardware
credentials as above before invoking a key replacement.

| Available authority | Example operation |
| --- | --- |
| Existing Primary or Release | Replace paper and/or passkey with `--via primary` or `--via release` and `--authorizer PRIVATE_KEY` |
| Passkey alone | Replace either/both signing keys or recovery methods with `--via passkey` |
| Paper alone | Same operations with `--via paper`; `--paper replace` is mandatory |
| Nothing usable | Explicit last-resort reset, then complete setup again |

```sh
# Replace both ordinary signing keys using only the current passkey.
sudo "$skills" recovery change --store "$production_store" --via passkey \
  --primary "$HOME/.ssh/id_louiselm_primary2" \
  --release "$HOME/.ssh/id_louiselm_release2"

# Paper-only recovery: write and confirm new paper in the same transaction.
sudo "$skills" recovery change --store "$production_store" --via paper \
  --paper replace --primary "$HOME/.ssh/id_louiselm_primary2" \
  --release "$HOME/.ssh/id_louiselm_release2"

# Replace the passkey using the OLD passkey; paper remains unchanged.
sudo "$skills" recovery change --store "$production_store" --via passkey \
  --passkey replace

# Replace both recovery methods using a working ordinary signing key.
sudo "$skills" recovery change --store "$production_store" --via primary \
  --authorizer "$HOME/.ssh/id_louiselm_primary" --paper replace --passkey replace

# Or use only the paper to replace the passkey and consume/reissue the paper.
sudo "$skills" recovery change --store "$production_store" --via paper \
  --paper replace --passkey replace
```

These are independent alternatives, not a sequence to run blindly: after a key
replacement, use the newly enrolled key paths. A newly registered passkey cannot
authorize its own enrollment. Existing passkey authority remains reusable; paper
authority is consumed only by a successful transaction, which also confirms
its replacement. Neither method can directly sign an Admission or release.

Current-key authority still requires exact-change confirmation. All routes use
the shared trust/approval lock and bind the full predecessor, including recorded
approval history. Successful Admission and `release sign` register exact approved
payloads; retired keys verify only that history, not new or backdated approvals.

## Refusal, cancellation and reset

Each local ceremony has a five-minute limit covering input, browser proof,
hardware signing and final confirmation. Ctrl-C cancels local input and restores
terminal modes. Browser cancel/expiry closes the listener; subprocess deadlines
bound signing and clean up its process group. A killed process can leave terminal
display modes dirty (`stty sane` repairs them), but cannot partially enroll keys
and recovery methods.

**Keep both papers until success is confirmed.** A failure before rename preserves
the prior state, but a directory-sync error can follow publication. A lost browser
response after submitting approval is likewise indeterminate. Inspect the trusted
terminal and `recovery status` before retrying; closing a page cannot undo a commit.

When every usable method is lost:

```sh
sudo "$skills" recovery reset --store "$production_store"
```

Read the displayed domain and snapshot, then type `reset` yourself. This discards
all enrolled methods/keys and recorded-history authorization, not just one factor.
The final snapshot is checked under the lock. Complete `recovery setup` again and
re-admit supply; do not restore retired authority or promote the provisional store.

## Development evidence and release packaging

No additional release component is needed: the local HTTP/browser adapter is
embedded in `louiselm-skills`; `louiselm-launch` remains the other executable.
The manifest binds trust schema `louiselm.skills.trust/4`, recovery change
`louiselm.skills.recovery-change/1`, initial setup
`louiselm.skills.recovery-setup/1` and public status
`louiselm.skills.recovery-status/1`. Unsupported state is refused, not migrated.

Run the Rust gates through the documented repository wrapper. The automated
suite includes real cryptographic verification with disposable credentials,
paper/passkey replacement, replay/role/namespace refusal, atomic publication,
history retention, bounded signer/TTY and browser transport/disposal checks.
The opt-in whole operator flow requires a foreground PTY and isolated Chrome
virtual authenticator (resident key, UV, backup eligibility and backup state):

```sh
./scripts/test-skills-core --lib \
  cli::recovery::setup::tests::virtual_setup_and_changes \
  -- --ignored --nocapture --test-threads=1
```

It uses only freshly generated software keys and an untrusted temporary store.
Complete setup, passkey-authorized paper replacement and paper-only replacement;
the fixture asserts committed state and that development readiness remains false.
Never substitute your browser profile or a real recovery phrase.

Build both unsigned `lm70` bundles from the final clean committed source. Verify
each component hash and schema set and use increasing build timestamps for the
upgrade/downgrade check. Record the source commit, toolchain, release IDs and
bundle paths in Beads. Do not reuse pre-recovery bundles or sign during Agent
preparation; signature, tamper, installation and physical acceptance remain
maintainer-operated checks in `lm70` and `.11.5`.

### Exact artifact transfer and release acceptance

The `lm70` record names `BUNDLE_ROOT`, the archive SHA-256 and both manifest
release IDs. The archive contains only the two unsigned bundle directories;
no trust store, private key, paper or passkey credential belongs in it. The
bundle's `bin/louiselm-skills` can serve as `DEV_TOOL` before installation;
running those bytes outside an installed release still reports development
provenance. Check the source commit and component hashes before trusting it.

On the host, after the maintainer has separately authorized starting the
[disposable VM](launcher-vm.md), transfer the exact recorded archive:

```sh
# Set BUNDLE_ROOT to the fresh path in lm70, never an older cache directory.
sha256sum "$BUNDLE_ROOT/unsigned-bundles.tar.gz"
./scripts/launcher-vm put "$BUNDLE_ROOT/unsigned-bundles.tar.gz" \
  /home/vm/unsigned-bundles.tar.gz
./scripts/launcher-vm exec sha256sum /home/vm/unsigned-bundles.tar.gz
```

Compare both hashes to the recorded hash; stop on any mismatch. In the VM's
trusted operator terminal, extract into a fresh directory and compare both
displayed release IDs with the Beads record:

```sh
acceptance_root=$(mktemp -d /home/vm/lm70.XXXXXX)
tar -xzf /home/vm/unsigned-bundles.tar.gz -C "$acceptance_root"
FIRST_BUNDLE="$acceptance_root/bundle-1"
SECOND_BUNDLE="$acceptance_root/bundle-2"
DEV_TOOL="$FIRST_BUNDLE/bin/louiselm-skills"
jq '{release_id, built_at_ms, source, toolchain}' "$FIRST_BUNDLE/manifest.json"
jq '{release_id, built_at_ms, source, toolchain}' "$SECOND_BUNDLE/manifest.json"
```

The first-release ceremony above signs both bundles and installs the older one.
Keep its provisional store available for this release-only acceptance, then:

```sh
# INSIDE the VM, after both signatures and first install have succeeded.
sudo "$skills" release install --store "$provisional_store" --bundle "$SECOND_BUNDLE"
sudo "$skills" release status --robot-json | jq -e '.trusted == true'
ls /usr/local/lib/louiselm/releases  # both recorded releases remain

# Must refuse the older bundle; current must still name the second release.
sudo "$skills" release install --store "$provisional_store" --bundle "$FIRST_BUNDLE"
readlink /usr/local/lib/louiselm/current
```

The last install is an expected failure, not a command to retry or bypass.
Perform destructive component-tampering acceptance only on a disposable VM
snapshot under the separate `lm70` checklist. Never mutate the host installation
or the only accepted release. Production recovery setup uses a fresh protected
store regardless of this release-only provisional store.

For local development, the existing `webauthn-rs` dependency requires system
OpenSSL headers/libraries and pkg-config. Browser checks use an isolated virtual
authenticator, never the maintainer's profile; see `tests/passkey_browser.js`
and the opt-in operator fixture above.
