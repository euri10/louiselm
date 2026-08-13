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
the exact certificate fingerprint carried by that one-time QR; the long-lived
device credential is encrypted by Android Keystore. WorkManager retries only
network, timeout, rate-limit, and server failures.

## Physical acceptance checklist

- Record while offline, stop, force-stop the app, reopen it, and confirm the
  capture remains queued.
- Start recording, background the app, and confirm `onStop` saves the capture.
- Pair from `:LouiselmCapturePair`, then inspect the same UUID in
  `:LouiselmInbox`.
- Interrupt an upload, reconnect, and confirm the receiver contains one capture
  with that UUID rather than duplicates.
- Reboot the phone with queued work and confirm WorkManager eventually uploads
  after connectivity returns.
- Revoke the device with `louiselm-capture revoke-device`, record again, and
  confirm upload needs operator attention while the original remains local.
- Change the receiver certificate and confirm the app rejects it rather than
  trusting the host or a replacement self-signed certificate.

Automated tests never use a real microphone, receiver, credential, or network.
