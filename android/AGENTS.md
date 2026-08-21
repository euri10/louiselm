# LouiseLM Android Development Contract

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
- Use programmatic platform Views for this deliberately small app. Do not add
  Compose, dependency injection, a database, coroutines, or an HTTP client
  unless the existing platform implementation has a demonstrated gap.
- WorkManager owns durable background upload scheduling. MediaRecorder owns
  capture. Android Keystore owns at-rest pairing credentials.

## Architecture and lifecycle

- Keep capture files and their state in `CaptureStore`; keep pairing secrets in
  `PairingStore`; keep HTTPS and receiver-identity pinning in `PinnedHttps`.
- Activities own presentation and permission prompts, not durable state.
- Workers must be restart-safe and idempotent. Never assume a callback or a
  process survives app termination.
- Do not hold an Activity, View, Bitmap, MediaRecorder, or Context beyond its
  lifecycle. Use `applicationContext` for durable helpers.
- Keep network and filesystem work off the main thread. UI changes return to
  the main thread explicitly.

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

## Kotlin style and errors

- Use `UpperCamelCase` for types, `lowerCamelCase` for functions/properties, and
  `UPPER_SNAKE_CASE` for constants.
- Prefer small final classes and top-level pure functions. Add an interface only
  for a real alternate implementation or test boundary.
- Avoid `!!`, unchecked casts, broad `catch (Exception)` without classification,
  and swallowed failures. Expected failures become explicit results or visible
  queue state.
- Validate all QR, JSON, filesystem, and network input before changing durable
  state. Error messages shown to users must be actionable and contain no
  secrets.

## Testing and completion

- Put pure protocol/state tests in `app/src/test`; use instrumentation only for
  Android framework behavior that cannot be isolated.
- Do not use live receivers, credentials, microphones, or networks in automated
  tests.
- Before handoff run `./gradlew test lint assembleDebug` when an Android SDK is
  available. Report unavailable gates honestly.
- Physical acceptance requires: record offline, restart, pair, upload, retry an
  interrupted upload with the same UUID, inspect the capture in Neovim, revoke
  the phone, and confirm later uploads are rejected while original audio remains.
