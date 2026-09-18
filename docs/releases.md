# Plugin releases (GitHub)

The core Lua plugin is independently installable. Capture, trusted tools and
Android are optional companions; core chat requires none of them. Their release
automation belongs to separate implementation slices. GitHub
[`euri10/louiselm`](https://github.com/euri10/louiselm) is public and is the
release authority. GitLab synchronization is tracked in
`louiselm-component-releases-oa0d.5`; it is not implemented by this slice.

## Status and first release

This workflow is prepared for review, **not operationally accepted**. No release
has been published. The checked-in `VERSION` and manifest initially contain
`0.0.0`, meaning unreleased. The previous ACP `0.1.0` literal was not a release.
Release Please proposes the explicit initial version `0.1.0`, considering commits
after `c905fdafa60370d1a07e6853bba741cce9da9870` (exclusive). Review the version
and baseline in `release-please-config.json` before activation. There is no fake
historical tag or release. The baseline remains the commit-policy audit boundary;
Release Please itself switches to the last real release after publication.

`VERSION` is the plugin version source. Release Please updates it, the plugin's
entry in `.release-please-manifest.json`, `CHANGELOG.md`, and the marked line in
`lua/louiselm/version.lua` in the same PR. `scripts/generate-plugin-version`
reproduces that Lua projection; its `--check` gate checks both the manifest and
the complete generated file. ACP initialize consumes the module. Lua import
does no Git, filesystem or process discovery. ACP `protocolVersion` stays a
separate wire-protocol value. Unreleased commits between releases retain the
last packaged version, so use a full commit pin when identifying development code.

## Changes and proposals

One pinned Release Please action (v4.4.1, commit
`5c625bfb5d1ff62eadeeb3772007f7f66fdcf071`, locked Release Please 17.3.0) prepares
separate component PRs on `main`. Only the plugin is configured here.

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
and checks the history after the bootstrap baseline before proposals run. Keep
such changes in separate non-release commits; fix their commit message before
merging. Do not squash them under a release-driving title. Directory exclusions
and the exception list must be reviewed when adding a new component or tooling
file. Keep Conventional Commits on main (squash with the reviewed PR title).

Breaking notes must explain the upgrade and name required enabled-companion
updates. They do not authorize compatibility shims. The version gate refuses
1.x, including an explicit `Release-As: 1.0.0`; a maintainer stability decision
must deliberately change the policy and gate. A breaking commit cannot infer 1.0.

## Activation and hosted acceptance

The maintainer must review/configure these repository settings. This change does
not provision credentials, change visibility, merge release PRs, or publish one.

1. Register a dedicated GitHub App and install it only on `euri10/louiselm` with
   Contents and Pull requests read/write, plus Issues read/write for Release
   Please labels/comments. Disable webhooks and user authorization; the App
   only authenticates automation. Generate a private key and store the complete
   PEM as repository Actions secret `RELEASE_PLEASE_APP_PRIVATE_KEY`. Store its
   numeric App ID as repository Actions secret `RELEASE_PLEASE_APP_ID` too.
   Neither value belongs in a commit or chat. The pinned
   `actions/create-github-app-token` v3.2.0 action creates a short-lived token
   scoped to this repository and those three permissions, and revokes it when
   the job ends. `RELEASE_PLEASE_TOKEN` is not used. Do not reuse personal CLI
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
   existing checks for the other components. Review the first baseline/version.
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

Until the first release exists, use the public HTTPS repository and an exact
existing commit, for example this reviewed baseline:

```lua
-- lazy.nvim example; no GitHub credential is required to clone public source.
{
  url = "https://github.com/euri10/louiselm.git",
  commit = "c905fdafa60370d1a07e6853bba741cce9da9870",
  config = function()
    require("louiselm").setup({
      agents = { codex = { command = "codex-acp", provider = "OpenAI" } },
    })
  end,
}
```

After verifying the first published release, replace `commit` with
`tag = "plugin-v0.1.0"`. That is an exact tag pin; a wildcard/automatic SemVer
selector may not understand component prefixes. The tag does not exist yet.
An authenticated GitHub CLI can download the source after publication:
`gh release download plugin-v0.1.0 --repo euri10/louiselm --archive tar.gz`.
Public browsing and source downloads require no repository access grant. Never put a PAT
in plugin configuration or a repository URL. Core chat still needs the documented
Neovim, SQLite and configured ACP Agent prerequisites, but no companion binary.

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

Upstream contracts: [Release Please configuration](https://github.com/googleapis/release-please/blob/v17.3.0/docs/manifest-releaser.md),
[App token action](https://github.com/actions/create-github-app-token/tree/bcd2ba49218906704ab6c1aa796996da409d3eb1),
[directory exclusion implementation](https://github.com/googleapis/release-please/blob/v17.3.0/src/util/commit-exclude.ts),
[action credentials](https://github.com/googleapis/release-please-action/tree/5c625bfb5d1ff62eadeeb3772007f7f66fdcf071#github-credentials),
[workflow triggering](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow),
and [immutable releases](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases).
