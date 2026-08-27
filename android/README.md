# LouiseLM Android capture

The Android app is a small offline-first recorder, not a workflow UI. One tap
starts speech capture; stopping atomically publishes immutable audio and
metadata in app-private storage. Pairing and upload can happen later.

## Build

Install Android SDK Platform 36, Build Tools 36.0.0, and Platform Tools. Gradle
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

The app uses the system camera to take a full-resolution image of the terminal
QR and bundled ML Kit to decode it offline. Pairing and uploads use HTTPS with
the stable receiver public-key identity carried by the version-2 one-time QR;
routine TLS certificate renewal with that key remains trusted. The long-lived
device credential and receiver identity are encrypted by Android Keystore.
WorkManager retries only network, timeout, rate-limit, and server failures.

Configure one private receiver profile before running
`:LouiselmCapturePair`; the receiver refuses pairing while it is loopback-only.
The Android app does not support version-1 exact-certificate pairing offers.

Each pending capture records its receiver-key owner separately from the
immutable audio and capture manifest. Captures made before pairing remain
unowned until the first-pair confirmation states their count. Scanning a QR
with the same receiver identity verifies `/v1/health` through the existing pin
and changes only the endpoint, preserving the device credential and queue.
Moving pending captures to a different receiver identity requires a separate
confirmation that states the affected count; reassignment resets upload retry
state but never changes or removes the original audio or capture UUID.

The phone status shows the current endpoint, pending count and oldest age, last
successful sync, and the latest actionable upload failure. **Sync now** remains
the manual WorkManager retry path.

## Physical acceptance checklist

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
- Renew the receiver certificate with the same key and confirm uploads continue;
  replace the receiver key and confirm the app rejects the new identity.
- Change the configured URL while retaining the receiver key, scan a fresh QR,
  and confirm the endpoint changes without a new device credential or queue
  reassignment.
- With pending captures, scan a QR for a different receiver key. Cancel once
  and confirm ownership is unchanged; confirm once and verify the displayed
  count migrates while every original UUID and audio file remains intact.

Automated tests never use a real microphone, receiver, credential, or network.
