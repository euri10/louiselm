# Plugin releases (GitHub)

The core Lua plugin is independently installable. Capture, trusted tools and
Android are optional companions; core chat requires none of them. Capture has its
own Cargo release and Linux download. Trusted-tool and Android release automation
belongs to separate implementation slices. GitHub
[`euri10/louiselm`](https://github.com/euri10/louiselm) is public and is the
release authority. GitLab synchronization is tracked in
`louiselm-component-releases-oa0d.5`; it is not implemented by this slice.

## Status and first release

The first alpha release, [`plugin-v0.1.0`](https://github.com/euri10/louiselm/releases/tag/plugin-v0.1.0),
was published on 2026-09-18 at 10:29:29 UTC and is immutable. Its tag targets
`096c25d2f7cb845e4af73d997f91ae81871d1b59`; `VERSION`, the manifest and the
consumed Lua version are `0.1.0`. Hosted acceptance in
`louiselm-component-releases-oa0d.1` records the maintainer-merged release PR,
successful CI on its exact head and merge commit, and publication/retry run
`35334925205`. Companion releases remain separate work.

For bootstrap, `VERSION` and the manifest initially contained `0.0.0`, meaning
unreleased; the older ACP `0.1.0` literal was not a release. Release Please
selected the explicit initial version `0.1.0` from the entire history, including
the root commit, subject to the component and commit type filters below.
No `bootstrap-sha` is set. `commit-search-depth` is JavaScript's
maximum safe integer (9007199254740991), avoiding the upstream 500-commit cutoff;
the scan ends at repository inception or the previous real plugin release.
There is no fake historical tag or release. The separate commit-policy audit
boundary does not restrict release notes or the complete source tree shipped.
The full first-release history can exceed GitHub's PR-description limit: the
pinned tool clips that preview at 65,536 characters. Review the generated
`CHANGELOG.md` for the complete notes, not just the PR description.

`VERSION` is the plugin version source. Release Please updates it, the plugin's
entry in `.release-please-manifest.json`, `CHANGELOG.md`, and the marked line in
`lua/louiselm/version.lua` in the same PR. `scripts/generate-plugin-version`
reproduces that Lua projection; its `--check` gate checks both the manifest and
the complete generated file. ACP initialize consumes the module. Lua import
does no Git, filesystem or process discovery. ACP `protocolVersion` stays a
separate wire-protocol value. Unreleased commits between releases retain the
last packaged version, so use a full commit pin when identifying development code.

## Changes and proposals

One pinned Release Please action (v5.0.0, commit
`45996ed1f6d02564a971a2fa1b5860e934307cf7`, locked Release Please 17.6.0) prepares
separate component PRs on `main`. Plugin and capture are configured here.

| Change | Plugin release effect |
| --- | --- |
| `fix:` touching plugin code | Patch |
| `feat:` touching plugin code | Minor |
| Breaking plugin change (`!` or `BREAKING CHANGE:`) | Minor during 0.x |
| Only excluded companion, tracker, site/demo/asset directories | None, including their `feat:`/`fix:` commits |
| Shared README, `docs/`, `doc/`, tests, plugin scripts, GitHub CI, policy and root release metadata | Intentionally plugin-owned; their `fix:`/`feat:` commits count |
| Site/companion tooling files outside their directories | Use `build:`, `chore:`, `ci:`, `docs:`, `style:` or `test:`, without breaking/Release-As trailers; CI enforces the enumerated file list |
| Mixed plugin and companion changes | Plugin participates; companions will classify their own paths |

The last convention is necessary because the pinned tool's `exclude-paths`
matches directories, **not individual files**. `scripts/check-release-commits.py`
lists the exceptions, including site npm metadata and companion installers,
and checks commits after its `AUDIT_BASELINE`
(`c905fdafa60370d1a07e6853bba741cce9da9870`, exclusive) before proposals run.
That is when the convention took effect; older commits are not retroactively
rejected and remain eligible for the first release notes. Keep
such changes in separate non-release commits; fix their commit message before
merging. Do not squash them under a release-driving title. Directory exclusions
and the exception list must be reviewed when adding a new component or tooling
file. Keep Conventional Commits on main (squash with the reviewed PR title).

Breaking notes must explain the upgrade and name required enabled-companion
updates. They do not authorize compatibility shims. The version gate refuses
1.x, including an explicit `Release-As: 1.0.0`; a maintainer stability decision
must deliberately change the policy and gate. A breaking commit cannot infer 1.0.

## Activation and hosted acceptance

The following setup and hosted acceptance were completed for the first release.
Keep these requirements when reviewing or reconfiguring the workflow; this
procedure does not authorize changing credentials or publishing another release.

1. Register a dedicated GitHub App and install it only on `euri10/louiselm` with
   Contents and Pull requests read/write, plus Issues read/write for Release
   Please labels/comments, and Administration **read-only** for the immutable
   releases settings check. Approve permission changes on the App installation
   as well as saving them on the App. Disable webhooks and user authorization; the App
   only authenticates automation. Generate a private key and store the complete
   PEM as repository Actions secret `RELEASE_PLEASE_APP_PRIVATE_KEY`. Store its
   numeric App ID as repository Actions secret `RELEASE_PLEASE_APP_ID` too.
   Neither value belongs in a commit or chat. The pinned
   `actions/create-github-app-token` v3.2.0 action creates a short-lived token
   scoped to this repository and the three release-writing permissions, plus
   a separate token with only Administration read. Both are revoked when the
   job ends. The publisher receives the latter as `GH_IMMUTABILITY_TOKEN` and
   uses it only for the immutable-releases settings GET; all other reads and
   publication retain the job's `GITHUB_TOKEN`. No Administration write is
   needed. `RELEASE_PLEASE_TOKEN` is not used. Do not reuse personal CLI
   credentials, protected bundle-signing keys, or a signing environment.
   The App does not need Actions write or Workflows write.
   If GitHub refuses preparation for an older commit
   whose workflow files differ from main, retry after reviewing that restriction;
   do not silently broaden the token. Ordinary PR CI uses read-only `GITHUB_TOKEN`.
   Release PRs are authored by the App, so the maintainer can approve them.
   A personal token would author PRs as its owner, who cannot self-approve.
2. Enable GitHub **immutable releases**. The publisher refuses to proceed while
   the repository's immutable-releases API reports disabled. This locks published
   tags/assets through GitHub, including against accidental manual replacement.
3. Protect `main` with maintainer review, no automatic bot merging, and required
   CI checks (including Plugin release contract and stable Lua gates). Retain
   existing checks for the other components. Review the first release history/version.
4. Set repository variable `PLUGIN_RELEASES_ENABLED=true`, then allow a normal
   main-push CI run to finish. The release workflow is disabled without this opt-in.
5. Inspect the resulting plugin-only bot PR. Confirm **pull_request** CI ran on
   its exact head SHA, including version/generator, stable Lua, and release
   fixtures. Record the PR URL, head SHA, run URL and results in
   `louiselm-component-releases-oa0d.1`. This is required before calling automation
   operational; fixtures alone do not prove GitHub credentials/event delivery.
6. The maintainer chooses publication by merging the checked release PR. Inspect
   CI for the resulting main commit, then the immutable `plugin-v0.1.0` release
   and tag. The implementation Agent must not perform this first merge/publication.

The default `GITHUB_TOKEN` suppresses ordinary downstream PR workflows; the
App installation token avoids that suppression. Publication independently
checks that a successful CI `pull_request` run exists on the release PR head.
Enabling Actions' ability to approve PRs is not a substitute for that token.

## Publication and retry contract

The workflow runs after successful **main-push** CI. It recovers any prepared
draft, asks Release Please to prepare unpublished drafts, publishes the checked
plugin draft, then proposes the next release. Draft preparation never forces a
Git tag. Publication requires a maintainer-merged plugin release PR, successful
CI on both its head and exact merge commit, matching VERSION/manifest/Lua/tag,
release notes, and enabled GitHub immutability. A newer main commit cannot borrow
an older commit's green run. Privileged workflow code comes from reviewed main,
never from PR artifacts or PR checkout.

The plugin ships the complete approved Git tree through its immutable tag and
GitHub's full source archives. It has no compiled asset upload or signing stage.
No source filtering or runtime build step is used. The tag points to the exact
remote commit; untracked or dirty local files cannot enter it. A failed gate
leaves a draft unpublished. Retry the workflow with `ci_run_id` set to the
successful main-push CI run for that release commit. Recovery happens before
Release Please so an interrupted draft/label update can be retried. Already
published immutable releases are verified and left untouched; tags are never
moved and assets are never overwritten. An unexpected tag target fails closed.
An unchecked newer draft waits for its own successful main CI/retry. Outstanding
plugin drafts suppress new proposals so an unpublished version cannot become a
false baseline for the next proposal.

Companion and GitLab-sync slices can consume GitHub release metadata:
`tag_name=plugin-v<version>`, `target_commitish=<full SHA>`, release `id`,
`immutable=true`, and VERSION plus the plugin `.` manifest entry at that SHA.
The publisher also emits JSON `{component, version, tag, sha, release_id}` records.
Reserve distinct prefixes for companions; do not publish a global `v0.1.0` tag
or rely on GitHub's repository-wide “latest” release for plugin selection.

## Installation and pinning

Use the public HTTPS repository and the exact published alpha tag:

```lua
-- lazy.nvim example; no GitHub credential is required to clone public source.
{
  url = "https://github.com/euri10/louiselm.git",
  tag = "plugin-v0.1.0",
  config = function()
    require("louiselm").setup({
      agents = { codex = { command = "codex-acp", provider = "OpenAI" } },
    })
  end,
}
```

This is an exact tag pin; a wildcard/automatic SemVer selector may not understand
component prefixes. For development code, replace `tag` with a reviewed full
commit SHA. An authenticated GitHub CLI can download the published source:
`gh release download plugin-v0.1.0 --repo euri10/louiselm --archive tar.gz`.
Public browsing and source downloads require no repository access grant. Never put a PAT
in plugin configuration or a repository URL. Core chat still needs the documented
Neovim, SQLite and configured ACP Agent prerequisites, but no companion binary.

## Capture releases and compatibility

Capture uses `capture-v0.x.y` tags and the Rust Release Please strategy. Its
canonical version is `capture-service/Cargo.toml`; Cargo.lock and the
`capture-service` release-manifest entry must agree. `0.0.0` means unpublished;
the first proposed release is `0.1.0`. `python3 scripts/capture_release.py --check`
enforces agreement and the alpha ceiling. Capture-only source commits never
bump the plugin. Mixed interface/client changes may release both components.

The existing publisher also checks capture release PR approval and exact PR/main
CI before building. It builds only a Git archive of the approved source using
Rust 1.97.1, locked dependencies and `x86_64-unknown-linux-gnu` on the workflow's
Ubuntu 24.04 runner. The download requires Linux x86-64 with glibc 2.39 or newer;
it is not a portable static binary. No other target is advertised.

Each release contains `louiselm-capture-VERSION-x86_64-unknown-linux-gnu.tar.gz`,
a matching `.json` identity record and `.sha256` checksum file. The archive holds
the executable and `metadata.json`; identity records bind package version,
source commit, target, toolchain, interface versions and binary digest. The
publisher verifies native `--version`/`metadata` output before packaging. Every
existing remote asset must match the rebuilt bytes; incomplete uploads may add
missing assets, but retries never replace assets or move tags. A mismatching
draft stays unpublished for inspection. Published releases are immutable.

After the maintainer reviews and merges the capture release PR, verify hosted
CI/publication and download the exact tag. This implementation alone is not
evidence that the first binary has been published. Example after `capture-v0.1.0`
exists (run in an empty download directory):

```sh
gh release download capture-v0.1.0 --repo euri10/louiselm \
  --pattern 'louiselm-capture-0.1.0-x86_64-unknown-linux-gnu.*'
sha256sum --check louiselm-capture-0.1.0-x86_64-unknown-linux-gnu.sha256
tar -xzf louiselm-capture-0.1.0-x86_64-unknown-linux-gnu.tar.gz
./louiselm-capture --version
./louiselm-capture metadata
```

Retain the previous executable and stop/restart the configured service at a safe
boundary after pending operations finish. Install the verified download at the
existing configured executable path; preserve the service identity, permissions,
environment and data directories. For a user-owned installation, use
`install -m 0755 ./louiselm-capture /exact/configured/path/louiselm-capture` while
the service is stopped. Root-owned deployments require the existing privileged
installation procedure. No release command changes live dotfiles or services.

Package versions do not decide compatibility. Current clients require interface
1 for the capture CLI, Run socket and Attention socket. Every CLI operation
passes `--require-interface=1`, checked before state discovery; initial socket
snapshots carry the actual daemon's package identity and interfaces. Missing,
malformed or unsupported metadata closes only that client before mutations.
The existing receiver health response also advertises bounded interface metadata
for Android's subsequent compatibility slice, without widening access.

Update an enabled companion before reloading a plugin that requires these
interfaces. An older binary without metadata is intentionally refused; core chat
and disabled integrations remain usable, and refusals preserve recordings and
durable Run/Attention state. Keep package pins separate from interface revisions
and persisted schemas. Bump the affected interface when its consumed contract
changes incompatibly.

Capture checks additionally run `python3 scripts/test-capture-release.py`, the
complete capture Rust and Lua gates, and the existing pinned Release Please
fixtures. Local fixtures prove refusal and retry behavior; hosted PR/event,
download and installed-service acceptance remain distinct.

## Checks and upstream references

Run `./scripts/generate-plugin-version --check`,
`python3 scripts/check-release-commits.py`, and
`python3 scripts/test-plugin-release.py`. CI also checks out the action at its
exact commit, installs only its locked production dependencies with scripts
disabled into a temporary directory, then runs
`NODE_PATH=<action-checkout>/node_modules node --test scripts/test-release-please.cjs`.
These are the approved release tool's API fixtures, not another release framework
or a new application dependency. Run the full Lua/generator gates as documented
in [testing](agent-testing.md).

Upstream contracts: [Release Please configuration](https://github.com/googleapis/release-please/blob/v17.6.0/docs/manifest-releaser.md),
[App token action](https://github.com/actions/create-github-app-token/tree/bcd2ba49218906704ab6c1aa796996da409d3eb1),
[directory exclusion implementation](https://github.com/googleapis/release-please/blob/v17.6.0/src/util/commit-exclude.ts),
[action credentials](https://github.com/googleapis/release-please-action/tree/45996ed1f6d02564a971a2fa1b5860e934307cf7#github-credentials),
[workflow triggering](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow),
and [immutable releases](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases).
