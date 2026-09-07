# LouiseLM Android Development Contract

This contract adds Kotlin/Android rules to the repository-root `AGENTS.md`.
Root rules for Beads, attribution, ownership, vocabulary, TDD, live acceptance,
and surgical commits still apply. Do not duplicate or weaken them here.

## Scope and product contract

This app is LouiseLM's default mobile speech-capture surface. It records raw
thought streams quickly, stores the original audio in app-private storage, and
uploads retry-safely to a paired `louiselm-capture` receiver. It does not sort,
summarize, split, or delete ideas. Those are later workflow stages.

The app is early-stage and has no external users. Prefer direct breaking
changes over compatibility layers. Priority is correctness and data safety,
then clarity, simplicity, and performance.

## Kotlin and Android baseline

- Use Kotlin with the Android Gradle Plugin's built-in Kotlin support.
- Target the latest configured Android SDK and keep `minSdk` at 28 unless a
  concrete product requirement changes it.
- Prefer Android platform APIs, Java/Kotlin standard-library facilities, and
  the dependencies already in `app/build.gradle.kts`.
- Keep the Gradle wrapper, Android Gradle Plugin, SDK levels and dependency
  versions explicit. Use the wrapper, not a machine's global Gradle. Do not
  upgrade tooling or raise `minSdk` incidentally to make a task compile.
- Check Java/Kotlin library calls against Android API availability, not just the
  host JDK. Use supported Android APIs; do not assume a newer desktop JDK makes
  those APIs available on the device.
- Use programmatic platform Views for this deliberately small app. Do not add
  Compose, dependency injection, a database, coroutines, or an HTTP client
  unless the existing platform implementation has a demonstrated gap.
- WorkManager owns durable background upload scheduling. MediaRecorder owns
  capture. Android Keystore owns at-rest pairing credentials.
- New runtime or development dependencies require a demonstrated gap and
  explicit maintainer approval. JUnit owns pure JVM tests; the approved
  test-only Robolectric dependency owns framework-backed JVM tests. Do not add
  overlapping mocking, formatting, lint or test frameworks by default.

## Architecture and lifecycle

- Keep capture files and their state in `CaptureStore`; keep pairing secrets in
  `PairingStore`; keep HTTPS and receiver-identity pinning in `PinnedHttps`.
- Activities own presentation and permission prompts, not durable state.
- Keep parsing, validation and state transitions independent of Views, Context,
  filesystem and network effects where practical. Prefer existing boundaries
  and plain functions over repositories/use-cases/interfaces with one caller.
- Keep mutable state with its lifecycle owner. No global Activity, recorder,
  executor, credential or mutable service locator. Prefer `private` or `internal`
  declarations; framework entrypoints are not a reason to expose their helpers.
- Workers must be restart-safe and idempotent. Never assume a callback or a
  process survives app termination.
- Do not hold an Activity, View, Bitmap, MediaRecorder, or Context beyond its
  lifecycle. Use `applicationContext` for durable helpers.
- Keep network and filesystem work off the main thread. UI changes return to
  the main thread explicitly.
- Every executor, task, observer, stream, cursor and recorder has an owner and a
  release path. Use `use` for closeable resources and `finally` where it owns
  cleanup. Never block the main thread on sleeps, polling, locks or futures.
- Check lifecycle validity when a queued UI callback executes, not only when
  it is submitted. Late completion after destruction must not touch Views,
  restart work, or erase a successfully stored capture. Remove observers when
  their owning lifecycle ends; cancellation is not permission to discard data.
- Recompute queue status from the durable store; do not maintain a second
  counter in the Activity. Capture completion must render its acknowledgement
  and current queue without depending on a later `onResume`.
- An executor serializes only its own tasks. Treat Activity callbacks and
  WorkManager completions as concurrent; test both relevant completion orders.
  Keep durable-state critical sections short and document lock ordering. Do not
  hold locks across network I/O or invoke external/UI callbacks while locked.

## Data and security invariants

- The original recording is immutable after successful capture and is never
  automatically deleted, including after upload or transcription.
- Write new captures through a hidden incoming directory and publish them with
  one rename only after audio and metadata are complete.
- Reuse one UUID for every retry of the same recording. A successful duplicate
  upload is not a second idea.
- Accept pairing only from an HTTPS QR payload. Pin the SHA-256 fingerprint of
  the receiver SubjectPublicKeyInfo from that one-time payload for pairing and
  uploads; routine certificate renewal must retain that key.
- Store the device credential encrypted with an Android Keystore key. Never log
  credentials, QR payloads, transcripts, audio, request bodies, or environment
  data.
- Bound response reads and use explicit connection/read timeouts.
- Treat certificate mismatch and malformed/authentication responses as
  operator-action failures; retry network, rate-limit, and server failures.
- Validate untrusted JSON, QR payloads, URIs, intent extras, persisted manifests
  and receiver responses before effects. Bound sizes, lengths, counts and
  numeric conversions. Missing or malformed values are not permissive defaults.
- Keep file access inside the intended app-private root. Validate identifiers,
  reject traversal and unexpected ownership, and preserve atomic publication.
  Check failed writes/renames; never acknowledge durability before it exists.
- Do not replace SPKI pinning with trust-all TLS or an allow-all hostname
  verifier. Preserve the existing receiver-key authority when an endpoint
  changes. Never log decrypted pairing data or grant broad file/URI access to
  fix a test. Keep exported components and temporary URI grants no broader than
  required.

## Kotlin style and errors

- Use `UpperCamelCase` for types, `lowerCamelCase` for functions/properties, and
  `UPPER_SNAKE_CASE` for constants.
- Prefer small final classes and top-level pure functions. Add an interface only
  for a real alternate implementation or test boundary.
- Follow Android's Kotlin style: four-space indentation, explicit imports,
  readable expressions and guard clauses. Do not nest scope functions or use
  clever chains where ownership/control flow becomes harder to follow.
- Prefer `val`, immutable data and read-only collections. They do not imply deep
  immutability: copy at an ownership boundary when mutation can cross threads.
- Model absent values with nullable types and mutually exclusive states with
  enums/sealed types. No production `!!` or unchecked casts. Use `lateinit`
  only for a lifecycle-established invariant; do not use it to hide optional
  or asynchronously initialized state.
- Reserve `check`/`checkNotNull`/`error` for documented internal invariants.
  Expected input, permission, storage and transport failures must become typed
  outcomes or a deliberately caught exception at their existing boundary.
  JVM `assert` is not external-input validation; it can be disabled.
- Do not swallow failures with `getOrNull`, empty catches or default success.
  A best-effort cleanup exception needs a local explanation of why data and
  security remain safe. `runCatching` catches `Throwable`: do not accidentally
  turn cancellation, interruption or fatal VM errors into ordinary retry results.
- Preserve causes internally and sanitize at presentation boundaries. Never
  display raw remote bodies, credential-bearing URLs, or exception dumps.
- Document reusable/public Kotlin boundaries with KDoc: purpose, parameters,
  failure outcomes, threading, ownership and lifecycle obligations where relevant.
  Explain constraints and decisions in comments, not the syntax. No speculative
  TODOs, reflection-based production dispatch, or new compatibility shims.

## User interface

- Put user-facing text in string resources, use format arguments instead of
  assembling translated fragments, and use density-independent layout values.
- Preserve readable labels, adequate touch targets, focus, accessibility and
  disabled/busy states. Permission denial and cancellation must leave a usable
  screen. Do not request unrelated permissions or show success before durable
  completion.
- A status snapshot, a saved-capture acknowledgement and an error are different
  facts. Preserve their intended composition; do not let one callback silently
  overwrite a newer state from another operation.

## Testing and completion

- Use red-green-refactor for behavior changes. For coverage of an already-fixed
  defect, prove the new test fails against the displaced behavior, then restore
  the fix. Documentation/build-only changes do not need an artificial red test.
- Put pure protocol/state tests and Robolectric tests in `app/src/test`.
  Robolectric exercises real Activity/View/lifecycle wiring; it is not a reason
  to mock away the code under test. Use instrumentation/physical QA for behavior
  the JVM cannot establish, including hardware, Keystore and real scheduling.
- Assert user-visible behavior and durable outcomes through real entrypoints.
  A test that only checks a helper's returned string does not prove that the
  Activity calls it. Keep any reflection or custom shadows confined to narrow
  test-fixture boundaries; never use them to invoke private behavior under test.
- Control background executors, main-loop queues and time separately. Background
  callbacks must actually run off the main thread; synchronous invocation alone
  does not cover scheduling. Avoid wall-clock sleeps and race-dependent polling.
- Cover missing optional configuration, malformed inputs, failure/cancellation,
  Activity destruction, queued callbacks, and relevant completion orderings.
  Preserve coverage of unchanged contracts; do not delete tests to make a
  refactor green.
- Do not use live receivers, credentials, microphones, or networks in automated
  tests. Use only isolated test storage and dummy bytes; never wipe or reinstall
  over the maintainer's app data as test setup. Resolve build dependencies before
  testing; dependency downloads are not permission for live service calls.
- Run focused tests while developing and `./gradlew test lint assembleDebug`
  before handoff. Kotlin compiler warnings and Android Lint errors fail the
  build. Resolve warnings in changed code; no new baselines, global suppression
  or `ignoreFailures` to make gates green. A narrow suppression names the
  diagnostic and explains the unavoidable boundary.
- Transitional gate gap: existing Android Lint warnings are visible but not
  fatal. `louiselm-16a6` tracks their review and enabling `warningsAsErrors`.
  Do not describe lint
  as warning-clean until that work lands, or silently upgrade SDKs/dependencies
  or weaken security to satisfy an advisory.
- CI runs the same Android gates. Report unavailable/failed checks; do not claim
  that merely configuring a gate proves it passed. Compiler, Android Lint and
  JUnit/Robolectric own their separate checks; no unapproved overlapping tools.
- For physical acceptance, select the affected cases from `README.md`; a local
  UI-only change does not require redoing unrelated pairing/revocation flows.
  Record the installed APK/build, exact capture UUID and the human-observed
  behavior. Green JVM tests do not replace an available exact live reproduction.

Reference conventions: [Android Kotlin style](https://developer.android.com/kotlin/style-guide),
[Kotlin compiler options](https://kotlinlang.org/docs/gradle-compiler-options.html),
and [Robolectric setup](https://robolectric.org/getting-started/).
