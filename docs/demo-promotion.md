# Interactive Demo promotion checklist

The `/demo/` asset set is promoted manually. Do not update a pin or publish a
new build until all four journeys below pass against the deployed route. Record
the deployed URL, artifact manifest hash, tester, UTC time, and evidence link
in the release record.

## Immutable inputs

| Asset | Version | Commit | Archive SHA256 |
| --- | --- | --- | --- |
| Neovim WASM | `v0.13.0-dev-1531+g48864161cd` | `48864161cd75ae4b58f7af94d6c9add0ba876107` | `851e16a89a159750af9d4d85cc0edcc309090fd9b453c74294bfa1161238e2b2` |
| Snacks | `v2.31.0` | `e6fd58c82f2f3fcddd3fe81703d47d6d48fc7b9f` | `171fa47c5751d07d1e5b2fbde24327e9a94bf84e2ca400601f8ec39699de6cd5` |
| which-key | `v3.17.0` | `fcbf4eea17cb299c02557d576f0d568878e354a4` | `6059cef541dd8d13abb19d6465a76bf447ca23b9b5f17dbbf5a4c251852b9941` |

`demo/assets.lock.json` is authoritative. The build verifies each archive and
license digest and writes hashes for every published demo file to
`demo/asset-manifest.json`. Snacks receives only the two recorded
LuaJIT-to-Neovim OS-check substitutions needed by the WASM build; source-shape
drift fails the build.

The Neovim archive is copied from the recorded upstream asset into the
commit-addressed project package because upstream replaces numeric assets on
every nightly release. CI authenticates with `CI_JOB_TOKEN`; local builds need
`LOUISELM_DEMO_PACKAGE_TOKEN` with package-read access.

## Deployed journey matrix

Leave Result as `pending` until the entire row passes on the deployed route.

| Profile | Language | Result | Evidence |
| --- | --- | --- | --- |
| Core | English | pending | |
| Core | Simplified Chinese | pending | |
| Enhanced | English | pending | |
| Enhanced | Simplified Chinese | pending | |

For each row, start in a fresh browser context and verify:

1. The summary identifies the guided Agent behavior as scripted, and the
   Neovim conversation retains its scripted-demo labels.
2. An arbitrary non-empty prompt opens the real LouiseLM diff review. Reject
   once and confirm the file is unchanged; retry, accept, and confirm the
   browser-local file now contains `left + right`.
3. Resume the seeded Session, create a Session on the other Agent, switch back,
   inspect the simulated reached limit, edit and submit the Handoff review, and
   return to the retained source Session.
4. Guide progress advances from observed Neovim state. Assisted entry stops
   before Enter; Skip and Reset remain available.
5. Completion names the practiced behaviors. Installation through onboarding
   is the sole primary action; replay and cross-profile replay are secondary.
6. Cross-profile replay returns to step one with one fresh Session. Core uses
   the native numbered selector and never requests `vendor-bundle.js`.
   Enhanced uses the searchable Snacks picker, shows LouiseLM descriptions
   after `Space l`, and never loads `snacks.util.spawn`.
7. The compact guide and complete Neovim panel fit without page scrolling at
   browser content sizes 1920×1030, 1440×900, and 1280×720. Resize while on the
   longest instruction and on completion; confirm all controls and Neovim's
   bottom status line remain visible. Neovim buffer scrolling stays inside
   Neovim. The static mobile/unsupported fallback may scroll normally.

The required GitLab `playwright` job downloads the candidate `site-build`
artifact and runs the layout regression before `site-deploy` can be started.
It uses the mirrored to-be-continuous component at
`c9ddc28361794d4103989501b6658f47f96236f3` (1.10.0), with Playwright Test
1.63.0 and the matching digest-pinned browser image. The component owns npm
installation, caching and JUnit reporting. Job rules require it on branch
and merge-request pipelines, including drafts; failure blocks deployment.

To run the same check locally against a built artifact:

```bash
npm ci
npx playwright install chromium
npm run site:build
npm run site:test:browser
```

Playwright starts and stops `scripts/serve-demo.mjs` on loopback, serving only
`_build/html` with the isolation headers below. Set `LOUISELM_SITE_ARTIFACT`
to an extracted CI artifact directory when testing a downloaded build.
The tests never use the component's environment URL or the production site.

The four profile/language tests walk guide steps using Skip at all three
sizes: 132 layout checks, including completion. They assert real Neovim startup,
document overflow, guide controls, and rendered Neovim rows/columns. Failures
exit nonzero and produce JUnit, screenshots and traces under `reports/`, kept
as GitLab artifacts. This gate supplements the real-action journey above;
Skip does not certify prompting, permissions, Resume or Handoff behavior.

## Privacy and fallback audit

- Confirm all runtime requests are same-origin static `GET` requests. After
  entering a unique prompt marker, verify no new request contains it in a URL,
  header, or body: there must be no prompt egress.
- Confirm the page requests no credentials and reads no Provider token or
  environment value.
- Confirm cookies, `localStorage`, `sessionStorage`, and IndexedDB contain no
  demo state before and after the journey: there is no persistence. Normal
  browser caching of immutable assets is allowed.
- Confirm there is no client analytics request or analytics script in the
  artifact.
- Confirm project reads and writes remain under the Neovim WASM in-memory
  filesystem. The demo must have no host filesystem access.
- At a mobile width and again without cross-origin isolation, confirm the
  localized static walkthrough and installation link are usable and that no
  WASM/runtime asset is requested.
- Confirm `/demo/**` responses carry
  `Cross-Origin-Opener-Policy: same-origin` and
  `Cross-Origin-Embedder-Policy: require-corp`.

Promotion is blocked by any failed or pending row, changed digest, unexpected
network request, retained demo state, missing fallback, or missing security
header. Update pins only in a reviewed change and rerun the complete matrix;
never resolve a moving `main`, nightly URL, package registry tag, or CDN asset
at runtime.
