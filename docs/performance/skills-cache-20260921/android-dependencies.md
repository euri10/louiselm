# Android dependency-cache prerequisite

Issue: `louiselm-cbc5d`. Separately authorized after the unchanged cache
experiment stopped on Maven Central HTTP 429; see `android-baseline-failure.log`.

## Change and limits

Reuse `actions/cache@v4`, already used in this workflow, to persist Gradle's
downloaded dependency artifacts/metadata and checksum-verified wrapper
distribution. Do not cache project build outputs, machine configuration or
credentials. Exclude dependency-cache lock and garbage-collection state.

The key includes runner OS, wrapper configuration and Gradle build inputs.
A dependency-only change can restore the preceding compatible wrapper's cache;
Gradle still resolves missing artifacts normally. A wrapper change starts a
separate cache namespace. No repository, dependency, SDK, retry policy, test,
lint or build task changes.

This reduces repeated requests, not upstream availability risk: a cold miss,
eviction or newly required artifact can still hit a rate limit. Do not claim
that a green build proves HTTP 429 can never recur. No test/build failure is
automatically retried or suppressed.

Gradle documents relocatable `modules-2` caches and excluding locks and
`gc.properties` in its [dependency-cache guide](https://docs.gradle.org/current/userguide/dependency_caching.html#sec:copying_dependency_cache).

## Acceptance

Build-only change: no artificial red test. The retained unchanged hosted
failure is the original evidence. A parsed workflow comparison verifies that
removing the added cache step produces exactly the original workflow.

- Local: `ANDROID_HOME=/home/lotso/Android/Sdk ./gradlew --no-daemon test lint lintQa assembleDebug assembleQa`
  in the isolated worktree: **passed in 57s**, 98/98 tasks executed;
  JUnit XML reports 69 tests, zero failures/errors/skips. QA APK application ID
  verified as `dev.louiselm.capture.qa`. See `android-local.log`.
- Hosted cold seed: run `35563981064`, revision `de69fc1d3b0ea90e488febc5e2b731ce15991f3d`.
  All 13 jobs passed. Android job: 200s; Gradle: 3m 1s, 98/98 tasks executed.
  Cache miss followed by successful save (647,924,025 bytes).
- Hosted reuse: run `35564328417`, identical revision, dispatched only after
  the entire seed workflow passed. Android job: 160s, 98/98 tasks executed;
  exact-key cache hit. All 13 jobs passed again. See `android-warm.log` and
  retained run metadata.

The observed 40s Android job reduction is one cold/warm pair, not a repeated
performance estimate. Acceptance is artifact reuse with all gates preserved,
not a statistically established speedup or proof against future HTTP 429s.
Logs here contain bounded evidence excerpts, not full runner logs.

These are Android prerequisite checks, not new skills-cache candidates or
accepted optimization samples. The skills-cache campaign remains at two of
three candidates consumed, with three of its ten hosted executions used.
Record any revised run budget explicitly before resuming that campaign.
