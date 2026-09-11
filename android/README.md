# LouiseLM Android capture

The Android app is a small offline-first recorder, not a workflow UI. One tap
starts speech capture; stopping atomically publishes immutable audio and
metadata in app-private storage. Pairing and upload can happen later.

## Build

Install Android SDK Platform 37.0, Build Tools 36.0.0, and Platform Tools. Gradle
must be able to find that SDK through `ANDROID_HOME` or an ignored
`local.properties` file in this directory:

```properties
sdk.dir=/absolute/path/to/Android/Sdk
```

Connect an API 28+ device, then run:

```sh
./gradlew test lint assembleDebug
./gradlew installDebug
```

### Side-by-side physical QA

Use the `qa` build when the normal `dev.louiselm.capture` app contains real
pairing or recordings. It uses the same runtime code, with package ID
`dev.louiselm.capture.qa`, label **LouiseLM QA**, and separate app storage and
Android Keystore identity. Debug and release package IDs remain unchanged.
No second Android user or Google account is required.

```sh
./gradlew test lint lintQa assembleDebug assembleQa
# Verify this exact APK before installing; never substitute app-debug.apk.
apkanalyzer manifest application-id app/build/outputs/apk/qa/app-qa.apk
adb -s DEVICE_SERIAL install --user 0 app/build/outputs/apk/qa/app-qa.apk
```

The package check must print `dev.louiselm.capture.qa`. Install without `-r`
for a fresh fixture: an existing QA installation must not be overwritten or
cleared without deciding whether its data is disposable. Open **LouiseLM QA**,
verify it is unpaired with zero recordings, and pair only to the disposable
QA receiver. Preserve the original app, its data, and the production receiver.
This bypasses the need for secondary-user enrollment; it does not repair an
OEM setup-wizard failure. Run the same physical acceptance cases below.

`test` includes pure JUnit tests and Robolectric Activity/View/lifecycle tests
on the JVM; no device is needed for those tests. Activity fixtures retain
Android 14/API 34 physical-regression coverage and exercise receiver permissions
on Android 17/API 37. They use isolated app storage, a shadow recorder, and
separately controlled background/UI queues. Robolectric is pinned to 4.17
for API 37 support; its SharedSecrets module export applies only to JVM tests.
Run just that regression with:

```sh
./gradlew :app:testDebugUnitTest --tests dev.louiselm.capture.MainActivityTest
```

Kotlin warnings and Android Lint warnings/errors fail the build. Narrow exceptions
document receiver SPKI trust and checked credential persistence.
CI uses JDK 25 and runs `test lint lintQa assembleDebug assembleQa`, then checks
the packaged QA application ID;
physical checks remain necessary for hardware, pairing/Keystore and real
background scheduling. Development rules are in
[AGENTS.md](AGENTS.md).

The app uses the system camera to take a full-resolution image of the terminal
QR and bundled ML Kit to decode it offline. Pairing and uploads use HTTPS with
the stable receiver public-key identity carried by the version-2 one-time QR;
routine TLS certificate renewal with that key remains trusted. The long-lived
device credential and receiver identity are encrypted by Android Keystore.
WorkManager retries only network, timeout, rate-limit, and server failures.

On Android 17+, Pair, Sync, and Refresh request Nearby devices permission for
direct access to the private receiver. Denial or revocation leaves offline
recording available. Grant access in app settings and retry the receiver action;
background uploads stop with an actionable failure while permission is missing.
The permission flow is framework-tested; physical Android 17 receiver access,
QR scanning, and recording have not yet been verified.

App data opts out of Android backup. Android 12+ extraction rules explicitly
exclude all app storage domains from cloud backup and device-to-device transfer,
including recordings and device-bound pairing credentials. Older devices retain
`allowBackup=false`. The packaged policy is regression-tested; OEM transfer
behavior has not been physically verified. See
[Android backup rules](https://developer.android.com/identity/data/autobackup).

Configure one private receiver profile before running
`:LouiselmCapturePair`; the receiver refuses pairing while it is loopback-only.
The Android app does not support version-1 exact-certificate pairing offers.

Each pending capture records its receiver-key owner separately from the
immutable audio and capture manifest. Captures made before pairing remain
unowned until the first-pair confirmation states their count. Scanning a QR
with the same receiver identity offers **Keep pairing** or **Renew pairing**.
Keep pairing verifies `/v1/health` through the existing pin and changes only the
endpoint, preserving the device credential and queue. After revocation, scan a
fresh QR and choose Renew pairing: it exchanges the one-use offer for a new
credential and immediately retries pending uploads without moving ownership or
changing original recordings/capture IDs. A rejected or expired offer leaves
the saved pairing and recordings unchanged; Cancel performs neither operation.
Moving pending captures to a different receiver identity requires a separate
confirmation that states the affected count; reassignment resets upload retry
state but never changes or removes the original audio or capture UUID.

The phone status shows the current endpoint, pending count and oldest age, last
successful sync, and the latest actionable upload failure. **Sync now** remains
the manual WorkManager retry path.

When paired, **Attention inbox** reads the authenticated `/v1/attention`
snapshot through the same pinned receiver identity. It displays only the
closed typed kind, fixed reason, bounded Session or Run identifier, optional
linked Run or stage, and age. It has no mutation controls; transient network
failures retain the last safely parsed screen and offer refresh, while revoked
pairing or pin failures require pairing/operator repair. The seen generation is
stored only in app-private preferences.

## Physical acceptance checklist

- On Android 17, deny Nearby devices access from Pair or Sync; verify offline
  recording still saves. Grant access and retry Pair, Sync, and Refresh. Revoke
  access with pending captures; verify originals remain and Sync resumes after
  access is restored.
- Record while offline, stop, force-stop the app, reopen it, and confirm the
  capture remains queued.
- Start recording, background the app, and confirm `onStop` saves the capture.
- Pair from `:LouiselmCapturePair`, then inspect the same UUID in
  `:LouiselmCaptureInbox`.
- Interrupt an upload, reconnect, and confirm the receiver contains one capture
  with that UUID rather than duplicates.
- Reboot the phone with queued work and confirm WorkManager eventually uploads
  after connectivity returns.
- Revoke the device with `louiselm-capture revoke-device`, record again, and
  confirm upload needs operator attention while the original remains local.
- Scan a fresh QR for that same receiver and choose Renew pairing; verify a
  fresh device registration and successful upload of the retained recording
  under its original UUID. Its local audio and manifest must remain unchanged.
- Renew the receiver certificate with the same key and confirm uploads continue;
  replace the receiver key and confirm the app rejects the new identity.
- Change the configured URL while retaining the receiver key, scan a fresh QR,
  choose Keep pairing, and confirm the endpoint changes without a new device credential or queue
  reassignment.
- With pending captures, scan a QR for a different receiver key. Cancel once
  and confirm ownership is unchanged; confirm once and verify the displayed
  count migrates while every original UUID and audio file remains intact.

Automated tests never use a real microphone, receiver, credential, or network.
